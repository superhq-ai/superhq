use super::diff_service::DiffService;
use super::diff_view;
use super::file_row::FileRowView;
use super::watcher::DiffResult;
use crate::ui::components::scrollbar::{self, ScrollbarState};
use crate::ui::components::text_input::{TextInput, TextInputEvent};
use crate::ui::theme as t;
use gpui::*;
use gpui::prelude::FluentBuilder as _;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

type DiscardedLines = HashSet<(usize, usize)>;

struct AskAgentState {
    position: Point<Pixels>,
    input: Entity<TextInput>,
    _subscription: Subscription,
}

pub struct ChangesTab {
    pub changed_files: Vec<ChangedFile>,
    file_views: HashMap<String, Entity<FileRowView>>,
    /// Files with pending discard/keep — filtered from bridge results until confirmed gone.
    suppressed: HashSet<String>,
    /// Service for async diff/file operations.
    pub service: Option<DiffService>,
    scroll_handle: ScrollHandle,
    scrollbar_state: ScrollbarState,
    ask_agent: Option<AskAgentState>,
    pub on_ask_agent: Option<Arc<dyn Fn(String, &mut App) + 'static>>,
}

#[derive(Clone)]
pub struct ChangedFile {
    pub path: String,
    pub status: FileStatus,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
}

/// Callbacks handed to each `FileRowView` so it can reach back into
/// `ChangesTab`/`SidePanel` without holding a direct reference.
pub struct RowCallbacks {
    pub on_keep: Box<dyn Fn(&str, FileStatus, &mut App) + 'static>,
    pub on_discard: Box<dyn Fn(&str, &mut App) + 'static>,
    pub on_apply: Box<dyn Fn(&str, DiscardedLines, &mut App) + 'static>,
    pub on_empty: Box<dyn Fn(&str, &mut App) + 'static>,
}

impl ChangesTab {
    pub fn new() -> Self {
        Self {
            changed_files: Vec::new(),
            file_views: HashMap::new(),
            suppressed: HashSet::new(),
            service: None,
            scroll_handle: ScrollHandle::new(),
            scrollbar_state: ScrollbarState::new(),
            ask_agent: None,
            on_ask_agent: None,
        }
    }

    pub fn clear(&mut self) {
        self.changed_files.clear();
        self.file_views.clear();
        self.suppressed.clear();
        self.ask_agent = None;
    }

    pub fn snapshot(&self) -> ChangesSnapshot {
        ChangesSnapshot { changed_files: self.changed_files.clone() }
    }

    pub fn restore(&mut self, snap: ChangesSnapshot) {
        self.changed_files = snap.changed_files;
    }

    pub fn apply_results(&mut self, result: DiffResult, cx: &mut Context<super::SidePanel>) {
        for path in &result.dirty_paths {
            self.suppressed.remove(path);
        }

        for path in &result.removed_paths {
            self.changed_files.retain(|f| f.path != *path);
            self.file_views.remove(path);
        }

        for (path, file) in result.updated_files {
            let existing_status = self
                .changed_files
                .iter()
                .find(|f| f.path == path)
                .map(|f| f.status);
            let action = decide_row_action(
                file.status,
                existing_status,
                self.suppressed.contains(&path),
            );

            match action {
                RowAction::Skip => {}
                RowAction::RefreshContent => {
                    if let Some(existing) = self
                        .changed_files
                        .iter_mut()
                        .find(|f| f.path == path)
                    {
                        *existing = file.clone();
                    }
                    if let Some(view) = self.file_views.get(&path) {
                        view.update(cx, |row, cx| {
                            row.refresh_content(cx);
                        });
                    }
                }
                RowAction::RefreshWithStatusChange { new_status } => {
                    if let Some(existing) = self
                        .changed_files
                        .iter_mut()
                        .find(|f| f.path == path)
                    {
                        *existing = file.clone();
                    }
                    if let Some(view) = self.file_views.get(&path) {
                        view.update(cx, |row, cx| {
                            row.update_status(new_status, cx);
                        });
                    }
                }
                RowAction::ProbeThenAdd => {
                    let Some(svc) = self.service.clone() else { continue };
                    let path_clone = path.clone();
                    let handle = svc.spawn_result(move |s| async move {
                        s.compute_stats(&path_clone).await
                    });
                    cx.spawn(async move |this, cx| {
                        let Ok((stats, is_binary)) = handle.await else { return };
                        let empty =
                            !is_binary && stats.additions == 0 && stats.deletions == 0;
                        if empty { return; }
                        let _ = cx.update(|cx| {
                            this.update(cx, |panel, cx| {
                                let tab = &mut panel.changes_tab;
                                if tab.suppressed.contains(&file.path) { return; }
                                if tab.changed_files.iter().any(|f| f.path == file.path) {
                                    return;
                                }
                                tab.changed_files.push(file);
                                cx.notify();
                            }).ok();
                        });
                    }).detach();
                }
                RowAction::AddNow => {
                    self.changed_files.push(file);
                }
            }
        }
    }

