use gpui::*;
use gpui::prelude::FluentBuilder as _;
use gpui_component::input::{Input, InputState};
use gpui_component::Sizable as _;

use crate::db::{Database, ExposeHostPort, PortMapping};
use crate::ui::theme as t;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Forward,
    ExposeHost,
}

enum View {
    List,
    AddForward,
    AddExposeHost,
}

pub struct PortsDialog {
    db: Arc<Database>,
    workspace_id: i64,
    tab: Tab,
    view: View,
    input_a: Entity<InputState>,
    input_b: Entity<InputState>,
    on_dismiss: Box<dyn Fn(&mut Window, &mut App) + 'static>,
    focus_handle: FocusHandle,
}

impl PortsDialog {
    pub fn new(
        db: Arc<Database>,
        workspace_id: i64,
        on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input_a = cx.new(|cx| InputState::new(window, cx));
        let input_b = cx.new(|cx| InputState::new(window, cx));
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        Self {
            db,
            workspace_id,
            tab: Tab::Forward,
            view: View::List,
            input_a,
            input_b,
            on_dismiss: Box::new(on_dismiss),
            focus_handle,
        }
    }

    fn switch_tab(&mut self, tab: Tab, _window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        self.view = View::List;
        cx.notify();
    }

    fn show_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tab {
            Tab::Forward => {
                self.view = View::AddForward;
                self.input_a.update(cx, |s, cx| {
                    s.set_placeholder("e.g. 3000", window, cx);
                    s.set_value("", window, cx);
                    s.focus(window, cx);
                });
                self.input_b.update(cx, |s, cx| {
                    s.set_placeholder("Defaults to same port", window, cx);
                    s.set_value("", window, cx);
                });
            }
            Tab::ExposeHost => {
                self.view = View::AddExposeHost;
                self.input_a.update(cx, |s, cx| {
                    s.set_placeholder("e.g. 5432", window, cx);
                    s.set_value("", window, cx);
                    s.focus(window, cx);
                });
                self.input_b.update(cx, |s, cx| {
                    s.set_placeholder("Defaults to same port", window, cx);
                    s.set_value("", window, cx);
                });
            }
        }
        cx.notify();
    }

    fn cancel_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view = View::List;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn submit_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let a = self.input_a.read(cx).value().to_string();
        let b = self.input_b.read(cx).value().to_string();
        let guest_port: u16 = match a.trim().parse() {
            Ok(p) if p > 0 => p,
            _ => return,
        };
        let host_port: u16 = if b.trim().is_empty() {
            guest_port
        } else {
            match b.trim().parse() {
                Ok(p) if p > 0 => p,
                _ => return,
            }
        };
        if let Err(e) = self.db.add_port_mapping(self.workspace_id, &PortMapping { guest_port, host_port }) {
            eprintln!("[ports] failed to add: {e}");
            return;
        }
        self.view = View::List;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn submit_expose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let a = self.input_a.read(cx).value().to_string();
        let b = self.input_b.read(cx).value().to_string();
        let host_port: u16 = match a.trim().parse() {
            Ok(p) if p > 0 => p,
            _ => return,
        };
        let guest_port: u16 = if b.trim().is_empty() {
            host_port
        } else {
            match b.trim().parse() {
                Ok(p) if p > 0 => p,
                _ => return,
            }
        };
        if let Err(e) = self.db.add_expose_host_port(self.workspace_id, &ExposeHostPort { host_port, guest_port }) {
            eprintln!("[ports] failed to add: {e}");
            return;
        }
        self.view = View::List;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn dismiss(&self, window: &mut Window, cx: &mut App) {
        (self.on_dismiss)(window, cx);
    }

    // ── Tab bar ──────────────────────────────────────────────────

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fwd_count = self.db.get_port_mappings(self.workspace_id).map(|m| m.len()).unwrap_or(0);
        let eh_count = self.db.get_expose_host_ports(self.workspace_id).map(|m| m.len()).unwrap_or(0);

        let tab_btn = |id: &str, label: &str, count: usize, tab: Tab, cx: &mut Context<Self>| {
            let active = self.tab == tab;
            div()
                .id(SharedString::from(id.to_string()))
                .px_2()
                .py_1p5()
                .cursor_pointer()
                .text_xs()
                .text_color(if active { t::text_secondary() } else { t::text_ghost() })
                .when(active, |el| {
                    el.border_b_2().border_color(t::text_secondary())
                })
                .hover(|s| s.text_color(t::text_muted()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.switch_tab(tab, window, cx);
                }))
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(label.to_string())
                .when(count > 0, |el| {
                    el.child(
                        div()
                            .px_1()
                            .rounded(px(4.0))
                            .bg(t::bg_hover())
                            .text_color(t::text_dim())
                            .child(format!("{}", count)),
                    )
                })
        };

        div()
            .px_2()
            .flex()
            .gap(px(2.0))
            .border_b_1()
            .border_color(t::border_subtle())
            .child(tab_btn("tab-fwd", "Forward", fwd_count, Tab::Forward, cx))
            .child(tab_btn("tab-expose", "Expose Host", eh_count, Tab::ExposeHost, cx))
    }

    // ── List views ───────────────────────────────────────────────

    fn render_forward_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let mappings = self.db.get_port_mappings(self.workspace_id).unwrap_or_default();
        let has = !mappings.is_empty();

        let mut rows = div().flex().flex_col();
        for pm in &mappings {
            let gp = pm.guest_port;
            rows = rows.child(
                self.row_chrome(format!("fwd-{gp}"))
                    .child(
                        div().flex_grow().text_xs().text_color(t::text_secondary())
                            .child(format!(":{} \u{2192} localhost:{}", pm.guest_port, pm.host_port)),
                    )
                    .child(
                        self.remove_btn(format!("rm-fwd-{gp}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let _ = this.db.remove_port_mapping(this.workspace_id, gp);
                                cx.notify();
                            })),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .when(has, |el| el.child(rows))
            .when(!has, |el| el.child(self.empty_state("No forwarded ports")))
            .child(self.add_footer("Forward Port", cx))
            .into_any_element()
    }

    fn render_expose_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let mappings = self.db.get_expose_host_ports(self.workspace_id).unwrap_or_default();
        let has = !mappings.is_empty();

        let mut rows = div().flex().flex_col();
        for m in &mappings {
            let hp = m.host_port;
            rows = rows.child(
                self.row_chrome(format!("eh-{hp}"))
                    .child(
                        div().flex_grow().text_xs().text_color(t::text_secondary())
                            .child(format!("localhost:{} \u{2192} sandbox:{}", m.host_port, m.guest_port)),
                    )
                    .child(
                        self.remove_btn(format!("rm-eh-{hp}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let _ = this.db.remove_expose_host_port(this.workspace_id, hp);
                                cx.notify();
                            })),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .when(has, |el| el.child(rows))
            .when(!has, |el| el.child(self.empty_state("No host ports exposed")))
            .child(self.add_footer("Expose Port", cx))
            .into_any_element()
    }

    // ── Add forms ────────────────────────────────────────────────

    fn render_add_forward(&self, cx: &mut Context<Self>) -> AnyElement {
        self.render_add_form(
            "Forward Port",
            "Port",
            "Forward to",
            "Leave empty to use the same port on host",
            View::AddForward,
            cx,
        )
    }

    fn render_add_expose(&self, cx: &mut Context<Self>) -> AnyElement {
        self.render_add_form(
            "Expose Host Port",
            "Host Port",
            "Sandbox Port",
            "Accessible at host.shuru.internal:<port> inside the sandbox",
            View::AddExposeHost,
            cx,
        )
    }

    // ── Shared UI helpers ────────────────────────────────────────

    fn row_chrome(&self, id: String) -> Stateful<Div> {
        div()
            .id(SharedString::from(id))
            .px_4()
            .py_2()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(t::border_subtle())
    }

    fn remove_btn(&self, id: String) -> Stateful<Div> {
        div()
            .id(SharedString::from(id))
            .px_1p5()
            .py(px(1.0))
            .rounded(px(3.0))
            .cursor_pointer()
            .text_xs()
            .text_color(t::text_ghost())
            .hover(|s| s.text_color(t::text_muted()).bg(t::bg_hover()))
            .child("Remove")
    }

    fn empty_state(&self, msg: &str) -> impl IntoElement {
        div()
            .px_4()
            .py_6()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_xs()
                    .text_color(t::text_faint())
                    .child(msg.to_string()),
            )
    }

    fn add_footer(&self, label: &str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(t::border_subtle())
            .flex()
            .justify_end()
            .child(
                div()
                    .id("port-add-btn")
                    .px_3()
                    .py_1()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_xs()
                    .bg(t::bg_selected())
                    .text_color(t::text_secondary())
                    .hover(|s| s.bg(t::bg_hover()).text_color(t::text_primary()))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_add(window, cx);
                    }))
                    .child(label.to_string()),
            )
    }

    fn render_add_form(
        &self,
        title: &str,
        label_a: &str,
        label_b: &str,
        hint: &str,
        view: View,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            // Header
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(t::border_subtle())
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t::text_secondary())
                            .child(title.to_string()),
                    ),
            )
            // Inputs
            .child(
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xs().text_color(t::text_dim()).child(label_a.to_string()))
                            .child(Input::new(&self.input_a).small()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xs().text_color(t::text_dim()).child(label_b.to_string()))
                            .child(Input::new(&self.input_b).small())
                            .child(div().text_xs().text_color(t::text_faint()).child(hint.to_string())),
                    ),
            )
            // Footer
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(t::border_subtle())
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .id("port-cancel-btn")
                            .px_3()
                            .py_1()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .text_xs()
                            .text_color(t::text_dim())
                            .hover(|s| s.bg(t::bg_hover()).text_color(t::text_tertiary()))
                            .on_click(cx.listener(|this, _, window, cx| this.cancel_add(window, cx)))
                            .child("Cancel"),
                    )
                    .child({
                        let is_forward = matches!(view, View::AddForward);
                        div()
                            .id("port-submit-btn")
                            .px_3()
                            .py_1()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .text_xs()
                            .bg(t::bg_selected())
                            .text_color(t::text_secondary())
                            .hover(|s| s.bg(t::bg_hover()).text_color(t::text_primary()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if is_forward {
                                    this.submit_forward(window, cx);
                                } else {
                                    this.submit_expose(window, cx);
                                }
                            }))
                            .child("Add")
                    }),
            )
            .into_any_element()
    }
}

impl Render for PortsDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.view {
            View::List => match self.tab {
                Tab::Forward => self.render_forward_list(cx),
                Tab::ExposeHost => self.render_expose_list(cx),
            },
            View::AddForward => self.render_add_forward(cx),
            View::AddExposeHost => self.render_add_expose(cx),
        };

        div()
            .id("ports-dialog-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000088))
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                this.dismiss(window, cx);
            }))
            .child(
                div()
                    .id("ports-dialog-card")
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            match this.view {
                                View::AddForward | View::AddExposeHost => this.cancel_add(window, cx),
                                View::List => this.dismiss(window, cx),
                            }
                        }
                    }))
                    .w(px(380.0))
                    .bg(t::bg_surface())
                    .border_1()
                    .border_color(t::border())
                    .rounded(px(10.0))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    // Tab bar (only in list view)
                    .when(matches!(self.view, View::List), |el| {
                        el.child(self.render_tabs(cx))
                    })
                    .child(body),
            )
    }
}
