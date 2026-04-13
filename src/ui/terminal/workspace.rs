use gpui::*;
use shuru_sdk::AsyncSandbox;
use std::sync::Arc;

use super::panel::MissingSecretsPrompt;
use super::session::{self, AgentStatus, SetupStep, TabKind, TerminalTab, WorkspaceSession};
use crate::agents;
use crate::db::{Agent, Workspace};
use crate::sandbox::agent_setup;
use crate::sandbox::secrets;
use crate::ui::review::SidePanel;
use crate::ui::theme as t;

impl super::TerminalPanel {
    pub fn set_side_panel(&mut self, panel: Entity<SidePanel>) {
        self.side_panel = Some(panel);
    }

    /// Returns the sandbox and tokio handle for the active tab in a workspace.
    pub fn get_active_sandbox(
        &self,
        ws_id: i64,
        cx: &App,
    ) -> Option<(Arc<AsyncSandbox>, tokio::runtime::Handle)> {
        let session = self.sessions.get(&ws_id)?;
        let s = session.read(cx);
        if let Some(sb) = s.active_sandbox() {
            return Some((sb, self.tokio_handle.clone()));
        }
        None
    }

    /// Notify the side panel with the best available sandbox for a workspace.
    /// Public variant for external callers (e.g., workspace_item click handler).
    pub fn notify_side_panel_pub(&self, ws_id: i64, cx: &mut Context<Self>) {
        self.notify_side_panel(ws_id, cx);
    }

    pub(super) fn notify_side_panel(&self, ws_id: i64, cx: &mut Context<Self>) {
        if self.active_workspace_id != Some(ws_id) {
            return;
        }
        let Some(ref side_panel) = self.side_panel else {
            return;
        };
        if let Some((sandbox, tokio_handle)) = self.get_active_sandbox(ws_id, cx) {
            side_panel.update(cx, |sp, cx| {
                sp.on_sandbox_ready(sandbox, tokio_handle, cx);
            });
        } else {
            side_panel.update(cx, |sp, cx| {
                sp.deactivate(cx);
            });
        }
    }

    pub fn set_on_open_settings(&mut self, cb: impl Fn(&mut Window, &mut App) + 'static) {
        self.on_open_settings = Some(Box::new(cb));
    }

    pub fn set_on_open_port_dialog(&mut self, cb: impl Fn(i64, Option<Arc<AsyncSandbox>>, tokio::runtime::Handle, &mut Window, &mut App) + 'static) {
        self.on_open_port_dialog = Some(Box::new(cb));
    }

    // --- Setup progress helpers ---

    pub(super) fn update_step_label(&mut self, ws_id: i64, tab_id: u64, step_idx: usize, label: SharedString, cx: &mut App) {
        if let Some(session) = self.sessions.get(&ws_id) {
            session.update(cx, |s: &mut session::WorkspaceSession, _cx: &mut gpui::Context<session::WorkspaceSession>| {
                s.update_step_label(tab_id, step_idx, label);
            });
        }
    }

    pub(super) fn advance_step(&mut self, ws_id: i64, tab_id: u64, done_idx: usize, next_idx: usize, cx: &mut App) {
        if let Some(session) = self.sessions.get(&ws_id) {
            session.update(cx, |s: &mut session::WorkspaceSession, _cx: &mut gpui::Context<session::WorkspaceSession>| {
                s.advance_step(tab_id, done_idx, next_idx);
            });
        }
    }

    /// Mark a setup step as failed. If `logs` is provided, saves them to a file
    /// and includes the path in the error message.
    pub(super) fn fail_setup(
        &mut self,
        ws_id: i64,
        tab_id: u64,
        step_idx: usize,
        error: String,
        logs: Option<String>,
        cx: &mut App,
    ) {
        let mut error_msg = error;

        // Save logs to file for debugging
        if let Some(log_content) = logs {
            if !log_content.trim().is_empty() {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                let log_dir = format!("{}/.local/share/superhq/logs", home);
                let _ = std::fs::create_dir_all(&log_dir);
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let log_path = format!("{}/setup-{}-{}.log", log_dir, tab_id, ts);
                if std::fs::write(&log_path, &log_content).is_ok() {
                    error_msg = format!("{} — logs saved to {}", error_msg, log_path);
                }
            }
        }

        if let Some(session) = self.sessions.get(&ws_id) {
            session.update(cx, |s: &mut session::WorkspaceSession, _cx: &mut gpui::Context<session::WorkspaceSession>| {
                s.fail_setup(tab_id, step_idx, error_msg);
            });
        }
    }