    fn suppress_file(&mut self, path: &str) {
        self.suppressed.insert(path.to_string());
        self.changed_files.retain(|f| f.path != path);
        self.file_views.remove(path);
    }

    /// Drop a file from the visible list entirely. Used when a computed diff
    /// turns out to be empty — the change has effectively been accepted and
    /// shouldn't linger in the list as a stale entry.
    fn drop_file(&mut self, path: &str) {
        self.changed_files.retain(|f| f.path != path);
        self.file_views.remove(path);
    }

    fn suppress_all(&mut self) {
        self.suppressed.extend(self.changed_files.iter().map(|f| f.path.clone()));
        self.changed_files.clear();
        self.file_views.clear();
    }

    pub fn render(&mut self, cx: &mut Context<super::SidePanel>) -> AnyElement {
        let has_changes = !self.changed_files.is_empty();
        // flex_grow + min_h_0: fill remaining space from parent flex_col,
        // min_h_0 allows shrinking below content height so scroll works.
        let mut content = div().flex_grow().min_h_0().w_full().flex().flex_col();

        if !has_changes {
            return content
                .child(div().px_3().py_4().text_xs().text_color(t::text_faint()).child("No changes"))
                .into_any_element();
        }

        // Sync file_views with changed_files: create new ones, drop stale.
        let callbacks = self.build_callbacks(cx);
        let current_paths: HashSet<String> =
            self.changed_files.iter().map(|f| f.path.clone()).collect();
        self.file_views.retain(|path, _| current_paths.contains(path));
        for file in &self.changed_files {
            if !self.file_views.contains_key(&file.path) {
                let path = file.path.clone();
                let status = file.status;
                let service = self.service.clone();
                let cbs = callbacks.clone();
                let parent_scroll = self.scroll_handle.clone();
                let view = cx.new(move |cx| {
                    FileRowView::new(path, status, service, cbs, parent_scroll, cx)
                });
                self.file_views.insert(file.path.clone(), view);
            }
        }

        let total_add: usize = self
            .file_views
            .values()
            .filter_map(|v| v.read(cx).effective_stats().map(|s| s.additions))
            .sum();
        let total_del: usize = self
            .file_views
            .values()
            .filter_map(|v| v.read(cx).effective_stats().map(|s| s.deletions))
            .sum();
        let file_count = self.changed_files.len();

        content = content.child(
            div().flex_shrink_0().px_3().py_1p5()
                .flex().items_center().justify_between()
                .border_b_1().border_color(t::border())
                .child(
                    div().flex().items_center().gap_1p5()
                        .child(div().text_xs().text_color(t::text_dim()).child(format!(
                            "{} file{}", file_count, if file_count == 1 { "" } else { "s" }
                        )))
                        .when(total_add > 0, |el: Div| el.child(
                            div().text_xs().text_color(t::diff_add_text()).child(format!("+{}", total_add))
                        ))
                        .when(total_del > 0, |el: Div| el.child(
                            div().text_xs().text_color(t::diff_del_text()).child(format!("-{}", total_del))
                        )),
                )
                .child(
                    div().flex().items_center().gap_1()
                        .child(
                            div().id("discard-all-btn").px_2().py(px(3.0)).rounded(px(4.0))
                                .text_xs().font_weight(FontWeight::MEDIUM)
                                .text_color(t::text_dim()).bg(t::bg_elevated())
                                .cursor_pointer()
                                .hover(|s: StyleRefinement| s.bg(t::bg_hover()).text_color(t::diff_del_text()))
                                .on_click(cx.listener(|panel, _: &ClickEvent, _window, cx| {
                                    let svc = panel.changes_tab.service.clone();
                                    if let Some(svc) = svc {
                                        let paths: Vec<String> = panel.changes_tab.changed_files.iter()
                                            .map(|f| f.path.clone()).collect();
                                        panel.changes_tab.suppress_all();
                                        cx.notify();
                                        svc.spawn(move |s| async move {
                                            for p in &paths { s.discard_file(p).await; }
                                        });
                                    }
                                }))
                                .child("Discard All"),
                        )
                        .child(
                            div().id("keep-all-btn").px_2().py(px(3.0)).rounded(px(4.0))
                                .text_xs().font_weight(FontWeight::MEDIUM)
                                .text_color(t::text_secondary()).bg(t::bg_active())
                                .cursor_pointer()
                                .hover(|s: StyleRefinement| s.bg(t::bg_selected()))
                                .on_click(cx.listener(|panel, _: &ClickEvent, _window, cx| {
                                    let svc = panel.changes_tab.service.clone();
                                    if let Some(svc) = svc {
                                        let items: Vec<(String, FileStatus)> = panel.changes_tab.changed_files.iter()
                                            .map(|f| (f.path.clone(), f.status)).collect();
                                        panel.changes_tab.suppress_all();
                                        cx.notify();
                                        svc.spawn(move |s| async move {
                                            for (path, status) in &items {
                                                s.keep_file(path, *status).await;
                                            }
                                        });
                                    }
                                }))
                                .child("Keep All"),
                        ),
                ),
        );

        let scroll_handle = self.scroll_handle.clone();
        let scrollbar_state = self.scrollbar_state.clone();

        let sb_for_scroll = scrollbar_state.clone();
        let mut scroll = div().id("changes-scroll").size_full().flex().flex_col()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .on_scroll_wheel(move |_, _, _| { sb_for_scroll.did_scroll(); })
            .pt_1();

        const MAX_VISIBLE_FILES: usize = 500;
        let overflow = self.changed_files.len().saturating_sub(MAX_VISIBLE_FILES);

        for file in self.changed_files.iter().take(MAX_VISIBLE_FILES) {
            if let Some(view) = self.file_views.get(&file.path) {
                scroll = scroll.child(view.clone());
            }
        }

        if overflow > 0 {
            scroll = scroll.child(
                div().flex_shrink_0().px_3().py_2()
                    .text_xs().text_color(t::text_ghost())
                    .child(format!("and {} more file{}...", overflow, if overflow == 1 { "" } else { "s" })),
            );
        }

        content = content.child(
            div()
                .flex_grow()
                .min_h_0()
                .overflow_hidden()
                .relative()
                .child(scroll)
                .child(
                    canvas(
                        move |_, _, _| {},
                        move |bounds, _, window, _cx| {
                            scrollbar::paint_scrollbar(bounds, &scroll_handle, &scrollbar_state, window);
                        },
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                ),
        );

        // Context menu: find any row with an open context menu (only one shows
        // at a time) and render it anchored at its requested position.
        for (file_path, view) in &self.file_views {
            let row = view.read(cx);
            let sel = row.selection().get();
            if let Some(pos) = sel.context_menu {
                let sel_state = row.selection().clone();
                let lines = row.display_lines().cloned();
                let has_ask_agent = self.on_ask_agent.is_some();

                content = content
                    .child(deferred(
                        anchored()
                            .position(pos)
                            .anchor(Corner::TopLeft)
                            .snap_to_window()
                            .child(
                                t::popover()
                                    .w(px(140.0))
                                    .child(
                                        t::menu_item()
                                            .id("diff-ctx-copy")
                                            .hover(|s| s.bg(t::bg_hover()))
                                            .on_mouse_down(MouseButton::Left, {
                                                let sel_state = sel_state.clone();
                                                let lines = lines.clone();
                                                move |_, _, cx| {
                                                    let s = sel_state.get();
                                                    if let Some(ref l) = lines {
                                                        diff_view::copy_selection(&s, l, cx);
                                                    }
                                                    let mut s = s;
                                                    s.context_menu = None;
                                                    sel_state.set(s);
                                                    cx.stop_propagation();
                                                }
                                            })
                                            .child("Copy")
                                            .child(
                                                div()
                                                    .ml_auto()
                                                    .text_xs()
                                                    .text_color(t::text_ghost())
                                                    .child("\u{2318}C"),
                                            ),
                                    )
                                    .when(has_ask_agent, |el| {
                                        let sel_state = sel_state.clone();
                                        let lines = lines.clone();
                                        let file_path = file_path.clone();
                                        let panel = cx.weak_entity();
                                        el.child(
                                            t::menu_item()
                                                .id("diff-ctx-ask-agent")
                                                .hover(|s| s.bg(t::bg_hover()))
                                                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                                    let selected_text = {
                                                        let s = sel_state.get();
                                                        lines.as_ref()
                                                            .map(|l| diff_view::extract_selection_text(&s, l))
                                                            .unwrap_or_default()
                                                    };
                                                    // Close context menu
                                                    let mut s = sel_state.get();
                                                    s.context_menu = None;
                                                    sel_state.set(s);
                                                    // Open ask-agent input popover
                                                    if let Some(panel) = panel.upgrade() {
                                                        panel.update(cx, |panel, cx| {
                                                            let on_ask = panel.changes_tab.on_ask_agent.clone();
                                                            let fp = file_path.clone();
                                                            let st = selected_text.clone();
                                                            let input = cx.new(|cx| {
                                                                let mut ti = TextInput::new(cx);
                                                                ti.set_placeholder("Type instruction...");
                                                                ti
                                                            });
                                                            let sub = cx.subscribe(&input, move |this, _input, event: &TextInputEvent, cx| {
                                                                if let TextInputEvent::Submit(instruction) = event {
                                                                    let msg = format!(
                                                                        "In `{}`:\n\n```\n{}\n```\n\n{}\n",
                                                                        fp, st, instruction
                                                                    );
                                                                    if let Some(ref cb) = on_ask {
                                                                        cb(msg, cx);
                                                                    }
                                                                    this.changes_tab.ask_agent = None;
                                                                    cx.notify();
                                                                }
                                                            });
                                                            panel.changes_tab.ask_agent = Some(AskAgentState {
                                                                position: pos,
                                                                input,
                                                                _subscription: sub,
                                                            });
                                                            cx.notify();
                                                        });
                                                    }
                                                    cx.stop_propagation();
                                                })
                                                .child("Ask Agent"),
                                        )
                                    }),
                            ),
                    ).with_priority(1))
                    .child(deferred(
                        div()
                            .id("diff-ctx-backdrop")
                            .absolute()
                            .top(px(-2000.0))
                            .left(px(-2000.0))
                            .w(px(8000.0))
                            .h(px(8000.0))
                            .occlude()
                            .on_mouse_down(MouseButton::Left, {
                                let sel_state = sel_state.clone();
                                move |_, _, cx| {
                                    let mut s = sel_state.get();
                                    s.context_menu = None;
                                    sel_state.set(s);
                                    cx.stop_propagation();
                                }
                            }),
                    ).with_priority(0));

                break; // only one context menu at a time
            }
        }

