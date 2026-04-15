mod about;
mod appearance;
pub mod card;
mod general;
mod providers;
mod sandbox;
mod shortcuts;

use gpui::*;
use gpui::prelude::FluentBuilder as _;
use crate::ui::components::actions::Cancel;
use crate::ui::components::scrollbar::{self, ScrollbarState};
use crate::ui::components::TextInput;
use crate::ui::components::Toast;
use std::sync::Arc;

use crate::db::Database;
use crate::ui::theme as t;

struct ProviderMeta {
    env_var: &'static str,
    label: &'static str,
    agents: &'static str,
    icon: Option<&'static str>,
    oauth: bool,
}

const PROVIDERS: &[ProviderMeta] = &[
    ProviderMeta {
        env_var: "ANTHROPIC_API_KEY",
        label: "Anthropic",
        agents: "Claude Code, Pi",
        icon: Some("icons/providers/anthropic.svg"),
        oauth: false,
    },
    ProviderMeta {
        env_var: "OPENAI_API_KEY",
        label: "OpenAI",
        agents: "Codex, Pi",
        icon: Some("icons/providers/openai.svg"),
        oauth: true,
    },
    ProviderMeta {
        env_var: "OPENROUTER_API_KEY",
        label: "OpenRouter",
        agents: "Codex",
        icon: Some("icons/providers/openrouter.svg"),
        oauth: false,
    },
];

fn provider_meta(env_var: &str) -> Option<&'static ProviderMeta> {
    PROVIDERS.iter().find(|p| p.env_var == env_var)
}

fn secret_display_info(env_var: &str) -> (&str, &str) {
    provider_meta(env_var)
        .map(|p| (p.label, p.agents))
        .unwrap_or(("Custom", "Custom secret"))
}

fn supports_oauth(env_var: &str) -> bool {
    provider_meta(env_var).is_some_and(|p| p.oauth)
}

pub(crate) fn provider_icon(env_var: &str) -> Option<&'static str> {
    provider_meta(env_var).and_then(|p| p.icon)
}

// ── Settings nav tabs ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Appearance,
    Secrets,
    Sandbox,
    Shortcuts,
    About,
}

impl SettingsTab {
    fn label(&self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Secrets => "Providers",
            Self::Sandbox => "Sandbox",
            Self::Shortcuts => "Shortcuts",
            Self::About => "About",
        }
    }

    fn all() -> &'static [SettingsTab] {
        &[
            SettingsTab::General,
            SettingsTab::Appearance,
            SettingsTab::Secrets,
            SettingsTab::Sandbox,
            SettingsTab::Shortcuts,
            SettingsTab::About,
        ]
    }
}

// ── OAuth state ──────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) enum OAuthStatus {
    Idle,
    InProgress,
    Connected { email: String, plan: String },
    Error(String),
}

// ── State for a secret row ───────────────────────────────────────

pub(crate) struct SecretRow {
    pub env_var: String,
    pub label: String,
    pub description: String,
    pub input: Entity<TextInput>,
    pub has_saved_value: bool,
    pub auth_method: String,
}

// ── Sandbox defaults (read from DB) ──────────────────────────────

pub(crate) struct SandboxInputs {
    pub cpus: Entity<TextInput>,
    pub memory_mb: Entity<TextInput>,
    pub disk_mb: Entity<TextInput>,
}

// ── SettingsPanel ────────────────────────────────────────────────

pub struct SettingsPanel {
    pub(crate) db: Arc<Database>,
    pub(crate) active_tab: SettingsTab,
    pub(crate) default_agent_id: Option<i64>,
    pub(crate) auto_launch_agent: bool,
    pub(crate) theme_id: String,
    agent_dropdown: Entity<crate::ui::components::Select>,
    pub(crate) secret_rows: Vec<SecretRow>,
    pub(crate) sandbox_inputs: SandboxInputs,
    pub(crate) oauth_status: OAuthStatus,
    pub(crate) toast: Entity<Toast>,
    pub(crate) oauth_cancel: Option<tokio::sync::oneshot::Sender<()>>,
    on_close: Box<dyn Fn(&mut Window, &mut App) + 'static>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    scrollbar_state: ScrollbarState,
}

