use gpui::*;
use gpui_terminal::{ColorPalette, TerminalConfig};

use super::panel::MissingSecretsPrompt;
use super::session::{SetupStep, TabKind};
use crate::agents;
use crate::sandbox::agent_setup;
use crate::sandbox::secrets;
use crate::ui::theme as t;

impl super::TerminalPanel {
    pub fn request_close_tab(&mut self, ws_id: i64, tab_id: u64, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get(&ws_id) {
            let is_agent_with_sandbox = session.read(cx).tabs.iter()
                .find(|t| t.tab_id == tab_id)
                .map_or(false, |t| matches!(&t.kind, TabKind::Agent { sandbox: Some(_), .. }));

            if is_agent_with_sandbox {
                self.pending_close = Some((ws_id, tab_id));
                cx.notify();
                return;
            }
        }
        self.force_close_tab(ws_id, tab_id, cx);
    }

    pub fn force_close_tab(&mut self, ws_id: i64, tab_id: u64, cx: &mut Context<Self>) {
        self.pending_close = None;
        if let Some(session) = self.sessions.get(&ws_id) {
            session.update(cx, |s, cx| {
                s.remove_tab(tab_id, cx);
            });

            let has_sandbox = session.read(cx).tabs.iter().any(|t| {
                matches!(&t.kind, TabKind::Agent { sandbox: Some(_), .. })
            });
            if !has_sandbox {
                if let Some(ref side_panel) = self.side_panel {
                    side_panel.update(cx, |sp, cx| sp.deactivate(cx));
                }
            }

            cx.notify();
        }
    }

    pub fn checkpoint_tab(&mut self, ws_id: i64, tab_id: u64, cx: &mut Context<Self>) {
        self.pending_close = None;
        let (sb, cp_name, agent_id, label) = {
            let Some(session) = self.sessions.get(&ws_id) else { return };
            let s = session.read(cx);
            let Some(tab) = s.tabs.iter().find(|t| t.tab_id == tab_id) else { return };
            match &tab.kind {
                TabKind::Agent { sandbox: Some(sandbox), agent_name, agent_id, .. } => {
                    let name = format!("tab-{}-{}", agent_name.to_lowercase(), tab_id);
                    (sandbox.clone(), name, *agent_id, tab.label.clone())
                }
                _ => return,
            }
        };

        if let Some(session) = self.sessions.get(&ws_id) {
            session.update(cx, |s, _cx| {
                if let Some(tab) = s.find_tab_mut(tab_id) {
                    tab.checkpointing = true;
                }
            });
        }
        cx.notify();

        let tokio_handle = self.tokio_handle.clone();
        let db = self.db.clone();

        cx.spawn(async move |this, cx| {
            let cp_result = {
                let n = cp_name.clone();
                tokio_handle
                    .spawn(async move { sb.checkpoint(&n).await })
                    .await
                    .unwrap()
            };

            if let Err(e) = cp_result {
                eprintln!("Checkpoint failed: {e}");
                let _ = cx.update(|cx| {
                    this.update(cx, |panel, cx| {
                        if let Some(session) = panel.sessions.get(&ws_id) {
                            session.update(cx, |s, _cx| {
                                if let Some(tab) = s.find_tab_mut(tab_id) {
                                    tab.checkpointing = false;
                                }
                            });
                        }
                        cx.notify();
                    }).ok();
                });
                return;
            }

            let _ = cx.update(|cx| {
                this.update(cx, |panel, cx| {
                    let db_id = db.save_checkpointed_tab(
                        ws_id, &label, agent_id, &cp_name,
                    ).ok();

                    if let Some(session) = panel.sessions.get(&ws_id) {
                        session.update(cx, |s, _cx| {
                            if let Some(tab) = s.find_tab_mut(tab_id) {
                                tab.checkpointing = false;
                                tab.terminal = None;
                                tab.checkpoint_name = Some(cp_name);
                                tab.tab_db_id = db_id;
                                if let TabKind::Agent { sandbox, .. } = &mut tab.kind {
                                    *sandbox = None;
                                }
                            }
                            s.tabs.retain(|t| {
                                match &t.kind {
                                    TabKind::Shell { parent_agent_tab_id, .. } => *parent_agent_tab_id != tab_id,
                                    _ => true,
                                }
                            });
                        });
                    }
                    cx.notify();
                }).ok();
            });
        }).detach();
    }

