use super::changes_tab::{ChangedFile, FileStatus};
use super::diff_engine::{self, DiffStats, FileDiff};
use shuru_sdk::AsyncSandbox;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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

pub struct DiffResult {
    pub dirty_paths: HashSet<String>,
    /// Files that were updated or newly added in this batch.
    pub updated_files: HashMap<String, ChangedFile>,
    pub updated_diffs: HashMap<String, FileDiff>,
    /// Paths whose diff was removed (file reverted to original).
    pub removed_paths: HashSet<String>,
}

/// Spawns a bridge thread that watches sandbox FS events, computes diffs,
/// and sends results over a flume channel.
pub struct WatchBridge {
    _handle: std::thread::JoinHandle<()>,
}

impl WatchBridge {
    pub fn new(
        sandbox: Arc<AsyncSandbox>,
        tokio_handle: tokio::runtime::Handle,
        workspace_path: String,
        host_mount_path: Option<String>,
    ) -> Option<(Self, flume::Receiver<DiffResult>)> {
        let (tx, rx) = flume::unbounded::<DiffResult>();

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
                    let mut cached_diffs: HashMap<String, FileDiff> = HashMap::new();
                    // Track last-seen sandbox content to skip no-op updates
                    // (scratch sandboxes have no host file, so watcher events
                    // would otherwise re-send the same diff every time).
                    let mut last_content: HashMap<String, Vec<u8>> = HashMap::new();

                    let gitignore = build_ignore_matcher(host_mount_path.as_deref());

                    loop {
                        let event = match watch.receiver.recv().await {
                            Some(e) => e,
                            None => break,
                        };

                        let mut raw_paths = vec![event.path];
                        while let Ok(ev) = watch.receiver.try_recv() {
                            raw_paths.push(ev.path);
                        }

                        let mut dirty: HashSet<String> = HashSet::new();
                        let ignore_root = std::path::Path::new(
                            host_mount_path.as_deref().unwrap_or("/workspace"),
                        );
                        for p in &raw_paths {
                            if let Some(rel) = p.strip_prefix(&prefix) {
                                let full = ignore_root.join(rel);
                                if gitignore.matched_path_or_any_parents(&full, false).is_ignore() {
                                    continue;
                                }
                                dirty.insert(rel.to_string());
                            }
                        }

                        let mut updated_files = HashMap::new();
                        let mut updated_diffs = HashMap::new();
                        let mut removed_paths = HashSet::new();

                        for path in &dirty {
                            let old = match host_mount_path.as_deref() {
                                Some(host) => diff_engine::read_host_file(path, host).await,
                                None => None,
                            };
                            let new = diff_engine::read_sandbox_file(path, "/workspace", &sandbox).await;

                            // Skip if sandbox content unchanged since last report
                            if let Some(new_bytes) = &new {
                                if last_content.get(path).map_or(false, |prev| prev == new_bytes) {
                                    continue;
                                }
                            }

                            let status = match (&old, &new) {
                                (Some(o), Some(n)) if o == n => {
                                    cached_files.remove(path);
                                    cached_diffs.remove(path);
                                    last_content.remove(path);
                                    removed_paths.insert(path.clone());
                                    continue;
                                }
                                (Some(_), Some(_)) => FileStatus::Modified,
                                (None, Some(_)) => FileStatus::Added,
                                (Some(_), None) => {
                                    last_content.remove(path);
                                    FileStatus::Deleted
                                }
                                (None, None) => {
                                    cached_files.remove(path);
                                    cached_diffs.remove(path);
                                    last_content.remove(path);
                                    removed_paths.insert(path.clone());
                                    continue;
                                }
                            };

                            let old_bytes = old.unwrap_or_default();
                            let new_bytes = new.unwrap_or_default();
                            last_content.insert(path.clone(), new_bytes.clone());
                            let diff = diff_engine::compute_file_diff(&old_bytes, &new_bytes);

                            let file = ChangedFile {
                                path: path.clone(),
                                status,
                                diff_stats: Some(DiffStats {
                                    additions: diff.additions,
                                    deletions: diff.deletions,
                                }),
                            };
                            cached_files.insert(path.clone(), file.clone());
                            cached_diffs.insert(path.clone(), diff.clone());
                            updated_files.insert(path.clone(), file);
                            updated_diffs.insert(path.clone(), diff);
                        }

                        let result = DiffResult {
                            dirty_paths: dirty,
                            updated_files,
                            updated_diffs,
                            removed_paths,
                        };
                        if tx.send(result).is_err() {
                            break;
                        }
                    }
                });
            })
            .ok()?;

        Some((Self { _handle: handle }, rx))
    }
}
