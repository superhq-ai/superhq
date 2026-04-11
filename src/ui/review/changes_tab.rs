use super::diff_engine::{DiffStats, FileDiff};
use super::diff_view::{self, DiffDisplayLine, DiffScrollState};
use super::watcher::DiffResult;
use crate::ui::theme as t;
use gpui::*;
use gpui::prelude::FluentBuilder as _;
use gpui_component::scroll::ScrollableElement as _;
use shuru_sdk::AsyncSandbox;
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
    /// Files with pending discard/keep — filtered from bridge results until confirmed gone.
    suppressed: HashSet<String>,
}

#[derive(Clone)]
pub struct ChangedFile {
    pub path: String,
    pub status: FileStatus,
    pub diff_stats: Option<DiffStats>,
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
            suppressed: HashSet::new(),
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
            self.file_diffs.remove(path);
            self.display_lines.remove(path);
            self.highlight_cache.remove(path);
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
        for (path, diff) in result.updated_diffs {
            if self.suppressed.contains(&path) { continue; }
            let lines = Arc::new(diff_view::collect_lines(&diff.hunks));
            // Only invalidate highlights if diff content actually changed
            let content_changed = self.display_lines.get(&path)
                .map_or(true, |old| old.len() != lines.len());
            self.display_lines.insert(path.clone(), lines);
            if content_changed {
                self.highlight_cache.remove(&path);
                self.highlighting.remove(&path);
            }
            self.file_diffs.insert(path, diff);
        }
    }

    fn suppress_file(&mut self, path: &str) {
        self.suppressed.insert(path.to_string());
        self.changed_files.retain(|f| f.path != path);
        self.file_diffs.remove(path);
        self.display_lines.remove(path);
        self.highlight_cache.remove(path);
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
        let mut content = div().size_full().flex().flex_col();

        if !has_changes {
            return content
                .child(div().px_3().py_4().text_xs().text_color(t::text_faint()).child("No changes"))
                .into_any_element();
        }

        let total_add: usize = self.changed_files.iter()
            .filter_map(|f| f.diff_stats.as_ref()).map(|s| s.additions).sum();
        let total_del: usize = self.changed_files.iter()
            .filter_map(|f| f.diff_stats.as_ref()).map(|s| s.deletions).sum();
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
                                    if let (Some(sb), Some(handle)) = (&panel.sandbox, &panel.tokio_handle) {
                                        let paths: Vec<String> = panel.changes_tab.changed_files.iter()
                                            .map(|f| f.path.clone()).collect();
                                        panel.changes_tab.suppress_all();
                                        cx.notify();
                                        let sb = sb.clone();
                                        handle.spawn(async move {
                                            for p in &paths {
                                                let full = format!("/workspace/{}", p);
                                                let _ = sb.discard_overlay(&full).await;
                                            }
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
                                    if let (Some(sb), Some(handle), Some(host)) =
                                        (&panel.sandbox, &panel.tokio_handle, &panel.host_mount_path)
                                    {
                                        let items: Vec<(String, FileStatus)> = panel.changes_tab.changed_files.iter()
                                            .map(|f| (f.path.clone(), f.status)).collect();
                                        panel.changes_tab.suppress_all();
                                        cx.notify();
                                        let sb = sb.clone();
                                        let host = host.clone();
                                        handle.spawn(async move {
                                            for (path, status) in &items {
                                                keep_one(path, *status, &host, &sb).await;
                                            }
                                        });
                                    }
                                }))
                                .child("Keep All"),
                        ),
                ),
        );

        if !self.file_diffs.is_empty() {
            let mut scroll = div().flex_grow().flex().flex_col().overflow_y_scrollbar().pt_1();

            for file in &self.changed_files {
                let diff = self.file_diffs.get(&file.path);

                if !self.scroll_states.contains_key(&file.path) {
                    self.scroll_states.insert(file.path.clone(), DiffScrollState::new());
                }
                let ss = self.scroll_states.get(&file.path).unwrap();

                if !self.expanded.contains_key(&file.path) {
                    self.expanded.insert(file.path.clone(), Rc::new(Cell::new(false)));
                }
                let expanded = self.expanded.get(&file.path).unwrap();

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

                let on_keep: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>> = {
                    let path = path.clone();
                    Some(Box::new(cx.listener(move |panel, _: &ClickEvent, _window, cx| {
                        if let (Some(sb), Some(handle), Some(host)) =
                            (&panel.sandbox, &panel.tokio_handle, &panel.host_mount_path)
                        {
                            panel.changes_tab.suppress_file(&path);
                            cx.notify();
                            let sb = sb.clone();
                            let host = host.clone();
                            let p = path.clone();
                            handle.spawn(async move { keep_one(&p, status, &host, &sb).await });
                        }
                    })))
                };

                let on_discard: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>> = {
                    let path = path.clone();
                    Some(Box::new(cx.listener(move |panel, _: &ClickEvent, _window, cx| {
                        if let (Some(sb), Some(handle)) = (&panel.sandbox, &panel.tokio_handle) {
                            panel.changes_tab.suppress_file(&path);
                            cx.notify();
                            let sb = sb.clone();
                            let p = path.clone();
                            handle.spawn(async move {
                                let full = format!("/workspace/{}", p);
                                let _ = sb.discard_overlay(&full).await;
                            });
                        }
                    })))
                };

                scroll = scroll.child(diff_view::render_file_section(
                    &file.path, file.status,
                    &file.diff_stats.clone().unwrap_or_default(),
                    diff, lines, ss, expanded, highlights, on_keep, on_discard,
                ));
            }

            content = content.child(scroll);
        }

        content.into_any_element()
    }
}

async fn keep_one(path: &str, status: FileStatus, host: &str, sandbox: &Arc<AsyncSandbox>) {
    if status == FileStatus::Deleted {
        let hp = format!("{}/{}", host, path);
        let _ = tokio::fs::remove_file(&hp).await;
    } else {
        let _ = super::diff_engine::copy_to_host(path, host, sandbox).await;
    }
}

pub struct ChangesSnapshot {
    pub changed_files: Vec<ChangedFile>,
}
