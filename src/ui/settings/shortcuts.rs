use gpui::*;
use super::SettingsPanel;
use crate::ui::theme as t;

const SHORTCUTS: &[(&str, &str)] = &[
    // Workspaces
    ("New workspace", "\u{2318}N"),
    ("Switch workspace 1\u{2013}9", "\u{2318}1 \u{2013} \u{2318}9"),
    // Tabs
    ("New agent tab", "\u{2318}T"),
    ("Close tab", "\u{2318}W"),
    ("Next tab", "\u{2318}\u{21E7}]"),
    ("Previous tab", "\u{2318}\u{21E7}["),
    ("Switch tab 1\u{2013}9", "\u{2303}1 \u{2013} \u{2303}9"),
    // App
    ("Settings", "\u{2318},"),
];

impl SettingsPanel {
    pub(super) fn render_shortcuts_tab() -> impl IntoElement {
        let mut rows = div().flex().flex_col().gap(px(2.0));

        for (label, shortcut) in SHORTCUTS {
            rows = rows.child(
                div()
                    .px_2()
                    .py_1p5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .rounded(px(4.0))
                    .hover(|s| s.bg(t::bg_hover()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(t::text_secondary())
                            .child(label.to_string()),
                    )
                    .child(
                        div()
                            .px_2()
                            .py(px(2.0))
                            .rounded(px(4.0))
                            .bg(t::bg_elevated())
                            .text_xs()
                            .text_color(t::text_muted())
                            .child(shortcut.to_string()),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t::text_muted())
                    .child("Keyboard Shortcuts"),
            )
            .child(rows)
    }
}
