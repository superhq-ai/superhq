mod changes_tab;
pub mod diff_engine;
mod diff_view;

pub use changes_tab::ChangesTab;

use changes_tab::ChangesSnapshot;

use crate::ui::theme as t;
use gpui::*;
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
fn build_ignore_matcher(host_mount_path: &str) -> ignore::gitignore::Gitignore {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(host_mount_path);
    for pattern in BUILTIN_IGNORE_PATTERNS {
        let _ = builder.add_line(None, pattern);
    }
    builder.add(format!("{}/.gitignore", host_mount_path));
    if let Ok(home) = std::env::var("HOME") {
        let global = format!("{}/.config/git/ignore", home);
        if std::path::Path::new(&global).exists() {
            builder.add(&global);
        }
    }
    builder.build().unwrap_or_else(|_| {
        let mut b = ignore::gitignore::GitignoreBuilder::new(host_mount_path);
        let _ = b.add_line(None, ".git");
        b.build().unwrap()
    })
}

/// Cached sidebar state for a workspace.
struct WorkspaceCache {
    changes: ChangesSnapshot,
}

struct DiffResult {
    dirty_paths: HashSet<String>,
    files: Vec<changes_tab::ChangedFile>,
    diffs: HashMap<String, diff_engine::FileDiff>,
}

/// Right sidebar: review panel showing agent changeset diffs.
pub struct SidePanel {
    pub visible: bool,
    workspace_id: Option<i64>,
    pub sandbox: Option<Arc<AsyncSandbox>>,
    pub tokio_handle: Option<tokio::runtime::Handle>,
    pub workspace_path: String,
    pub host_mount_path: Option<String>,
    pub changes_tab: ChangesTab,
    cache: HashMap<i64, WorkspaceCache>,
    _watch_task: Option<Task<()>>,
}

impl SidePanel {
    pub fn new() -> Self {
        Self {
            visible: false,
            workspace_id: None,
            sandbox: None,
            tokio_handle: None,
            workspace_path: "/workspace".to_string(),
            host_mount_path: None,
            changes_tab: ChangesTab::new(),
            cache: HashMap::new(),
            _watch_task: None,
        }
    }

    pub fn on_sandbox_ready(
        &mut self,
        sandbox: Arc<AsyncSandbox>,
        tokio_handle: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) {
        if let Some(ref existing) = self.sandbox {
            if Arc::ptr_eq(existing, &sandbox) {
                return;
            }
        }

        self.sandbox = Some(sandbox.clone());
        self.tokio_handle = Some(tokio_handle.clone());
        self.visible = true;

        self.start_watching(cx);
        cx.notify();
    }

    pub fn show_waiting(
        &mut self,
        workspace_id: i64,
        workspace_path: String,
        host_mount_path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let switching = self.workspace_id != Some(workspace_id);

        if switching {
            if let Some(old_id) = self.workspace_id {
                self.cache.insert(old_id, WorkspaceCache {
                    changes: self.changes_tab.snapshot(),
                });
            }

            self.sandbox = None;
            self.tokio_handle = None;
            self._watch_task = None;
            self.workspace_path = workspace_path;
            self.host_mount_path = host_mount_path;
            self.workspace_id = Some(workspace_id);

            if let Some(cached) = self.cache.remove(&workspace_id) {
                self.changes_tab.restore(cached.changes);
            } else {
                self.changes_tab.clear();
            }
        }
        cx.notify();
    }

    pub fn deactivate(&mut self, cx: &mut Context<Self>) {
        self.visible = false;
        self.workspace_id = None;
        self.sandbox = None;
        self.tokio_handle = None;
        self.host_mount_path = None;
        self._watch_task = None;
        self.changes_tab.clear();
        cx.notify();
    }

    fn start_watching(&mut self, cx: &mut Context<Self>) {
        let Some(sandbox) = self.sandbox.clone() else { return };
        let Some(tokio_handle) = self.tokio_handle.clone() else { return };
        self.start_guest_watch(sandbox, tokio_handle, cx);
    }

    fn apply_diff_result(&mut self, result: DiffResult, cx: &mut Context<Self>) {
        self.changes_tab.apply_results(result.files, result.diffs, &result.dirty_paths);
        cx.notify();
    }