    // --- Core methods ---

    /// Remove a workspace session and all its tabs/sandboxes.
    pub fn remove_session(&mut self, workspace_id: i64, cx: &mut Context<Self>) {
        self.sessions.remove(&workspace_id);
        if self.active_workspace_id == Some(workspace_id) {
            self.active_workspace_id = None;
            // Hide the side panel since no workspace is active
            if let Some(ref side_panel) = self.side_panel {
                side_panel.update(cx, |sp, cx| sp.deactivate(cx));
            }
        }
        cx.notify();
    }

    fn default_agent(&self) -> Option<&Agent> {
        let settings = self.db.get_settings().ok()?;
        let agent_id = settings.default_agent_id?;
        self.agents.iter().find(|a| a.id == agent_id)
    }

    pub fn activate_workspace(
        &mut self,
        workspace: &Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Only clear the missing secrets prompt if switching to a different workspace
        if self.active_workspace_id != Some(workspace.id) {
            self.missing_secrets_prompt = None;
        }

        if let Some(session) = self.sessions.get(&workspace.id) {
            let s = session.read(cx);
            if let Some(tab) = s.tabs.get(s.active_tab) {
                if let Some(ref terminal) = tab.terminal {
                    terminal.read(cx).focus_handle().focus(window);
                }
            }
            self.active_workspace_id = Some(workspace.id);
            cx.notify();
            return;
        }

        let ws_id = workspace.id;

        // Restore any saved checkpointed tabs from DB
        let saved_tabs = self.db.load_checkpointed_tabs(ws_id).unwrap_or_default();
        let mut restored_tabs: Vec<TerminalTab> = Vec::new();

        for saved in &saved_tabs {
            let agent = saved.agent_id.and_then(|aid| self.agents.iter().find(|a| a.id == aid));
            let tab_id = self.next_tab_id;
            self.next_tab_id += 1;

            restored_tabs.push(TerminalTab {
                tab_id,
                label: SharedString::from(saved.label.clone()),
                dynamic_title: std::rc::Rc::new(std::cell::RefCell::new(None)),
                terminal: None,
                setup_steps: None,
                setup_error: None,
                agent_color: agent.and_then(|a| a.color.as_ref().and_then(|c| t::parse_hex_color(c))),
                icon_path: agent.and_then(|a| a.icon.as_ref().map(|i| SharedString::from(i.clone()))),
                kind: TabKind::Agent {
                    agent_id: saved.agent_id.unwrap_or(0),
                    agent_name: agent.map(|a| a.display_name.clone()).unwrap_or_default(),
                    sandbox: None,
                    auth_gateway: None,
                },
                agent_status: AgentStatus::Unknown,
                event_service: None,
                checkpointing: false,
                checkpoint_name: saved.checkpoint_name.clone(),
                tab_db_id: Some(saved.id),
            });
        }

        let session = cx.new(|_| {
            let mut s = WorkspaceSession::new(ws_id, workspace.name.clone(), workspace.mount_path.clone());
            s.tabs = restored_tabs;
            s
        });
        self.sessions.insert(ws_id, session);
        self.active_workspace_id = Some(ws_id);

        // Open default agent tab if no restored tabs and auto-launch is enabled
        let auto_launch = self.db.get_settings()
            .map(|s| s.auto_launch_agent)
            .unwrap_or(true);
        if saved_tabs.is_empty() && auto_launch {
            let agent = self.default_agent().cloned()
                .or_else(|| self.agents.first().cloned());
            if let Some(agent) = agent {
                self.open_agent_tab(
                    agent.id,
                    agent.display_name.clone(),
                    agent.command.clone(),
                    agent.color.as_ref().and_then(|c| t::parse_hex_color(c)),
                    agent.icon.as_ref().map(|i| SharedString::from(i.clone())),
                    workspace.sandbox_checkpoint_name.clone(),
                    workspace.initial_prompt.clone(),
                    cx,
                );
            }
        }

        cx.notify();
    }

