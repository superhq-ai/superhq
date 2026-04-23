use super::changes_tab::{ChangedFile, FileStatus};
use futures_util::future::join_all;
use shuru_sdk::AsyncSandbox;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Notify;

/// Patterns always ignored in the review panel, regardless of .gitignore.
const BUILTIN_IGNORE_PATTERNS: &[&str] = &[
    ".git",
    "node_modules",
    "__pycache__",
    ".venv",
    "target/",
    ".DS_Store",
    ".next",
    "dist/",
    "build/",
];

/// Build a gitignore matcher that combines .gitignore, global gitignore,
/// and our built-in ignore patterns.
fn build_ignore_matcher(host_mount_path: Option<&str>) -> ignore::gitignore::Gitignore {
    let root = host_mount_path.unwrap_or("/workspace");
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    for pattern in BUILTIN_IGNORE_PATTERNS {
        let _ = builder.add_line(None, pattern);
    }
    if let Some(host) = host_mount_path {
        builder.add(format!("{}/.gitignore", host));
    }
    if let Ok(home) = std::env::var("HOME") {
        let global = format!("{}/.config/git/ignore", home);
        if std::path::Path::new(&global).exists() {
            builder.add(&global);
        }
    }
    builder.build().unwrap_or_else(|_| {
        let mut b = ignore::gitignore::GitignoreBuilder::new(root);
        let _ = b.add_line(None, ".git");
        b.build().unwrap()
    })
}

#[derive(Clone)]
pub struct DiffResult {
    pub dirty_paths: HashSet<String>,
    pub updated_files: HashMap<String, ChangedFile>,
    pub removed_paths: HashSet<String>,
}

/// Spawns a bridge thread that watches sandbox FS events, determines file
/// status (Added/Modified/Deleted), and sends results over a flume channel.
/// Does NOT read file contents or compute diffs — those are done lazily on expand.
pub struct WatchBridge {
    stop: Arc<Notify>,
    _handle: std::thread::JoinHandle<()>,
}

impl Drop for WatchBridge {
    fn drop(&mut self) {
        self.stop.notify_one();
    }
}

impl WatchBridge {
    pub fn new(
        sandbox: Arc<AsyncSandbox>,
        tokio_handle: tokio::runtime::Handle,
        workspace_path: String,
        host_mount_path: Option<String>,
    ) -> Option<(Self, flume::Receiver<DiffResult>)> {
        let (tx, rx) = flume::unbounded::<DiffResult>();
        let stop = Arc::new(Notify::new());
        let stop_notify = stop.clone();

        let handle = std::thread::Builder::new()
            .name("shuru-watch-bridge".into())
            .spawn(move || {
                tokio_handle.block_on(async move {
                    let mut watch = match sandbox.open_watch(&workspace_path, true).await {
                        Ok(w) => w,
                        Err(_) => return,
                    };

                    let prefix = format!("{}/", workspace_path);
                    let mut cached_files: HashMap<String, ChangedFile> = HashMap::new();

                    let gitignore = build_ignore_matcher(host_mount_path.as_deref());

                    let ignore_root = std::path::Path::new(
                        host_mount_path.as_deref().unwrap_or("/workspace"),
                    );

                    loop {
                        // Block until event or stop signal
                        let event = tokio::select! {
                            e = watch.receiver.recv() => match e {
                                Some(e) => e,
                                None => break,
                            },
                            _ = stop_notify.notified() => break,
                        };

                        // Track the last event kind per path — DELETE wins over
                        // MODIFY so a `rm` isn't clobbered by a trailing
                        // attribute event from the same syscall.
                        let mut raw: HashMap<String, u8> = HashMap::new();
                        let mut push = |p: String, k: u8| {
                            let slot = raw.entry(p).or_insert(k);
                            if k == shuru_proto::watch_kind::DELETE {
                                *slot = k;
                            }
                        };
                        push(event.path, event.kind);

                        // Settle: drain until 50ms with no new events
                        loop {
                            while let Ok(ev) = watch.receiver.try_recv() {
                                push(ev.path, ev.kind);
                            }
                            match tokio::time::timeout(
                                std::time::Duration::from_millis(50),
                                watch.receiver.recv(),
                            ).await {
                                Ok(Some(ev)) => push(ev.path, ev.kind),
                                Ok(None) => return,
                                Err(_) => break,
                            }
                        }

                        let mut dirty: HashMap<String, u8> = HashMap::new();
                        for (path, kind) in raw.drain() {
                            if let Some(rel) = path.strip_prefix(&prefix) {
                                let full = ignore_root.join(rel);
                                if !gitignore.matched_path_or_any_parents(&full, false).is_ignore() {
                                    dirty.insert(rel.to_string(), kind);
                                }
                            }
                        }

                        if dirty.is_empty() {
                            continue;
                        }

                        let result = classify_changes(
                            dirty,
                            host_mount_path.as_deref(),
                            &mut cached_files,
                        ).await;

                        if !result.updated_files.is_empty() || !result.removed_paths.is_empty() {
                            if tx.send(result).is_err() {
                                break;
                            }
                        }
                    }
                });
            })
            .ok()?;

        Some((Self { stop, _handle: handle }, rx))
    }
}