    // ── Watch ───────────────────────────────────────────────────

    fn start_guest_watch(
        &mut self,
        sandbox: Arc<AsyncSandbox>,
        tokio_handle: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) {
        let workspace_path = self.workspace_path.clone();
        let host_mount_path = match self.host_mount_path.clone() {
            Some(h) => h,
            None => return,
        };

        let (tx, rx) = flume::unbounded::<DiffResult>();

        // Gitignore filtering is done guest-side in handle_watch —
        // the bridge thread only needs to skip .git/ paths.
        let thread_ok = std::thread::Builder::new()
            .name("shuru-watch-bridge".into())
            .spawn(move || {
                tokio_handle.block_on(async move {
                    let mut watch = match sandbox.open_watch(&workspace_path, true).await {
                        Ok(w) => w,
                        Err(_) => return,
                    };

                    let prefix = format!("{}/", workspace_path);
                    let mut cached_files: HashMap<String, changes_tab::ChangedFile> = HashMap::new();
                    let mut cached_diffs: HashMap<String, diff_engine::FileDiff> = HashMap::new();

                    // Build gitignore matcher from host mount
                    let gitignore = build_ignore_matcher(&host_mount_path);

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
                        for p in &raw_paths {
                            if let Some(rel) = p.strip_prefix(&prefix) {
                                let full = std::path::Path::new(&host_mount_path).join(rel);
                                if gitignore.matched(&full, false).is_ignore() {
                                    continue;
                                }
                                dirty.insert(rel.to_string());
                            }
                        }

                        for path in &dirty {
                            let (old, new) = tokio::join!(
                                diff_engine::read_host_file(path, &host_mount_path),
                                diff_engine::read_sandbox_file(path, "/workspace", &sandbox),
                            );

                            let status = match (&old, &new) {
                                (Some(o), Some(n)) if o == n => {
                                    cached_files.remove(path);
                                    cached_diffs.remove(path);
                                    continue;
                                }
                                (Some(_), Some(_)) => changes_tab::FileStatus::Modified,
                                (None, Some(_)) => changes_tab::FileStatus::Added,
                                (Some(_), None) => changes_tab::FileStatus::Deleted,
                                (None, None) => {
                                    cached_files.remove(path);
                                    cached_diffs.remove(path);
                                    continue;
                                }
                            };

                            let old_bytes = old.unwrap_or_default();
                            let new_bytes = new.unwrap_or_default();
                            let diff = diff_engine::compute_file_diff(&old_bytes, &new_bytes);

                            cached_files.insert(path.clone(), changes_tab::ChangedFile {
                                path: path.clone(),
                                status,
                                diff_stats: Some(diff_engine::DiffStats {
                                    additions: diff.additions,
                                    deletions: diff.deletions,
                                }),
                            });
                            cached_diffs.insert(path.clone(), diff);
                        }

                        let result = DiffResult {
                            dirty_paths: dirty,
                            files: cached_files.values().cloned().collect(),
                            diffs: cached_diffs.clone(),
                        };
                        if tx.send(result).is_err() {
                            break;
                        }
                    }
                });
            })
            .is_ok();

        if !thread_ok {
            return;
        }

        self._watch_task = Some(cx.spawn(async move |this, cx| {
            loop {
                match rx.recv_async().await {
                    Ok(result) => {
                        let ok = cx
                            .update(|cx| {
                                this.update(cx, |panel, cx| {
                                    panel.apply_diff_result(result, cx);
                                    panel.visible
                                })
                                .unwrap_or(false)
                            })
                            .unwrap_or(false);

                        if !ok {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }));
    }
}

impl Render for SidePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div();
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .h(px(36.0))
                    .flex_shrink_0()
                    .w_full()
                    .flex()
                    .items_center()
                    .px_2()
                    .bg(t::bg_elevated())
                    .border_b_1()
                    .border_color(t::border())
                    .child(
                        div()
                            .text_xs()
                            .text_color(t::text_secondary())
                            .child("Review"),
                    ),
            )
            .child(
                div()
                    .flex_grow()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(self.changes_tab.render(cx)),
            )
    }
}