        // Ask-agent input popover
        if let Some(ref state) = self.ask_agent {
            let input = state.input.clone();
            let panel = cx.weak_entity();

            content = content
                .child(deferred(
                    anchored()
                        .position(state.position)
                        .anchor(Corner::TopLeft)
                        .snap_to_window()
                        .child(
                            t::popover()
                                .id("ask-agent-popover")
                                .w(px(260.0))
                                .p_2()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .occlude()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .child(
                                    div().text_xs().font_weight(FontWeight::MEDIUM)
                                        .text_color(t::text_secondary())
                                        .child("Ask agent about selection"),
                                )
                                .child(input)
                                .on_key_down({
                                    let panel = panel.clone();
                                    move |event, _window, cx| {
                                        if event.keystroke.key.as_str() == "escape" {
                                            if let Some(panel) = panel.upgrade() {
                                                panel.update(cx, |panel, cx| {
                                                    panel.changes_tab.ask_agent = None;
                                                    cx.notify();
                                                });
                                            }
                                            cx.stop_propagation();
                                        }
                                    }
                                }),
                        ),
                ).with_priority(2))
                .child(deferred(
                    div()
                        .id("ask-agent-backdrop")
                        .absolute()
                        .top(px(-2000.0))
                        .left(px(-2000.0))
                        .w(px(8000.0))
                        .h(px(8000.0))
                        .occlude()
                        .on_mouse_down(MouseButton::Left, {
                            let panel = panel.clone();
                            move |_, _, cx| {
                                if let Some(panel) = panel.upgrade() {
                                    panel.update(cx, |panel, cx| {
                                        panel.changes_tab.ask_agent = None;
                                        cx.notify();
                                    });
                                }
                                cx.stop_propagation();
                            }
                        }),
                ).with_priority(1));
        }

