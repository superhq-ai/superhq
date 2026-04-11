mod changes_tab;
pub mod diff_engine;
mod diff_service;
mod diff_view;
mod watcher;

pub use changes_tab::ChangesTab;

use changes_tab::ChangesSnapshot;
use diff_service::DiffService;
use watcher::{DiffResult, WatchBridge};

use crate::ui::dock::{DockPosition, Panel, PanelEvent};
use crate::ui::theme as t;
use gpui::*;
use shuru_sdk::AsyncSandbox;
use std::collections::HashMap;
use std::sync::Arc;

/// Cached sidebar state for a workspace.
struct WorkspaceCache {
    changes: ChangesSnapshot,
}

/// Stable key for per-sandbox caching (Arc pointer address).
fn sandbox_key(sb: &Arc<AsyncSandbox>) -> usize {
    Arc::as_ptr(sb) as usize
}

/// A background watcher for a single sandbox, accumulating changes independently.
struct SandboxWatcher {
    _bridge: WatchBridge,
    _task: Task<()>,
}

/// Right sidebar: review panel showing agent changeset diffs.
pub struct SidePanel {
    pub visible: bool,
    workspace_id: Option<i64>,
    active_sandbox_key: Option<usize>,
    pub sandbox: Option<Arc<AsyncSandbox>>,
    pub tokio_handle: Option<tokio::runtime::Handle>,
    pub workspace_path: String,
    pub host_mount_path: Option<String>,
    pub changes_tab: ChangesTab,
    cache: HashMap<i64, WorkspaceCache>,
    /// Per-sandbox changes, accumulated in the background.
    sandbox_changes: HashMap<usize, ChangesTab>,
    /// Per-sandbox watchers, kept alive as long as the sandbox exists.
    sandbox_watchers: HashMap<usize, SandboxWatcher>,
}

impl SidePanel {
    pub fn new() -> Self {
        Self {
            visible: false,
            workspace_id: None,
            active_sandbox_key: None,
            sandbox: None,
            tokio_handle: None,
            workspace_path: "/workspace".to_string(),
            host_mount_path: None,
            changes_tab: ChangesTab::new(),
            cache: HashMap::new(),
            sandbox_changes: HashMap::new(),
            sandbox_watchers: HashMap::new(),
        }
    }

    pub fn on_sandbox_ready(
        &mut self,
        sandbox: Arc<AsyncSandbox>,
        tokio_handle: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) {
        let key = sandbox_key(&sandbox);

        // Start a background watcher for this sandbox if we haven't already
        if !self.sandbox_watchers.contains_key(&key) {
            let mut tab = ChangesTab::new();
            tab.service = Some(DiffService::new(
                sandbox.clone(),
                self.host_mount_path.clone(),
                tokio_handle.clone(),
            ));
            self.sandbox_changes.insert(key, tab);
            self.start_sandbox_watcher(key, sandbox.clone(), tokio_handle.clone(), cx);
        }

        // Swap the visible changes_tab with the per-sandbox one
        if let Some(old_key) = self.active_sandbox_key {
            if old_key != key {
                // Store current visible state back
                if let Some(stored) = self.sandbox_changes.get_mut(&old_key) {
                    std::mem::swap(&mut self.changes_tab, stored);
                }
                // Bring the new sandbox's state to front
                if let Some(stored) = self.sandbox_changes.get_mut(&key) {
                    std::mem::swap(&mut self.changes_tab, stored);
                }
            }
        } else {
            // First activation — swap in
            if let Some(stored) = self.sandbox_changes.get_mut(&key) {
                std::mem::swap(&mut self.changes_tab, stored);
            }
        }

        self.active_sandbox_key = Some(key);
        self.sandbox = Some(sandbox.clone());
        self.tokio_handle = Some(tokio_handle.clone());
        self.visible = true;

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
            self.active_sandbox_key = None;
            // Drop all watchers from the old workspace
            self.sandbox_watchers.clear();
            self.sandbox_changes.clear();
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
        self.active_sandbox_key = None;
        self.sandbox = None;
        self.tokio_handle = None;
        self.host_mount_path = None;
        self.sandbox_watchers.clear();
        self.sandbox_changes.clear();
        self.changes_tab.service = None;
        self.changes_tab.clear();
        cx.notify();
    }

    /// Start a background watcher for a specific sandbox. It accumulates
    /// changes in `sandbox_changes[key]` independently. If this sandbox is
    /// the active one, it also updates the visible `changes_tab`.
    fn start_sandbox_watcher(
        &mut self,
        key: usize,
        sandbox: Arc<AsyncSandbox>,
        tokio_handle: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) {
        let Some((bridge, rx)) = WatchBridge::new(
            sandbox,
            tokio_handle,
            self.workspace_path.clone(),
            self.host_mount_path.clone(),
        ) else {
            return;
        };

        let task = cx.spawn(async move |this, cx| {
            loop {
                match rx.recv_async().await {
                    Ok(result) => {
                        let ok = cx
                            .update(|cx| {
                                this.update(cx, |panel, cx| {
                                    // Always update the background changes for this sandbox
                                    if let Some(tab) = panel.sandbox_changes.get_mut(&key) {
                                        tab.apply_results(result.clone());
                                    }
                                    // If this is the active sandbox, also update the visible tab
                                    if panel.active_sandbox_key == Some(key) {
                                        panel.apply_diff_result(result, cx);
                                    }
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
        });

        self.sandbox_watchers.insert(key, SandboxWatcher {
            _bridge: bridge,
            _task: task,
        });
    }

    fn apply_diff_result(&mut self, result: DiffResult, cx: &mut Context<Self>) {
        self.changes_tab.apply_results(result);
        cx.notify();
    }
}

impl Render for SidePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().id("review-panel");
        }

        div()
            .id("review-panel")
            .size_full()
            .flex()
            .flex_col()
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
            .child(self.changes_tab.render(cx))
    }
}

impl EventEmitter<PanelEvent> for SidePanel {}

impl Panel for SidePanel {
    fn name(&self) -> &'static str { "Review" }
    fn icon(&self) -> Option<&'static str> { None }
    fn position(&self) -> DockPosition { DockPosition::Right }
    fn default_size(&self) -> Pixels { px(340.0) }
}
