use gpui::*;
use super::SettingsPanel;
use super::card::section_header;
use crate::ui::review::load_syntax_theme;
use crate::ui::theme::{self as t, PreviewPalette, THEMES, AUTO_THEME};

const CARD_W: f32 = 116.0;
const CARD_H: f32 = 72.0;

/// One Appearance card — a mini window preview + label underneath.
fn theme_card(
    id: &'static str,
    label: &'static str,
    selected: bool,
    preview: ThemePreview,
    on_click: impl Fn(&mut SettingsPanel, &ClickEvent, &mut Window, &mut Context<SettingsPanel>) + 'static,
    cx: &mut Context<SettingsPanel>,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("theme-card-{id}")))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.0))
        .cursor_pointer()
        .on_click(cx.listener(on_click))
        .child(
            div()
                .w(px(CARD_W))
                .h(px(CARD_H))
                .rounded(px(6.0))
                .border_2()
                .border_color(if selected { t::accent() } else { t::border_subtle() })
                .overflow_hidden()
                .child(preview.render()),
        )
        .child(
            div()
                .text_xs()
                .text_color(if selected {
                    t::text_secondary()
                } else {
                    t::text_dim()
                })
                .child(label),
        )
}

/// Visual content of a theme card preview. Either one palette
/// (for concrete themes) or two halves (for Auto).
enum ThemePreview {
    Solid(PreviewPalette),
    Split { light: PreviewPalette, dark: PreviewPalette },
}

impl ThemePreview {
    fn render(self) -> AnyElement {
        match self {
            ThemePreview::Solid(p) => render_preview(p).into_any_element(),
            ThemePreview::Split { light, dark } => {
                div()
                    .size_full()
                    .flex()
                    .flex_row()
                    .child(div().w(px(CARD_W / 2.0)).h_full().overflow_hidden().child(render_preview(light)))
                    .child(div().w(px(CARD_W / 2.0)).h_full().overflow_hidden().child(render_preview(dark)))
                    .into_any_element()
            }
        }
    }
}

/// Minimalist window preview inside a card:
/// title bar with three dots + a sidebar strip + content bars.
fn render_preview(p: PreviewPalette) -> impl IntoElement {
    div()
        .w(px(CARD_W))
        .h(px(CARD_H))
        .bg(p.bg)
        .flex()
        .flex_col()
        // Title bar
        .child(
            div()
                .h(px(12.0))
                .w_full()
                .bg(p.surface)
                .border_b_1()
                .border_color(p.border)
                .flex()
                .items_center()
                .px(px(5.0))
                .gap(px(3.0))
                .child(div().size(px(4.0)).rounded_full().bg(p.muted))
                .child(div().size(px(4.0)).rounded_full().bg(p.muted))
                .child(div().size(px(4.0)).rounded_full().bg(p.muted)),
        )
        // Body: sidebar + content
        .child(
            div()
                .flex()
                .flex_row()
                .flex_grow()
                // Sidebar
                .child(
                    div()
                        .w(px(24.0))
                        .h_full()
                        .bg(p.surface)
                        .border_r_1()
                        .border_color(p.border)
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .p(px(4.0))
                        .child(div().h(px(3.0)).w_full().rounded(px(1.0)).bg(p.accent))
                        .child(div().h(px(3.0)).w_full().rounded(px(1.0)).bg(p.muted))
                        .child(div().h(px(3.0)).w_full().rounded(px(1.0)).bg(p.muted)),
                )
                // Content
                .child(
                    div()
                        .flex_grow()
                        .h_full()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .p(px(5.0))
                        .child(div().h(px(4.0)).w(px(60.0)).rounded(px(1.0)).bg(p.text))
                        .child(div().h(px(3.0)).w(px(70.0)).rounded(px(1.0)).bg(p.muted))
                        .child(div().h(px(3.0)).w(px(50.0)).rounded(px(1.0)).bg(p.muted)),
                ),
        )
}

impl SettingsPanel {
    pub(super) fn render_appearance_tab(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.theme_id.clone();

        let dark_palette = t::preview_palette("superhq-dark")
            .expect("superhq-dark preview palette");
        let light_palette = t::preview_palette("superhq-light")
            .expect("superhq-light preview palette");

        let mut cards: Vec<AnyElement> = Vec::new();

        for entry in THEMES {
            let preview = t::preview_palette(entry.id)
                .map(ThemePreview::Solid)
                .unwrap_or(ThemePreview::Solid(dark_palette));
            let is_selected = selected == entry.id;
            let id = entry.id;
            cards.push(
                theme_card(
                    id,
                    entry.label,
                    is_selected,
                    preview,
                    move |this, _, _window, cx| this.apply_theme(id, cx),
                    cx,
                )
                .into_any_element(),
            );
        }

        // Auto card — split light/dark preview.
        let auto_selected = selected == AUTO_THEME;
        cards.push(
            theme_card(
                AUTO_THEME,
                "Auto",
                auto_selected,
                ThemePreview::Split {
                    light: light_palette,
                    dark: dark_palette,
                },
                move |this, _, _window, cx| this.apply_theme(AUTO_THEME, cx),
                cx,
            )
            .into_any_element(),
        );

        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .child(section_header("Appearance"))
            .child(
                div()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(t::border_subtle())
                    .bg(t::bg_elevated())
                    .p_4()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(12.0))
                    .children(cards),
            )
    }

    /// Persist the chosen theme id, swap the active theme, and redraw.
    pub(crate) fn apply_theme(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Err(e) = self.db.update_theme(id) {
            eprintln!("Failed to save theme: {e}");
        }
        self.theme_id = id.to_string();

        let resolved = t::resolve_theme_id(id, cx.window_appearance());
        t::load_theme(resolved);
        load_syntax_theme(resolved);
        cx.refresh_windows();
        cx.notify();
    }
}
