use gpui::*;
use gpui::prelude::FluentBuilder as _;
use gpui_terminal::{ColorPalette, TerminalConfig, TerminalView};
use shuru_sdk::{AsyncSandbox, ExposeHostMapping, MountConfig, SandboxConfig};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::agents;
use crate::db::{Agent, Database, RequiredSecretEntry, Workspace};
use crate::ui::review::SidePanel;
use crate::sandbox::agent_config;
use crate::sandbox::agent_setup;
use crate::sandbox::auth_gateway::{AuthGateway, AuthGatewayConfig};
use crate::sandbox::dotenv;
use crate::sandbox::pty_adapter::{ShuruPtyReader, ShuruPtyResizer, ShuruPtyWriter};
use crate::sandbox::secrets;
use crate::ui::animation;
use crate::ui::theme as t;

// Tab navigation and lifecycle actions
actions!(terminal_panel, [
    CloseActiveTab,
    NextTab,
    PrevTab,
]);

// --- Setup progress types ---

#[derive(Clone, Copy, PartialEq)]
enum StepStatus {
    Pending,
    Active,
    Done,
    Failed,
}

#[derive(Clone)]
struct SetupStep {
    label: SharedString,
    status: StepStatus,
}

impl SetupStep {
    fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), status: StepStatus::Pending }
    }
    fn active(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), status: StepStatus::Active }
    }
}

// --- Tab types ---

/// Distinguishes agent tabs (own a sandbox) from shell tabs (borrow one).
enum TabKind {
    /// Owns a sandbox microVM. Closing this tab stops the VM.
    Agent {
        #[allow(dead_code)]
        agent_id: i64,
        agent_name: String,
        sandbox: Option<Arc<AsyncSandbox>>, // None during setup
        auth_gateway: Option<AuthGateway>,  // Dropped on tab close to shut down
    },
    /// Borrows an agent tab's sandbox via open_shell(). Like `docker exec`.
    Shell {
        parent_agent_tab_id: u64,
        #[allow(dead_code)]
        sandbox: Arc<AsyncSandbox>,
    },
}

/// A single terminal tab within a workspace.
struct TerminalTab {
    tab_id: u64,
    label: SharedString,
    terminal: Option<Entity<TerminalView>>,   // None during setup or stopped
    setup_steps: Option<Vec<SetupStep>>,      // Some during setup
    setup_error: Option<String>,              // Set if setup failed
    agent_color: Option<Rgba>,
    icon_path: Option<SharedString>,
    kind: TabKind,
    /// Set when tab is checkpointed and stopped. Sandbox is gone but can be forked.
    checkpoint_name: Option<String>,
    /// DB row id for persisted checkpointed tabs (None for in-memory-only tabs).
    tab_db_id: Option<i64>,
}

impl TerminalTab {
    fn is_setting_up(&self) -> bool {
        self.setup_steps.is_some()
    }

    fn is_stopped(&self) -> bool {
        self.checkpoint_name.is_some() && self.terminal.is_none()
    }
}

/// Drag payload for tab reordering.
#[derive(Clone)]
struct DraggedTab {
    ws_id: i64,
    tab_ix: usize,
    label: SharedString,
    icon_path: Option<SharedString>,
    color: Option<Rgba>,
}

/// Ghost view rendered while dragging a tab.
struct DraggedTabView {
    tab: DraggedTab,
}

impl Render for DraggedTabView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let tab = &self.tab;
        let mut el = div()
            .px_2()
            .py_1()
            .rounded(px(5.0))
            .bg(t::bg_surface())
            .text_xs()
            .text_color(t::text_secondary())
            .flex()
            .items_center()
            .gap_1()
;

        if let Some(ref path) = tab.icon_path {
            let icon_color = tab.color.unwrap_or(t::text_dim());
            el = el.child(
                svg().path(path.clone()).size(px(14.0)).text_color(icon_color),
            );
        }

        el.child(tab.label.clone())
    }
}

/// All state for a workspace's terminal tabs.
struct WorkspaceSession {
    #[allow(dead_code)]
    workspace_name: String,
    mount_path: Option<String>,
    tabs: Vec<TerminalTab>,
    active_tab: usize,
    tab_scroll: ScrollHandle,
}

/// Saved state for resuming an agent boot after secrets are configured.
struct MissingSecretsPrompt {
    ws_id: i64,
    tab_id: u64,
    agent_id: i64,
    agent_name: String,
    agent_command: String,
    checkpoint_from: Option<String>,
    missing: Vec<RequiredSecretEntry>,
}

/// Manages the center terminal panel with tabbed terminals per workspace.
pub struct TerminalPanel {
    db: Arc<Database>,
    agents: Vec<Agent>,
    pub active_workspace_id: Option<i64>,
    sessions: HashMap<i64, WorkspaceSession>,
    tokio_handle: tokio::runtime::Handle,
    pub show_agent_menu: bool,
    pub agent_menu_index: usize,
    pub agent_menu_focus: FocusHandle,
    next_tab_id: u64,
    /// Pending close confirmation: (workspace_id, tab_id)
    pending_close: Option<(i64, u64)>,
    /// Pending missing secrets prompt — boot resumes when secrets are configured.
    missing_secrets_prompt: Option<MissingSecretsPrompt>,
    /// Set to skip secret check on next open_agent_tab call (used by "Skip" button).
    skip_secret_check: bool,
    /// Callback to open settings panel (avoids action dispatch issues).
    on_open_settings: Option<Box<dyn Fn(&mut Window, &mut App) + 'static>>,
    /// Callback to open the ports dialog. Receives workspace_id.
    on_open_port_dialog: Option<Box<dyn Fn(i64, &mut Window, &mut App) + 'static>>,
    /// Reference to the right sidebar for sandbox-ready notifications.
    side_panel: Option<Entity<SidePanel>>,
    /// Whether to show tab index badges (when Ctrl is held).
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

    pub fn set_side_panel(&mut self, panel: Entity<SidePanel>) {
        self.side_panel = Some(panel);
    }

    /// Returns the sandbox and tokio handle for the active tab in a workspace.
    pub fn get_active_sandbox(
        &self,
        ws_id: i64,
    ) -> Option<(Arc<AsyncSandbox>, tokio::runtime::Handle)> {
        let session = self.sessions.get(&ws_id)?;
        if let Some(tab) = session.tabs.get(session.active_tab) {
            if let TabKind::Agent {
                sandbox: Some(ref sb),
                ..
            } = tab.kind
            {
                return Some((sb.clone(), self.tokio_handle.clone()));
            }
            // Shell tabs also have sandbox access
            if let TabKind::Shell { ref sandbox, .. } = tab.kind {
                return Some((sandbox.clone(), self.tokio_handle.clone()));
            }
        }
        // Fall back to any agent tab with a ready sandbox
        for tab in &session.tabs {
            if let TabKind::Agent {
                sandbox: Some(ref sb),
                ..
            } = tab.kind
            {
                return Some((sb.clone(), self.tokio_handle.clone()));
            }
        }
        None
    }

    /// Notify the side panel with the best available sandbox for a workspace.
    /// Public variant for external callers (e.g., workspace_item click handler).
    pub fn notify_side_panel_pub(&self, ws_id: i64, cx: &mut Context<Self>) {
        self.notify_side_panel(ws_id, cx);
    }

    fn notify_side_panel(&self, ws_id: i64, cx: &mut Context<Self>) {
        eprintln!("[side-panel] notify_side_panel called for ws_id={}", ws_id);
        eprintln!("[side-panel]   active_workspace_id={:?}", self.active_workspace_id);
        if self.active_workspace_id != Some(ws_id) {
            eprintln!("[side-panel]   BAIL: workspace not active");
            return;
        }
        let Some(ref side_panel) = self.side_panel else {
            eprintln!("[side-panel]   BAIL: no side_panel entity");
            return;
        };
        let sandbox_result = self.get_active_sandbox(ws_id);
        eprintln!("[side-panel]   get_active_sandbox returned: {}", sandbox_result.is_some());
        if let Some((sandbox, tokio_handle)) = sandbox_result {
            side_panel.update(cx, |sp, cx| {
                eprintln!("[side-panel]   calling on_sandbox_ready, current sandbox.is_some()={}", sp.sandbox.is_some());
                sp.on_sandbox_ready(sandbox, tokio_handle, cx);
            });
        }
    }

    pub fn set_on_open_settings(&mut self, cb: impl Fn(&mut Window, &mut App) + 'static) {
        self.on_open_settings = Some(Box::new(cb));
    }

    pub fn set_on_open_port_dialog(&mut self, cb: impl Fn(i64, &mut Window, &mut App) + 'static) {
        self.on_open_port_dialog = Some(Box::new(cb));
    }

    // --- Setup progress helpers ---

    fn set_step(&mut self, ws_id: i64, tab_id: u64, idx: usize, status: StepStatus) {
        if let Some(session) = self.sessions.get_mut(&ws_id) {
            if let Some(tab) = session.tabs.iter_mut().find(|t| t.tab_id == tab_id) {
                if let Some(ref mut steps) = tab.setup_steps {
                    if let Some(step) = steps.get_mut(idx) {
                        step.status = status;
                    }
                }
            }
        }
    }

