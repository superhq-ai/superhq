use gpui::*;
use super::SettingsPanel;
use crate::ui::theme as t;

const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    ("Workspaces", &[
        ("New workspace", "\u{2318}N"),
        ("Switch workspace 1\u{2013}9", "\u{2318}1\u{2013}9"),
        ("Next workspace", "\u{2303}\u{2318}]"),
        ("Previous workspace", "\u{2303}\u{2318}["),
    ]),
    ("Tabs", &[
        ("New agent tab", "\u{2318}T"),
        ("Close tab", "\u{2318}W"),
        ("Switch tab 1\u{2013}9", "\u{2303}1\u{2013}9"),
        ("Next tab", "\u{2318}\u{21E7}]"),
        ("Previous tab", "\u{2318}\u{21E7}["),
    ]),
    ("App", &[
        ("Settings", "\u{2318},"),
        ("Toggle review panel", "\u{2318}B"),
        ("Toggle sidebar", "\u{2318}\u{21E7}B"),
        ("Ports", "\u{2318}\u{21E7}P"),
    ]),
];

impl SettingsPanel {
    pub(super) fn render_shortcuts_tab() -> impl IntoElement {
        let mut container = div().flex().flex_col().gap_3();

        for (section_title, shortcuts) in SHORTCUTS {
            let mut section = div().flex().flex_col().gap(px(2.0));
            section = section.child(
                div()
                    .text_xs()
                    .text_color(t::text_ghost())
                    .font_weight(FontWeight::MEDIUM)
                    .px_2()
                    .pb_1()
                    .child(section_title.to_string()),
            );
            for (label, shortcut) in *shortcuts {
                section = section.child(
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
            container = container.child(section);
        }

        container
    }
}