        content.into_any_element()
    }

    fn build_callbacks(&self, cx: &mut Context<super::SidePanel>) -> Rc<RowCallbacks> {
        let panel = cx.weak_entity();
        let panel_keep = panel.clone();
        let panel_discard = panel.clone();
        let panel_apply = panel.clone();
        let panel_empty = panel;
        Rc::new(RowCallbacks {
            on_keep: Box::new(move |path: &str, status: FileStatus, cx: &mut App| {
                let panel = panel_keep.upgrade();
                let path = path.to_string();
                if let Some(panel) = panel {
                    panel.update(cx, |panel, cx| {
                        let svc = panel.changes_tab.service.clone();
                        if let Some(svc) = svc {
                            panel.changes_tab.suppress_file(&path);
                            cx.notify();
                            let p = path.clone();
                            svc.spawn(move |s| async move { s.keep_file(&p, status).await });
                        }
                    });
                }
            }),
            on_discard: Box::new(move |path: &str, cx: &mut App| {
                let panel = panel_discard.upgrade();
                let path = path.to_string();
                if let Some(panel) = panel {
                    panel.update(cx, |panel, cx| {
                        let svc = panel.changes_tab.service.clone();
                        if let Some(svc) = svc {
                            panel.changes_tab.suppress_file(&path);
                            cx.notify();
                            let p = path.clone();
                            svc.spawn(move |s| async move { s.discard_file(&p).await });
                        }
                    });
                }
            }),
            on_apply: Box::new(move |path: &str, discarded: DiscardedLines, cx: &mut App| {
                let panel = panel_apply.upgrade();
                let path = path.to_string();
                if let Some(panel) = panel {
                    panel.update(cx, |panel, cx| {
                        let svc = panel.changes_tab.service.clone();
                        if let Some(svc) = svc {
                            panel.changes_tab.suppress_file(&path);
                            cx.notify();
                            let p = path.clone();
                            svc.spawn(move |s| async move { s.apply_partial(&p, discarded).await });
                        }
                    });
                }
            }),
            on_empty: Box::new(move |path: &str, cx: &mut App| {
                let panel = panel_empty.upgrade();
                let path = path.to_string();
                if let Some(panel) = panel {
                    panel.update(cx, |panel, cx| {
                        panel.changes_tab.drop_file(&path);
                        cx.notify();
                    });
                }
            }),
        })
    }
}

