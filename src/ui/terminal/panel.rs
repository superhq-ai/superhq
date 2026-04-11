use gpui::*;
use shuru_sdk::AsyncSandbox;
use std::collections::HashMap;
use std::sync::Arc;

use super::session::WorkspaceSession;
use crate::db::{Agent, Database, RequiredSecretEntry};
use crate::ui::review::SidePanel;

/// Saved state for resuming an agent boot after secrets are configured.
pub(super) struct MissingSecretsPrompt {
    pub ws_id: i64,
    pub tab_id: u64,
    pub agent_id: i64,
    pub agent_name: String,
    pub agent_command: String,
    pub checkpoint_from: Option<String>,
    pub missing: Vec<RequiredSecretEntry>,
}

/// Manages the center terminal panel with tabbed terminals per workspace.
pub struct TerminalPanel {
    pub(super) db: Arc<Database>,
    pub(super) agents: Vec<Agent>,
    pub active_workspace_id: Option<i64>,
    pub(super) sessions: HashMap<i64, Entity<WorkspaceSession>>,
    pub(super) tokio_handle: tokio::runtime::Handle,
    pub show_agent_menu: bool,
    pub agent_menu_index: usize,
    pub agent_menu_focus: FocusHandle,
    pub(super) next_tab_id: u64,
    pub(super) pending_close: Option<(i64, u64)>,
    pub(super) missing_secrets_prompt: Option<MissingSecretsPrompt>,
    pub(super) skip_secret_check: bool,
    pub(super) on_open_settings: Option<Box<dyn Fn(&mut Window, &mut App) + 'static>>,
    pub(super) on_open_port_dialog: Option<Box<dyn Fn(i64, Option<Arc<AsyncSandbox>>, tokio::runtime::Handle, &mut Window, &mut App) + 'static>>,
    pub(super) side_panel: Option<Entity<SidePanel>>,
    pub show_tab_badges: bool,
}

impl TerminalPanel {
    pub fn new(db: Arc<Database>, cx: &mut Context<Self>) -> Self {
        let agents = db.list_agents().unwrap_or_default();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");
        let handle = rt.handle().clone();

        std::thread::Builder::new()
            .name("tokio-runtime".into())
            .spawn(move || rt.block_on(std::future::pending::<()>()))
            .expect("Failed to spawn tokio runtime thread");

        Self {
            db,
            agents,
            active_workspace_id: None,
            sessions: HashMap::new(),
            tokio_handle: handle,
            show_agent_menu: false,
            agent_menu_index: 0,
            agent_menu_focus: cx.focus_handle(),
            next_tab_id: 1,
            pending_close: None,
            missing_secrets_prompt: None,
            skip_secret_check: false,
            on_open_settings: None,
            on_open_port_dialog: None,
            side_panel: None,
            show_tab_badges: false,
        }
    }

    /// Get the highest-priority agent status across all tabs in a workspace,
    /// along with the agent name for display.
    pub fn workspace_agent_status(&self, ws_id: i64, cx: &App) -> (String, super::session::AgentStatus) {
        self.sessions.get(&ws_id).map(|session| {
            session.read(cx).tabs.iter()
                .max_by_key(|tab| tab.agent_status.priority())
                .map(|tab| {
                    let name = match &tab.kind {
                        super::session::TabKind::Agent { agent_name, .. } => agent_name.clone(),
                        _ => String::new(),
                    };
                    (name, tab.agent_status.clone())
                })
                .unwrap_or_default()
        }).unwrap_or_default()
    }
}
