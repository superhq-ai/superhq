use crate::ui::theme as t;
use gpui::*;
use super::diff_engine::{DiffLineKind, FileDiff, DiffStats, DiffHunk};
use super::changes_tab::FileStatus;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

const LINE_HEIGHT: f32 = 18.0;
const GUTTER_WIDTH: f32 = 48.0;
const GUTTER_PAD: f32 = 8.0;
const CONTENT_PAD: f32 = 6.0;

// Scrollbar constants
const SCROLLBAR_TRACK_HEIGHT: f32 = 14.0;
const THUMB_WIDTH: f32 = 6.0;
const THUMB_ACTIVE_WIDTH: f32 = 8.0;
const THUMB_INSET: f32 = 4.0;
const THUMB_RADIUS: f32 = 3.0;
const THUMB_ACTIVE_RADIUS: f32 = 4.0;
const MIN_THUMB_SIZE: f32 = 48.0;
const FADE_OUT_DELAY: f32 = 2.0;
const FADE_OUT_DURATION: f32 = 3.0;

// ── Display line ────────────────────────────────────────────────

#[derive(Clone)]
pub struct DiffDisplayLine {
    pub kind: DiffLineKind,
    pub lineno: SharedString,
    pub content: SharedString,
    pub is_hunk_header: bool,
}

pub fn collect_lines(hunks: &[DiffHunk]) -> Vec<DiffDisplayLine> {
    let mut lines = Vec::new();
    for hunk in hunks {
        lines.push(DiffDisplayLine {
            kind: DiffLineKind::Context,
            lineno: SharedString::default(),
            content: SharedString::from(format!(
                "@@ -{},{} +{},{} @@",
                hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
            )),
            is_hunk_header: true,
        });
        for line in &hunk.lines {
            let lineno = if line.kind == DiffLineKind::Deletion {
                line.old_lineno
            } else {
                line.new_lineno
            };
            lines.push(DiffDisplayLine {
                kind: line.kind,
                lineno: lineno.map(|n| SharedString::from(n.to_string())).unwrap_or_default(),
                content: SharedString::from(line.content.trim_end_matches('\n').to_string()),
                is_hunk_header: false,
            });
        }
    }
    lines
}

// ── Language detection ──────────────────────────────────────────

fn language_from_path(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hxx" => "cpp",
        "rb" => "ruby",
        "swift" => "swift",
        "scala" => "scala",
        "zig" => "zig",
        "sh" | "bash" | "zsh" => "bash",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "sql" => "sql",
        "graphql" | "gql" => "graphql",
        "proto" => "proto",
        "ex" | "exs" => "elixir",
        "cs" => "csharp",
        "cmake" => "cmake",
        "lock" => {
            if path.contains("Cargo") { "toml" }
            else { "text" }
        }
        _ => "text",
    }
}

// ── Syntax highlighting (cached) ────────────────────────────────

/// Per-line highlight spans from tree-sitter. Cached in ChangesTab,
/// converted to TextRuns at render time with the current font.
pub type HighlightCache = Vec<Option<Vec<(Range<usize>, HighlightStyle)>>>;

/// Run tree-sitter once for a file's diff hunks. Called from ChangesTab
/// when diffs load, result is cached.
pub fn compute_highlights(path: &str, hunks: &[DiffHunk]) -> HighlightCache {
    let language = language_from_path(path);
    let lines = collect_lines(hunks);

    if language == "text" {
        return lines.iter().map(|_| None).collect();
    }

    // Build full text, track byte ranges per line
    let mut full_text = String::new();
    let mut line_ranges: Vec<Option<Range<usize>>> = Vec::with_capacity(lines.len());

    for line in &lines {
        if line.is_hunk_header || line.content.is_empty() {
            line_ranges.push(None);
        } else {
            let start = full_text.len();
            full_text.push_str(&line.content);
            let end = full_text.len();
            full_text.push('\n');
            line_ranges.push(Some(start..end));
        }
    }

    let all_styles = super::highlighter::highlight(language, &full_text);

    // Extract per-line spans
    lines.iter().enumerate().map(|(i, _line)| {
        let Some(ref byte_range) = line_ranges[i] else {
            return None;
        };

        let line_start = byte_range.start;
        let line_end = byte_range.end;
        let mut relevant: Vec<(Range<usize>, HighlightStyle)> = Vec::new();

        for (range, style) in &all_styles {
            if range.end <= line_start || range.start >= line_end {
                continue;
            }
            let clamped_start = range.start.max(line_start) - line_start;
            let clamped_end = range.end.min(line_end) - line_start;
            if clamped_start < clamped_end {
                relevant.push((clamped_start..clamped_end, *style));
            }
        }

        if relevant.is_empty() {
            None
        } else {
            relevant.sort_by_key(|(r, _)| r.start);
            Some(relevant)
        }
    }).collect()
}