pub struct ChangesSnapshot {
    pub changed_files: Vec<ChangedFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowAction {
    /// Path is suppressed pending a user action. Ignore this event.
    Skip,
    /// Row exists and the status is unchanged. Re-read stats/diff but
    /// preserve any partial-discard staging the user is working on.
    RefreshContent,
    /// Row exists but the status changed (Added to Deleted, etc).
    /// Full reset including staging — old per-line selections are
    /// meaningless against the new status.
    RefreshWithStatusChange { new_status: FileStatus },
    /// No row yet, status is Modified. Gate on a non-empty diff before
    /// showing so identical-on-both-sides files don't flicker in and out.
    ProbeThenAdd,
    /// No row yet, status is Added or Deleted. Show immediately.
    AddNow,
}

pub(super) fn decide_row_action(
    event_status: FileStatus,
    existing_status: Option<FileStatus>,
    suppressed: bool,
) -> RowAction {
    if suppressed {
        return RowAction::Skip;
    }
    match existing_status {
        Some(prev) if prev == event_status => RowAction::RefreshContent,
        Some(_) => RowAction::RefreshWithStatusChange { new_status: event_status },
        None => match event_status {
            FileStatus::Modified => RowAction::ProbeThenAdd,
            FileStatus::Added | FileStatus::Deleted => RowAction::AddNow,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{decide_row_action, FileStatus, RowAction};

    #[test]
    fn suppressed_events_are_skipped() {
        let action = decide_row_action(FileStatus::Modified, Some(FileStatus::Added), true);
        assert_eq!(action, RowAction::Skip);
    }

    #[test]
    fn new_added_file_is_added_immediately() {
        let action = decide_row_action(FileStatus::Added, None, false);
        assert_eq!(action, RowAction::AddNow);
    }

    #[test]
    fn new_deleted_file_is_added_immediately() {
        let action = decide_row_action(FileStatus::Deleted, None, false);
        assert_eq!(action, RowAction::AddNow);
    }

    #[test]
    fn new_modified_file_is_probed_first() {
        let action = decide_row_action(FileStatus::Modified, None, false);
        assert_eq!(action, RowAction::ProbeThenAdd);
    }

    /// Regression: `touch hello.txt` (creates empty file, classified as
    /// Added) followed by `echo Hello > hello.txt` (classified as Added
    /// again because the file still doesn't exist on the host). Previously
    /// the second event was silently dropped by a status-change gate, so
    /// the diff bar stayed empty. Must now refresh content without wiping
    /// staging.
    #[test]
    fn same_status_event_refreshes_content_only() {
        let action = decide_row_action(FileStatus::Added, Some(FileStatus::Added), false);
        assert_eq!(action, RowAction::RefreshContent);
    }

    #[test]
    fn modified_after_modified_refreshes_content_only() {
        let action = decide_row_action(
            FileStatus::Modified,
            Some(FileStatus::Modified),
            false,
        );
        assert_eq!(action, RowAction::RefreshContent);
    }

    /// A Modified row that receives a follow-up MODIFY (typical editor
    /// autosave) must take the content-only path. The row-level handler
    /// preserves partial-discard staging on that path; wiping it would
    /// silently drop mid-triage work.
    #[test]
    fn modified_event_on_modified_row_preserves_staging_path() {
        let action = decide_row_action(
            FileStatus::Modified,
            Some(FileStatus::Modified),
            false,
        );
        assert_ne!(
            action,
            RowAction::RefreshWithStatusChange { new_status: FileStatus::Modified },
            "same-status events must take the staging-preserving path",
        );
        assert_eq!(action, RowAction::RefreshContent);
    }

    #[test]
    fn status_transition_refreshes_with_status_change() {
        let action = decide_row_action(FileStatus::Deleted, Some(FileStatus::Added), false);
        assert_eq!(
            action,
            RowAction::RefreshWithStatusChange { new_status: FileStatus::Deleted },
        );
    }

    #[test]
    fn added_to_modified_uses_status_change_path() {
        // Added then the host-side baseline appears (e.g. user creates
        // the file on host matching the sandbox content). Status goes
        // Added -> Modified; staging built against the "new file" view
        // no longer maps to the "existing file" hunk layout, so a full
        // reset is the right call.
        let action = decide_row_action(FileStatus::Modified, Some(FileStatus::Added), false);
        assert_eq!(
            action,
            RowAction::RefreshWithStatusChange { new_status: FileStatus::Modified },
        );
    }
}