    pub fn on_settings_closed(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.missing_secrets_prompt.take() {
            let still_missing = secrets::check_missing(&self.db, &prompt.missing);
            if still_missing.is_empty() {
                let slug = self.agents.iter()
                    .find(|a| a.id == prompt.agent_id)
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                let static_steps: Vec<&str> = agents::builtin_agents()
                    .into_iter()
                    .find(|c| c.name == slug)
                    .map(|c| c.install_steps.iter().map(|s| s.label()).collect())
                    .unwrap_or_default();
                let needs_install = prompt.checkpoint_from.is_none()
                    && !static_steps.is_empty()
                    && !agent_setup::checkpoint_exists(&agent_setup::agent_checkpoint_name(&slug));
                let setup_steps = if needs_install {
                    let mut steps = vec![SetupStep::active("Preparing sandbox")];
                    for label in &static_steps {
                        steps.push(SetupStep::new(*label));
                    }
                    steps.push(SetupStep::new("Saving for next time"));
                    steps.push(SetupStep::new("Starting workspace"));
                    steps
                } else {
                    vec![SetupStep::active("Starting workspace")]
                };

                if let Some(session) = self.sessions.get(&prompt.ws_id) {
                    session.update(cx, |s, _cx| {
                        if let Some(tab) = s.find_tab_mut(prompt.tab_id) {
                            tab.setup_error = None;
                            tab.setup_steps = Some(setup_steps);
                        }
                    });
                }

                self.boot_agent_tab(
                    prompt.ws_id, prompt.tab_id, prompt.agent_id,
                    prompt.agent_command, prompt.checkpoint_from, cx,
                );
            } else {
                self.missing_secrets_prompt = Some(MissingSecretsPrompt {
                    missing: still_missing,
                    ..prompt
                });
                cx.notify();
            }
        }
    }

    pub fn dismiss_missing_secrets(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.missing_secrets_prompt.take() {
            if let Some(session) = self.sessions.get(&prompt.ws_id) {
                session.update(cx, |s, _cx| {
                    if let Some(tab) = s.find_tab_mut(prompt.tab_id) {
                        tab.setup_error = None;
                        tab.setup_steps = Some(vec![SetupStep::active("Starting workspace")]);
                    }
                });
            }

            self.boot_agent_tab(
                prompt.ws_id, prompt.tab_id, prompt.agent_id,
                prompt.agent_command, prompt.checkpoint_from, cx,
            );
        }
    }

    pub fn fork_tab(&mut self, ws_id: i64, tab_id: u64, cx: &mut Context<Self>) {
        let (checkpoint_name, agent_id, agent_name, agent_color, icon_path) = {
            let session = match self.sessions.get(&ws_id) {
                Some(s) => s,
                None => return,
            };
            let s = session.read(cx);
            let tab = match s.tabs.iter().find(|t| t.tab_id == tab_id) {
                Some(t) => t,
                None => return,
            };
            let cp = match &tab.checkpoint_name {
                Some(cp) => cp.clone(),
                None => return,
            };
            let (aid, aname) = match &tab.kind {
                TabKind::Agent { agent_id, agent_name, .. } => (*agent_id, agent_name.clone()),
                _ => return,
            };
            (cp, aid, aname, tab.agent_color, tab.icon_path.clone())
        };

        let agent = self.agents.iter().find(|a| a.id == agent_id);
        let command = agent.map(|a| a.command.clone()).unwrap_or_default();

        self.open_agent_tab(
            agent_id, agent_name, command, agent_color, icon_path,
            Some(checkpoint_name), None, cx,
        );
    }

