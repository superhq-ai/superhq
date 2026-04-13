use crate::ui::theme as t;
use gpui::*;
use std::time::Duration;

/// Lightweight centered toast — small pill at top-center, auto-dismisses.
pub struct Toast {
    message: SharedString,
    visible: bool,
    _dismiss_task: Option<Task<()>>,
}

impl Toast {
    pub fn new() -> Self {
        Self {
            message: "".into(),
            visible: false,
            _dismiss_task: None,
        }
    }

    /// Show a toast message that auto-dismisses after 2 seconds.
    /// Cancels any pending dismiss from a previous call.
    pub fn show(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.message = message.into();
        self.visible = true;
        // Drop previous timer by replacing it
        self._dismiss_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(2))
                .await;
            let _ = cx.update(|cx| {
                let _ = this.update(cx, |toast, cx| {
                    toast.visible = false;
                    cx.notify();
                });
            });
        }));
        cx.notify();
    }
}

impl Render for Toast {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div();
        }

        div()
            .absolute()
            .top(px(44.0)) // below 36px titlebar + 8px padding
            .w_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .px_4()
                    .py(px(7.0))
                    .rounded(px(8.0))
                    .bg(t::bg_elevated())
                    .border_1()
                    .border_color(t::border())
                    .shadow_sm()
                    .text_xs()
                    .text_color(t::text_secondary())
                    .child(self.message.clone()),
            )
    }
}
