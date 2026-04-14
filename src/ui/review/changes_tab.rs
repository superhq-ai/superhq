use super::diff_service::DiffService;
use super::diff_view;
use super::file_row::FileRowView;
use super::watcher::DiffResult;
use crate::ui::components::scrollbar::{self, ScrollbarState};
use crate::ui::theme as t;
use gpui::*;
use gpui::prelude::FluentBuilder as _;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

pub struct ChangesTab {
    pub changed_files: Vec<ChangedFile>,
    file_views: HashMap<String, Entity<FileRowView>>,
    /// Files with pending discard/keep — filtered from bridge results until confirmed gone.
    suppressed: HashSet<String>,
    /// Service for async diff/file operations.
    pub service: Option<DiffService>,
    scroll_handle: ScrollHandle,
    scrollbar_state: ScrollbarState,
}

#[derive(Clone)]
pub struct ChangedFile {
    pub path: String,
    pub status: FileStatus,
}

#[derive(Clone, Copy, PartialEq)]
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
        }
    }

    pub fn clear(&mut self) {
        self.changed_files.clear();
        self.file_views.clear();
        self.suppressed.clear();
    }

    pub fn snapshot(&self) -> ChangesSnapshot {
        ChangesSnapshot { changed_files: self.changed_files.clone() }
    }

    pub fn restore(&mut self, snap: ChangesSnapshot) {
        self.changed_files = snap.changed_files;
    }

    pub fn apply_results(&mut self, result: DiffResult, cx: &mut Context<super::SidePanel>) {
        // Lift suppression for any path the bridge has now reported on.
        for path in &result.dirty_paths {
            self.suppressed.remove(path);
        }

        // Remove reverted files
        for path in &result.removed_paths {
            self.changed_files.retain(|f| f.path != *path);
            self.file_views.remove(path);
        }

        // Merge updated files and notify any existing row views that their
        // status changed so they invalidate cached diffs.
        for (path, file) in result.updated_files {
            if self.suppressed.contains(&path) { continue; }
            if let Some(existing) = self.changed_files.iter_mut().find(|f| f.path == path) {
                let status_changed = existing.status != file.status;
                *existing = file.clone();
                if status_changed {
                    if let Some(view) = self.file_views.get(&path) {
                        view.update(cx, |row, cx| {
                            row.update_status(file.status, cx);
                        });
                    }
                }
            } else {
                self.changed_files.push(file);
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
            .filter_map(|v| v.read(cx).stats().map(|s| s.additions))
            .sum();
        let total_del: usize = self
            .file_views
            .values()
            .filter_map(|v| v.read(cx).stats().map(|s| s.deletions))
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
        for view in self.file_views.values() {
            let row = view.read(cx);
            let sel = row.selection().get();
            if let Some(pos) = sel.context_menu {
                let sel_state = row.selection().clone();
                let lines = row.display_lines().cloned();

                content = content
                    .child(deferred(
                        anchored()
                            .position(pos)
                            .anchor(Corner::TopLeft)
                            .snap_to_window()
                            .child(
                                t::popover()
                                    .w(px(120.0))
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
                                    ),
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

        content.into_any_element()
    }

    fn build_callbacks(&self, cx: &mut Context<super::SidePanel>) -> Rc<RowCallbacks> {
        let panel = cx.weak_entity();
        let panel_keep = panel.clone();
        let panel_discard = panel.clone();
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