    pub fn remove_stopped_tab(&mut self, ws_id: i64, tab_id: u64, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get(&ws_id) {
            let db_id = session.read(cx).find_tab(tab_id).and_then(|t| t.tab_db_id);
            if let Some(db_id) = db_id {
                let _ = self.db.delete_checkpointed_tab(db_id);
            }
        }
        self.force_close_tab(ws_id, tab_id, cx);
    }

    pub fn activate_tab_by_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let ws_id = match self.active_workspace_id { Some(id) => id, None => return };
        if let Some(session) = self.sessions.get(&ws_id) {
            session.update(cx, |s, cx| { s.activate_tab(index, cx); });
            let s = session.read(cx);
            if let Some(tab) = s.tabs.get(s.active_tab) {
                if let Some(ref terminal) = tab.terminal {
                    terminal.read(cx).focus_handle().focus(window);
                }
            }
            cx.notify();
        }
        self.notify_side_panel(ws_id, cx);
    }

    pub fn activate_agent_menu_item(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let running_agents = self.active_agent_tabs(cx);
        let shell_count = if running_agents.is_empty() { 0 } else { running_agents.len() };
        let idx = self.agent_menu_index;

        if idx < shell_count {
            let tab_id = running_agents[idx].tab_id;
            self.open_shell_tab(tab_id, cx);
        } else if idx == shell_count {
            // Host shell
            self.open_host_shell_tab(cx);
        } else {
            let agent_idx = idx - shell_count - 1;
            if let Some(a) = self.agents.get(agent_idx).cloned() {
                let color = a.color.as_ref().and_then(|c| t::parse_hex_color(c));
                let icon = a.icon.clone().map(SharedString::from);
                self.open_agent_tab(a.id, a.display_name, a.command, color, icon, None, None, cx);
            }
        }
        self.show_agent_menu = false;
        cx.notify();
    }

    pub fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        let ws_id = match self.active_workspace_id { Some(id) => id, None => return };
        let tab_id = self.sessions.get(&ws_id)
            .and_then(|session| {
                let s = session.read(cx);
                s.active_tab_ref().map(|t| t.tab_id)
            });
        if let Some(tab_id) = tab_id {
            self.request_close_tab(ws_id, tab_id, cx);
        }
    }

    pub fn next_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ws_id = match self.active_workspace_id { Some(id) => id, None => return };
        if let Some(session) = self.sessions.get(&ws_id) {
            session.update(cx, |s, cx| { s.next_tab(cx); });
            let s = session.read(cx);
            if let Some(tab) = s.tabs.get(s.active_tab) {
                if let Some(ref terminal) = tab.terminal {
                    terminal.read(cx).focus_handle().focus(window);
                }
            }
            cx.notify();
        }
    }

    pub fn prev_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ws_id = match self.active_workspace_id { Some(id) => id, None => return };
        if let Some(session) = self.sessions.get(&ws_id) {
            session.update(cx, |s, cx| { s.prev_tab(cx); });
            let s = session.read(cx);
            if let Some(tab) = s.tabs.get(s.active_tab) {
                if let Some(ref terminal) = tab.terminal {
                    terminal.read(cx).focus_handle().focus(window);
                }
            }
            cx.notify();
        }
    }

    pub fn make_terminal_config() -> TerminalConfig {
        TerminalConfig {
            font_family: "Menlo".into(),
            font_size: px(13.0),
            cols: 80,
            rows: 24,
            scrollback: 10000,
            line_height_multiplier: 1.3,
            padding: Edges::all(px(8.0)),
            colors: {
                let (br, bg, bb) = t::rgb_bytes(t::bg_terminal());
                let (fr, fg, fb) = t::rgb_bytes(t::terminal_foreground());
                let (cr, cg, cb) = t::rgb_bytes(t::terminal_cursor());
                ColorPalette::builder()
                    .background(br, bg, bb)
                    .foreground(fr, fg, fb)
                    .cursor(cr, cg, cb)
                    .build()
            },
            scrollbar_thumb: t::scrollbar_thumb().into(),
        }
    }
}
