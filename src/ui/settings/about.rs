use gpui::*;
use super::SettingsPanel;
use super::card::*;
use crate::ui::theme as t;

impl SettingsPanel {
    pub(super) fn render_about_tab() -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .child(section_header("About"))
            .child(settings_card(vec![
                settings_row(
                    "SuperHQ",
                    "Sandboxed AI agent orchestration",
                    div()
                        .text_xs()
                        .text_color(t::text_muted())
                        .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
                )
                .into_any_element(),
                settings_row(
                    "License",
                    "AGPL-3.0-only",
                    div()
                        .text_xs()
                        .text_color(t::text_muted())
                        .child("Open source"),
                )
                .into_any_element(),
                settings_row(
                    "Website",
                    "Learn more about SuperHQ",
                    div()
                        .id("about-website-link")
                        .px_2()
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_xs()
                        .text_color(t::text_secondary())
                        .hover(|s| s.bg(t::bg_hover()))
                        .on_click(|_, _, _| {
                            let _ = open::that("https://superhq.ai");
                        })
                        .child("superhq.ai"),
                )
                .into_any_element(),
            ]))
    }
}
