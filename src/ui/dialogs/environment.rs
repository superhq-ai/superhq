use gpui::*;
use gpui::prelude::FluentBuilder as _;
use gpui_component::scroll::ScrollableElement as _;

use crate::db::{Database, WorkspaceEnvRule, WorkspaceSecretsConfig};
use crate::sandbox::dotenv;
use crate::sandbox::secrets;
use crate::ui::components::text_input::TextInputEvent;
use crate::ui::components::TextInput;
use crate::ui::theme as t;
use std::path::Path;
use std::sync::Arc;

struct EnvRow {
    env_var: String,
    proxied: bool,
    hosts_input: Entity<TextInput>,
}

pub struct EnvironmentDialog {
    db: Arc<Database>,
    workspace_id: i64,
    env_rows: Vec<EnvRow>,
    search_input: Entity<TextInput>,
    search_query: String,
    expanded_env: Option<String>,
    on_dismiss: Box<dyn Fn(&mut Window, &mut App) + 'static>,
    focus_handle: FocusHandle,
}

impl EnvironmentDialog {
    pub fn new(
        db: Arc<Database>,
        workspace_id: i64,
        on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let env_rows = Self::load_env_rows(&db, workspace_id, cx);
        let search_input = cx.new(|cx| {
            let mut input = TextInput::new(cx);
            input.set_placeholder("Search environment variables");
            input
        });
        cx.subscribe(&search_input, |this: &mut Self, _, event: &TextInputEvent, cx| {
            if let TextInputEvent::Changed(value) = event {
                this.search_query = value.to_string();
                cx.notify();
            }
        })
        .detach();

        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);

        let expanded_env = env_rows.first().map(|row| row.env_var.clone());

