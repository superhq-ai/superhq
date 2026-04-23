//! Terminal rendering module.
//!
//! This module provides [`TerminalRenderer`], which handles efficient rendering of
//! terminal content using GPUI's text and drawing systems.
//!
//! # Rendering Pipeline
//!
//! The renderer processes the terminal grid in several stages:
//!
//! ```text
//! Terminal Grid → Layout Phase → Paint Phase
//!                      │              │
//!                      ├─ Collect backgrounds
//!                      ├─ Batch text runs
//!                      │              │
//!                      │              ├─ Paint default background
//!                      │              ├─ Paint non-default backgrounds
//!                      │              ├─ Paint text characters
//!                      │              └─ Paint cursor
//! ```
//!
//! # Optimizations
//!
//! The renderer includes several optimizations to minimize draw calls:
//!
//! 1. **Background Merging**: Adjacent cells with the same background color are
//!    merged into single rectangles, reducing the number of quads to paint.
//!
//! 2. **Text Batching**: Adjacent cells with identical styling (color, bold, italic)
//!    are grouped into [`BatchedTextRun`]s for efficient text shaping.
//!
//! 3. **Default Background Skip**: Cells with the default background color don't
//!    generate separate background rectangles.
//!
//! 4. **Cell Measurement**: Font metrics are measured once using the '│' (BOX DRAWINGS
//!    LIGHT VERTICAL) character and cached for consistent cell dimensions.
//!
//! # Cell Dimensions
//!
//! Cell size is calculated from actual font metrics using the '│' character,
//! which spans the full cell height in properly designed terminal fonts:
//!
//! - **Width**: Measured from shaped '│' character
//! - **Height**: `(ascent + descent) × line_height_multiplier`
//!
//! The `line_height_multiplier` (default 1.0) can be adjusted to add extra
//! vertical space if needed for specific fonts.
//!
//! # Example
//!
//! ```ignore
//! use gpui::px;
//! use gpui_terminal::{ColorPalette, TerminalRenderer};
//!
//! let renderer = TerminalRenderer::new(
//!     "JetBrains Mono".to_string(),
//!     px(14.0),
//!     1.0,  // line height multiplier
//!     ColorPalette::default(),
//! );
//! ```

use crate::block_elements;
use crate::box_drawing;
use crate::colors::ColorPalette;
use crate::event::GpuiEventProxy;
use std::sync::LazyLock;

static URL_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"https?://[^\s)>\]'""]+"#).expect("Invalid URL regex")
});

/// A detected URL in the terminal grid.
#[derive(Clone, Debug)]
pub struct UrlHit {
    pub line_idx: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub url: String,
}
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point as AlacPoint};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::Color;
use gpui::{
    App, Bounds, Edges, Font, FontFeatures, FontStyle, FontWeight, Hsla, Pixels, Point,
    SharedString, Size, TextRun, UnderlineStyle, Window, px, quad, transparent_black,
};

/// A batched run of text with consistent styling.
///
/// This struct groups adjacent terminal cells with identical visual attributes
/// to reduce the number of text rendering calls.
#[derive(Debug, Clone)]
pub struct BatchedTextRun {
    /// The text content to render
    pub text: String,

    /// Starting column position
    pub start_col: usize,

    /// Row position
    pub row: usize,

    /// Foreground color
    pub fg_color: Hsla,

    /// Background color
    pub bg_color: Hsla,

    /// Bold flag
    pub bold: bool,

    /// Italic flag
    pub italic: bool,

    /// Underline flag
    pub underline: bool,
}

/// Background rectangle to paint.
///
/// Represents a rectangular region with a solid color background.
#[derive(Debug, Clone)]
pub struct BackgroundRect {
    /// Starting column position
    pub start_col: usize,

    /// Ending column position (exclusive)
    pub end_col: usize,

    /// Row position
    pub row: usize,

    /// Background color
    pub color: Hsla,
}