    fn update_step_label(&mut self, ws_id: i64, tab_id: u64, step_idx: usize, label: SharedString) {
        if let Some(session) = self.sessions.get_mut(&ws_id) {
            if let Some(tab) = session.tabs.iter_mut().find(|t| t.tab_id == tab_id) {
                if let Some(ref mut steps) = tab.setup_steps {
                    if let Some(step) = steps.get_mut(step_idx) {
                        step.label = label;
                    }
                }
            }
        }
    }

    fn advance_step(&mut self, ws_id: i64, tab_id: u64, done_idx: usize, next_idx: usize) {
        self.set_step(ws_id, tab_id, done_idx, StepStatus::Done);
        self.set_step(ws_id, tab_id, next_idx, StepStatus::Active);
    }

    /// Mark a setup step as failed. If `logs` is provided, saves them to a file
    /// and includes the path in the error message.
    fn fail_setup(
        &mut self,
        ws_id: i64,
        tab_id: u64,
        step_idx: usize,
        error: String,
        logs: Option<String>,
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

        if let Some(session) = self.sessions.get_mut(&ws_id) {
            if let Some(tab) = session.tabs.iter_mut().find(|t| t.tab_id == tab_id) {
                if let Some(ref mut steps) = tab.setup_steps {
                    if let Some(step) = steps.get_mut(step_idx) {
                        step.status = StepStatus::Failed;
                    }
                }
                tab.setup_error = Some(error_msg);
            }
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
            if let Some(tab) = session.tabs.get(session.active_tab) {
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
                checkpoint_name: saved.checkpoint_name.clone(),
                tab_db_id: Some(saved.id),
            });
        }

        self.sessions.insert(ws_id, WorkspaceSession {
            workspace_name: workspace.name.clone(),
            mount_path: workspace.mount_path.clone(),
            tabs: restored_tabs,
            active_tab: 0,
            tab_scroll: ScrollHandle::new(),
        });
        self.active_workspace_id = Some(ws_id);

        // Open default agent tab if no restored tabs
        if saved_tabs.is_empty() {
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
            let same_name_count = session.tabs.iter()
                .filter(|t| matches!(&t.kind, TabKind::Agent { agent_name: n, .. } if n == &agent_name))
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
        if let Some(session) = self.sessions.get_mut(&ws_id) {
            session.tabs.push(TerminalTab {
                tab_id,
                label: display_label,
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
                checkpoint_name: None,
                tab_db_id: None,
            });
            session.active_tab = session.tabs.len() - 1;
            session.tab_scroll.scroll_to_item(session.tabs.len() - 1);
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
                if let Some(session) = self.sessions.get_mut(&ws_id) {
                    if let Some(tab) = session.tabs.iter_mut().find(|t| t.tab_id == tab_id) {
                        tab.setup_steps = None;
                        tab.setup_error = Some(format!("Missing: {missing_list}"));
                    }
                }
                cx.notify();
                return;
            }
        }