impl SettingsPanel {
    pub fn new(
        db: Arc<Database>,
        toast: Entity<Toast>,
        on_close: impl Fn(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let agents = db.list_agents().unwrap_or_default();
        let mut env_vars: Vec<String> = agents
            .iter()
            .flat_map(|a| a.required_secrets.iter().map(|e| e.env_var().to_string()))
            .collect();
        env_vars.sort();
        env_vars.dedup();

        let existing_secrets = db.list_secrets().unwrap_or_default();

        let oauth_status = existing_secrets
            .iter()
            .find(|s| s.env_var == "OPENAI_API_KEY" && s.auth_method == "oauth")
            .map(|_| OAuthStatus::Connected {
                email: "OpenAI account".into(),
                plan: String::new(),
            })
            .unwrap_or(OAuthStatus::Idle);

        let secret_rows = env_vars
            .into_iter()
            .map(|env_var| {
                let (lbl, desc) = secret_display_info(&env_var);
                let label = lbl.to_string();
                let description = desc.to_string();
                let has_saved = db.has_secret(&env_var).unwrap_or(false);
                let auth_method = existing_secrets
                    .iter()
                    .find(|s| s.env_var == env_var)
                    .map(|s| s.auth_method.clone())
                    .unwrap_or_else(|| "api_key".into());
                let placeholder_label = label.clone();
                let input = cx.new(|cx| {
                    let mut input = TextInput::new(cx);
                    input.set_placeholder(format!("Enter {placeholder_label} API key"));
                    input.set_masked(true);
                    input
                });
                SecretRow {
                    env_var,
                    label,
                    description,
                    input,
                    has_saved_value: has_saved,
                    auth_method,
                }
            })
            .collect();

        let settings = db.get_settings().ok();
        let cpus_val = settings.as_ref().map(|s| s.sandbox_cpus).unwrap_or(2);
        let mem_val = settings.as_ref().map(|s| s.sandbox_memory_mb).unwrap_or(8192);
        let disk_val = settings.as_ref().map(|s| s.sandbox_disk_mb).unwrap_or(16384);

        let sandbox_inputs = SandboxInputs {
            cpus: cx.new(|cx| {
                let mut s = TextInput::new(cx);
                s.set_value(cpus_val.to_string(), cx);
                s
            }),
            memory_mb: cx.new(|cx| {
                let mut s = TextInput::new(cx);
                s.set_value(mem_val.to_string(), cx);
                s
            }),
            disk_mb: cx.new(|cx| {
                let mut s = TextInput::new(cx);
                s.set_value(disk_val.to_string(), cx);
                s
            }),
        };

        let all_agents = db.list_agents().unwrap_or_default();
        let default_agent_id = settings.as_ref().and_then(|s| s.default_agent_id);
        let agent_dropdown = Self::init_agent_dropdown(&all_agents, default_agent_id, cx);
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);

        let theme_id = settings
            .as_ref()
            .map(|s| s.theme.clone())
            .filter(|id| id == crate::ui::theme::AUTO_THEME
                || crate::ui::theme::theme_entry(id).is_some())
            .unwrap_or_else(|| "superhq-dark".to_string());

        Self {
            db,
            active_tab: SettingsTab::General,
            default_agent_id,
            auto_launch_agent: settings.as_ref().map(|s| s.auto_launch_agent).unwrap_or(true),
            theme_id,
            agent_dropdown,
            secret_rows,
            sandbox_inputs,
            toast,
            oauth_status,
            oauth_cancel: None,
            on_close: Box::new(on_close),
            focus_handle,
            scroll_handle: ScrollHandle::new(),
            scrollbar_state: ScrollbarState::new(),
        }
    }

    fn close(&self, window: &mut Window, cx: &mut App) {
        (self.on_close)(window, cx);
    }

