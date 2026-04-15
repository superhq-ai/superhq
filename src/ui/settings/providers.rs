use gpui::*;
use gpui::prelude::FluentBuilder as _;
use crate::ui::theme as t;
use crate::sandbox::secrets;
use super::{SettingsPanel, OAuthStatus, supports_oauth, provider_icon};
use super::card::*;

const PROVIDER_ICON_SIZE: f32 = 13.0;
const PROVIDER_ICON_GAP: f32 = 8.0;

impl SettingsPanel {
    pub(super) fn save_secret(&mut self, index: usize, cx: &mut Context<Self>) {
        let row = &self.secret_rows[index];
        let value = row.input.read(cx).value().to_string();
        if value.is_empty() {
            return;
        }
        let label = row.label.clone();
        let hosts = secrets::default_hosts(&row.env_var);
        if let Err(e) = self.db.save_secret(&row.env_var, &row.label, &value, &hosts) {
            eprintln!("Failed to save secret: {e}");
            return;
        }
        let row = &mut self.secret_rows[index];
        row.has_saved_value = true;
        row.auth_method = "api_key".into();
        if row.env_var == "OPENAI_API_KEY" {
            self.oauth_status = OAuthStatus::Idle;
        }
        self.toast.update(cx, |t, cx| t.show(format!("{label} key saved"), cx));
        cx.notify();
    }

    pub(super) fn remove_secret(&mut self, index: usize, _window: &mut Window, cx: &mut Context<Self>) {
        let row = &self.secret_rows[index];
        let env_var = row.env_var.clone();
        let label = row.label.clone();
        if let Err(e) = self.db.remove_secret(&env_var) {
            eprintln!("Failed to remove secret: {e}");
            return;
        }
        let row = &mut self.secret_rows[index];
        row.has_saved_value = false;
        row.auth_method = "api_key".into();
        row.input.update(cx, |state, cx| {
            state.set_masked(false);
            state.set_value("", cx);
        });
        if env_var == "OPENAI_API_KEY" {
            self.oauth_status = OAuthStatus::Idle;
        }
        self.toast.update(cx, |t, cx| t.show(format!("{label} key removed"), cx));
        cx.notify();
    }

