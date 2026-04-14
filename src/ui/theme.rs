use gpui::{div, px, Div, ParentElement as _, Rgba, Styled as _, WindowAppearance};
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

// ── Color type with hex deserialization ─────────────────────────

/// A color parsed from a hex string like "#1a1b26" or "#ffffff0a".
#[derive(Clone, Copy)]
pub struct Color(pub Rgba);

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: String = Deserialize::deserialize(deserializer)?;
        parse_hex_to_rgba(&s)
            .map(Color)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid hex color: {s}")))
    }
}

fn parse_hex_to_rgba(hex: &str) -> Option<Rgba> {
    let hex = hex.trim_start_matches('#');
    let (r, g, b, a) = match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            (r, g, b, 255u8)
        }
        8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
            (r, g, b, a)
        }
        _ => return None,
    };
    Some(Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    })
}

// ── Theme struct — every field maps 1:1 to JSON ────────────────

#[derive(Deserialize)]
pub struct ThemeColors {
    pub bg_base: Color,
    pub bg_terminal: Color,
    pub terminal_foreground: Color,
    pub terminal_cursor: Color,
    pub bg_surface: Color,
    pub bg_elevated: Color,
    pub bg_hover: Color,
    pub bg_active: Color,
    pub bg_selected: Color,
    pub bg_input: Color,

    pub border: Color,
    pub border_subtle: Color,
    pub border_strong: Color,
    pub border_focus: Color,
    pub transparent: Color,
    pub accent: Color,
    pub selection_bg: Color,

    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_tertiary: Color,
    pub text_muted: Color,
    pub text_dim: Color,
    pub text_ghost: Color,
    pub text_faint: Color,
    pub text_invisible: Color,

    pub status_green_dim: Color,
    pub status_dim: Color,

    pub agent_running: Color,
    pub agent_needs_input: Color,

    pub error_text: Color,
    pub error_bg: Color,
    pub error_border: Color,


    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
    pub diff_add_text: Color,
    pub diff_del_text: Color,
    pub diff_hunk_header: Color,

    pub scrollbar_thumb: Color,
}

// ── Bundled theme catalog ──────────────────────────────────────
//
// Themes are compiled into the binary. Adding one means adding a JSON
// in assets/themes/ and an entry to `THEME_SOURCES`.

/// A user-selectable entry in the Appearance picker.
#[derive(Clone, Copy, Debug)]
pub struct ThemeEntry {
    /// ID persisted to the DB (`settings.theme`).
    pub id: &'static str,
    /// Label shown in the picker.
    pub label: &'static str,
}

const THEME_SOURCES: &[(&str, &str)] = &[
    ("superhq-dark",  include_str!("../../assets/themes/superhq-dark.json")),
    ("superhq-light", include_str!("../../assets/themes/superhq-light.json")),
    ("washi",         include_str!("../../assets/themes/washi.json")),
    ("sumi",          include_str!("../../assets/themes/sumi.json")),
];

pub const THEMES: &[ThemeEntry] = &[
    ThemeEntry { id: "superhq-light", label: "Light" },
    ThemeEntry { id: "superhq-dark",  label: "Dark"  },
    ThemeEntry { id: "washi",         label: "Washi" },
    ThemeEntry { id: "sumi",          label: "Sumi"  },
];

/// Reserved ID for "follow the system light/dark setting".
pub const AUTO_THEME: &str = "auto";

fn parse_theme(id: &str) -> Option<ThemeColors> {
    let src = THEME_SOURCES.iter().find(|(k, _)| *k == id)?.1;
    match serde_json::from_str(src) {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("Failed to parse theme {id}: {e}");
            None
        }
    }
}

static THEME: RwLock<Option<Arc<ThemeColors>>> = RwLock::new(None);
static THEME_GEN: AtomicU64 = AtomicU64::new(1);

/// A monotonic counter that increments every time `load_theme` succeeds.
/// UI widgets that cache derived state (like the terminal's color palette)
/// can compare this against their last-seen value to detect theme changes.
pub fn theme_generation() -> u64 {
    THEME_GEN.load(Ordering::Relaxed)
}

/// Whether the active theme's base background is closer to black than white.
/// Derived from luminance so new themes work without any flag bookkeeping.
pub fn is_dark() -> bool {
    let bg = current().bg_base.0;
    let luma = 0.299 * bg.r + 0.587 * bg.g + 0.114 * bg.b;
    luma < 0.5
}

fn current() -> Arc<ThemeColors> {
    if let Some(t) = THEME.read().unwrap().clone() {
        return t;
    }
    let theme = Arc::new(
        parse_theme("superhq-dark").expect("default theme superhq-dark failed to parse"),
    );
    *THEME.write().unwrap() = Some(theme.clone());
    theme
}

/// Load a theme by id. Caller is responsible for triggering a redraw
/// (e.g. `cx.refresh_windows()`) once the swap is complete.
///
/// Returns `false` if the id is unknown.
pub fn load_theme(id: &str) -> bool {
    let Some(colors) = parse_theme(id) else { return false };
    *THEME.write().unwrap() = Some(Arc::new(colors));
    THEME_GEN.fetch_add(1, Ordering::Relaxed);
    true
}

/// Resolve a *saved* theme id — which may be "auto" — to a concrete theme id
/// using the given system appearance.
pub fn resolve_theme_id(saved: &str, appearance: WindowAppearance) -> &str {
    if saved == AUTO_THEME {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => "superhq-dark",
            WindowAppearance::Light | WindowAppearance::VibrantLight => "superhq-light",
        }
    } else if THEME_SOURCES.iter().any(|(k, _)| *k == saved) {
        saved
    } else {
        "superhq-dark"
    }
}

