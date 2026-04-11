mod changes_tab;
pub mod diff_engine;
mod diff_view;
mod watcher;

pub use changes_tab::ChangesTab;

use changes_tab::ChangesSnapshot;
use watcher::{DiffResult, WatchBridge};

use crate::ui::theme as t;
use gpui::*;
use shuru_sdk::AsyncSandbox;
use std::collections::HashMap;
use std::sync::Arc;

/// Cached sidebar state for a workspace.
struct WorkspaceCache {
    changes: ChangesSnapshot,
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
    _watch_bridge: Option<WatchBridge>,
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
            _watch_bridge: None,
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
            self._watch_bridge = None;
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
        self._watch_bridge = None;
        self._watch_task = None;
        self.changes_tab.clear();
        cx.notify();
    }

    fn start_watching(&mut self, cx: &mut Context<Self>) {
        let Some(sandbox) = self.sandbox.clone() else { return };
        let Some(tokio_handle) = self.tokio_handle.clone() else { return };

        let Some((bridge, rx)) = WatchBridge::new(
            sandbox,
            tokio_handle,
            self.workspace_path.clone(),
            self.host_mount_path.clone(),
        ) else {
            return;
        };

        self._watch_bridge = Some(bridge);
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

    fn apply_diff_result(&mut self, result: DiffResult, cx: &mut Context<Self>) {
        self.changes_tab.apply_results(result);
        cx.notify();
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