async fn classify_changes(
    dirty: HashMap<String, u8>,
    host_mount_path: Option<&str>,
    cached_files: &mut HashMap<String, ChangedFile>,
) -> DiffResult {
    let host_probes = dirty.keys().map(|rel| {
        let rel = rel.clone();
        async move {
            let exists = host_file_exists(host_mount_path, &rel).await;
            (rel, exists)
        }
    });
    let host_exists_map: HashMap<String, bool> =
        join_all(host_probes).await.into_iter().collect();

    let mut updated_files = HashMap::new();
    let mut removed_paths = HashSet::new();

    for (rel, kind) in &dirty {
        let host_exists = host_exists_map.get(rel).copied().unwrap_or(false);
        let sandbox_exists = *kind != shuru_proto::watch_kind::DELETE;

        let status = match (sandbox_exists, host_exists) {
            (false, true) => FileStatus::Deleted,
            (false, false) => {
                cached_files.remove(rel);
                removed_paths.insert(rel.clone());
                continue;
            }
            (true, true) => FileStatus::Modified,
            (true, false) => FileStatus::Added,
        };

        let file = ChangedFile { path: rel.clone(), status };
        let prev_status = cached_files.get(rel).map(|e| e.status);
        cached_files.insert(rel.clone(), file.clone());
        let is_repeat_delete = status == FileStatus::Deleted
            && prev_status == Some(status);
        if !is_repeat_delete {
            updated_files.insert(rel.clone(), file);
        }
    }

    DiffResult {
        dirty_paths: dirty.into_keys().collect(),
        updated_files,
        removed_paths,
    }
}

async fn host_file_exists(host_mount_path: Option<&str>, rel: &str) -> bool {
    let Some(host) = host_mount_path else { return false };
    match tokio::fs::metadata(format!("{}/{}", host, rel)).await {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "superhq-watcher-test-{}-{}-{}",
                name,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            ));
            fs::remove_dir_all(&dir).ok();
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl std::ops::Deref for ScratchDir {
        type Target = Path;
        fn deref(&self) -> &Path { &self.0 }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            }
            fs::remove_dir_all(&self.0).ok();
        }
    }

    fn write_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn dirty(entries: &[(&str, u8)]) -> HashMap<String, u8> {
        entries.iter().map(|(p, k)| (p.to_string(), *k)).collect()
    }

    #[tokio::test]
    async fn modify_existing_file_is_modified() {
        let host = ScratchDir::new("mod-existing");
        write_file(&host.join("src/main.rs"), "fn main() {}");
        let mut cache = HashMap::new();

        let result = classify_changes(
            dirty(&[("src/main.rs", shuru_proto::watch_kind::MODIFY)]),
            Some(host.to_str().unwrap()),
            &mut cache,
        ).await;

        assert_eq!(
            result.updated_files.get("src/main.rs").map(|f| f.status),
            Some(FileStatus::Modified),
        );
    }

    #[tokio::test]
    async fn modify_missing_file_is_added() {
        let host = ScratchDir::new("add-missing");
        let mut cache = HashMap::new();

        let result = classify_changes(
            dirty(&[("src/new.rs", shuru_proto::watch_kind::MODIFY)]),
            Some(host.to_str().unwrap()),
            &mut cache,
        ).await;

        assert_eq!(
            result.updated_files.get("src/new.rs").map(|f| f.status),
            Some(FileStatus::Added),
        );
    }

    #[tokio::test]
    async fn delete_existing_file_is_deleted() {
        let host = ScratchDir::new("del-existing");
        write_file(&host.join("src/main.rs"), "fn main() {}");
        let mut cache = HashMap::new();

        let result = classify_changes(
            dirty(&[("src/main.rs", shuru_proto::watch_kind::DELETE)]),
            Some(host.to_str().unwrap()),
            &mut cache,
        ).await;

        assert_eq!(
            result.updated_files.get("src/main.rs").map(|f| f.status),
            Some(FileStatus::Deleted),
        );
    }

    #[tokio::test]
    async fn delete_missing_file_goes_to_removed_paths() {
        let host = ScratchDir::new("del-missing");
        let mut cache = HashMap::new();

        let result = classify_changes(
            dirty(&[("src/gone.rs", shuru_proto::watch_kind::DELETE)]),
            Some(host.to_str().unwrap()),
            &mut cache,
        ).await;

        assert!(result.updated_files.is_empty());
        assert!(result.removed_paths.contains("src/gone.rs"));
    }

    #[tokio::test]
    async fn scratch_workspace_modify_is_added() {
        let mut cache = HashMap::new();

        let result = classify_changes(
            dirty(&[("src/main.rs", shuru_proto::watch_kind::MODIFY)]),
            None,
            &mut cache,
        ).await;

        assert_eq!(
            result.updated_files.get("src/main.rs").map(|f| f.status),
            Some(FileStatus::Added),
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn permission_denied_is_not_misclassified_as_added() {
        use std::os::unix::fs::PermissionsExt;
        let host = ScratchDir::new("perm-denied");
        let locked = host.join("locked");
        fs::create_dir_all(&locked).unwrap();
        write_file(&locked.join("main.rs"), "fn main() {}");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let mut cache = HashMap::new();
        let result = classify_changes(
            dirty(&[("locked/main.rs", shuru_proto::watch_kind::MODIFY)]),
            Some(host.to_str().unwrap()),
            &mut cache,
        ).await;

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            result.updated_files.get("locked/main.rs").map(|f| f.status),
            Some(FileStatus::Modified),
            "permission-denied stat must not downgrade to Added",
        );
    }

    #[tokio::test]
    async fn repeated_delete_is_suppressed_from_updated_files() {
        let host = ScratchDir::new("repeat-del");
        write_file(&host.join("src/main.rs"), "fn main() {}");
        let mut cache = HashMap::new();
        cache.insert(
            "src/main.rs".to_string(),
            ChangedFile { path: "src/main.rs".to_string(), status: FileStatus::Deleted },
        );

        let result = classify_changes(
            dirty(&[("src/main.rs", shuru_proto::watch_kind::DELETE)]),
            Some(host.to_str().unwrap()),
            &mut cache,
        ).await;

        assert!(
            result.updated_files.is_empty(),
            "repeat DELETE must not re-emit an update",
        );
    }
}
