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

pub struct ChangesTab {
    pub changed_files: Vec<ChangedFile>,
    file_diffs: HashMap<String, FileDiff>,
    /// Pre-computed display lines (Arc so DiffBlock gets the same instances across frames).
    display_lines: HashMap<String, Arc<Vec<DiffDisplayLine>>>,
    scroll_states: HashMap<String, DiffScrollState>,
    expanded: HashMap<String, Rc<Cell<bool>>>,
    highlight_cache: HashMap<String, Arc<diff_view::HighlightCache>>,
    /// Paths with in-flight highlight computation (prevents duplicate spawns).
    highlighting: HashSet<String>,
    /// Paths with in-flight diff computation (prevents duplicate spawns).
    diffing: HashSet<String>,
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
            file_diffs: HashMap::new(),
            display_lines: HashMap::new(),
            scroll_states: HashMap::new(),
            expanded: HashMap::new(),
            highlight_cache: HashMap::new(),
            highlighting: HashSet::new(),
            diffing: HashSet::new(),
            suppressed: HashSet::new(),
            service: None,
            scroll_handle: ScrollHandle::new(),
            scrollbar_state: ScrollbarState::new(),
        }
    }

    pub fn clear(&mut self) {
        self.changed_files.clear();
        self.file_diffs.clear();
        self.display_lines.clear();
        self.scroll_states.clear();
        self.expanded.clear();
        self.highlight_cache.clear();
        self.highlighting.clear();
        self.diffing.clear();
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
        self.file_diffs.remove(path);
        self.display_lines.remove(path);
        self.highlight_cache.remove(path);
    }

    fn suppress_file(&mut self, path: &str) {
        self.suppressed.insert(path.to_string());
        self.changed_files.retain(|f| f.path != path);
        self.purge_diff_cache(path);
    }

    fn suppress_all(&mut self) {
        self.suppressed.extend(self.changed_files.iter().map(|f| f.path.clone()));
        self.changed_files.clear();
        self.file_diffs.clear();
        self.display_lines.clear();
        self.highlight_cache.clear();
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

        let total_add: usize = self.file_diffs.values().map(|d| d.additions).sum();
        let total_del: usize = self.file_diffs.values().map(|d| d.deletions).sum();
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
                if !self.scroll_states.contains_key(&file.path) {
                    self.scroll_states.insert(file.path.clone(), DiffScrollState::new());
                }
                let ss = self.scroll_states.get(&file.path).unwrap();

                if !self.expanded.contains_key(&file.path) {
                    self.expanded.insert(file.path.clone(), Rc::new(Cell::new(false)));
                }
                let expanded = self.expanded.get(&file.path).unwrap();

                // Lazy: compute diff off the UI thread when expanded
                if expanded.get()
                    && !self.file_diffs.contains_key(&file.path)
                    && !self.diffing.contains(&file.path)
                {
                    let svc = self.service.clone();
                    if let Some(svc) = svc {
                        self.diffing.insert(file.path.clone());
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
                                    panel.changes_tab.diffing.remove(&path2);
                                    panel.changes_tab.display_lines.insert(path2.clone(), lines);
                                    panel.changes_tab.file_diffs.insert(path2, diff);
                                    cx.notify();
                                }).ok();
                            });
                        }).detach();
                    }
                }

                let diff = self.file_diffs.get(&file.path);
                let lines = self.display_lines.get(&file.path);

                // Lazy: compute highlights off the UI thread when expanded
                let highlights = if expanded.get() {
                    if !self.highlight_cache.contains_key(&file.path)
                        && !self.highlighting.contains(&file.path)
                    {
                        if let Some(d) = diff {
                            self.highlighting.insert(file.path.clone());
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
                                            panel.changes_tab.highlighting.remove(&path2);
                                            panel.changes_tab.highlight_cache.insert(path2, Arc::new(cache));
                                            cx.notify();
                                        }).ok();
                                    });
                                }
                            }).detach();
                        }
                    }
                    self.highlight_cache.get(&file.path)
                } else {
                    None
                };
                let path = file.path.clone();
                let status = file.status;

                // Only create keep/discard closures when expanded — avoids
                // 1000 heap allocations per frame for collapsed items.
                let (on_keep, on_discard) = if expanded.get() {
                    let keep = {
                        let path = path.clone();
                        Some(Box::new(cx.listener(move |panel, _: &ClickEvent, _window, cx| {
                            let svc = panel.changes_tab.service.clone();
                            if let Some(svc) = svc {
                                panel.changes_tab.suppress_file(&path);
                                cx.notify();
                                let p = path.clone();
                                svc.spawn(move |s| async move { s.keep_file(&p, status).await });
                            }
                        })) as Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>)
                    };
                    let discard = {
                        let path = path.clone();
                        Some(Box::new(cx.listener(move |panel, _: &ClickEvent, _window, cx| {
                            let svc = panel.changes_tab.service.clone();
                            if let Some(svc) = svc {
                                panel.changes_tab.suppress_file(&path);
                                cx.notify();
                                let p = path.clone();
                                svc.spawn(move |s| async move { s.discard_file(&p).await });
                            }
                        })) as Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>)
                    };
                    (keep, discard)
                } else {
                    (None, None)
                };

                let stats = diff.map(|d| DiffStats { additions: d.additions, deletions: d.deletions })
                    .unwrap_or_default();
                scroll = scroll.child(diff_view::render_file_section(
                    &file.path, file.status,
                    &stats,
                    diff, lines, ss, expanded, highlights, on_keep, on_discard,
                ));
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

        content.into_any_element()
    }
}

pub struct ChangesSnapshot {
    pub changed_files: Vec<ChangedFile>,
}