/// Convert cached highlight spans to TextRuns for a single line,
/// using the current font. Called per-line in prepaint.
fn highlight_to_runs(
    content: &str,
    spans: &[(Range<usize>, HighlightStyle)],
    fallback_color: Hsla,
    base_font: &Font,
) -> Vec<TextRun> {
    let content_len = content.len();
    let mut runs = Vec::new();
    let mut pos = 0usize;

    for (range, style) in spans {
        if range.start > pos {
            runs.push(TextRun {
                len: range.start - pos,
                font: base_font.clone(),
                color: fallback_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
        runs.push(TextRun {
            len: range.end - range.start,
            font: Font {
                weight: style.font_weight.unwrap_or(base_font.weight),
                style: style.font_style.unwrap_or(base_font.style),
                ..base_font.clone()
            },
            color: style.color.unwrap_or(fallback_color),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        pos = range.end;
    }

    if pos < content_len {
        runs.push(TextRun {
            len: content_len - pos,
            font: base_font.clone(),
            color: fallback_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }

    runs
}

// ── Scroll + scrollbar state (persisted across frames) ──────────

#[derive(Clone, Copy)]
struct ScrollbarInner {
    offset_x: f32,
    dragging: bool,
    drag_pos_x: f32,
    hovered: bool,
    hovered_thumb: bool,
    last_scroll_time: Option<Instant>,
}

impl Default for ScrollbarInner {
    fn default() -> Self {
        Self {
            offset_x: 0.0,
            dragging: false,
            drag_pos_x: 0.0,
            hovered: false,
            hovered_thumb: false,
            last_scroll_time: None,
        }
    }
}

#[derive(Clone)]
pub struct DiffScrollState(Rc<Cell<ScrollbarInner>>);

impl DiffScrollState {
    pub fn new() -> Self {
        Self(Rc::new(Cell::new(ScrollbarInner::default())))
    }

    fn get(&self) -> ScrollbarInner { self.0.get() }
    fn set(&self, v: ScrollbarInner) { self.0.set(v); }
}

// ── DiffBlock element ───────────────────────────────────────────

pub struct DiffBlock {
    id: ElementId,
    lines: Arc<Vec<DiffDisplayLine>>,
    highlights: Option<Arc<HighlightCache>>,
    scroll: DiffScrollState,
}

impl DiffBlock {
    pub fn new(
        id: ElementId,
        lines: Arc<Vec<DiffDisplayLine>>,
        highlights: Option<Arc<HighlightCache>>,
        scroll: DiffScrollState,
    ) -> Self {
        Self { id, lines, highlights, scroll }
    }
}

// Prepaint output
pub struct DiffPrepaint {
    shaped_lines: Vec<(DiffDisplayLine, Option<ShapedLine>, Option<ShapedLine>)>,
    hitbox: Hitbox,
    bar_hitbox: Hitbox,
    content_width: Pixels,
    thumb_bounds: Option<ThumbGeometry>,
}

#[derive(Clone, Copy)]
struct ThumbGeometry {
    track_bounds: Bounds<Pixels>,
    thumb_bounds: Bounds<Pixels>,
    container_w: f32,
    total_w: f32,
    thumb_w: f32,
}

impl IntoElement for DiffBlock {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for DiffBlock {
    type RequestLayoutState = ();
    type PrepaintState = DiffPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        let layout_id = window.request_layout(style, [], cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> DiffPrepaint {
        let font_sz = px(13.0);
        let mono = font("Menlo");

        let mut max_content_w = px(0.0);
        let mut shaped_lines = Vec::with_capacity(self.lines.len());

        for (i, line) in self.lines.iter().enumerate() {
            let shaped_gutter = if !line.lineno.is_empty() {
                let run = TextRun {
                    len: line.lineno.len(),
                    font: mono.clone(),
                    color: t::text_faint().into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                Some(window.text_system().shape_line(
                    line.lineno.clone(),
                    font_sz,
                    &[run],
                    None,
                ))
            } else {
                None
            };

            let shaped_content = if !line.content.is_empty() {
                let (fallback_color, _) = line_colors(line);

                // Use cached highlight spans if available for this line
                let runs = self.highlights.as_ref()
                    .and_then(|h| h.get(i))
                    .and_then(|s| s.as_ref())
                    .map(|spans| highlight_to_runs(&line.content, spans, fallback_color.into(), &mono))
                    .unwrap_or_else(|| vec![TextRun {
                        len: line.content.len(),
                        font: mono.clone(),
                        color: fallback_color.into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }]);

                let shaped = window.text_system().shape_line(
                    line.content.clone(),
                    font_sz,
                    &runs,
                    None,
                );
                if shaped.width > max_content_w {
                    max_content_w = shaped.width;
                }
                Some(shaped)
            } else {
                None
            };

            shaped_lines.push((line.clone(), shaped_gutter, shaped_content));
        }

        let content_width = max_content_w + px(CONTENT_PAD + 16.0);
        let gutter_x_end = bounds.origin.x + px(GUTTER_WIDTH + GUTTER_PAD);
        let content_area_w = bounds.size.width - px(GUTTER_WIDTH + GUTTER_PAD);

        let max_scroll_x = (f32::from(content_width) - f32::from(content_area_w)).max(0.0);
        let mut s = self.scroll.get();
        s.offset_x = s.offset_x.clamp(0.0, max_scroll_x);
        self.scroll.set(s);

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        let (bar_hitbox, thumb_bounds) = if content_width > content_area_w {
            let container_w = f32::from(content_area_w);
            let total_w = f32::from(content_width);
            let thumb_w = (container_w / total_w * container_w).max(MIN_THUMB_SIZE);
            let scroll_frac = if (total_w - container_w).abs() > 0.01 {
                s.offset_x / (total_w - container_w)
            } else {
                0.0
            };
            let thumb_x = scroll_frac * (container_w - thumb_w);

            let track_bounds = Bounds {
                origin: point(
                    gutter_x_end,
                    bounds.origin.y + bounds.size.height - px(SCROLLBAR_TRACK_HEIGHT),
                ),
                size: size(content_area_w, px(SCROLLBAR_TRACK_HEIGHT)),
            };

            let is_active = s.dragging || s.hovered_thumb;
            let tw = if is_active { THUMB_ACTIVE_WIDTH } else { THUMB_WIDTH };

            let thumb_bounds = Bounds {
                origin: point(
                    gutter_x_end + px(THUMB_INSET + thumb_x),
                    bounds.origin.y + bounds.size.height - px(THUMB_INSET + tw),
                ),
                size: size(px(thumb_w - THUMB_INSET * 2.0), px(tw)),
            };

            let bh = window.insert_hitbox(track_bounds, HitboxBehavior::Normal);
            (bh, Some(ThumbGeometry {
                track_bounds,
                thumb_bounds,
                container_w,
                total_w,
                thumb_w,
            }))
        } else {
            let bh = window.insert_hitbox(
                Bounds { origin: bounds.origin, size: Size::default() },
                HitboxBehavior::Normal,
            );
            (bh, None)
        };

        DiffPrepaint {
            shaped_lines,
            hitbox,
            bar_hitbox,
            content_width,
            thumb_bounds,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut DiffPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let line_h = px(LINE_HEIGHT);
        let s = self.scroll.get();
        let scroll_x = px(s.offset_x);
        let gutter_x_end = bounds.origin.x + px(GUTTER_WIDTH + GUTTER_PAD);
        let line_count = prepaint.shaped_lines.len();

        let content_bounds = Bounds {
            origin: point(gutter_x_end, bounds.origin.y),
            size: size(bounds.size.width - px(GUTTER_WIDTH + GUTTER_PAD), bounds.size.height),
        };

        // 1) Line backgrounds (full width)
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for i in 0..line_count {
                let (ref line, _, _) = prepaint.shaped_lines[i];
                let y = bounds.origin.y + px(i as f32 * LINE_HEIGHT);
                let (_, bg) = line_colors(line);
                if let Some(bg_color) = bg {
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(bounds.origin.x, y),
                            size: size(bounds.size.width, line_h),
                        },
                        bg_color,
                    ));
                }
            }
        });

        // 2) Content text (clipped to content area, scrolled)
        window.with_content_mask(Some(ContentMask { bounds: content_bounds }), |window| {
            for i in 0..line_count {
                let (_, _, ref shaped_content) = prepaint.shaped_lines[i];
                let y = bounds.origin.y + px(i as f32 * LINE_HEIGHT);
                if let Some(sc) = shaped_content {
                    let content_x = gutter_x_end + px(CONTENT_PAD) - scroll_x;
                    let _ = sc.paint(point(content_x, y), line_h, window, cx);
                }
            }
        });

        // 3) Gutter overlay (fixed, on top)
        let gutter_bounds = Bounds {
            origin: bounds.origin,
            size: size(px(GUTTER_WIDTH + GUTTER_PAD), bounds.size.height),
        };
        window.with_content_mask(Some(ContentMask { bounds: gutter_bounds }), |window| {
            window.paint_quad(fill(gutter_bounds, t::bg_base()));
            for i in 0..line_count {
                let (ref line, _, _) = prepaint.shaped_lines[i];
                let y = bounds.origin.y + px(i as f32 * LINE_HEIGHT);
                let (_, bg) = line_colors(line);
                if let Some(bg_color) = bg {
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(bounds.origin.x, y),
                            size: size(px(GUTTER_WIDTH + GUTTER_PAD), line_h),
                        },
                        bg_color,
                    ));
                }
            }
            for i in 0..line_count {
                let (_, ref shaped_gutter, _) = prepaint.shaped_lines[i];
                let y = bounds.origin.y + px(i as f32 * LINE_HEIGHT);
                if let Some(sg) = shaped_gutter {
                    let gx = bounds.origin.x + px(GUTTER_WIDTH) - sg.width - px(4.0);
                    let _ = sg.paint(point(gx, y), line_h, window, cx);
                }
            }
            let sep_x = bounds.origin.x + px(GUTTER_WIDTH + GUTTER_PAD / 2.0);
            window.paint_quad(fill(
                Bounds {
                    origin: point(sep_x, bounds.origin.y),
                    size: size(px(1.0), bounds.size.height),
                },
                t::border(),
            ));
        });

        // 4) Scrollbar rendering + interaction
        if let Some(geom) = prepaint.thumb_bounds {
            let is_visible = s.dragging || s.hovered || scrollbar_is_visible(&self.scroll);

            if is_visible {
                let (thumb_color, radius) = if s.dragging || s.hovered_thumb {
                    (gpui::hsla(0.0, 0.0, 1.0, 0.5), px(THUMB_ACTIVE_RADIUS))
                } else if s.hovered {
                    (gpui::hsla(0.0, 0.0, 1.0, 0.4), px(THUMB_ACTIVE_RADIUS))
                } else {
                    (gpui::hsla(0.0, 0.0, 1.0, scrollbar_opacity(&self.scroll) * 0.35), px(THUMB_RADIUS))
                };

                window.paint_quad(
                    fill(geom.thumb_bounds, thumb_color).corner_radii(radius),
                );
            }

            window.set_cursor_style(CursorStyle::default(), &prepaint.bar_hitbox);

            let scroll = self.scroll.clone();
            let thumb_bounds = geom.thumb_bounds;
            let track_bounds = geom.track_bounds;
            let thumb_w = geom.thumb_w;
            let total_w = geom.total_w;
            let container_w = geom.container_w;

            window.on_mouse_event({
                let scroll = scroll.clone();
                move |event: &MouseDownEvent, phase, _window, cx| {
                    if !phase.bubble() { return; }
                    if !track_bounds.contains(&event.position) { return; }
                    cx.stop_propagation();

                    let mut s = scroll.get();
                    s.last_scroll_time = Some(Instant::now());

                    if thumb_bounds.contains(&event.position) {
                        s.dragging = true;
                        s.drag_pos_x = f32::from(event.position.x - thumb_bounds.origin.x);
                    } else {
                        let percentage = ((f32::from(event.position.x) - f32::from(track_bounds.origin.x) - thumb_w / 2.0)
                            / (container_w - thumb_w))
                            .clamp(0.0, 1.0);
                        s.offset_x = percentage * (total_w - container_w);
                    }
                    scroll.set(s);
                }
            });

            window.on_mouse_event({
                let scroll = scroll.clone();
                let thumb_bounds = geom.thumb_bounds;
                let track_bounds = geom.track_bounds;
                move |event: &MouseMoveEvent, _phase, window, _cx| {
                    let mut s = scroll.get();
                    let mut changed = false;

                    let was_hovered = s.hovered;
                    s.hovered = track_bounds.contains(&event.position);
                    if s.hovered != was_hovered {
                        if s.hovered { s.last_scroll_time = Some(Instant::now()); }
                        changed = true;
                    }

                    let was_thumb = s.hovered_thumb;
                    s.hovered_thumb = thumb_bounds.contains(&event.position);
                    if s.hovered_thumb != was_thumb { changed = true; }

                    if s.dragging && event.dragging() {
                        let percentage = ((f32::from(event.position.x) - s.drag_pos_x - f32::from(track_bounds.origin.x))
                            / (container_w - thumb_w))
                            .clamp(0.0, 1.0);
                        let new_offset = percentage * (total_w - container_w);
                        if (new_offset - s.offset_x).abs() > 0.5 {
                            s.offset_x = new_offset;
                            s.last_scroll_time = Some(Instant::now());
                            changed = true;
                        }
                    }

                    if changed {
                        scroll.set(s);
                        window.refresh();
                    }
                }
            });

            window.on_mouse_event({
                let scroll = scroll.clone();
                move |_event: &MouseUpEvent, phase, window, _cx| {
                    if !phase.bubble() { return; }
                    let mut s = scroll.get();
                    if s.dragging {
                        s.dragging = false;
                        scroll.set(s);
                        window.refresh();
                    }
                }
            });
        }

        // 5) Scroll wheel handler
        let scroll = self.scroll.clone();
        let hitbox_id = prepaint.hitbox.id;
        let content_w = prepaint.content_width;
        let content_area_w = bounds.size.width - px(GUTTER_WIDTH + GUTTER_PAD);

        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, _cx| {
            if phase == DispatchPhase::Bubble && hitbox_id.should_handle_scroll(window) {
                let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
                let max_x = f32::from(content_w - content_area_w).max(0.0);
                let mut s = scroll.get();
                let new_x = (s.offset_x - f32::from(delta.x)).clamp(0.0, max_x);
                if (new_x - s.offset_x).abs() > 0.01 {
                    s.offset_x = new_x;
                    s.last_scroll_time = Some(Instant::now());
                    scroll.set(s);
                    window.refresh();
                }
            }
        });
    }
}

// ── Helpers ─────────────────────────────────────────────────────

fn truncate_path_middle(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    let filename = parts[parts.len() - 1];
    let first = parts[0];
    format!("{}/\u{2026}/{}", first, filename)
}

fn line_colors(line: &DiffDisplayLine) -> (Rgba, Option<Rgba>) {
    if line.is_hunk_header {
        (t::diff_hunk_header(), Some(t::bg_elevated()))
    } else {
        match line.kind {
            DiffLineKind::Context => (t::text_muted(), None),
            DiffLineKind::Addition => (t::diff_add_text(), Some(t::diff_add_bg())),
            DiffLineKind::Deletion => (t::diff_del_text(), Some(t::diff_del_bg())),
        }
    }
}

fn scrollbar_is_visible(scroll: &DiffScrollState) -> bool {
    let s = scroll.get();
    if s.dragging || s.hovered { return true; }
    match s.last_scroll_time {
        None => false,
        Some(t) => Instant::now().duration_since(t).as_secs_f32() < FADE_OUT_DURATION,
    }
}

fn scrollbar_opacity(scroll: &DiffScrollState) -> f32 {
    let s = scroll.get();
    if s.dragging || s.hovered { return 1.0; }
    match s.last_scroll_time {
        None => 0.0,
        Some(t) => {
            let elapsed = Instant::now().duration_since(t).as_secs_f32();
            if elapsed < FADE_OUT_DELAY {
                1.0
            } else if elapsed < FADE_OUT_DURATION {
                1.0 - ((elapsed - FADE_OUT_DELAY) / (FADE_OUT_DURATION - FADE_OUT_DELAY))
            } else {
                0.0
            }
        }
    }
}

// ── File section (header + diff block) ──────────────────────────

pub fn render_file_section(
    path: &str,
    status: FileStatus,
    stats: &DiffStats,
    diff: Option<&FileDiff>,
    lines: Option<&Arc<Vec<DiffDisplayLine>>>,
    scroll: &DiffScrollState,
    expanded: &Rc<Cell<bool>>,
    highlights: Option<&Arc<HighlightCache>>,
    on_keep: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    on_discard: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
) -> Div {
    let mut el = div().flex().flex_col().flex_shrink_0().pb_1();
    el = el.child(render_header(path, status, stats, expanded, on_keep, on_discard));

    if !expanded.get() {
        return el;
    }

    if let (Some(diff), Some(lines)) = (diff, lines) {
        if diff.is_binary {
            el = el.child(
                div().px_3().py_1().text_xs().text_color(t::text_faint())
                    .font_family("monospace").child("Binary file"),
            );
        } else if !lines.is_empty() {
            let block_height = lines.len() as f32 * LINE_HEIGHT + SCROLLBAR_TRACK_HEIGHT;

            el = el.child(
                div().mx_1().flex_shrink_0().h(px(block_height)).child(
                    DiffBlock::new(
                        ElementId::Name(SharedString::from(format!("diff-{}", path))),
                        lines.clone(),
                        highlights.cloned(),
                        scroll.clone(),
                    ),
                ),
            );
        }
    }

    el
}

fn render_header(
    path: &str,
    status: FileStatus,
    stats: &DiffStats,
    expanded: &Rc<Cell<bool>>,
    on_keep: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    on_discard: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let is_expanded = expanded.get();
    let chevron = if is_expanded { "▾" } else { "▸" };
    let expanded = expanded.clone();

    let mut h = div()
        .id(SharedString::from(format!("hdr-{}", path)))
        .mx_1().mt_1().px_2().py(px(6.0))
        .flex().items_center().gap_1p5().overflow_hidden()
        .rounded(px(5.0))
        .bg(t::bg_surface()).border_1().border_color(t::border())
        .cursor_pointer()
        .hover(|s: StyleRefinement| s.bg(t::bg_hover()))
        .on_click(move |_: &ClickEvent, _window, _cx| {
            expanded.set(!expanded.get());
        })
        .child(
            div().text_xs().text_color(t::text_faint()).flex_shrink_0()
                .w(px(10.0)).child(chevron),
        )
        .child(
            div().text_xs().font_weight(FontWeight::BOLD)
                .text_color(status.color()).flex_shrink_0().child(status.label()),
        )
        .child(
            div().text_xs().text_color(t::text_secondary())
                .font_weight(FontWeight::MEDIUM).flex_grow()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(SharedString::from(truncate_path_middle(path))),
        );
    if stats.additions > 0 {
        h = h.child(div().text_xs().text_color(t::diff_add_text()).flex_shrink_0()
            .child(SharedString::from(format!("+{}", stats.additions))));
    }
    if stats.deletions > 0 {
        h = h.child(div().text_xs().text_color(t::diff_del_text()).flex_shrink_0()
            .child(SharedString::from(format!("-{}", stats.deletions))));
    }

    // Per-file action buttons (stop propagation so they don't toggle the accordion)
    if let Some(on_discard) = on_discard {
        h = h.child(
            div().id(SharedString::from(format!("discard-{}", path)))
                .ml_1().px_1p5().py(px(2.0)).rounded(px(3.0))
                .text_xs().text_color(t::text_dim()).flex_shrink_0()
                .cursor_pointer()
                .hover(|s: StyleRefinement| s.bg(t::bg_hover()).text_color(t::diff_del_text()))
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    on_discard(event, window, cx);
                })
                .child("Discard"),
        );
    }
    if let Some(on_keep) = on_keep {
        h = h.child(
            div().id(SharedString::from(format!("keep-{}", path)))
                .px_1p5().py(px(2.0)).rounded(px(3.0))
                .text_xs().text_color(t::text_dim()).flex_shrink_0()
                .cursor_pointer()
                .hover(|s: StyleRefinement| s.bg(t::bg_hover()).text_color(t::diff_add_text()))
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    on_keep(event, window, cx);
                })
                .child("Keep"),
        );
    }

    h
}
