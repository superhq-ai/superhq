use gpui::*;
use crate::ui::theme as t;

pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py(px(4.0))
            .rounded(px(4.0))
            .bg(t::bg_elevated())
            .border_1()
            .border_color(t::border())
            .shadow_sm()
            .text_xs()
            .text_color(t::text_secondary())
            .max_w(px(300.0))
            .child(self.text.clone())
    }
}