        self.boot_agent_tab(ws_id, tab_id, agent_id, agent_command, checkpoint_from, cx);
    }

    /// Spawn the async boot sequence for an existing tab.
    fn boot_agent_tab(
        &mut self,
        ws_id: i64,
        tab_id: u64,
        agent_id: i64,
        agent_command: String,
        checkpoint_from: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let agent_info = self.agents.iter().find(|a| a.id == agent_id);
        let required_secrets = agent_info
            .map(|a| a.required_secrets.clone())
            .unwrap_or_default();
        let agent_slug = agent_info.map(|a| a.name.clone()).unwrap_or_default();
        let agent_for_config = agent_info.cloned();

        // Get install steps from static config
        let static_cfg = agents::builtin_agents()
            .into_iter()
            .find(|c| c.name == agent_slug);
        let install_steps: Vec<agents::InstallStep> = static_cfg
            .map(|c| c.install_steps)
            .unwrap_or_default();

        let needs_install = checkpoint_from.is_none()
            && !install_steps.is_empty()
            && !agent_setup::checkpoint_exists(&agent_setup::agent_checkpoint_name(&agent_slug));

        // The "Starting workspace" step index depends on whether install is needed
        let start_ws_step = if needs_install { install_steps.len() + 2 } else { 0 };

        let db_for_secrets = self.db.clone();
        let mount_path = self.sessions.get(&ws_id)
            .and_then(|s| s.mount_path.clone());

        let tokio_handle = self.tokio_handle.clone();
        let this = cx.entity().downgrade();

        cx.spawn(async move |_, cx| {
            // === CHECKPOINT PHASE ===
            let boot_from = if checkpoint_from.is_some() {
                checkpoint_from
            } else if needs_install {
                // Step 0: Preparing sandbox (already Active)
                let sb_settings_install = db_for_secrets.get_settings().ok();
                let mut install_config = SandboxConfig::default();
                install_config.allow_net = true;
                install_config.memory_mb = sb_settings_install.as_ref().map(|s| s.sandbox_memory_mb as u64).unwrap_or(8192);
                install_config.disk_size_mb = sb_settings_install.as_ref().map(|s| s.sandbox_disk_mb as u64).unwrap_or(16384);

                let install_sb = tokio_handle
                    .spawn(async move { AsyncSandbox::boot(install_config).await })
                    .await
                    .unwrap();

                let install_sb = match install_sb {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        cx.update(|cx| {
                            this.update(cx, |p, cx| {
                                p.fail_setup(ws_id, tab_id, 0, format!("{e}"), None);
                                cx.notify();
                            }).ok();
                        }).ok();
                        return;
                    }
                };

                // Run each install step, advancing the UI after each
                for (step_ix, step) in install_steps.iter().enumerate() {
                    let ui_ix = step_ix + 1; // step 0 is "Preparing sandbox"

                    cx.update(|cx| {
                        this.update(cx, |p, cx| {
                            p.advance_step(ws_id, tab_id, ui_ix.saturating_sub(1), ui_ix);
                            cx.notify();
                        }).ok();
                    }).ok();

                    // Check skip_if
                    if let Some(skip_cmd) = step.skip_if() {
                        let sb = install_sb.clone();
                        let cmd = skip_cmd.to_string();
                        if let Ok(Ok(r)) = tokio_handle
                            .spawn(async move { sb.exec_in("bash", &cmd).await })
                            .await
                        {
                            if r.exit_code == 0 {
                                continue;
                            }
                        }
                    }

                    // Execute the step
                    let step_result: Result<(), String> = match step {
                        agents::InstallStep::Cmd { command, .. } => {
                            let sb = install_sb.clone();
                            let cmd = command.to_string();
                            match tokio_handle
                                .spawn(async move { sb.exec_in("bash", &cmd).await })
                                .await
                                .unwrap()
                            {
                                Ok(r) if r.exit_code != 0 => {
                                    Err(format!("exit code {} — {}{}", r.exit_code, r.stdout, r.stderr))
                                }
                                Err(e) => Err(format!("{e}")),
                                _ => Ok(()),
                            }
                        }
                        agents::InstallStep::Group { steps: sub_steps, .. } => {
                            let mut group_result = Ok(());
                            for sub in sub_steps {
                                let r: Result<(), String> = match sub {
                                    agents::InstallStep::Cmd { command, .. } => {
                                        let sb = install_sb.clone();
                                        let cmd = command.to_string();
                                        match tokio_handle.spawn(async move { sb.exec_in("bash", &cmd).await }).await.unwrap() {
                                            Ok(r) if r.exit_code != 0 => Err(format!("exit code {} — {}{}", r.exit_code, r.stdout, r.stderr)),
                                            Err(e) => Err(format!("{e}")),
                                            _ => Ok(()),
                                        }
                                    }
                                    agents::InstallStep::Download { label: dl_label, url, path, extract, .. } => {
                                        let sb = install_sb.clone();
                                        let url = url.to_string();
                                        let path = path.to_string();
                                        let extract = *extract;
                                        let base_label = *dl_label;
                                        match tokio_handle.spawn(async move { sb.download(&url, &path, extract).await }).await.unwrap() {
                                            Ok((mut reply_rx, progress_rx)) => {
                                                loop {
                                                    let mut last_progress = None;
                                                    while let Ok(p) = progress_rx.try_recv() {
                                                        last_progress = Some(p);
                                                    }
                                                    if let Some(p) = last_progress {
                                                        let mb = p.bytes_downloaded / (1024 * 1024);
                                                        let label = if let Some(total) = p.total_bytes {
                                                            let total_mb = total / (1024 * 1024);
                                                            SharedString::from(format!("{base_label} ({mb}/{total_mb} MB)"))
                                                        } else {
                                                            SharedString::from(format!("{base_label} ({mb} MB)"))
                                                        };
                                                        cx.update(|cx| {
                                                            this.update(cx, |panel, cx| {
                                                                panel.update_step_label(ws_id, tab_id, ui_ix, label);
                                                                cx.notify();
                                                            }).ok();
                                                        }).ok();
                                                    }
                                                    match reply_rx.try_recv() {
                                                        Ok(Ok(())) => break Ok(()),
                                                        Ok(Err(e)) => break Err(format!("{e}")),
                                                        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => break Err("closed".into()),
                                                        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                                                            cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => Err(format!("{e}")),
                                        }
                                    }
                                    agents::InstallStep::Rename { from, to, .. } => {
                                        let sb = install_sb.clone();
                                        let f = from.to_string();
                                        let t = to.to_string();
                                        tokio_handle.spawn(async move { sb.rename(&f, &t).await }).await.unwrap().map_err(|e| format!("{e}"))
                                    }
                                    agents::InstallStep::Chmod { path, mode, .. } => {
                                        let sb = install_sb.clone();
                                        let p = path.to_string();
                                        let m = *mode;
                                        tokio_handle.spawn(async move { sb.chmod(&p, m).await }).await.unwrap().map_err(|e| format!("{e}"))
                                    }
                                    _ => Ok(()),
                                };
                                if let Err(e) = r {
                                    group_result = Err(e);
                                    break;
                                }
                            }
                            group_result
                        }
                        agents::InstallStep::Download { label, url, path, extract, .. } => {
                            let sb = install_sb.clone();
                            let url = url.to_string();
                            let path = path.to_string();
                            let extract = *extract;
                            let base_label = *label;
                            match tokio_handle
                                .spawn(async move { sb.download(&url, &path, extract).await })
                                .await
                                .unwrap()
                            {
                                Ok((mut reply_rx, progress_rx)) => {
                                    loop {
                                        // Drain progress updates
                                        let mut last_progress = None;
                                        while let Ok(p) = progress_rx.try_recv() {
                                            last_progress = Some(p);
                                        }
                                        if let Some(p) = last_progress {
                                            let mb = p.bytes_downloaded / (1024 * 1024);
                                            let label = if let Some(total) = p.total_bytes {
                                                let total_mb = total / (1024 * 1024);
                                                SharedString::from(format!("{base_label} ({mb}/{total_mb} MB)"))
                                            } else {
                                                SharedString::from(format!("{base_label} ({mb} MB)"))
                                            };
                                            cx.update(|cx| {
                                                this.update(cx, |panel, cx| {
                                                    panel.update_step_label(ws_id, tab_id, ui_ix, label);
                                                    cx.notify();
                                                }).ok();
                                            }).ok();
                                        }

                                        match reply_rx.try_recv() {
                                            Ok(Ok(())) => break Ok(()),
                                            Ok(Err(e)) => break Err(format!("{e}")),
                                            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                                                break Err("download channel closed".into())
                                            }
                                            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                                                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                                            }
                                        }
                                    }
                                }
                                Err(e) => Err(format!("{e}")),
                            }
                        }
                        agents::InstallStep::Rename { from, to, .. } => {
                            let sb = install_sb.clone();
                            let f = from.to_string();
                            let t = to.to_string();
                            tokio_handle.spawn(async move { sb.rename(&f, &t).await }).await.unwrap().map_err(|e| format!("{e}"))
                        }
                        agents::InstallStep::Chmod { path, mode, .. } => {
                            let sb = install_sb.clone();
                            let p = path.to_string();
                            let m = *mode;
                            tokio_handle.spawn(async move { sb.chmod(&p, m).await }).await.unwrap().map_err(|e| format!("{e}"))
                        }
                    };

                    if let Err(msg) = step_result {
                        cx.update(|cx| {
                            this.update(cx, |p, cx| {
                                p.fail_setup(ws_id, tab_id, ui_ix, msg, None);
                                cx.notify();
                            }).ok();
                        }).ok();
                        return;
                    }
                }

                // All install steps done → Saving checkpoint
                let save_step_ix = install_steps.len() + 1;
                cx.update(|cx| {
                    this.update(cx, |p, cx| {
                        p.advance_step(ws_id, tab_id, save_step_ix - 1, save_step_ix);
                        cx.notify();
                    }).ok();
                }).ok();

                let cp_name = agent_setup::agent_checkpoint_name(&agent_slug);
                let sb = install_sb.clone();
                let n = cp_name.clone();
                let cp_result = tokio_handle
                    .spawn(async move { sb.checkpoint(&n).await })
                    .await
                    .unwrap();

                if let Err(e) = cp_result {
                    eprintln!("Checkpoint save failed (non-fatal): {e}");
                }

                // Stop install sandbox
                let sb = install_sb;
                let _ = tokio_handle.spawn(async move { sb.stop().await }).await;

                // Checkpoint done → Starting workspace
                let start_step_ix = save_step_ix + 1;
                cx.update(|cx| {
                    this.update(cx, |p, cx| {
                        p.advance_step(ws_id, tab_id, save_step_ix, start_step_ix);
                        cx.notify();
                    }).ok();
                }).ok();

                Some(cp_name)
            } else if !install_steps.is_empty() {
                // Checkpoint exists — boot from it
                Some(agent_setup::agent_checkpoint_name(&agent_slug))
            } else {
                // No install script — boot from base
                None
            };

            // === LOOK UP STATIC AGENT CONFIG (for auth gateway spec) ===
            let static_config = agents::builtin_agents()
                .into_iter()
                .find(|c| c.name == agent_slug);
            let gateway_spec = static_config.as_ref().and_then(|c| c.auth_gateway.as_ref());

            // === REFRESH OAUTH TOKENS & BUILD SECRETS ===
            {
                let db = db_for_secrets.clone();
                let required = required_secrets.clone();
                let _ = tokio_handle
                    .spawn(async move { secrets::refresh_oauth_tokens(&db, &required).await })
                    .await;
            }
            // Skip MITM proxy setup for secrets handled by the auth gateway
            let gateway_env_vars: HashSet<&str> = gateway_spec
                .map(|s| [s.secret_env_var].into_iter().collect())
                .unwrap_or_default();
            let mut secrets_map = secrets::build_secrets_map(&db_for_secrets, &required_secrets, &gateway_env_vars)
                .map(|r| r.secrets)
                .unwrap_or_default();

            // === PARSE .env FILES FROM MOUNT PATH ===
            // Instead of mounting .env files directly (exposing plaintext secrets),
            // parse them on the host side and route values through the secrets proxy.
            // The proxy generates placeholder tokens as env vars; real values only
            // appear in HTTP traffic via MITM substitution.
            let mut dotenv_guest_path: Option<&str> = None;
            if let Some(ref path) = mount_path {
                let dir = std::path::Path::new(path);
                let dotenv_vars = dotenv::parse_env(dir);
                dotenv_guest_path = dotenv::env_guest_path(dir);

                for (key, value) in dotenv_vars {
                    if secrets_map.contains_key(&key) {
                        continue; // vault secret takes precedence
                    }
                    // Use specific hosts for well-known API keys, catch-all for the rest
                    let hosts = secrets::default_hosts(&key);
                    let hosts = if hosts.is_empty() {
                        vec!["*".into()]
                    } else {
                        hosts
                    };
                    secrets_map.insert(
                        key.clone(),
                        shuru_sdk::SecretConfig {
                            from: key,
                            hosts,
                            value: Some(value),
                        },
                    );
                }
            }

            // === START AUTH GATEWAY (if agent uses one) ===
            let mut auth_gateway_handle: Option<AuthGateway> = None;
            let mut gateway_env: HashMap<String, String> = HashMap::new();
            if let Some(spec) = gateway_spec {
                let gw_config = AuthGatewayConfig {
                    db: db_for_secrets.clone(),
                    secret_env_var: spec.secret_env_var.to_string(),
                    upstream_base: spec.upstream_base.to_string(),
                };
                match tokio_handle
                    .spawn(async move { AuthGateway::start(gw_config).await })
                    .await
                    .unwrap()
                {
                    Ok(gw) => {
                        let gw_url = format!("http://host.shuru.internal:{}/v1", spec.guest_port);
                        if let Some(env_var) = spec.base_url_env {
                            gateway_env.insert(env_var.to_string(), gw_url.clone());
                        }
                        // Always pass the URL so auth_setup can use it
                        gateway_env.insert("_GATEWAY_BASE_URL".to_string(), gw_url);
                        gateway_env.insert(
                            spec.secret_env_var.to_string(),
                            spec.dummy_key.to_string(),
                        );
                        auth_gateway_handle = Some(gw);
                    }
                    Err(e) => {
                        eprintln!("[boot] auth gateway start failed: {e}");
                    }
                }
            }

            // === BOOT TAB SANDBOX ===
            let sb_settings = db_for_secrets.get_settings().ok();
            let mut config = SandboxConfig::default();
            config.allow_net = true;
            config.cpus = sb_settings.as_ref().map(|s| s.sandbox_cpus as usize).unwrap_or(2);
            config.memory_mb = sb_settings.as_ref().map(|s| s.sandbox_memory_mb as u64).unwrap_or(8192);
            config.disk_size_mb = sb_settings.as_ref().map(|s| s.sandbox_disk_mb as u64).unwrap_or(16384);
            config.secrets = secrets_map;
            if let Some(ref path) = mount_path {
                config.mounts.push(MountConfig {
                    host_path: path.clone(),
                    guest_path: "/workspace".to_string(),
                    read_only: true,
                });
            }
            // Expose the auth gateway into the sandbox
            if let (Some(spec), Some(gw)) = (gateway_spec, &auth_gateway_handle) {
                config.expose_host.push(ExposeHostMapping {
                    host_port: gw.host_port,
                    guest_port: spec.guest_port,
                });
            }
            // Port forwarding: sandbox ports accessible on the host
            if let Ok(port_mappings) = db_for_secrets.get_port_mappings(ws_id) {
                for pm in &port_mappings {
                    config.ports.push(shuru_proto::PortMapping {
                        host_port: pm.host_port,
                        guest_port: pm.guest_port,
                    });
                }
            }
            // Expose host ports into the sandbox
            if let Ok(expose_ports) = db_for_secrets.get_expose_host_ports(ws_id) {
                for ep in &expose_ports {
                    config.expose_host.push(ExposeHostMapping {
                        host_port: ep.host_port,
                        guest_port: ep.guest_port,
                    });
                }
            }
            if let Some(ref checkpoint) = boot_from {
                config.from = Some(checkpoint.clone());
            }

            let sandbox = tokio_handle
                .spawn(async move { AsyncSandbox::boot(config).await })
                .await
                .unwrap();

            let sandbox = match sandbox {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    let step_idx = start_ws_step;
                    cx.update(|cx| {
                        this.update(cx, |p, cx| {
                            p.fail_setup(ws_id, tab_id, step_idx, format!("{e}"), None);
                            cx.notify();
                        }).ok();
                    }).ok();
                    return;
                }
            };

            // For scratch sandboxes (no host mount), create /workspace
            if mount_path.is_none() {
                let sb = sandbox.clone();
                let _ = tokio_handle
                    .spawn(async move { sb.mkdir("/workspace", true).await })
                    .await;
            }

            // Remove .env from the sandbox so secrets aren't readable as plaintext
            if let Some(guest_path) = dotenv_guest_path {
                let sb = sandbox.clone();
                let p = guest_path.to_string();
                let _ = tokio_handle
                    .spawn(async move {
                        let _ = sb.exec_in("sh", &format!("rm -f {p}")).await;
                    })
                    .await;
            }

            // Copy host's global gitignore into the VM
            {
                let sb = sandbox.clone();
                let _ = tokio_handle
                    .spawn(async move {
                        if let Some(content) = read_host_global_gitignore() {
                            let _ = sb.mkdir("/root/.config/git", true).await;
                            let _ = sb.write_file("/root/.config/git/ignore", &content).await;
                        }
                    })
                    .await;
            }

            // Write auth config files and persist env vars
            if let Some(ref agent) = agent_for_config {
                let sb = sandbox.clone();
                let agent = agent.clone();
                let gw_env = gateway_env.clone();
                let _ = tokio_handle
                    .spawn(async move {
                        agent_config::run_auth_setup(&sb, &agent, &gw_env).await;
                    })
                    .await;
            }

            // Run the agent binary directly — no shell wrapper.
            // Pass gateway + cert env vars through the SDK so the agent
            // inherits them without needing to source profile.d.
            let agent_argv: Vec<String> = agent_command
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            let mut agent_env = gateway_env.clone();
            // Add proxy CA cert env if MITM is active
            agent_env.insert(
                "NODE_EXTRA_CA_CERTS".into(),
                "/usr/local/share/ca-certificates/shuru-proxy.crt".into(),
            );
            agent_env.insert(
                "SSL_CERT_FILE".into(),
                "/etc/ssl/certs/ca-certificates.crt".into(),
            );
            let shell = {
                let sb = sandbox.clone();
                let argv = agent_argv;
                tokio_handle
                    .spawn(async move {
                        let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
                        sb.open_shell(24, 80, Some("/workspace"), Some(&argv_refs), agent_env).await
                    })
                    .await
                    .unwrap()
            };

            let shell = match shell {
                Ok(s) => s,
                Err(e) => {
                    let step_idx = start_ws_step;
                    cx.update(|cx| {
                        this.update(cx, |p, cx| {
                            p.fail_setup(ws_id, tab_id, step_idx, format!("{e}"), None);
                            cx.notify();
                        }).ok();
                    }).ok();
                    return;
                }
            };

            let (writer, reader) = shell.split();
            let pty_writer = ShuruPtyWriter::new(writer.clone());
            let pty_reader = ShuruPtyReader::new(reader, tokio_handle.clone());
            let resizer = ShuruPtyResizer::new(writer.clone());

            let terminal_config = Self::make_terminal_config();
            let resize_callback = move |cols: usize, rows: usize| {
                resizer.resize(cols as u16, rows as u16);
            };

            cx.update(|cx| {
                this.update(cx, |panel, cx| {
                    let terminal = cx.new(|cx| {
                        TerminalView::new(
                            Box::new(pty_writer),
                            Box::new(pty_reader),
                            terminal_config,
                            cx,
                        )
                        .with_resize_callback(resize_callback)
                    });

                    // Store terminal + sandbox but keep setup view visible.
                    // The terminal buffers output in the background.
                    if let Some(session) = panel.sessions.get_mut(&ws_id) {
                        if let Some(tab) = session.tabs.iter_mut().find(|t| t.tab_id == tab_id) {
                            tab.terminal = Some(terminal.clone());
                            if let TabKind::Agent { sandbox: ref mut sb, auth_gateway: ref mut ag, .. } = tab.kind {
                                *sb = Some(sandbox);
                                *ag = auth_gateway_handle;
                            }
                        }
                    }

                    // Notify the side panel that a sandbox is ready
                    eprintln!("[side-panel] setup complete for ws_id={} tab_id={}", ws_id, tab_id);
                    panel.notify_side_panel(ws_id, cx);

                    // Wait for TUI to take over (cursor hidden or alt screen).
                    let weak_panel = cx.entity().downgrade();
                    cx.spawn(async move |_, cx| {
                        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                        loop {
                            let ready = cx.update(|cx| {
                                terminal.read(cx).tui_active()
                            }).unwrap_or(false);

                            if ready || std::time::Instant::now() >= deadline {
                                cx.update(|cx| {
                                    weak_panel.update(cx, |panel, cx| {
                                        if let Some(session) = panel.sessions.get_mut(&ws_id) {
                                            if let Some(tab) = session.tabs.iter_mut().find(|t| t.tab_id == tab_id) {
                                                tab.setup_steps = None;
                                                tab.setup_error = None;
                                            }
                                            // Auto-focus the terminal if this is the active tab
                                            if let Some(active) = session.tabs.get(session.active_tab) {
                                                if active.tab_id == tab_id {
                                                    if let Some(ref terminal) = active.terminal {
                                                        // cx is &mut App here, no window access — defer focus
                                                        let fh = terminal.read(cx).focus_handle().clone();
                                                        cx.defer(move |cx| {
                                                            if let Some(window) = cx.active_window() {
                                                                window.update(cx, |_, window, _cx| {
                                                                    fh.focus(window);
                                                                }).ok();
                                                            }
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                        cx.notify();
                                    }).ok();
                                }).ok();
                                break;
                            }

                            cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                        }
                    }).detach();

                    cx.notify();
                })
                .ok();
            })
            .ok();
        })
        .detach();
    }

    /// Open a shell tab on an existing agent's sandbox (like `docker exec`).
    pub fn open_shell_tab(
        &mut self,
        parent_agent_tab_id: u64,
        cx: &mut Context<Self>,
    ) {
        let ws_id = match self.active_workspace_id {
            Some(id) => id,
            None => return,
        };

        let session = match self.sessions.get(&ws_id) {
            Some(s) => s,
            None => return,
        };

        // Find the parent agent tab — must have a ready sandbox
        let (sandbox, parent_label) = match session.tabs.iter().find(|t| t.tab_id == parent_agent_tab_id) {
            Some(tab) => match &tab.kind {
                TabKind::Agent { sandbox: Some(sandbox), .. } => (sandbox.clone(), tab.label.clone()),
                _ => return, // Still setting up or not an agent
            },
            None => return,
        };

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;

        let label = format!("Shell ({})", parent_label);
        let tokio_handle = self.tokio_handle.clone();
        let this = cx.entity().downgrade();
        let sandbox_for_kind = sandbox.clone();

        cx.spawn(async move |_, cx| {
            let shell = {
                let sb = sandbox.clone();
                tokio_handle
                    .spawn(async move { sb.open_shell(24, 80, Some("/workspace"), None, HashMap::new()).await })
                    .await
                    .unwrap()
            };

            let shell = match shell {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to open shell: {e}");
                    return;
                }
            };

            let (writer, reader) = shell.split();
            let pty_writer = ShuruPtyWriter::new(writer.clone());
            let pty_reader = ShuruPtyReader::new(reader, tokio_handle.clone());
            let resizer = ShuruPtyResizer::new(writer.clone());

            let terminal_config = Self::make_terminal_config();
            let resize_callback = move |cols: usize, rows: usize| {
                resizer.resize(cols as u16, rows as u16);
            };

            cx.update(|cx| {
                this.update(cx, |panel, cx| {
                    let terminal = cx.new(|cx| {
                        TerminalView::new(
                            Box::new(pty_writer),
                            Box::new(pty_reader),
                            terminal_config,
                            cx,
                        )
                        .with_resize_callback(resize_callback)
                    });

                    if let Some(session) = panel.sessions.get_mut(&ws_id) {
                        let new_idx = session.tabs.len();
                        session.tabs.push(TerminalTab {
                            tab_id,
                            label: SharedString::from(label.clone()),
                            terminal: Some(terminal.clone()),
                            setup_steps: None,
                            setup_error: None,
                            agent_color: None,
                            icon_path: Some(SharedString::from("icons/agents/shell.svg")),
                            kind: TabKind::Shell {
                                parent_agent_tab_id,
                                sandbox: sandbox_for_kind,
                            },
                            checkpoint_name: None,
                            tab_db_id: None,
                                    });
                        session.active_tab = new_idx;
                        session.tab_scroll.scroll_to_item(session.tabs.len() - 1);
                    }
                    // Auto-focus the new shell terminal
                    let fh = terminal.read(cx).focus_handle().clone();
                    cx.defer(move |cx| {
                        if let Some(window) = cx.active_window() {
                            window.update(cx, |_, window, _cx| {
                                fh.focus(window);
                            }).ok();
                        }
                    });
                    cx.notify();
                })
                .ok();
            })
            .ok();
        })
        .detach();
    }

    /// Close a terminal tab by its unique ID.
    /// Closing an agent tab cascades: removes all shell tabs connected to it.
    /// Request tab close — shows confirmation for agent tabs (which own a VM).
    fn request_close_tab(&mut self, ws_id: i64, tab_id: u64, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get(&ws_id) {
            let is_agent_with_sandbox = session.tabs.iter()
                .find(|t| t.tab_id == tab_id)
                .map_or(false, |t| matches!(&t.kind, TabKind::Agent { sandbox: Some(_), .. }));

            if is_agent_with_sandbox {
                // Show confirmation dialog
                self.pending_close = Some((ws_id, tab_id));
                cx.notify();
                return;
            }
        }
        // Shell tabs or setup tabs close immediately
        self.force_close_tab(ws_id, tab_id, cx);
    }

    /// Close a tab without confirmation.
    fn force_close_tab(&mut self, ws_id: i64, tab_id: u64, cx: &mut Context<Self>) {
        self.pending_close = None;
        if let Some(session) = self.sessions.get_mut(&ws_id) {
            let is_agent = session.tabs.iter()
                .find(|t| t.tab_id == tab_id)
                .map_or(false, |t| matches!(&t.kind, TabKind::Agent { .. }));

            if is_agent {
                session.tabs.retain(|t| {
                    if t.tab_id == tab_id { return false; }
                    match &t.kind {
                        TabKind::Shell { parent_agent_tab_id, .. } => *parent_agent_tab_id != tab_id,
                        _ => true,
                    }
                });
            } else {
                session.tabs.retain(|t| t.tab_id != tab_id);
            }

            if session.tabs.is_empty() {
                session.active_tab = 0;
            } else {
                // Focus the last tab
                session.active_tab = session.tabs.len() - 1;
            }

            cx.notify();
        }
    }

    /// Checkpoint the sandbox, then stop it. Tab stays as a "stopped" state.
    /// Persists the checkpointed tab to the DB so it survives app restart.
    fn checkpoint_tab(&mut self, ws_id: i64, tab_id: u64, cx: &mut Context<Self>) {
        self.pending_close = None;
        if let Some(session) = self.sessions.get_mut(&ws_id) {
            if let Some(tab) = session.tabs.iter_mut().find(|t| t.tab_id == tab_id) {
                if let TabKind::Agent { sandbox: Some(sandbox), agent_name, agent_id, .. } = &tab.kind {
                    let cp_name = format!("tab-{}-{}", agent_name.to_lowercase(), tab_id);
                    let sb = sandbox.clone();
                    let n = cp_name.clone();
                    let handle = self.tokio_handle.clone();
                    std::thread::spawn(move || {
                        let _ = handle.block_on(async { sb.checkpoint(&n).await });
                    });

                    // Save to DB
                    let db_id = self.db.save_checkpointed_tab(
                        ws_id, &tab.label, *agent_id, &cp_name,
                    ).ok();

                    tab.terminal = None;
                    tab.checkpoint_name = Some(cp_name);
                    tab.tab_db_id = db_id;
                }
                if let TabKind::Agent { sandbox, .. } = &mut tab.kind {
                    *sandbox = None;
                }
                let agent_tab_id = tab_id;
                session.tabs.retain(|t| {
                    match &t.kind {
                        TabKind::Shell { parent_agent_tab_id, .. } => *parent_agent_tab_id != agent_tab_id,
                        _ => true,
                    }
                });
            }
        }
        cx.notify();
    }

    /// Called when the settings panel is closed.
    /// Re-checks missing secrets and resumes boot on the existing tab if configured.
    pub fn on_settings_closed(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.missing_secrets_prompt.take() {
            let still_missing = secrets::check_missing(&self.db, &prompt.missing);
            if still_missing.is_empty() {
                // Clear error and reset setup steps on the existing tab
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

                if let Some(session) = self.sessions.get_mut(&prompt.ws_id) {
                    if let Some(tab) = session.tabs.iter_mut().find(|t| t.tab_id == prompt.tab_id) {
                        tab.setup_error = None;
                        tab.setup_steps = Some(setup_steps);
                    }
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

    /// Dismiss the missing secrets prompt and boot without secrets (agent may fail).
    fn dismiss_missing_secrets(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.missing_secrets_prompt.take() {
            // Clear error and set setup steps on the existing tab
            if let Some(session) = self.sessions.get_mut(&prompt.ws_id) {
                if let Some(tab) = session.tabs.iter_mut().find(|t| t.tab_id == prompt.tab_id) {
                    tab.setup_error = None;
                    tab.setup_steps = Some(vec![SetupStep::active("Starting workspace")]);
                }
            }

            self.boot_agent_tab(
                prompt.ws_id, prompt.tab_id, prompt.agent_id,
                prompt.agent_command, prompt.checkpoint_from, cx,
            );
        }
    }

    /// Fork a stopped/checkpointed tab — boot a NEW sandbox from the checkpoint.
    /// The stopped tab stays so you can fork multiple times from the same snapshot.
    fn fork_tab(&mut self, ws_id: i64, tab_id: u64, cx: &mut Context<Self>) {
        let (checkpoint_name, agent_id, agent_name, agent_color, icon_path) = {
            let session = match self.sessions.get(&ws_id) {
                Some(s) => s,
                None => return,
            };
            let tab = match session.tabs.iter().find(|t| t.tab_id == tab_id) {
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
            agent_id,
            agent_name,
            command,
            agent_color,
            icon_path,
            Some(checkpoint_name),
            None,
            cx,
        );
    }

    /// Remove a stopped/checkpointed tab permanently.
    fn remove_stopped_tab(&mut self, ws_id: i64, tab_id: u64, cx: &mut Context<Self>) {
        // Delete from DB
        if let Some(session) = self.sessions.get(&ws_id) {
            if let Some(tab) = session.tabs.iter().find(|t| t.tab_id == tab_id) {
                if let Some(db_id) = tab.tab_db_id {
                    let _ = self.db.delete_checkpointed_tab(db_id);
                }
            }
        }
        self.force_close_tab(ws_id, tab_id, cx);
    }

    /// Switch to the Nth tab (0-indexed).
    pub fn activate_tab_by_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let ws_id = match self.active_workspace_id { Some(id) => id, None => return };
        if let Some(session) = self.sessions.get_mut(&ws_id) {
            if index >= session.tabs.len() { return; }
            session.active_tab = index;
            if let Some(tab) = session.tabs.get(index) {
                if let Some(ref terminal) = tab.terminal {
                    terminal.read(cx).focus_handle().focus(window);
                }
            }
            cx.notify();
        }
        self.notify_side_panel(ws_id, cx);
    }

    /// Activate the currently focused agent menu item.
    fn activate_agent_menu_item(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let running_agents = self.active_agent_tabs();
        let shell_count = if running_agents.is_empty() { 0 } else { running_agents.len() };
        let idx = self.agent_menu_index;

        if idx < shell_count {
            // Shell item
            let tab_id = running_agents[idx].tab_id;
            self.open_shell_tab(tab_id, cx);
        } else {
            // Agent item
            let agent_idx = idx - shell_count;
            if let Some(a) = self.agents.get(agent_idx).cloned() {
                let color = a.color.as_ref().and_then(|c| t::parse_hex_color(c));
                let icon = a.icon.clone().map(SharedString::from);
                self.open_agent_tab(a.id, a.display_name, a.command, color, icon, None, None, cx);
            }
        }
        self.show_agent_menu = false;
        cx.notify();
    }

    /// Close the active tab in the current workspace.
    pub fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        let ws_id = match self.active_workspace_id { Some(id) => id, None => return };
        if let Some(session) = self.sessions.get(&ws_id) {
            if let Some(tab) = session.tabs.get(session.active_tab) {
                let tab_id = tab.tab_id;
                self.request_close_tab(ws_id, tab_id, cx);
            }
        }
    }

    /// Switch to the next tab.
    pub fn next_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ws_id = match self.active_workspace_id { Some(id) => id, None => return };
        if let Some(session) = self.sessions.get_mut(&ws_id) {
            if session.tabs.is_empty() { return; }
            session.active_tab = (session.active_tab + 1) % session.tabs.len();
            if let Some(tab) = session.tabs.get(session.active_tab) {
                if let Some(ref terminal) = tab.terminal {
                    terminal.read(cx).focus_handle().focus(window);
                }
            }
            cx.notify();
        }
    }

    /// Switch to the previous tab.
    pub fn prev_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ws_id = match self.active_workspace_id { Some(id) => id, None => return };
        if let Some(session) = self.sessions.get_mut(&ws_id) {
            if session.tabs.is_empty() { return; }
            session.active_tab = if session.active_tab == 0 {
                session.tabs.len() - 1
            } else {
                session.active_tab - 1
            };
            if let Some(tab) = session.tabs.get(session.active_tab) {
                if let Some(ref terminal) = tab.terminal {
                    terminal.read(cx).focus_handle().focus(window);
                }
            }
            cx.notify();
        }
    }

    /// Switch to tab at index (0-based).

    fn make_terminal_config() -> TerminalConfig {
        TerminalConfig {
            font_family: "Menlo".into(),
            font_size: px(13.0),
            cols: 80,
            rows: 24,
            scrollback: 10000,
            line_height_multiplier: 1.3,
            padding: Edges::all(px(8.0)),
            colors: ColorPalette::builder()
                .background(0x11, 0x11, 0x11)
                .foreground(0xcc, 0xcc, 0xcc)
                .cursor(0xcc, 0xcc, 0xcc)
                .build(),
        }
    }
}

// --- Rendering ---

struct AgentTabInfo {
    tab_id: u64,
    name: String,
}

impl TerminalPanel {
    fn render_tab_icon(&self, icon_path: &Option<SharedString>, color: Option<Rgba>, is_active: bool) -> impl IntoElement {
        let icon_color = color.unwrap_or(if is_active { t::text_secondary() } else { t::text_dim() });
        div()
            .w(px(14.0))
            .h(px(14.0))
            .flex()
            .items_center()
            .justify_center()
            .children(icon_path.as_ref().map(|path| {
                svg()
                    .path(path.clone())
                    .size(px(12.0))
                    .text_color(icon_color)
            }))
    }

    fn render_menu_item(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        icon_path: Option<&str>,
        color: Option<Rgba>,
        enabled: bool,
        focused: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        let icon_color = if enabled { color.unwrap_or(t::text_dim()) } else { t::text_dim() };
        let label_color = if enabled { t::text_secondary() } else { t::text_ghost() };
        let label = label.to_string();
        let icon = icon_path.map(|p| SharedString::from(p.to_string()));

        div()
            .id(id.into())
            .px_2p5()
            .py(px(5.0))
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(4.0))
            .when(!enabled, |s| s.opacity(0.4))
            .when(enabled && !focused, |s| s.cursor_pointer().hover(|s| s.bg(t::bg_hover())))
            .when(enabled && focused, |s| s.bg(t::bg_hover()).cursor_pointer())
            .when(enabled, |s| {
                s.on_click(cx.listener(move |this, _, _, cx| {
                    on_click(this, cx);
                    this.show_agent_menu = false;
                    cx.notify();
                }))
            })
            .children(icon.map(|path| {
                svg()
                    .path(path)
                    .size(px(14.0))
                    .text_color(icon_color)
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(label_color)
                    .child(label),
            )
    }

    /// Collect info about ready (non-setup) agent tabs for shell menu.
    fn active_agent_tabs(&self) -> Vec<AgentTabInfo> {
        let ws_id = match self.active_workspace_id {
            Some(id) => id,
            None => return vec![],
        };
        let session = match self.sessions.get(&ws_id) {
            Some(s) => s,
            None => return vec![],
        };
        session.tabs.iter()
            .filter_map(|tab| match &tab.kind {
                TabKind::Agent { sandbox: Some(_), .. } => Some(AgentTabInfo {
                    tab_id: tab.tab_id,
                    name: tab.label.to_string(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Render the setup progress view. Clean step list, error at bottom on failure.
    /// Active step indicator breathes to show liveness.
    fn render_setup_view(
        &self,
        steps: &[SetupStep],
        error: &Option<String>,
        agent_color: Option<Rgba>,
        icon_path: &Option<SharedString>,
    ) -> impl IntoElement {
        let accent = agent_color.unwrap_or(t::text_secondary());

        let mut step_list = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .w(px(280.0));

        for (i, step) in steps.iter().enumerate() {
            let is_active = step.status == StepStatus::Active;

            let (indicator, indicator_color, label_color) = match step.status {
                StepStatus::Done => (
                    "\u{2713}", // ✓
                    t::status_green_dim(),
                    t::text_ghost(),
                ),
                StepStatus::Active => (
                    "\u{25CF}", // ●
                    accent,
                    t::text_secondary(),
                ),
                StepStatus::Pending => (
                    "\u{25CB}", // ○
                    t::text_invisible(),
                    t::text_invisible(),
                ),
                StepStatus::Failed => (
                    "\u{2717}", // ✗
                    t::error_text(),
                    t::error_text(),
                ),
            };

            // Indicator dot — breathing animation when active
            let indicator_el = div()
                .w(px(14.0))
                .text_xs()
                .text_color(indicator_color)
                .flex()
                .justify_center()
                .child(indicator);

            let step_row = if is_active {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py(px(2.0))
                    .child(
                        indicator_el
                            .with_animation(
                                SharedString::from(format!("step-pulse-{i}")),
                                animation::breathing(2.0),
                                |el, t| el.opacity(t),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(label_color)
                            .child(step.label.clone()),
                    )
            } else {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py(px(2.0))
                    .child(indicator_el)
                    .child(
                        div()
                            .text_xs()
                            .text_color(label_color)
                            .child(step.label.clone()),
                    )
            };

            step_list = step_list.child(step_row);
        }

        // Agent icon (static — no breathing animation)
        let icon_el = icon_path.as_ref().map(|path| {
            div()
                .w(px(32.0))
                .h(px(32.0))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    svg()
                        .path(path.clone())
                        .size(px(28.0))
                        .text_color(accent),
                )
        });

        let mut view = div()
            .size_full()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_4()
                    .children(icon_el)
                    .child(step_list),
            );

        if let Some(err) = error {
            // Split error into message and log path (if present)
            let (msg, log_path) = if let Some(idx) = err.find("logs saved to ") {
                let path = err[idx + 14..].trim().to_string();
                let msg = err[..idx].trim_end_matches(" \u{2014} ").to_string(); // trim " — "
                (msg, Some(path))
            } else {
                (err.clone(), None)
            };

            let mut error_bar = div()
                .max_w(px(500.0))
                .px_3()
                .py_2()
                .rounded(px(6.0))
                .bg(t::error_bg())
                .border_1()
                .border_color(t::error_border())
                .text_xs()
                .text_color(t::error_text())
                .flex()
                .flex_col()
                .gap_1()
                .child(msg);

            if let Some(path) = log_path {
                let path_for_click = path.clone();
                error_bar = error_bar.child(
                    div()
                        .id("open-log-file")
                        .text_color(t::text_ghost())
                        .cursor_pointer()
                        .hover(|s| s.text_color(t::text_secondary()))
                        .on_click(move |_, _, _cx| {
                            let _ = std::process::Command::new("open")
                                .arg(&path_for_click)
                                .spawn();
                        })
                        .child(format!("View logs: {path}")),
                );
            }

            view = view.child(
                div()
                    .absolute()
                    .bottom(px(24.0))
                    .left_0()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(error_bar),
            );
        }

        view
    }
}

/// Read the host's global gitignore (~/.config/git/ignore).
fn read_host_global_gitignore() -> Option<Vec<u8>> {
    let home = std::env::var("HOME").ok()?;
    std::fs::read(format!("{home}/.config/git/ignore")).ok()
}

impl Render for TerminalPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self
            .active_workspace_id
            .and_then(|id| self.sessions.get(&id));

        match active {
            Some(session) => {
                let active_tab_idx = session.active_tab;
                let ws_id = self.active_workspace_id.unwrap();
                let show_menu = self.show_agent_menu;
                let has_tabs = !session.tabs.is_empty();

                // Capture active tab's state for content rendering
                let active_tab_terminal = session.tabs.get(active_tab_idx)
                    .and_then(|t| t.terminal.clone());
                let active_tab_setup = session.tabs.get(active_tab_idx)
                    .filter(|t| t.is_setting_up())
                    .map(|t| (
                        t.setup_steps.clone().unwrap_or_default(),
                        t.setup_error.clone(),
                        t.agent_color,
                        t.icon_path.clone(),
                    ));
                let active_tab_stopped = session.tabs.get(active_tab_idx)
                    .filter(|t| t.is_stopped())
                    .map(|t| t.tab_id);

                // Build tab bar items
                let mut tab_elements: Vec<Stateful<Div>> = Vec::new();

                for (i, tab) in session.tabs.iter().enumerate() {
                    let is_active = i == active_tab_idx;
                    let is_setup = tab.is_setting_up();
                    let is_stopped = tab.is_stopped();
                    let tab_ws_id = ws_id;
                    let icon_path = tab.icon_path.clone();
                    let color = if is_stopped { None } else { tab.agent_color };
                    let close_tab_id = tab.tab_id;

                    let display_label: SharedString = if is_setup {
                        SharedString::from(format!("{} (initializing...)", tab.label))
                    } else {
                        tab.label.clone()
                    };


                    tab_elements.push(
                        div()
                            .id(("term-tab", i))
                            .px_2()
                            .py_1()
                            .rounded(px(5.0))
                            .cursor_pointer()
                            .text_xs()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_1()
                            .when(is_stopped, |s| s.opacity(0.5))
                            .when(is_active && !is_stopped, |s| {
                                s.bg(t::bg_selected())
                                    .text_color(t::text_secondary())
                            })
                            .when(!is_active && !is_stopped, |s| {
                                s.text_color(t::text_dim())
                                    .hover(|s| s.bg(t::bg_hover()))
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(session) = this.sessions.get_mut(&tab_ws_id) {
                                    session.active_tab = i;
                                    if let Some(tab) = session.tabs.get(i) {
                                        if let Some(ref terminal) = tab.terminal {
                                            terminal.read(cx).focus_handle().focus(window);
                                        }
                                    }
                                    cx.notify();
                                }
                                this.notify_side_panel(tab_ws_id, cx);
                            }))
                            .on_drag(
                                DraggedTab {
                                    ws_id: tab_ws_id,
                                    tab_ix: i,
                                    label: display_label.clone(),
                                    icon_path: icon_path.clone(),
                                    color,
                                },
                                |tab, _offset, _window, cx| {
                                    cx.new(|_| DraggedTabView { tab: tab.clone() })
                                },
                            )
                            .drag_over::<DraggedTab>(|style, _, _, _| {
                                style.bg(t::bg_hover())
                            })
                            .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                                if dragged.ws_id != tab_ws_id {
                                    return;
                                }
                                if let Some(session) = this.sessions.get_mut(&tab_ws_id) {
                                    let from = dragged.tab_ix;
                                    let to = i;
                                    if from != to && from < session.tabs.len() && to < session.tabs.len() {
                                        let tab = session.tabs.remove(from);
                                        session.tabs.insert(to, tab);
                                        session.active_tab = to;
                                    }
                                    cx.notify();
                                }
                            }))
                            .child(self.render_tab_icon(&icon_path, color, is_active))
                            .child(display_label)
                            .when(self.show_tab_badges && i < 9, |el| {
                                el.child(
                                    div()
                                        .px(px(4.0))
                                        .py(px(0.0))
                                        .rounded(px(3.0))
                                        .bg(t::bg_selected())
                                        .text_color(t::text_muted())
                                        .text_xs()
                                        .child(format!("\u{2303}{}", i + 1)),
                                )
                            })
                            .child(
                                div()
                                    .id(("close-tab", i))
                                    .ml_1()
                                    .px(px(3.0))
                                    .rounded(px(3.0))
                                    .text_color(t::text_ghost())
                                    .hover(|s| s.bg(t::bg_hover()).text_color(t::text_muted()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.request_close_tab(tab_ws_id, close_tab_id, cx);
                                    }))
                                    .child("\u{00D7}"), // ×
                            ),
                    );
                }

                // Build agent menu dropdown
                let agent_menu_el = if show_menu {
                    let running_agents = self.active_agent_tabs();

                    let agents: Vec<(i64, String, String, Option<Rgba>, Option<String>)> = self
                        .agents
                        .iter()
                        .map(|a| (
                            a.id,
                            a.display_name.clone(),
                            a.command.clone(),
                            a.color.as_ref().and_then(|c| t::parse_hex_color(c)),
                            a.icon.clone(),
                        ))
                        .collect();

                    // Count selectable items for keyboard nav
                    let shell_count = if running_agents.is_empty() { 0 } else { running_agents.len() };
                    let total_items = shell_count + agents.len();
                    let focused_idx = self.agent_menu_index.min(total_items.saturating_sub(1));

                    let mut menu = div()
                        .id("agent-menu-popup")
                        .track_focus(&self.agent_menu_focus)
                        .w(px(200.0))
                        .bg(t::bg_surface())
                        .border_1()
                        .border_color(t::border())
                        .rounded(px(8.0))
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .py_1()
                        .px_1()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            match event.keystroke.key.as_str() {
                                "up" => {
                                    if total_items > 0 {
                                        this.agent_menu_index = if this.agent_menu_index == 0 {
                                            total_items - 1
                                        } else {
                                            this.agent_menu_index - 1
                                        };
                                        cx.notify();
                                    }
                                }
                                "down" => {
                                    if total_items > 0 {
                                        this.agent_menu_index = (this.agent_menu_index + 1) % total_items;
                                        cx.notify();
                                    }
                                }
                                "enter" => {
                                    this.activate_agent_menu_item(window, cx);
                                }
                                "escape" => {
                                    this.show_agent_menu = false;
                                    cx.notify();
                                }
                                _ => {}
                            }
                        }));

                    let mut item_idx: usize = 0;

                    // Shell items
                    if running_agents.is_empty() {
                        menu = menu.child(self.render_menu_item(
                            "menu-shell-disabled", "Shell", Some("icons/agents/shell.svg"),
                            None, false, false, cx, |_, _| {},
                        ));
                    } else if running_agents.len() == 1 {
                        let agent_tab_id = running_agents[0].tab_id;
                        let focused = item_idx == focused_idx;
                        menu = menu.child(self.render_menu_item(
                            "menu-shell",
                            &format!("Shell ({})", running_agents[0].name),
                            Some("icons/agents/shell.svg"),
                            None, true, focused, cx,
                            move |this, cx| { this.open_shell_tab(agent_tab_id, cx); },
                        ));
                        item_idx += 1;
                    } else {
                        for (i, info) in running_agents.iter().enumerate() {
                            let agent_tab_id = info.tab_id;
                            let focused = item_idx == focused_idx;
                            menu = menu.child(self.render_menu_item(
                                SharedString::from(format!("menu-shell-{}", i)),
                                &format!("Shell ({})", info.name),
                                Some("icons/agents/shell.svg"),
                                None, true, focused, cx,
                                move |this, cx| { this.open_shell_tab(agent_tab_id, cx); },
                            ));
                            item_idx += 1;
                        }
                    }

                    menu = menu.child(div().h(px(1.0)).mx_1p5().my_0p5().bg(t::border_subtle()));

                    for (id, name, command, color, icon) in agents {
                        let n = name.clone();
                        let cmd = command.clone();
                        let ic = icon.clone().map(SharedString::from);
                        let focused = item_idx == focused_idx;
                        menu = menu.child(self.render_menu_item(
                            SharedString::from(format!("menu-agent-{}", name.to_lowercase())),
                            &name,
                            icon.as_deref(),
                            color, true, focused, cx,
                            move |this, cx| {
                                this.open_agent_tab(
                                    id,
                                    n.clone(),
                                    cmd.clone(),
                                    color,
                                    ic.clone(),
                                    None,
                                    None,
                                    cx,
                                );
                            },
                        ));
                        item_idx += 1;
                    }

                    Some(deferred(
                        anchored().anchor(Corner::TopLeft).snap_to_window()
                            .child(menu)
                    ).with_priority(1))
                } else {
                    None
                };

                let scroll_container = div()
                    .id("tabs-scroll")
                    .flex_shrink()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .overflow_scroll()
                    .track_scroll(&session.tab_scroll)
                    .children(tab_elements);

                let tab_bar = div()
                    .h(px(36.0))
                    .flex_shrink_0()
                    .w_full()
                    .flex()
                    .items_center()
                    .bg(t::bg_elevated())
                    .border_b_1()
                    .border_color(t::border())
                    .child(
                        scroll_container
                            .on_drag_move::<DraggedTab>({
                                let scroll = session.tab_scroll.clone();
                                move |event, _, _| {
                                    let mouse_x = event.event.position.x;
                                    let bounds = event.bounds;
                                    let edge = px(40.0);
                                    let speed = px(8.0);

                                    let mut offset = scroll.offset();
                                    if mouse_x < bounds.left() + edge {
                                        // Near left edge — scroll left (offset toward 0)
                                        offset.x = (offset.x + speed).min(px(0.0));
                                        scroll.set_offset(offset);
                                    } else if mouse_x > bounds.right() - edge {
                                        // Near right edge — scroll right (offset more negative)
                                        offset.x -= speed;
                                        scroll.set_offset(offset);
                                    }
                                }
                            })
                            .pl_1(),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .px_1()
                            .child(
                                div()
                                    .id("add-tab-btn")
                                    .px_2()
                                    .py_1()
                                    .rounded(px(5.0))
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(t::text_ghost())
                                    .hover(|s| s.bg(t::bg_hover()).text_color(t::text_muted()))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_agent_menu = !this.show_agent_menu;
                                        this.agent_menu_index = 0;
                                        if this.show_agent_menu {
                                            this.agent_menu_focus.focus(window);
                                        }
                                        cx.notify();
                                    }))
                                    .child("+"),
                            )
                            .children(agent_menu_el),
                    );

                let mut content = div()
                    .size_full()
                    .relative()
                    .flex()
                    .flex_col()
                    .on_action(cx.listener(|this, _: &CloseActiveTab, _, cx| {
                        if let Some(ws_id) = this.active_workspace_id {
                            if let Some(session) = this.sessions.get(&ws_id) {
                                if let Some(tab) = session.tabs.get(session.active_tab) {
                                    let tab_id = tab.tab_id;
                                    this.request_close_tab(ws_id, tab_id, cx);
                                }
                            }
                        }
                    }))
                    .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                        this.next_tab(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &PrevTab, window, cx| {
                        this.prev_tab(window, cx);
                    }))
                    .child(tab_bar);

                // Close confirmation bar (between tab bar and content)
                if let Some((close_ws_id, close_tab_id)) = self.pending_close {
                    let tab_label = self.sessions.get(&close_ws_id)
                        .and_then(|s| s.tabs.iter().find(|t| t.tab_id == close_tab_id))
                        .map(|t| t.label.clone())
                        .unwrap_or_else(|| SharedString::from("this tab"));

                    content = content.child(
                        div()
                            .w_full()
                            .flex_shrink_0()
                            .px_3()
                            .py_1p5()
                            .bg(t::bg_elevated())
                            .border_b_1()
                            .border_color(t::border())
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            // Message
                            .child(
                                div()
                                    .text_color(t::text_secondary())
                                    .child(format!("Close \u{201C}{tab_label}\u{201D}? Sandbox will be stopped.")),
                            )
                            .child(div().flex_grow())
                            // Cancel
                            .child(
                                div()
                                    .id("close-cancel")
                                    .px_2()
                                    .py(px(3.0))
                                    .rounded(px(4.0))
                                    .cursor_pointer()
                                    .text_color(t::text_dim())
                                    .hover(|s| s.bg(t::bg_hover()))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.pending_close = None;
                                        cx.notify();
                                    }))
                                    .child("Cancel"),
                            )
                            // Checkpoint & Close
                            .child(
                                div()
                                    .id("close-checkpoint")
                                    .px_2()
                                    .py(px(3.0))
                                    .rounded(px(4.0))
                                    .cursor_pointer()
                                    .text_color(t::text_secondary())
                                    .hover(|s| s.bg(t::bg_hover()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.checkpoint_tab(close_ws_id, close_tab_id, cx);
                                    }))
                                    .child("Checkpoint"),
                            )
                            // Close (destructive)
                            .child(
                                div()
                                    .id("close-confirm")
                                    .px_2()
                                    .py(px(3.0))
                                    .rounded(px(4.0))
                                    .cursor_pointer()
                                    .text_color(gpui::rgba(0xFF6B6BFF))
                                    .hover(|s| s.bg(gpui::rgba(0xFF6B6B15)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.force_close_tab(close_ws_id, close_tab_id, cx);
                                    }))
                                    .child("Close"),
                            ),
                    );
                }

                // Missing secrets prompt bar — only show on the affected tab
                let active_tab_id = session.tabs.get(session.active_tab).map(|t| t.tab_id);
                if let Some(ref prompt) = self.missing_secrets_prompt {
                  if active_tab_id == Some(prompt.tab_id) {
                    let missing_list = prompt.missing.iter().map(|e| e.env_var().to_string()).collect::<Vec<_>>().join(", ");
                    let agent_name = prompt.agent_name.clone();
                    content = content.child(
                        div()
                            .w_full()
                            .flex_shrink_0()
                            .px_3()
                            .py_1p5()
                            .bg(t::bg_elevated())
                            .border_b_1()
                            .border_color(t::border())
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .child(
                                div()
                                    .text_color(t::text_secondary())
                                    .child(format!(
                                        "Missing API key for {agent_name}: {missing_list}"
                                    )),
                            )
                            .child(div().flex_grow())
                            .child(
                                div()
                                    .id("secrets-open-settings")
                                    .px_2()
                                    .py(px(3.0))
                                    .rounded(px(4.0))
                                    .cursor_pointer()
                                    .text_color(t::text_secondary())
                                    .hover(|s| s.bg(t::bg_hover()))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if let Some(ref cb) = this.on_open_settings {
                                            cb(window, cx);
                                        }
                                    }))
                                    .child("Open Settings"),
                            )
                            .child(
                                div()
                                    .id("secrets-skip")
                                    .px_2()
                                    .py(px(3.0))
                                    .rounded(px(4.0))
                                    .cursor_pointer()
                                    .text_color(t::text_dim())
                                    .hover(|s| s.bg(t::bg_hover()))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.dismiss_missing_secrets(cx);
                                    }))
                                    .child("Skip"),
                            ),
                    );
                  }
                }

                // Content area: setup view, stopped state, or terminal
                if let Some((steps, error, color, icon)) = active_tab_setup {
                    content = content.child(
                        div()
                            .flex_grow()
                            .size_full()
                            .child(self.render_setup_view(&steps, &error, color, &icon)),
                    );
                } else if let Some(stopped_tab_id) = active_tab_stopped {
                    // Stopped/checkpointed tab — show action bar + empty state
                    let fork_ws = ws_id;
                    let fork_tab = stopped_tab_id;
                    let remove_ws = ws_id;
                    let remove_tab = stopped_tab_id;

                    content = content
                        .child(
                            div()
                                .w_full()
                                .flex_shrink_0()
                                .px_3()
                                .py_1p5()
                                .bg(t::bg_elevated())
                                .border_b_1()
                                .border_color(t::border())
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .text_color(t::text_ghost())
                                        .child("Checkpointed. Sandbox stopped."),
                                )
                                .child(div().flex_grow())
                                .child(
                                    div()
                                        .id("fork-tab")
                                        .px_2()
                                        .py(px(3.0))
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .text_color(t::text_secondary())
                                        .hover(|s| s.bg(t::bg_hover()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.fork_tab(fork_ws, fork_tab, cx);
                                        }))
                                        .child("Fork & Continue"),
                                )
                                .child(
                                    div()
                                        .id("remove-tab")
                                        .px_2()
                                        .py(px(3.0))
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .text_color(t::text_dim())
                                        .hover(|s| s.bg(t::bg_hover()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.remove_stopped_tab(remove_ws, remove_tab, cx);
                                        }))
                                        .child("Remove"),
                                ),
                        )
                        .child(
                            div()
                                .flex_grow()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(t::text_faint())
                                        .child("Sandbox stopped"),
                                ),
                        );
                } else if has_tabs && active_tab_terminal.is_some() {
                    content = content.child(
                        div()
                            .flex_grow()
                            .size_full()
                            .children(active_tab_terminal),
                    );
                } else {
                    content = content.child(
                        div()
                            .flex_grow()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(t::text_faint())
                                    .child("Click + to open an agent"),
                            ),
                    );
                }

                // Status bar: resources on left, ports on right
                if has_tabs {
                    let sb_settings = self.db.get_settings().ok();
                    let cpus = sb_settings.as_ref().map(|s| s.sandbox_cpus).unwrap_or(2);
                    let mem_mb = sb_settings.as_ref().map(|s| s.sandbox_memory_mb).unwrap_or(8192);
                    let mem_display = if mem_mb >= 1024 && mem_mb % 1024 == 0 {
                        format!("{} GB", mem_mb / 1024)
                    } else {
                        format!("{} MB", mem_mb)
                    };
                    let port_count = self.db.get_port_mappings(ws_id)
                        .map(|m| m.len())
                        .unwrap_or(0)
                        + self.db.get_expose_host_ports(ws_id)
                            .map(|m| m.len())
                            .unwrap_or(0);

                    let status_item = |id: &str, icon_path: &str, label: String| {
                        div()
                            .id(SharedString::from(id.to_string()))
                            .px_1p5()
                            .py(px(1.0))
                            .rounded(px(3.0))
                            .text_xs()
                            .text_color(t::text_ghost())
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                svg()
                                    .path(SharedString::from(icon_path.to_string()))
                                    .size(px(12.0))
                                    .text_color(t::text_ghost()),
                            )
                            .child(label)
                    };

                    content = content.child(
                        div()
                            .h(px(24.0))
                            .flex_shrink_0()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_2()
                            .bg(t::bg_elevated())
                            .border_t_1()
                            .border_color(t::border())
                            // Left: resources
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(status_item("cpu-info", "icons/cpu.svg", format!("{} CPU", cpus)))
                                    .child(status_item("mem-info", "icons/memory.svg", mem_display)),
                            )
                            // Right: ports
                            .child(
                                status_item(
                                    "ports-status-btn",
                                    "icons/network.svg",
                                    if port_count > 0 { format!("Ports: {}", port_count) } else { "Ports".to_string() },
                                )
                                .cursor_pointer()
                                .hover(|s| s.bg(t::bg_hover()).text_color(t::text_muted()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some(ref cb) = this.on_open_port_dialog {
                                        cb(ws_id, window, cx);
                                    }
                                })),
                            ),
                    );
                }

                // Backdrop for dismissing menu
                if show_menu {
                    content = content.child(deferred(
                        div()
                            .id("agent-menu-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .occlude()
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                                this.show_agent_menu = false;
                                cx.notify();
                            })),
                    ).with_priority(0));
                }

                content
            }
            None => {
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_2()
                            .child(
                                div().text_sm().text_color(t::text_faint()).child(
                                    "No workspace selected",
                                ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(t::text_invisible())
                                    .child("Create one from the sidebar to get started"),
                            ),
                    )
            }
        }
    }
}