    fn render_nav(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_tab;

        div()
            .w(px(180.0))
            .min_w(px(180.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .py_2()
            .px_2()
            .border_r_1()
            .border_color(t::border_subtle())
            .children(SettingsTab::all().iter().map(|tab| {
                let tab = *tab;
                let is_active = tab == active;
                div()
                    .id(SharedString::from(format!("settings-tab-{:?}", tab)))
                    .px_2p5()
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_xs()
                    .text_color(if is_active {
                        t::text_secondary()
                    } else {
                        t::text_dim()
                    })
                    .when(is_active, |el| el.bg(t::bg_selected()))
                    .hover(|s| s.bg(t::bg_hover()).text_color(t::text_tertiary()))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.active_tab = tab;
                        cx.notify();
                    }))
                    .child(tab.label())
            }))
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000088))
            .occlude()
            .child(
                div()
                    .id("settings-card")
                    .key_context("Dialog")
                    .track_focus(&self.focus_handle)
                    .tab_group()
                    .w(px(720.0))
                    .h(px(520.0))
                    .bg(t::bg_surface())
                    .border_1()
                    .border_color(t::border())
                    .rounded(px(10.0))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                        this.close(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                        this.close(window, cx);
                    }))
                    .on_key_down(|event, window, cx| {
                        use crate::ui::components::actions::KEY_TAB;
                        if event.keystroke.key.as_str() == KEY_TAB {
                            if event.keystroke.modifiers.shift {
                                window.focus_prev();
                            } else {
                                window.focus_next();
                            }
                            cx.stop_propagation();
                        }
                    })
                    // Top bar
                    .child(
                        div()
                            .px_4()
                            .py_2p5()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(t::border_subtle())
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t::text_muted())
                                    .child("Settings"),
                            )
                            .child(
                                div()
                                    .id("settings-close")
                                    .px_2()
                                    .py_1()
                                    .rounded(px(4.0))
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(t::text_ghost())
                                    .hover(|s| s.bg(t::bg_hover()).text_color(t::text_dim()))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close(window, cx);
                                    }))
                                    .child("Close"),
                            ),
                    )
                    // Body: nav + content
                    .child(
                        div()
                            .flex_grow()
                            .flex()
                            .overflow_hidden()
                            .child(self.render_nav(cx))
                            .child(
                                div()
                                    .flex_grow()
                                    .min_h_0()
                                    .relative()
                                    .child({
                                        let sb_for_scroll = self.scrollbar_state.clone();
                                        div()
                                            .id("settings-content")
                                            .absolute()
                                            .top_0()
                                            .left_0()
                                            .size_full()
                                            .overflow_y_scroll()
                                            .track_scroll(&self.scroll_handle)
                                            .on_scroll_wheel(move |_, _, _| {
                                                sb_for_scroll.did_scroll();
                                            })
                                            .p_6()
                                            .child(match self.active_tab {
                                                SettingsTab::General => {
                                                    self.render_general_tab(cx).into_any_element()
                                                }
                                                SettingsTab::Appearance => {
                                                    self.render_appearance_tab(cx).into_any_element()
                                                }
                                                SettingsTab::Secrets => {
                                                    self.render_secrets_tab(cx).into_any_element()
                                                }
                                                SettingsTab::Sandbox => {
                                                    self.render_sandbox_tab(cx).into_any_element()
                                                }
                                                SettingsTab::Shortcuts => {
                                                    Self::render_shortcuts_tab().into_any_element()
                                                }
                                                SettingsTab::About => {
                                                    Self::render_about_tab().into_any_element()
                                                }
                                            })
                                    })
                                    .child({
                                        let scroll_handle = self.scroll_handle.clone();
                                        let scrollbar_state = self.scrollbar_state.clone();
                                        canvas(
                                            move |_, _, _| {},
                                            move |bounds, _, window, _cx| {
                                                scrollbar::paint_scrollbar(
                                                    bounds,
                                                    &scroll_handle,
                                                    &scrollbar_state,
                                                    window,
                                                );
                                            },
                                        )
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .size_full()
                                    }),
                            ),
                    ),
            )
    }
}
