use super::diff_engine::{DiffStats, FileDiff};
use super::diff_service::DiffService;
use super::diff_view::{self, DiffDisplayLine, DiffScrollState};
use super::watcher::DiffResult;
use crate::ui::components::scrollbar::{self, ScrollbarState};
use crate::ui::theme as t;
use gpui::*;
use gpui::prelude::FluentBuilder as _;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

struct PerFileState {
    diff: Option<FileDiff>,
    display_lines: Option<Arc<Vec<DiffDisplayLine>>>,
    scroll: DiffScrollState,
    selection: diff_view::SelectionState,
    expanded: Rc<Cell<bool>>,
    highlights: Option<Arc<diff_view::HighlightCache>>,
    highlighting: bool,
    diffing: bool,
    focus: FocusHandle,
    char_width_cache: Rc<Cell<Option<Pixels>>>,
}

impl PerFileState {
    fn new(cx: &mut App) -> Self {
        Self {
            diff: None,
            display_lines: None,
            scroll: DiffScrollState::new(),
            selection: diff_view::SelectionState::new(),
            expanded: Rc::new(Cell::new(false)),
            highlights: None,
            highlighting: false,
            diffing: false,
            focus: cx.focus_handle(),
            char_width_cache: Rc::new(Cell::new(None)),
        }
    }
}

pub struct ChangesTab {
    pub changed_files: Vec<ChangedFile>,
    file_states: HashMap<String, PerFileState>,
    /// File path with an active context menu, if any.
    context_menu: Option<(String, Point<Pixels>)>,
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

impl FileStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
        }
    }

    pub fn color(&self) -> Rgba {
        match self {
            Self::Modified => t::status_modified(),
            Self::Added => t::status_added(),
            Self::Deleted => t::status_deleted(),
        }
    }
}

impl ChangesTab {
    pub fn new() -> Self {
        Self {
            changed_files: Vec::new(),
            file_states: HashMap::new(),
            context_menu: None,
            suppressed: HashSet::new(),
            service: None,
            scroll_handle: ScrollHandle::new(),
            scrollbar_state: ScrollbarState::new(),
        }
    }

    pub fn clear(&mut self) {
        self.changed_files.clear();
        self.file_states.clear();
        self.context_menu = None;
        self.suppressed.clear();
    }

    pub fn snapshot(&self) -> ChangesSnapshot {
        ChangesSnapshot { changed_files: self.changed_files.clone() }
    }

    pub fn restore(&mut self, snap: ChangesSnapshot) {
        self.changed_files = snap.changed_files;
    }

    pub fn apply_results(&mut self, result: DiffResult) {
        // Lift suppression for any path the bridge has now reported on.
        for path in &result.dirty_paths {
            self.suppressed.remove(path);
        }

        // Remove reverted files
        for path in &result.removed_paths {
            self.changed_files.retain(|f| f.path != *path);
            self.purge_diff_cache(path);
        }

        // Invalidate cached diffs for files whose status changed
        for path in result.updated_files.keys() {
            self.purge_diff_cache(path);
        }

        // Merge updated files
        for (path, file) in result.updated_files {
            if self.suppressed.contains(&path) { continue; }
            if let Some(existing) = self.changed_files.iter_mut().find(|f| f.path == path) {
                *existing = file;
            } else {
                self.changed_files.push(file);
            }
        }
    }

    fn purge_diff_cache(&mut self, path: &str) {
        if let Some(fs) = self.file_states.get_mut(path) {
            fs.diff = None;
            fs.display_lines = None;
            fs.highlights = None;
        }
    }

    fn suppress_file(&mut self, path: &str) {
        self.suppressed.insert(path.to_string());
        self.changed_files.retain(|f| f.path != path);
        self.purge_diff_cache(path);
    }

