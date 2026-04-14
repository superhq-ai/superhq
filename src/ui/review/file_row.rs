use super::changes_tab::{FileStatus, RowCallbacks};
use super::diff_engine::{DiffStats, FileDiff};
use super::diff_service::DiffService;
use super::diff_view::{self, DiffDisplayLine, DiffScrollState, SelectionState, HighlightCache};
use gpui::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// A single file row in the review panel. Owns all per-file UI state so
/// hover/click only re-renders this entity instead of the whole panel.
pub struct FileRowView {
    path: String,
    status: FileStatus,
    service: Option<DiffService>,
    callbacks: Rc<RowCallbacks>,
    parent_scroll: ScrollHandle,

    /// Populated eagerly for the header totals.
    stats: Option<DiffStats>,
    is_binary: bool,
    stats_loading: bool,

    /// Populated lazily on expand.
    diff: Option<FileDiff>,
    display_lines: Option<Arc<Vec<DiffDisplayLine>>>,
    highlights: Option<Arc<HighlightCache>>,
    highlighting: bool,
    diffing: bool,

    scroll: DiffScrollState,
    selection: SelectionState,
    expanded: Rc<Cell<bool>>,
    focus: FocusHandle,
    char_width_cache: Rc<Cell<Option<Pixels>>>,
}

impl FileRowView {
    pub fn new(
        path: String,
        status: FileStatus,
        service: Option<DiffService>,
        callbacks: Rc<RowCallbacks>,
        parent_scroll: ScrollHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut row = Self {
            path,
            status,
            service,
            callbacks,
            parent_scroll,
            stats: None,
            is_binary: false,
            stats_loading: false,
            diff: None,
            display_lines: None,
            highlights: None,
            highlighting: false,
            diffing: false,
            scroll: DiffScrollState::new(),
            selection: SelectionState::new(),
            expanded: Rc::new(Cell::new(false)),
            focus: cx.focus_handle(),
            char_width_cache: Rc::new(Cell::new(None)),
        };
        row.kick_stats(cx);
        row
    }

    pub fn stats(&self) -> Option<&DiffStats> {
        self.stats.as_ref()
    }

    pub fn selection(&self) -> &SelectionState {
        &self.selection
    }

    pub fn display_lines(&self) -> Option<&Arc<Vec<DiffDisplayLine>>> {
        self.display_lines.as_ref()
    }

    /// Called when the file's status changes upstream (e.g., Modified → Deleted).
    /// Resets cached diff/stats so they're recomputed against the new state.
    pub fn update_status(&mut self, status: FileStatus, cx: &mut Context<Self>) {
        self.status = status;
        self.purge_cache();
        self.kick_stats(cx);
        if self.expanded.get() {
            self.kick_diff(cx);
        }
        cx.notify();
    }

    fn purge_cache(&mut self) {
        self.stats = None;
        self.is_binary = false;
        self.diff = None;
        self.display_lines = None;
        self.highlights = None;
    }

    fn kick_stats(&mut self, cx: &mut Context<Self>) {
        if self.stats.is_some() || self.stats_loading || self.diff.is_some() {
            return;
        }
        let Some(svc) = self.service.clone() else { return };
        self.stats_loading = true;
        let path = self.path.clone();
        let handle = svc.spawn_result(move |s| async move { s.compute_stats(&path).await });
        cx.spawn(async move |this, cx| {
            let Ok((stats, is_binary)) = handle.await else { return };
            let _ = cx.update(|cx| {
                this.update(cx, |row, cx| {
                    let empty = !is_binary && stats.additions == 0 && stats.deletions == 0;
                    if empty {
                        (row.callbacks.on_empty)(&row.path, cx);
                    } else {
                        row.stats_loading = false;
                        row.stats = Some(stats);
                        row.is_binary = is_binary;
                        cx.notify();
                    }
                }).ok();
            });
        }).detach();
    }

    fn kick_diff(&mut self, cx: &mut Context<Self>) {
        if self.diff.is_some() || self.diffing {
            return;
        }
        let Some(svc) = self.service.clone() else { return };
        self.diffing = true;
        let path = self.path.clone();
        let handle = svc.spawn_result(move |s| async move { s.compute_diff(&path).await });
        cx.spawn(async move |this, cx| {
            let Ok(diff) = handle.await else { return };
            let lines = Arc::new(diff_view::collect_lines(&diff.hunks));
            let _ = cx.update(|cx| {
                this.update(cx, |row, cx| {
                    if diff.is_empty() {
                        (row.callbacks.on_empty)(&row.path, cx);
                        return;
                    }
                    row.diffing = false;
                    row.stats = Some(DiffStats {
                        additions: diff.additions,
                        deletions: diff.deletions,
                    });
                    row.is_binary = diff.is_binary;
                    row.display_lines = Some(lines);
                    row.diff = Some(diff);
                    cx.notify();
                }).ok();
            });
        }).detach();
    }

    fn kick_highlights(&mut self, cx: &mut Context<Self>) {
        if self.highlights.is_some() || self.highlighting {
            return;
        }
        let Some(d) = self.diff.as_ref() else { return };
        self.highlighting = true;
        let path = self.path.clone();
        let hunks = d.hunks.clone();
        cx.spawn(async move |this, cx| {
            let result = std::thread::spawn(move || {
                diff_view::compute_highlights(&path, &hunks)
            }).join().ok();
            if let Some(cache) = result {
                let _ = cx.update(|cx| {
                    this.update(cx, |row, cx| {
                        row.highlighting = false;
                        row.highlights = Some(Arc::new(cache));
                        cx.notify();
                    }).ok();
                });
            }
        }).detach();
    }
}

impl Render for FileRowView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Kick off lazy work when the row transitions to expanded.
        if self.expanded.get() {
            if self.diff.is_none() && !self.diffing {
                self.kick_diff(cx);
            } else if self.diff.is_some() && self.highlights.is_none() && !self.highlighting {
                self.kick_highlights(cx);
            }
        }

        let stats = self.stats.clone().unwrap_or_default();
        let diff = self.diff.as_ref();
        let lines = self.display_lines.as_ref();
        let highlights = if self.expanded.get() {
            self.highlights.as_ref()
        } else {
            None
        };

        let path = self.path.clone();
        let status = self.status;

        let on_keep: Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static> = {
            let path = path.clone();
            let cb = self.callbacks.clone();
            Box::new(move |_: &ClickEvent, _window, cx| {
                (cb.on_keep)(&path, status, cx);
            })
        };
        let on_discard: Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static> = {
            let path = path.clone();
            let cb = self.callbacks.clone();
            Box::new(move |_: &ClickEvent, _window, cx| {
                (cb.on_discard)(&path, cx);
            })
        };

        diff_view::render_file_section(diff_view::FileSectionParams {
            path: &self.path,
            status: self.status,
            stats,
            diff,
            lines,
            highlights,
            scroll: &self.scroll,
            selection: &self.selection,
            expanded: &self.expanded,
            focus_handle: &self.focus,
            char_width_cache: &self.char_width_cache,
            parent_scroll: Some(&self.parent_scroll),
            on_keep,
            on_discard,
        })
    }
}