    /// Open a new agent tab — appears immediately with setup progress,
    /// then boots its own sandbox in the background.
    pub fn open_agent_tab(
        &mut self,
        agent_id: i64,
        agent_name: String,
        agent_command: String,
        agent_color: Option<Rgba>,
        icon_path: Option<SharedString>,
        checkpoint_from: Option<String>,
        initial_prompt: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let ws_id = match self.active_workspace_id {
            Some(id) => id,
            None => return,
        };

        let agent_info = self.agents.iter().find(|a| a.id == agent_id);
        let required = agent_info
            .map(|a| a.required_secrets.clone())
            .unwrap_or_default();
        let agent_slug = agent_info.map(|a| a.name.clone()).unwrap_or_default();

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;

        // Compute label synchronously (needs current session state for duplicate numbering)
        let display_label = if let Some(ref prompt) = initial_prompt {
            let truncated = if prompt.len() > 30 {
                format!("{}...", &prompt[..27])
            } else {
                prompt.clone()
            };
            SharedString::from(truncated)
        } else if let Some(session) = self.sessions.get(&ws_id) {
            let s = session.read(cx);
            let same_name_count = s.tabs.iter()
                .filter(|t| matches!(&t.kind, TabKind::Agent { agent_name: n, .. } if *n == agent_name))
                .count();
            if same_name_count > 0 {
                SharedString::from(format!("{} ({})", agent_name, same_name_count + 1))
            } else {
                SharedString::from(agent_name.clone())
            }
        } else {
            SharedString::from(agent_name.clone())
        };

        // Determine setup steps from static agent config
        let static_steps: Vec<&str> = agents::builtin_agents()
            .into_iter()
            .find(|c| c.name == agent_slug)
            .map(|c| c.install_steps.iter().map(|s| s.label()).collect())
            .unwrap_or_default();

        let needs_install = checkpoint_from.is_none()
            && !static_steps.is_empty()
            && !agent_setup::checkpoint_exists(&agent_setup::agent_checkpoint_name(&agent_slug));

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

        // Create tab immediately
        if let Some(session) = self.sessions.get(&ws_id) {
            session.update(cx, |s: &mut session::WorkspaceSession, cx: &mut gpui::Context<session::WorkspaceSession>| {
                s.add_tab(TerminalTab {
                    tab_id,
                    label: display_label,
                    dynamic_title: std::rc::Rc::new(std::cell::RefCell::new(None)),
                    terminal: None,
                    setup_steps: Some(setup_steps),
                    setup_error: None,
                    agent_color,
                    icon_path: icon_path.clone(),
                    kind: TabKind::Agent {
                        agent_id,
                        agent_name: agent_name.clone(),
                        sandbox: None,
                        auth_gateway: None,
                    },
                    agent_status: AgentStatus::Unknown,
                    event_service: None,
                    checkpointing: false,
                    checkpoint_name: None,
                    tab_db_id: None,
                }, cx);
                s.tab_scroll.scroll_to_item(s.tabs.len() - 1);
            });
        }
        cx.notify();

        // Check for missing secrets — show error on the tab, don't block creation
        let do_check = !self.skip_secret_check;
        self.skip_secret_check = false;
        if do_check {
            let missing = secrets::check_missing(&self.db, &required);
            if !missing.is_empty() {
                let missing_list = missing.iter()
                    .map(|e| e.env_var().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.missing_secrets_prompt = Some(MissingSecretsPrompt {
                    ws_id,
                    tab_id,
                    agent_id,
                    agent_name,
                    agent_command,
                    checkpoint_from,
                    missing,
                });
                if let Some(session) = self.sessions.get(&ws_id) {
                    session.update(cx, |s: &mut session::WorkspaceSession, _cx: &mut gpui::Context<session::WorkspaceSession>| {
                        if let Some(tab) = s.find_tab_mut(tab_id) {
                            tab.setup_steps = None;
                            tab.setup_error = Some(format!("Missing: {missing_list}"));
                        }
                    });
                }
                cx.notify();
                return;
            }
        }

        self.boot_agent_tab(ws_id, tab_id, agent_id, agent_command, checkpoint_from, cx);
    }
}