    pub(super) fn start_oauth_login(&mut self, cx: &mut Context<Self>) {
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        self.oauth_status = OAuthStatus::InProgress;
        self.oauth_cancel = Some(cancel_tx);
        cx.notify();

        let db = self.db.clone();
        let this = cx.entity().downgrade();

        cx.spawn(async move |_, cx| {
            let result = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
                rt.block_on(crate::oauth::login(cancel_rx))
            })
            .join()
            .map_err(|_| anyhow::anyhow!("OAuth thread panicked"))
            .and_then(|r| r);

            cx.update(|cx| {
                this.update(cx, |panel, cx| {
                    match result {
                        Ok(tokens) => {
                            panel.oauth_cancel = None;
                            match crate::oauth::save_openai_oauth(&db, &tokens) {
                                Ok(info) => {
                                    panel.oauth_status = OAuthStatus::Connected {
                                        email: info
                                            .email
                                            .unwrap_or_else(|| "OpenAI account".into()),
                                        plan: info.plan.unwrap_or_default(),
                                    };
                                    if let Some(row) = panel
                                        .secret_rows
                                        .iter_mut()
                                        .find(|r| r.env_var == "OPENAI_API_KEY")
                                    {
                                        row.has_saved_value = true;
                                        row.auth_method = "oauth".into();
                                    }
                                }
                                Err(e) => {
                                    panel.oauth_status =
                                        OAuthStatus::Error(format!("Failed to save: {e}"));
                                }
                            }
                        }
                        Err(e) => {
                            if panel.oauth_cancel.take().is_some() {
                                panel.oauth_status = OAuthStatus::Error(format!("{e}"));
                            }
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn cancel_oauth_login(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.oauth_cancel.take() {
            let _ = cancel.send(());
        }
        self.oauth_status = OAuthStatus::Idle;
        cx.notify();
    }

    fn render_oauth_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut section = div().flex().flex_col().gap(px(6.0));

        section = section.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .py_1()
                .child(div().flex_grow().h(px(1.0)).bg(t::border_subtle()))
                .child(
                    div()
                        .text_xs()
                        .text_color(t::text_faint())
                        .child("or sign in with your ChatGPT account"),
                )
                .child(div().flex_grow().h(px(1.0)).bg(t::border_subtle())),
        );

        match &self.oauth_status {
            OAuthStatus::Idle => {
                section = section.child(
                    div()
                        .id("oauth-login")
                        .px_3()
                        .py(px(7.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .border_1()
                        .border_color(t::border_subtle())
                        .text_color(t::text_dim())
                        .hover(|s| {
                            s.bg(t::bg_hover())
                                .border_color(t::border_strong())
                                .text_color(t::text_secondary())
                        })
                        .text_center()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.start_oauth_login(cx);
                        }))
                        .child("Sign in with OpenAI"),
                );
            }
            OAuthStatus::InProgress => {
                section = section.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px_3()
                        .py(px(7.0))
                        .rounded(px(6.0))
                        .child(
                            div()
                                .text_xs()
                                .text_color(t::text_dim())
                                .child("Waiting for browser sign-in..."),
                        )
                        .child(
                                t::button("Cancel")
                                .id("cancel-oauth")
                                .hover(|s| s.bg(t::bg_hover()).text_color(t::text_tertiary()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_oauth_login(cx);
                                })),
                        ),
                );
            }
            OAuthStatus::Connected { email, plan } => {
                let status_text = if plan.is_empty() {
                    format!("Signed in as {email}")
                } else {
                    format!("Signed in as {email} ({plan})")
                };
                section = section.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px_3()
                        .py(px(7.0))
                        .rounded(px(6.0))
                        .bg(t::bg_selected())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .w(px(6.0))
                                        .h(px(6.0))
                                        .rounded_full()
                                        .bg(t::status_green_dim()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(t::text_secondary())
                                        .child(status_text),
                                ),
                        )
                        .child(
                            div()
                                .id("oauth-signout")
                                .px_2()
                                .py(px(3.0))
                                .rounded(px(4.0))
                                .cursor_pointer()
                                .text_xs()
                                .text_color(t::text_ghost())
                                .hover(|s| {
                                    s.bg(t::error_bg()).text_color(t::error_text())
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if let Some(i) = this
                                        .secret_rows
                                        .iter()
                                        .position(|r| r.env_var == "OPENAI_API_KEY")
                                    {
                                        this.remove_secret(i, window, cx);
                                    }
                                }))
                                .child("Sign out"),
                        ),
                );
            }
            OAuthStatus::Error(msg) => {
                let msg = msg.clone();
                section = section
                    .child(
                        div()
                            .px_3()
                            .py(px(5.0))
                            .rounded(px(6.0))
                            .bg(t::error_bg())
                            .text_xs()
                            .text_color(t::error_text())
                            .child(msg),
                    )
                    .child(
                        div()
                            .id("oauth-retry")
                            .px_3()
                            .py(px(7.0))
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .text_xs()
                            .border_1()
                            .border_color(t::border_subtle())
                            .text_color(t::text_dim())
                            .hover(|s| {
                                s.bg(t::bg_hover())
                                    .border_color(t::border_strong())
                                    .text_color(t::text_secondary())
                            })
                            .text_center()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_oauth_login(cx);
                            }))
                            .child("Try again"),
                    );
            }
        }

        section
    }

    pub(super) fn render_secrets_tab(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let row_count = self.secret_rows.len();
        let mut rows: Vec<AnyElement> = Vec::new();

        for i in 0..row_count {
            let row = &self.secret_rows[i];
            let has_saved = row.has_saved_value;
            let is_oauth = row.auth_method == "oauth";
            let has_oauth_option = supports_oauth(&row.env_var);

            let status = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(
                            div()
                                .w(px(5.0))
                                .h(px(5.0))
                                .rounded_full()
                                .bg(if has_saved {
                                    t::status_green_dim()
                                } else {
                                    t::status_dim()
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(if has_saved {
                                    t::status_green_dim()
                                } else {
                                    t::text_faint()
                                })
                                .child(if has_saved {
                                    if is_oauth { "OAuth" } else { "Configured" }
                                } else {
                                    "Not set"
                                }),
                        ),
                )
                .when(!is_oauth && has_saved, |el: Div| {
                    el.child(
                            t::button_danger("Remove")
                            .id(SharedString::from(format!("remove-secret-{i}")))
                            .hover(|s: StyleRefinement| s.bg(t::error_bg()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.remove_secret(i, window, cx);
                            })),
                    )
                });

            let icon_path = provider_icon(&row.env_var);

            let mut row_el = div()
                .px_4()
                .py_3()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(PROVIDER_ICON_GAP))
                                        .when_some(icon_path, |el, path| {
                                            el.child(
                                                svg()
                                                    .path(SharedString::from(path))
                                                    .size(px(PROVIDER_ICON_SIZE))
                                                    .text_color(t::text_secondary()),
                                            )
                                        })
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(t::text_secondary())
                                                .child(row.label.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(t::text_ghost())
                                        .when(icon_path.is_some(), |el| {
                                            el.pl(px(PROVIDER_ICON_SIZE + PROVIDER_ICON_GAP))
                                        })
                                        .child(format!("{} \u{00b7} {}", row.env_var, row.description)),
                                ),
                        )
                        .child(status),
                );

            if !is_oauth && !has_saved {
                row_el = row_el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_grow()
                                .bg(t::bg_input())
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(t::bg_input())
                                .hover(|s| s.border_color(t::border_subtle()))
                                .child(row.input.clone()),
                        )
                        .child(
                            t::button_primary("Save")
                                .id(SharedString::from(format!("save-secret-{i}")))
                                .hover(|s| s.bg(t::bg_hover()).text_color(t::text_primary()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.save_secret(i, cx);
                                })),
                        ),
                );
            }

            if has_oauth_option {
                row_el = row_el.child(self.render_oauth_section(cx));
            }

            rows.push(row_el.into_any_element());
        }

        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .child(section_header("Providers"))
            .when(rows.len() > 0, |el: Div| {
                el.child(settings_card(rows))
            })
    }
}