/// Look up a theme entry by its id.
pub fn theme_entry(id: &str) -> Option<&'static ThemeEntry> {
    THEMES.iter().find(|t| t.id == id)
}

/// A minimal palette for rendering theme preview cards. Cheap to copy.
#[derive(Clone, Copy)]
pub struct PreviewPalette {
    pub bg: Rgba,
    pub surface: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
}

/// Build a preview palette for a concrete theme id (not "auto").
pub fn preview_palette(id: &str) -> Option<PreviewPalette> {
    let t = parse_theme(id)?;
    Some(PreviewPalette {
        bg: t.bg_base.0,
        surface: t.bg_elevated.0,
        border: t.border.0,
        text: t.text_primary.0,
        muted: t.text_muted.0,
        accent: t.accent.0,
    })
}

// ── Public accessors (same API as before) ──────────────────────

pub fn bg_base() -> Rgba { current().bg_base.0 }
pub fn bg_terminal() -> Rgba { current().bg_terminal.0 }
pub fn terminal_foreground() -> Rgba { current().terminal_foreground.0 }
pub fn terminal_cursor() -> Rgba { current().terminal_cursor.0 }
pub fn bg_surface() -> Rgba { current().bg_surface.0 }
pub fn bg_elevated() -> Rgba { current().bg_elevated.0 }
pub fn bg_hover() -> Rgba { current().bg_hover.0 }
pub fn bg_active() -> Rgba { current().bg_active.0 }
pub fn bg_selected() -> Rgba { current().bg_selected.0 }
pub fn bg_input() -> Rgba { current().bg_input.0 }

pub fn border() -> Rgba { current().border.0 }
pub fn border_subtle() -> Rgba { current().border_subtle.0 }
pub fn border_strong() -> Rgba { current().border_strong.0 }
pub fn border_focus() -> Rgba { current().border_focus.0 }
pub fn transparent() -> Rgba { current().transparent.0 }
pub fn accent() -> Rgba { current().accent.0 }
pub fn selection_bg() -> Rgba { current().selection_bg.0 }

pub fn text_primary() -> Rgba { current().text_primary.0 }
pub fn text_secondary() -> Rgba { current().text_secondary.0 }
pub fn text_tertiary() -> Rgba { current().text_tertiary.0 }
pub fn text_muted() -> Rgba { current().text_muted.0 }
pub fn text_dim() -> Rgba { current().text_dim.0 }
pub fn text_ghost() -> Rgba { current().text_ghost.0 }
pub fn text_faint() -> Rgba { current().text_faint.0 }
pub fn text_invisible() -> Rgba { current().text_invisible.0 }

pub fn status_green_dim() -> Rgba { current().status_green_dim.0 }
pub fn status_dim() -> Rgba { current().status_dim.0 }

pub fn agent_running() -> Rgba { current().agent_running.0 }
pub fn agent_needs_input() -> Rgba { current().agent_needs_input.0 }

pub fn error_text() -> Rgba { current().error_text.0 }
pub fn error_bg() -> Rgba { current().error_bg.0 }
pub fn error_border() -> Rgba { current().error_border.0 }


pub fn diff_add_bg() -> Rgba { current().diff_add_bg.0 }
pub fn diff_del_bg() -> Rgba { current().diff_del_bg.0 }
pub fn diff_add_text() -> Rgba { current().diff_add_text.0 }
pub fn diff_del_text() -> Rgba { current().diff_del_text.0 }
pub fn diff_hunk_header() -> Rgba { current().diff_hunk_header.0 }

pub fn scrollbar_thumb() -> Rgba { current().scrollbar_thumb.0 }

// ── Shared styles ──────────────────────────────────────────────

pub fn popover() -> Div {
    div()
        .bg(bg_surface())
        .border_1()
        .border_color(border())
        .rounded(px(8.0))
        .shadow_lg()
        .py_1()
        .px_1()
        .flex()
        .flex_col()
}

pub fn menu_item() -> Div {
    div()
        .px_2p5()
        .py(px(5.0))
        .rounded(px(4.0))
        .text_xs()
        .cursor_pointer()
        .text_color(text_secondary())
        .flex()
        .items_center()
        .gap(px(6.0))
}

pub fn button(label: &str) -> Div {
    div()
        .px_3()
        .py(px(5.0))
        .rounded(px(6.0))
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .text_color(text_dim())
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(label.to_string())
}

pub fn button_primary(label: &str) -> Div {
    div()
        .px_3()
        .py(px(5.0))
        .rounded(px(6.0))
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .bg(bg_selected())
        .text_color(text_secondary())
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(label.to_string())
}

pub fn button_danger(label: &str) -> Div {
    div()
        .px_3()
        .py(px(5.0))
        .rounded(px(6.0))
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .text_color(error_text())
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(label.to_string())
}

pub fn menu_separator() -> Div {
    div()
        .mx_2()
        .my_1()
        .h(px(1.0))
        .bg(border())
}

// ── Utilities ──────────────────────────────────────────────────

pub fn parse_hex_color(hex: &str) -> Option<Rgba> {
    parse_hex_to_rgba(hex)
}

/// Returns (r, g, b) as u8 values for a theme color. Used by terminal config.
pub fn rgb_bytes(color: Rgba) -> (u8, u8, u8) {
    (
        (color.r * 255.0) as u8,
        (color.g * 255.0) as u8,
        (color.b * 255.0) as u8,
    )
}