    fn suppress_all(&mut self) {
        self.suppressed.extend(self.changed_files.iter().map(|f| f.path.clone()));
        self.changed_files.clear();
        for fs in self.file_states.values_mut() {
            fs.diff = None;
            fs.display_lines = None;
            fs.highlights = None;
        }
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

        let total_add: usize = self.file_states.values().filter_map(|fs| fs.diff.as_ref()).map(|d| d.additions).sum();
        let total_del: usize = self.file_states.values().filter_map(|fs| fs.diff.as_ref()).map(|d| d.deletions).sum();
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
        scrollbar_state.did_scroll(); // show on content change

        let mut scroll = div().id("changes-scroll").size_full().flex().flex_col()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .pt_1();

            const MAX_VISIBLE_FILES: usize = 500;
            let overflow = self.changed_files.len().saturating_sub(MAX_VISIBLE_FILES);

            for file in self.changed_files.iter().take(MAX_VISIBLE_FILES) {
                let fs = self.file_states.entry(file.path.clone()).or_insert_with(|| PerFileState::new(cx));

                if fs.expanded.get() && fs.diff.is_none() && !fs.diffing {
                    let svc = self.service.clone();
                    if let Some(svc) = svc {
                        fs.diffing = true;
                        let path = file.path.clone();
                        let path2 = path.clone();
                        let handle = svc.spawn_result(move |s| async move {
                            s.compute_diff(&path).await
                        });
                        cx.spawn(async move |this, cx| {
                            let Ok(diff) = handle.await else { return };
                            let lines = Arc::new(diff_view::collect_lines(&diff.hunks));
                            let _ = cx.update(|cx| {
                                this.update(cx, |panel, cx| {
                                    if let Some(fs) = panel.changes_tab.file_states.get_mut(&path2) {
                                        fs.diffing = false;
                                        fs.display_lines = Some(lines);
                                        fs.diff = Some(diff);
                                    }
                                    cx.notify();
                                }).ok();
                            });
                        }).detach();
                    }
                }

                let diff = fs.diff.as_ref();
                let lines = fs.display_lines.as_ref();

                let highlights = if fs.expanded.get() {
                    if fs.highlights.is_none() && !fs.highlighting {
                        if let Some(d) = diff {
                            fs.highlighting = true;
                            let path = file.path.clone();
                            let path2 = path.clone();
                            let hunks = d.hunks.clone();
                            cx.spawn(async move |this, cx| {
                                let result = std::thread::spawn(move || {
                                    diff_view::compute_highlights(&path, &hunks)
                                }).join().ok();
                                if let Some(cache) = result {
                                    let _ = cx.update(|cx| {
                                        this.update(cx, |panel, cx| {
                                            if let Some(fs) = panel.changes_tab.file_states.get_mut(&path2) {
                                                fs.highlighting = false;
                                                fs.highlights = Some(Arc::new(cache));
                                            }
                                            cx.notify();
                                        }).ok();
                                    });
                                }
                            }).detach();
                        }
                    }
                    fs.highlights.as_ref()
                } else {
                    None
                };
                let path = file.path.clone();
                let status = file.status;

                let on_keep = {
                    let path = path.clone();
                    Box::new(cx.listener(move |panel, _: &ClickEvent, _window, cx| {
                        let svc = panel.changes_tab.service.clone();
                        if let Some(svc) = svc {
                            panel.changes_tab.suppress_file(&path);
                            cx.notify();
                            let p = path.clone();
                            svc.spawn(move |s| async move { s.keep_file(&p, status).await });
                        }
                    })) as Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>
                };
                let on_discard = {
                    let path = path.clone();
                    Box::new(cx.listener(move |panel, _: &ClickEvent, _window, cx| {
                        let svc = panel.changes_tab.service.clone();
                        if let Some(svc) = svc {
                            panel.changes_tab.suppress_file(&path);
                            cx.notify();
                            let p = path.clone();
                            svc.spawn(move |s| async move { s.discard_file(&p).await });
                        }
                    })) as Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>
                };

                scroll = scroll.child(diff_view::render_file_section(diff_view::FileSectionParams {
                    path: &file.path,
                    status: file.status,
                    stats: diff.map(|d| DiffStats { additions: d.additions, deletions: d.deletions })
                        .unwrap_or_default(),
                    diff,
                    lines,
                    highlights,
                    scroll: &fs.scroll,
                    selection: &fs.selection,
                    expanded: &fs.expanded,
                    focus_handle: &fs.focus,
                    char_width_cache: &fs.char_width_cache,
                    parent_scroll: Some(&self.scroll_handle),
                    on_keep: on_keep,
                    on_discard: on_discard,
                }));
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

        // Context menu — rendered outside the scroll container so positioning is correct
        for fs in self.file_states.values() {
            let sel = fs.selection.get();
            if let Some(pos) = sel.context_menu {
                let sel_state = fs.selection.clone();
                let lines = fs.display_lines.clone();

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
}

pub struct ChangesSnapshot {
    pub changed_files: Vec<ChangedFile>,
}