impl BackgroundRect {
    /// Check if this rectangle can be merged with another.
    ///
    /// Two rectangles can be merged if they:
    /// - Are on the same row
    /// - Have the same color
    /// - Are horizontally adjacent
    fn can_merge_with(&self, other: &Self) -> bool {
        self.row == other.row && self.color == other.color && self.end_col == other.start_col
    }
}

/// Terminal renderer with font settings and cell dimensions.
///
/// This struct manages the rendering of terminal content, including text,
/// backgrounds, and cursor. It maintains font metrics and provides the
/// [`paint`](Self::paint) method for drawing the terminal grid.
///
/// # Font Metrics
///
/// Cell dimensions are calculated from actual font measurements via
/// [`measure_cell`](Self::measure_cell). This ensures accurate character
/// positioning regardless of the font used.
///
/// # Usage
///
/// The renderer is typically used internally by [`TerminalView`](crate::TerminalView),
/// but can also be used directly for custom rendering:
///
/// ```ignore
/// // Measure cell dimensions (call once per font change)
/// renderer.measure_cell(window);
///
/// // Paint the terminal grid
/// renderer.paint(bounds, padding, &term, window, cx);
/// ```
///
/// # Performance
///
/// For optimal performance:
/// - Call `measure_cell` only when font settings change
/// - The `paint` method is designed to be called every frame
/// - Background and text batching minimize GPU draw calls
#[derive(Clone)]
pub struct TerminalRenderer {
    /// Font family name (e.g., "Fira Code", "Menlo")
    pub font_family: String,

    /// Font size in pixels
    pub font_size: Pixels,

    /// Width of a single character cell
    pub cell_width: Pixels,

    /// Height of a single character cell (line height)
    pub cell_height: Pixels,

    /// Multiplier for line height to accommodate tall glyphs
    pub line_height_multiplier: f32,

    /// Color palette for resolving terminal colors
    pub palette: ColorPalette,
}

impl TerminalRenderer {
    /// Creates a new terminal renderer with the given font settings and color palette.
    ///
    /// # Arguments
    ///
    /// * `font_family` - The name of the font family to use
    /// * `font_size` - The font size in pixels
    /// * `line_height_multiplier` - Multiplier for line height (e.g., 1.2 for 20% extra)
    /// * `palette` - The color palette to use for terminal colors
    ///
    /// # Returns
    ///
    /// A new `TerminalRenderer` instance with default cell dimensions.
    ///
    /// # Examples
    ///
    /// ```
    /// use gpui::px;
    /// use gpui_terminal::render::TerminalRenderer;
    /// use gpui_terminal::ColorPalette;
    ///
    /// let renderer = TerminalRenderer::new("Fira Code".to_string(), px(14.0), 1.0, ColorPalette::default());
    /// ```
    pub fn new(
        font_family: String,
        font_size: Pixels,
        line_height_multiplier: f32,
        palette: ColorPalette,
    ) -> Self {
        // Default cell dimensions - will be measured on first paint
        // Using 0.6 as approximate em-width ratio for monospace fonts
        let cell_width = font_size * 0.6;
        let cell_height = font_size * 1.4; // Line height with some spacing

        Self {
            font_family,
            font_size,
            cell_width,
            cell_height,
            line_height_multiplier,
            palette,
        }
    }

