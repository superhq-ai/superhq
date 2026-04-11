use gpui::{div, px, rgb, rgba, Div, ParentElement as _, Rgba, Styled as _};

// ── Surface / background ─────────────────────────────────────────
pub fn bg_base() -> Rgba { rgb(0x111111) }
pub fn bg_surface() -> Rgba { rgb(0x1e1e1e) }
pub fn bg_elevated() -> Rgba { rgba(0xffffff05) }
pub fn bg_hover() -> Rgba { rgba(0xffffff08) }
pub fn bg_active() -> Rgba { rgba(0xffffff0c) }
pub fn bg_selected() -> Rgba { rgba(0xffffff0f) }
pub fn bg_input() -> Rgba { rgba(0xffffff06) }

// ── Borders ──────────────────────────────────────────────────────
pub fn border() -> Rgba { rgba(0xffffff0a) }
pub fn border_subtle() -> Rgba { rgba(0xffffff06) }
pub fn border_strong() -> Rgba { rgba(0xffffff18) }
pub fn border_focus() -> Rgba { rgba(0xffffff30) }
pub fn transparent() -> Rgba { rgba(0x00000000) }
pub fn accent() -> Rgba { rgba(0x60a5facc) }
pub fn selection_bg() -> Rgba { rgba(0x60a5fa30) }

// ── Text ─────────────────────────────────────────────────────────
pub fn text_primary() -> Rgba { rgba(0xffffffee) }
pub fn text_secondary() -> Rgba { rgba(0xffffffcc) }
pub fn text_tertiary() -> Rgba { rgba(0xffffffaa) }
pub fn text_muted() -> Rgba { rgba(0xffffff88) }
pub fn text_dim() -> Rgba { rgba(0xffffff55) }
pub fn text_ghost() -> Rgba { rgba(0xffffff44) }
pub fn text_faint() -> Rgba { rgba(0xffffff33) }
pub fn text_invisible() -> Rgba { rgba(0xffffff1a) }

// ── Status indicators ────────────────────────────────────────────
pub fn status_green_dim() -> Rgba { rgba(0x4ade80aa) }
pub fn status_dim() -> Rgba { rgba(0xffffff1a) }

// ── Agent status ────────────────────────────────────────────────
pub fn agent_running() -> Rgba { rgba(0x3B82F6FF) }
pub fn agent_needs_input() -> Rgba { rgba(0xF59E0BFF) }

// Error
pub fn error_text() -> Rgba { rgba(0xEF4444FF) }
pub fn error_bg() -> Rgba { rgba(0x1A0505FF) }
pub fn error_border() -> Rgba { rgba(0x3B1111FF) }

// ── Git file status ──────────────────────────────────────────────
pub fn status_modified() -> Rgba { rgba(0xe5c07bff) }
pub fn status_added() -> Rgba { rgba(0x98c379ff) }
pub fn status_deleted() -> Rgba { rgba(0xe06c75ff) }

// ── Diff viewer ─────────────────────────────────────────────────
pub fn diff_add_bg() -> Rgba { rgba(0x98c37930) }
pub fn diff_del_bg() -> Rgba { rgba(0xe06c7530) }
pub fn diff_add_text() -> Rgba { rgba(0x98c379cc) }
pub fn diff_del_text() -> Rgba { rgba(0xe06c75cc) }
pub fn diff_hunk_header() -> Rgba { rgba(0x61afef66) }

// ── Shared styles ───────────────────────────────────────────────

/// Standard popover/menu container style. Used by context menus, dropdowns,
/// select popups, workspace menus, and the new tab menu.
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

/// Standard menu item row. Callers add .id(), .on_click(), .children() etc.
/// Callers must add `.id()` then `.hover(|s| s.bg(t::bg_hover()))` after.
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

/// Standard button styling. Callers add `.id()`, `.on_click()`.
/// For Default variant. Use `button_primary()` for emphasis.
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

/// Primary button — emphasis styling with background.
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

/// Danger button — destructive action styling.
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

/// Standard menu separator line.
pub fn menu_separator() -> Div {
    div()
        .mx_2()
        .my_1()
        .h(px(1.0))
        .bg(border())
}

// ── Utilities ────────────────────────────────────────────────────
pub fn parse_hex_color(hex: &str) -> Option<Rgba> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    })
}