        Self {
            db,
            workspace_id,
            env_rows,
            search_input,
            search_query: String::new(),
            expanded_env,
            on_dismiss: Box::new(on_dismiss),
            focus_handle,
        }
    }

    fn load_env_rows(
        db: &Arc<Database>,
        workspace_id: i64,
        cx: &mut Context<Self>,
    ) -> Vec<EnvRow> {
        let mount_path = db.get_workspace_mount_path(workspace_id).ok().flatten();
        let saved_config = db
            .get_workspace_secrets_config(workspace_id)
            .unwrap_or_default();
        let mut env_vars: Vec<String> = mount_path
            .as_deref()
            .map(Path::new)
            .map(dotenv::parse_env)
            .unwrap_or_default()
            .into_keys()
            .collect();
        env_vars.sort();

        env_vars
            .into_iter()
            .map(|env_var| {
                let rule = saved_config.rule_for(&env_var);
                let proxied = rule.map(|r| r.proxied).unwrap_or(true);
                let hosts = rule
                    .map(|r| r.hosts.clone())
                    .filter(|h| !h.is_empty())
                    .unwrap_or_else(|| {
                        let hosts = secrets::default_hosts(&env_var);
                        if hosts.is_empty() {
                            vec!["*".into()]
                        } else {
                            hosts
                        }
                    });
                let hosts_value = hosts.join(", ");
                let hosts_input = cx.new(|cx| {
                    let mut input = TextInput::new(cx);
                    input.set_placeholder("e.g. api.openai.com, chatgpt.com");
                    input.set_value(hosts_value, cx);
                    input
                });
                EnvRow {
                    env_var,
                    proxied,
                    hosts_input,
                }
            })
            .collect()
    }

    fn parse_hosts(raw: &str) -> Vec<String> {
        raw.split(|c: char| c == ',' || c.is_whitespace())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    fn host_count(row: &EnvRow, cx: &App) -> usize {
        Self::parse_hosts(row.hosts_input.read(cx).value()).len()
    }

    fn filtered_indices(&self) -> Vec<usize> {
        let query = self.search_query.trim().to_ascii_lowercase();
        self.env_rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                query.is_empty() || row.env_var.to_ascii_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn toggle_row_expanded(&mut self, env_var: &str, cx: &mut Context<Self>) {
        if self.expanded_env.as_deref() == Some(env_var) {
            self.expanded_env = None;
        } else {
            self.expanded_env = Some(env_var.to_string());
        }
        cx.notify();
    }

    fn toggle_env_proxy(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.env_rows.get_mut(index) {
            row.proxied = !row.proxied;
            cx.notify();
        }
    }

    fn save_environment(&mut self, cx: &mut Context<Self>) {
        let env_rules: Vec<WorkspaceEnvRule> = self
            .env_rows
            .iter()
            .map(|row| {
                let mut hosts = Self::parse_hosts(row.hosts_input.read(cx).value());
                if row.proxied && hosts.is_empty() {
                    hosts = {
                        let defaults = secrets::default_hosts(&row.env_var);
                        if defaults.is_empty() {
                            vec!["*".into()]
                        } else {
                            defaults
                        }
                    };
                    row.hosts_input.update(cx, |input, cx| {
                        input.set_value(hosts.join(", "), cx);
                    });
                }

                WorkspaceEnvRule {
                    env_var: row.env_var.clone(),
                    proxied: row.proxied,
                    hosts,
                }
            })
            .collect();

        if let Err(e) = self.db.set_workspace_secrets_config(
            self.workspace_id,
            &WorkspaceSecretsConfig { env_rules },
        ) {
            eprintln!("[environment] failed to save config: {e}");
            return;
        }

        cx.notify();
    }

    fn dismiss(&self, window: &mut Window, cx: &mut App) {
        (self.on_dismiss)(window, cx);
    }

    fn mode_button(&self, label: &'static str, active: bool) -> Div {
        if active {
            t::button_primary(label)
        } else {
            t::button(label)
        }
    }

    fn empty_state(&self, msg: &str) -> impl IntoElement {
        div()
            .px_4()
            .py_6()
            .flex()
            .items_center()
            .justify_center()
            .child(div().text_xs().text_color(t::text_faint()).child(msg.to_string()))
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(t::border_subtle())
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(t::text_faint())
                    .child("Saving updates the workspace config. Existing sessions pick it up on next boot."),
            )
            .child(
                t::button_primary("Save Environment")
                    .id("env-save-btn")
                    .hover(|s| s.bg(t::bg_hover()).text_color(t::text_primary()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save_environment(cx);
                    })),
            )
    }

    fn render_row(
        &self,
        index: usize,
        row: &EnvRow,
        is_first: bool,
        is_last: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_expanded = self.expanded_env.as_deref() == Some(row.env_var.as_str());
        let proxied = row.proxied;
        let env_var = row.env_var.clone();
        let host_count = Self::host_count(row, cx);
        let summary = if proxied {
            if host_count == 0 {
                "Proxy".to_string()
            } else if host_count == 1 {
                "Proxy · 1 host".to_string()
            } else {
                format!("Proxy · {} hosts", host_count)
            }
        } else {
            "Direct env".to_string()
        };

        div()
            .id(SharedString::from(format!("env-row-{env_var}")))
            .bg(t::bg_surface())
            .overflow_hidden()
            .when(is_first, |el| el.rounded_t(px(12.0)))
            .when(is_last && !is_expanded, |el| el.rounded_b(px(12.0)))
            .border_b_1()
            .border_color(if is_last { t::transparent() } else { t::border_subtle() })
            .child(
                div()
                    .id(SharedString::from(format!("env-row-header-{env_var}")))
                    .px_4()
                    .py_2p5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .cursor_pointer()
                    .hover(|s| s.bg(t::bg_hover()))
                    .when(is_first, |el| el.rounded_t(px(12.0)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_row_expanded(&env_var, cx);
                    }))
                    .child(
                        div()
                            .flex_grow()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t::text_secondary())
                                    .child(row.env_var.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(t::text_dim())
                                    .child(summary),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                svg()
                                    .path(SharedString::from(if is_expanded {
                                        "icons/files/chevron-down.svg"
                                    } else {
                                        "icons/files/chevron-right.svg"
                                    }))
                                    .size(px(12.0))
                                    .text_color(t::text_ghost()),
                            ),
                    ),
            )
            .when(is_expanded, |el| {
                el.child(
                    div()
                        .px_4()
                        .py_3()
                        .border_t_1()
                        .border_color(t::border_subtle())
                        .when(is_last, |el| el.rounded_b(px(12.0)))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(t::text_dim())
                                        .child("Mode"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            self.mode_button("Proxy", proxied)
                                            .id(SharedString::from(format!("env-proxy-{index}")))
                                            .hover(|s| s.bg(t::bg_hover()).text_color(t::text_primary()))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if !this.env_rows[index].proxied {
                                                    this.toggle_env_proxy(index, cx);
                                                }
                                            })),
                                        )
                                        .child(
                                            self.mode_button("Direct", !proxied)
                                            .id(SharedString::from(format!("env-direct-{index}")))
                                            .hover(|s| s.bg(t::bg_hover()).text_color(t::text_primary()))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if this.env_rows[index].proxied {
                                                    this.toggle_env_proxy(index, cx);
                                                }
                                            })),
                                        ),
                                ),
                        )
                        .when(proxied, |el| {
                            el.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(t::text_dim())
                                            .child("Allowed Hosts"),
                                    )
                                    .child(
                                        div()
                                            .bg(t::bg_input())
                                            .rounded(px(6.0))
                                            .border_1()
                                            .border_color(t::bg_input())
                                            .hover(|s| s.border_color(t::border_subtle()))
                                            .child(row.hosts_input.clone()),
                                    ),
                            )
                        }),
                )
            })
    }

    fn render_rows(&self, cx: &mut Context<Self>) -> AnyElement {
        let filtered = self.filtered_indices();

        if self.env_rows.is_empty() {
            return div()
                .flex()
                .flex_col()
                .flex_grow()
                .min_h_0()
                .child(self.empty_state("No root .env file found for this workspace"))
                .child(self.footer(cx))
                .into_any_element();
        }

        let mut list = div()
            .bg(t::bg_elevated())
            .border_1()
            .border_color(t::border())
            .rounded(px(12.0))
            .overflow_hidden()
            .flex()
            .flex_col();

        let mut content = div()
            .flex()
            .flex_col()
            .px_4()
            .pt_4()
            .pb_4();

        let scroll = div()
            .flex_grow()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_y_scrollbar();

        if filtered.is_empty() {
            content = content.child(self.empty_state("No environment variables match your search."));
        } else {
            let total = filtered.len();
            for (position, index) in filtered.into_iter().enumerate() {
                let row = &self.env_rows[index];
                list = list.child(self.render_row(
                    index,
                    row,
                    position == 0,
                    position + 1 == total,
                    cx,
                ));
            }
            content = content.child(list);
        }

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .min_h_0()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(t::border_subtle())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(t::text_faint())
                            .child("Select how each .env key is injected on next workspace boot."),
                    )
                    .child(
                        div()
                            .bg(t::bg_input())
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(t::bg_input())
                            .hover(|s| s.border_color(t::border_subtle()))
                            .child(self.search_input.clone()),
                    ),
            )
            .child(scroll.child(content))
            .child(self.footer(cx))
            .into_any_element()
    }
}

impl Render for EnvironmentDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("environment-dialog-backdrop")
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
                    .id("environment-dialog-card")
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            this.dismiss(window, cx);
                        }
                    }))
                    .w(px(640.0))
                    .h(px(680.0))
                    .max_h(relative(0.85))
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
                                    .child("Environment"),
                            ),
                    )
                    .child(self.render_rows(cx)),
            )
    }
}
