use super::changes_tab::{ChangedFile, FileStatus};
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
                    // Drop our Arc so the watcher doesn't keep the VM alive.
                    // The watch vsock stream is independent of the Arc.
                    drop(sandbox);

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

                        let mut raw_paths: Vec<(String, u8)> = vec![(event.path, event.kind)];

                        // Settle: drain until 50ms with no new events
                        loop {
                            while let Ok(ev) = watch.receiver.try_recv() {
                                raw_paths.push((ev.path, ev.kind));
                            }
                            match tokio::time::timeout(
                                std::time::Duration::from_millis(50),
                                watch.receiver.recv(),
                            ).await {
                                Ok(Some(ev)) => raw_paths.push((ev.path, ev.kind)),
                                Ok(None) => return,
                                Err(_) => break,
                            }
                        }

                        let mut dirty: HashMap<String, u8> = HashMap::new();
                        for (path, kind) in &raw_paths {
                            if let Some(rel) = path.strip_prefix(&prefix) {
                                let full = ignore_root.join(rel);
                                if !gitignore.matched_path_or_any_parents(&full, false).is_ignore() {
                                    dirty.insert(rel.to_string(), *kind);
                                }
                            }
                        }

                        if dirty.is_empty() {
                            continue;
                        }

                        let mut updated_files = HashMap::new();
                        let mut removed_paths = HashSet::new();

                        for (rel, kind) in &dirty {
                            let host_exists = match host_mount_path.as_deref() {
                                Some(host) => {
                                    let full = format!("{}/{}", host, rel);
                                    tokio::fs::metadata(&full).await.is_ok()
                                }
                                None => false,
                            };

                            let status = if *kind == shuru_proto::watch_kind::DELETE {
                                if host_exists {
                                    FileStatus::Deleted
                                } else {
                                    // File deleted in sandbox, never existed on host — remove
                                    cached_files.remove(rel);
                                    removed_paths.insert(rel.clone());
                                    continue;
                                }
                            } else if host_exists {
                                FileStatus::Modified
                            } else {
                                FileStatus::Added
                            };

                            if cached_files.get(rel).map_or(true, |e| e.status != status) {
                                let file = ChangedFile { path: rel.clone(), status };
                                cached_files.insert(rel.clone(), file.clone());
                                updated_files.insert(rel.clone(), file);
                            }
                        }

                        if !updated_files.is_empty() || !removed_paths.is_empty() {
                            let result = DiffResult {
                                dirty_paths: dirty.into_keys().collect(),
                                updated_files,
                                removed_paths,
                            };
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