    /// Measure cell dimensions based on actual font metrics.
    ///
    /// This method measures the actual width and height of characters
    /// using the GPUI text system. It uses the '│' (BOX DRAWINGS LIGHT VERTICAL)
    /// character which spans the full cell height in properly designed terminal fonts.
    ///
    /// # Arguments
    ///
    /// * `window` - The GPUI window for text system access
    pub fn measure_cell(&mut self, window: &mut Window) {
        // Measure using '│' (U+2502, BOX DRAWINGS LIGHT VERTICAL)
        // This character spans the full cell height in terminal fonts, making it
        // ideal for measuring exact cell dimensions used by TUIs
        let font = Font {
            family: self.font_family.clone().into(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
        };

        let text_run = TextRun {
            len: "│".len(),
            font,
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        // Shape the box-drawing character to get cell metrics
        let shaped = window
            .text_system()
            .shape_line("│".into(), self.font_size, &[text_run], None);

        // Get the width from the shaped line
        if shaped.width > px(0.0) {
            self.cell_width = shaped.width;
        }

        // Calculate height from ascent + descent with optional multiplier
        let line_height = (shaped.ascent + shaped.descent).ceil();
        if line_height > px(0.0) {
            self.cell_height = line_height * self.line_height_multiplier;
        }
    }

    /// Layout cells into batched text runs and background rects for a single row.
    ///
    /// This method processes a row of terminal cells and groups adjacent cells
    /// with identical styling into batched runs. It also collects background
    /// rectangles that need to be painted.
    ///
    /// # Arguments
    ///
    /// * `row` - The row number
    /// * `cells` - Iterator over (column, Cell) pairs
    /// * `colors` - Terminal color configuration
    ///
    /// # Returns
    ///
    /// A tuple of `(backgrounds, text_runs)` where:
    /// - `backgrounds` is a vector of merged background rectangles
    /// - `text_runs` is a vector of batched text runs
    pub fn layout_row(
        &self,
        row: usize,
        cells: impl Iterator<Item = (usize, Cell)>,
        colors: &Colors,
    ) -> (Vec<BackgroundRect>, Vec<BatchedTextRun>) {
        let mut backgrounds = Vec::new();
        let mut text_runs = Vec::new();

        let mut current_run: Option<BatchedTextRun> = None;
        let mut current_bg: Option<BackgroundRect> = None;

        for (col, cell) in cells {
            // Skip wide character spacers
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }

            // Extract cell styling
            let fg_color = self.palette.resolve(cell.fg, colors);
            let bg_color = self.palette.resolve(cell.bg, colors);
            let bold = cell.flags.contains(Flags::BOLD);
            let italic = cell.flags.contains(Flags::ITALIC);
            let underline = cell.flags.contains(Flags::UNDERLINE);

            // Get the character (or space if empty)
            let ch = if cell.c == ' ' || cell.c == '\0' {
                ' '
            } else {
                cell.c
            };

            // Handle background rectangles
            if let Some(ref mut bg_rect) = current_bg {
                if bg_rect.color == bg_color && bg_rect.end_col == col {
                    // Extend current background
                    bg_rect.end_col = col + 1;
                } else {
                    // Save current background and start new one
                    backgrounds.push(bg_rect.clone());
                    current_bg = Some(BackgroundRect {
                        start_col: col,
                        end_col: col + 1,
                        row,
                        color: bg_color,
                    });
                }
            } else {
                // Start new background
                current_bg = Some(BackgroundRect {
                    start_col: col,
                    end_col: col + 1,
                    row,
                    color: bg_color,
                });
            }

            // Handle text runs
            if let Some(ref mut run) = current_run {
                if run.fg_color == fg_color
                    && run.bg_color == bg_color
                    && run.bold == bold
                    && run.italic == italic
                    && run.underline == underline
                {
                    // Extend current run
                    run.text.push(ch);
                } else {
                    // Save current run and start new one
                    text_runs.push(run.clone());
                    current_run = Some(BatchedTextRun {
                        text: ch.to_string(),
                        start_col: col,
                        row,
                        fg_color,
                        bg_color,
                        bold,
                        italic,
                        underline,
                    });
                }
            } else {
                // Start new run
                current_run = Some(BatchedTextRun {
                    text: ch.to_string(),
                    start_col: col,
                    row,
                    fg_color,
                    bg_color,
                    bold,
                    italic,
                    underline,
                });
            }
        }

        // Push final run and background
        if let Some(run) = current_run {
            text_runs.push(run);
        }
        if let Some(bg) = current_bg {
            backgrounds.push(bg);
        }

        // Merge adjacent backgrounds with same color
        let merged_backgrounds = self.merge_backgrounds(backgrounds);

        (merged_backgrounds, text_runs)
    }

    /// Merge adjacent background rects with same color.
    ///
    /// This optimization reduces the number of rectangles to paint by
    /// combining horizontally adjacent rectangles that share the same color.
    ///
    /// # Arguments
    ///
    /// * `rects` - Vector of background rectangles to merge
    ///
    /// # Returns
    ///
    /// A new vector with merged rectangles
    fn merge_backgrounds(&self, mut rects: Vec<BackgroundRect>) -> Vec<BackgroundRect> {
        if rects.is_empty() {
            return rects;
        }

        let mut merged = Vec::new();
        let mut current = rects.remove(0);

        for rect in rects {
            if current.can_merge_with(&rect) {
                current.end_col = rect.end_col;
            } else {
                merged.push(current);
                current = rect;
            }
        }

        merged.push(current);
        merged
    }

    /// Paint terminal content to the window.
    ///
    /// This is the main rendering method that draws the terminal grid,
    /// including backgrounds, text, and cursor.
    ///
    /// # Arguments
    ///
    /// * `bounds` - The bounding box to render within
    /// * `padding` - Padding around the terminal content
    /// * `term` - The terminal state
    /// * `window` - The GPUI window
    /// * `cx` - The application context
    pub fn paint(
        &self,
        bounds: Bounds<Pixels>,
        padding: Edges<Pixels>,
        term: &Term<GpuiEventProxy>,
        selection_range: &Option<crate::terminal::SelectionRange>,
        cursor_shape: crate::terminal::CursorShape,
        is_focused: bool,
        hovered_url: Option<&std::rc::Rc<std::cell::RefCell<Option<UrlHit>>>>,
        url_hits_out: Option<&std::rc::Rc<std::cell::RefCell<Vec<UrlHit>>>>,
        window: &mut Window,
        _cx: &mut App,
    ) {
        // Get terminal dimensions
        let grid = term.grid();
        let num_lines = grid.screen_lines();
        let num_cols = grid.columns();
        let colors = term.colors();

        // Calculate default background color
        let default_bg = self.palette.resolve(
            Color::Named(alacritty_terminal::vte::ansi::NamedColor::Background),
            colors,
        );

        // Paint default background (covers full bounds including padding)
        window.paint_quad(quad(
            bounds,
            px(0.0),
            default_bg,
            Edges::<Pixels>::default(),
            transparent_black(),
            Default::default(),
        ));

        // Calculate origin offset (content starts after padding)
        let origin = Point {
            x: bounds.origin.x + padding.left,
            y: bounds.origin.y + padding.top,
        };

        // display_offset: 0 = at bottom (latest), N = scrolled back N lines
        let display_offset = grid.display_offset() as i32;

        // Iterate over visible lines, offsetting into scrollback when scrolled
        for line_idx in 0..num_lines {
            // Line(0) = bottom of visible area. Negative lines = scrollback history.
            // When display_offset > 0, shift all lines up into history.
            let line = Line(line_idx as i32 - display_offset);

            // Collect cells for this line
            let cells: Vec<(usize, Cell)> = (0..num_cols)
                .map(|col_idx| {
                    let col = Column(col_idx);
                    let point = AlacPoint::new(line, col);
                    let cell = grid[point].clone();
                    (col_idx, cell)
                })
                .collect();

            // Layout the row for backgrounds
            let (backgrounds, _) = self.layout_row(line_idx, cells.iter().cloned(), colors);

            // Paint backgrounds
            for bg_rect in backgrounds {
                // Skip if it's the default background color
                if bg_rect.color == default_bg {
                    continue;
                }

                let x = origin.x + self.cell_width * (bg_rect.start_col as f32);
                let y = origin.y + self.cell_height * (bg_rect.row as f32);
                let width = self.cell_width * ((bg_rect.end_col - bg_rect.start_col) as f32);
                let height = self.cell_height;

                let rect_bounds = Bounds {
                    origin: Point { x, y },
                    size: Size { width, height },
                };

                window.paint_quad(quad(
                    rect_bounds,
                    px(0.0),
                    bg_rect.color,
                    Edges::<Pixels>::default(),
                    transparent_black(),
                    Default::default(),
                ));
            }

            // Calculate vertical offset to center text in cell
            // The multiplier adds extra height; we want to distribute it evenly top/bottom
            let base_height = self.cell_height / self.line_height_multiplier;
            let vertical_offset = (self.cell_height - base_height) / 2.0;

            let y_base = origin.y + self.cell_height * (line_idx as f32);
            let cy = y_base + self.cell_height / 2.0;

            // Use cells vec for multiple passes (already collected above)
            let cells_vec = &cells;

            // First pass: find and draw horizontal spans of box-drawing characters
            // This draws continuous lines across multiple cells to avoid gaps
            let mut processed_horizontal: std::collections::HashSet<usize> = std::collections::HashSet::new();

            let mut i = 0;
            while i < cells_vec.len() {
                let (col_idx, ref cell) = cells_vec[i];
                let ch = cell.c;

                // Check if this starts a horizontal span
                if let Some(weight) = box_drawing::get_horizontal_weight(ch) {
                    let fg_color = self.palette.resolve(cell.fg, colors);
                    let start_col = col_idx;
                    let mut end_col = col_idx;

                    // Look ahead for consecutive cells with same horizontal weight
                    let mut j = i + 1;
                    while j < cells_vec.len() {
                        let (next_col, ref next_cell) = cells_vec[j];
                        // Must be adjacent
                        if next_col != end_col + 1 {
                            break;
                        }
                        // Must have same horizontal weight and same color
                        let next_fg = self.palette.resolve(next_cell.fg, colors);
                        if box_drawing::get_horizontal_weight(next_cell.c) == Some(weight)
                            && next_fg == fg_color
                        {
                            end_col = next_col;
                            j += 1;
                        } else {
                            break;
                        }
                    }

                    // Draw the horizontal span
                    let start_x = origin.x + self.cell_width * (start_col as f32);
                    let end_x = origin.x + self.cell_width * ((end_col + 1) as f32);

                    box_drawing::draw_horizontal_span(
                        start_x,
                        end_x,
                        cy,
                        weight,
                        self.cell_width,
                        fg_color,
                        window,
                    );

                    // Mark these columns as having horizontal drawn
                    for col in start_col..=end_col {
                        processed_horizontal.insert(col);
                    }

                    // Skip past this span
                    i = j;
                    continue;
                }
                i += 1;
            }

            // Second pass: draw vertical components and non-horizontal box chars
            for (col_idx, cell) in cells_vec.iter() {
                let ch = cell.c;

                if ch == ' ' || ch == '\0' {
                    continue;
                }

                let x = origin.x + self.cell_width * (*col_idx as f32);
                let fg_color = self.palette.resolve(cell.fg, colors);

                if box_drawing::is_box_drawing_char(ch) {
                    let cell_bounds = Bounds {
                        origin: Point { x, y: y_base },
                        size: Size {
                            width: self.cell_width,
                            height: self.cell_height,
                        },
                    };

                    if processed_horizontal.contains(col_idx) {
                        // Horizontal already drawn, just draw vertical components
                        box_drawing::draw_vertical_components(
                            ch,
                            cell_bounds,
                            fg_color,
                            self.cell_width,
                            window,
                        );
                    } else {
                        // Not part of a horizontal span, draw the whole character
                        box_drawing::draw_box_character(
                            ch,
                            cell_bounds,
                            fg_color,
                            self.cell_width,
                            window,
                        );
                    }
                    continue;
                }

                if block_elements::is_block_element(ch) {
                    let cell_bounds = Bounds {
                        origin: Point { x, y: y_base },
                        size: Size {
                            width: self.cell_width,
                            height: self.cell_height,
                        },
                    };
                    block_elements::draw_block_element(ch, cell_bounds, fg_color, window);
                    continue;
                }
            }

            // Third pass: draw regular text characters
            for (col_idx, cell) in cells_vec.iter() {
                let ch = cell.c;

                // Skip empty cells and box/block glyphs (already handled)
                if ch == ' '
                    || ch == '\0'
                    || box_drawing::is_box_drawing_char(ch)
                    || block_elements::is_block_element(ch)
                {
                    continue;
                }

                let x = origin.x + self.cell_width * (*col_idx as f32);
                let fg_color = self.palette.resolve(cell.fg, colors);

                // For regular text, apply vertical offset for centering
                let y = y_base + vertical_offset;

                // Get cell flags for styling
                let flags = cell.flags;
                let bold = flags.contains(alacritty_terminal::term::cell::Flags::BOLD);
                let italic = flags.contains(alacritty_terminal::term::cell::Flags::ITALIC);
                let underline = flags.contains(alacritty_terminal::term::cell::Flags::UNDERLINE);

                // Create font with styling
                let font = Font {
                    family: self.font_family.clone().into(),
                    features: FontFeatures::default(),
                    fallbacks: None,
                    weight: if bold {
                        FontWeight::BOLD
                    } else {
                        FontWeight::NORMAL
                    },
                    style: if italic {
                        FontStyle::Italic
                    } else {
                        FontStyle::Normal
                    },
                };

                // Create text run for this single character
                let char_str = ch.to_string();
                let text_run = TextRun {
                    len: char_str.len(),
                    font,
                    color: fg_color,
                    background_color: None,
                    underline: if underline {
                        Some(UnderlineStyle {
                            thickness: px(1.0),
                            color: Some(fg_color),
                            wavy: false,
                        })
                    } else {
                        None
                    },
                    strikethrough: None,
                };

                // Shape and paint the character
                let text: SharedString = char_str.into();
                let shaped_line =
                    window
                        .text_system()
                        .shape_line(text, self.font_size, &[text_run], None);

                // Paint at exact cell position (ignore errors)
                let _ = shaped_line.paint(Point { x, y }, self.cell_height, window, _cx);
            }
        }

        // Detect and paint URL underlines
        let url_hits = self.detect_urls(grid, display_offset, num_lines, num_cols);
        if !url_hits.is_empty() {
            let hovered_idx = hovered_url.as_ref()
                .and_then(|h| h.borrow().as_ref().map(|u| u.line_idx * 10000 + u.start_col));
            for hit in &url_hits {
                let hit_key = hit.line_idx * 10000 + hit.start_col;
                let is_hovered = hovered_idx == Some(hit_key);
                let underline_color = if is_hovered {
                    gpui::hsla(0.0, 0.0, 0.7, 0.8)
                } else {
                    gpui::hsla(0.0, 0.0, 0.5, 0.3)
                };
                let y = origin.y + self.cell_height * (hit.line_idx as f32) + self.cell_height - px(1.0);
                let x = origin.x + self.cell_width * (hit.start_col as f32);
                let w = self.cell_width * ((hit.end_col - hit.start_col) as f32);
                window.paint_quad(quad(
                    Bounds {
                        origin: Point { x, y },
                        size: Size { width: w, height: px(1.0) },
                    },
                    px(0.0),
                    underline_color,
                    Edges::<Pixels>::default(),
                    transparent_black(),
                    Default::default(),
                ));
            }
        }

        // Store URL hits for mouse handler access
        if let Some(url_state) = url_hits_out {
            *url_state.borrow_mut() = url_hits;
        }

        // Paint selection highlight (between backgrounds and cursor)
        if let Some(sel_range) = selection_range {
            self.paint_selection(sel_range, display_offset, num_lines, num_cols, origin, window);
        }

        // Paint cursor. Its grid line is fixed; scrollback shifts its visual
        // row by `display_offset`. Hide it only when that row falls outside
        // the viewport, not the moment the user scrolls.
        use crate::terminal::CursorShape;

        let cursor_point = grid.cursor.point;
        let visual_line = cursor_point.line.0 + display_offset;
        if visual_line < 0 || visual_line >= num_lines as i32 {
            return;
        }

        let cursor_x = origin.x + self.cell_width * (cursor_point.column.0 as f32);
        let cursor_y = origin.y + self.cell_height * (visual_line as f32);

        let cursor_color = self.palette.resolve(
            Color::Named(alacritty_terminal::vte::ansi::NamedColor::Cursor),
            colors,
        );

        // Override to hollow when unfocused
        let shape = if !is_focused {
            CursorShape::Hollow
        } else {
            cursor_shape
        };

        match shape {
            CursorShape::Block => {
                // Filled block
                let cursor_bounds = Bounds {
                    origin: Point { x: cursor_x, y: cursor_y },
                    size: Size { width: self.cell_width, height: self.cell_height },
                };
                window.paint_quad(quad(
                    cursor_bounds, px(0.0), cursor_color,
                    Edges::<Pixels>::default(), transparent_black(), Default::default(),
                ));
            }
            CursorShape::Bar => {
                // Thin vertical line at left edge (2px wide)
                let bar_width = px(2.0);
                let cursor_bounds = Bounds {
                    origin: Point { x: cursor_x, y: cursor_y },
                    size: Size { width: bar_width, height: self.cell_height },
                };
                window.paint_quad(quad(
                    cursor_bounds, px(0.0), cursor_color,
                    Edges::<Pixels>::default(), transparent_black(), Default::default(),
                ));
            }
            CursorShape::Underline => {
                // Thin horizontal line at bottom (2px tall)
                let underline_height = px(2.0);
                let cursor_bounds = Bounds {
                    origin: Point {
                        x: cursor_x,
                        y: cursor_y + self.cell_height - underline_height,
                    },
                    size: Size { width: self.cell_width, height: underline_height },
                };
                window.paint_quad(quad(
                    cursor_bounds, px(0.0), cursor_color,
                    Edges::<Pixels>::default(), transparent_black(), Default::default(),
                ));
            }
            CursorShape::Hollow => {
                // Outline rect (border only, no fill)
                let border_width = px(1.0);
                let cursor_bounds = Bounds {
                    origin: Point { x: cursor_x, y: cursor_y },
                    size: Size { width: self.cell_width, height: self.cell_height },
                };
                window.paint_quad(quad(
                    cursor_bounds, px(0.0), transparent_black(),
                    Edges::all(border_width), cursor_color, Default::default(),
                ));
            }
        }
    }

    /// Paint selection highlight rectangles.
    ///
    /// Selection coordinates are in grid space (negative lines = scrollback).
    /// `display_offset` converts them to visual viewport rows:
    ///   visual_row = grid_line + display_offset
    fn detect_urls(
        &self,
        grid: &alacritty_terminal::grid::Grid<Cell>,
        display_offset: i32,
        num_lines: usize,
        num_cols: usize,
    ) -> Vec<UrlHit> {
        let mut hits = Vec::new();
        for line_idx in 0..num_lines {
            let line = Line(line_idx as i32 - display_offset);
            let mut row_text = String::with_capacity(num_cols);
            let mut col_offsets: Vec<usize> = Vec::with_capacity(num_cols);

            for col_idx in 0..num_cols {
                let point = AlacPoint::new(line, Column(col_idx));
                let cell = &grid[point];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                col_offsets.push(col_idx);
                let ch = if cell.c == '\0' { ' ' } else { cell.c };
                row_text.push(ch);
            }

            for m in URL_REGEX.find_iter(&row_text) {
                let char_start = row_text[..m.start()].chars().count();
                let char_end = row_text[..m.end()].chars().count();
                if char_start < col_offsets.len() && char_end <= col_offsets.len() {
                    let start_col = col_offsets[char_start];
                    let end_col = if char_end < col_offsets.len() {
                        col_offsets[char_end]
                    } else {
                        col_offsets.last().map(|c| c + 1).unwrap_or(0)
                    };
                    hits.push(UrlHit {
                        line_idx,
                        start_col,
                        end_col,
                        url: m.as_str().to_string(),
                    });
                }
            }
        }
        hits
    }

    fn paint_selection(
        &self,
        sel: &crate::terminal::SelectionRange,
        display_offset: i32,
        num_lines: usize,
        num_cols: usize,
        origin: Point<Pixels>,
        window: &mut Window,
    ) {
        let sel_color = gpui::rgba(0x264f7844); // Semi-transparent blue

        // Convert grid lines to visual viewport rows
        let start_visual = sel.start.line.0 + display_offset;
        let end_visual = sel.end.line.0 + display_offset;
        let start_col = sel.start.column.0;
        let end_col = sel.end.column.0;

        let max_line = num_lines as i32 - 1;

        for visual_line in start_visual..=end_visual.min(max_line) {
            // Skip lines outside the visible viewport
            if visual_line < 0 || visual_line >= num_lines as i32 {
                continue;
            }

            let col_start = if visual_line == start_visual { start_col } else { 0 };
            let col_end = if visual_line == end_visual {
                end_col + 1
            } else {
                num_cols
            };

            if col_start >= col_end {
                continue;
            }

            let x = origin.x + self.cell_width * (col_start as f32);
            let y = origin.y + self.cell_height * (visual_line as f32);
            let width = self.cell_width * ((col_end - col_start) as f32);

            window.paint_quad(quad(
                Bounds {
                    origin: Point { x, y },
                    size: Size {
                        width,
                        height: self.cell_height,
                    },
                },
                px(0.0),
                sel_color,
                Edges::<Pixels>::default(),
                transparent_black(),
                Default::default(),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_renderer_creation() {
        let renderer = TerminalRenderer::new(
            "Fira Code".to_string(),
            px(14.0),
            1.0,
            ColorPalette::default(),
        );
        assert_eq!(renderer.font_family, "Fira Code");
        assert_eq!(renderer.font_size, px(14.0));
        assert_eq!(renderer.line_height_multiplier, 1.0);
    }

    #[test]
    fn test_background_rect_merge() {
        let black = Hsla::black();

        let rect1 = BackgroundRect {
            start_col: 0,
            end_col: 5,
            row: 0,
            color: black,
        };

        let rect2 = BackgroundRect {
            start_col: 5,
            end_col: 10,
            row: 0,
            color: black,
        };

        assert!(rect1.can_merge_with(&rect2));

        let rect3 = BackgroundRect {
            start_col: 5,
            end_col: 10,
            row: 1,
            color: black,
        };

        assert!(!rect1.can_merge_with(&rect3));
    }

    #[test]
    fn test_merge_backgrounds() {
        let renderer = TerminalRenderer::new(
            "monospace".to_string(),
            px(14.0),
            1.0,
            ColorPalette::default(),
        );
        let black = Hsla::black();

        let rects = vec![
            BackgroundRect {
                start_col: 0,
                end_col: 5,
                row: 0,
                color: black,
            },
            BackgroundRect {
                start_col: 5,
                end_col: 10,
                row: 0,
                color: black,
            },
        ];

        let merged = renderer.merge_backgrounds(rects);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].start_col, 0);
        assert_eq!(merged[0].end_col, 10);
    }
}
