use gpui::*;
use gpui_terminal::TerminalView;
use shuru_sdk::AsyncSandbox;
use std::sync::Arc;

use crate::sandbox::auth_gateway::AuthGateway;
use crate::sandbox::event_watcher::AgentEventService;

// --- Setup progress types ---

#[derive(Clone, Copy, PartialEq)]
pub enum StepStatus {
    Pending,
    Active,
    Done,
    Failed,
}

#[derive(Clone)]
pub struct SetupStep {
    pub label: SharedString,
    pub status: StepStatus,
}

impl SetupStep {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), status: StepStatus::Pending }
    }
    pub fn active(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), status: StepStatus::Active }
    }
}

// --- Agent status (from lifecycle hooks) ---

#[derive(Clone, Debug, Default)]
pub enum AgentStatus {
    #[default]
    Unknown,
    Running { tool: Option<String> },
    NeedsInput { message: Option<String> },
    Idle,
}

impl AgentStatus {
    /// Short display text for sidebar status, including agent names.
    pub fn display_text(&self, names: &[String]) -> Option<String> {
        let label = match names.len() {
            0 => "Agent".to_string(),
            1 => names[0].clone(),
            _ => names.join(", "),
        };
        match self {
            Self::Unknown => None,
            Self::Running { .. } if names.len() > 1 => Some(format!("{label} are running")),
            Self::Running { .. } => Some(format!("{label} is running")),
            Self::NeedsInput { message: Some(m) } if !m.is_empty() => Some(m.clone()),
            Self::NeedsInput { .. } if names.len() > 1 => Some(format!("{label} need input")),
            Self::NeedsInput { .. } => Some(format!("{label} is waiting for input")),
            Self::Idle if names.len() > 1 => Some(format!("{label} are ready")),
            Self::Idle => Some(format!("{label} is ready")),
        }
    }

    /// Priority for aggregating across tabs (higher = more important).
    pub fn priority(&self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Idle => 1,
            Self::Running { .. } => 2,
            Self::NeedsInput { .. } => 3,
        }
    }
}

// --- Tab types ---

pub enum TabKind {
    Agent {
        agent_id: i64,
        agent_name: String,
        sandbox: Option<Arc<AsyncSandbox>>,
        auth_gateway: Option<AuthGateway>,
    },
    Shell {
        parent_agent_tab_id: u64,
        sandbox: Arc<AsyncSandbox>,
    },
    HostShell {
        #[allow(dead_code)]
        pty_master: Arc<parking_lot::Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    },
}

pub struct TerminalTab {
    pub tab_id: u64,
    pub label: SharedString,
    pub terminal: Option<Entity<TerminalView>>,
    pub setup_steps: Option<Vec<SetupStep>>,
    pub setup_error: Option<String>,
    pub agent_color: Option<Rgba>,
    pub icon_path: Option<SharedString>,
    pub kind: TabKind,
    pub agent_status: AgentStatus,
    pub event_service: Option<AgentEventService>,
    pub checkpointing: bool,
    pub checkpoint_name: Option<String>,
    pub tab_db_id: Option<i64>,
}

impl TerminalTab {
    pub fn is_setting_up(&self) -> bool {
        self.setup_steps.is_some()
    }

    pub fn is_stopped(&self) -> bool {
        self.checkpoint_name.is_some() && self.terminal.is_none()
    }
}

// --- Session events ---

#[derive(Clone)]
pub enum SessionEvent {
    TabAdded { tab_id: u64 },
    TabRemoved { tab_id: u64 },
    TabActivated { tab_id: u64 },
    SandboxReady { tab_id: u64 },
    NoSandboxRemaining,
}

// --- Session ---

pub struct WorkspaceSession {
    pub workspace_id: i64,
    pub workspace_name: String,
    pub mount_path: Option<String>,
    pub tabs: Vec<TerminalTab>,
    pub active_tab: usize,
    pub tab_scroll: ScrollHandle,
}

impl EventEmitter<SessionEvent> for WorkspaceSession {}

impl WorkspaceSession {
    pub fn new(workspace_id: i64, workspace_name: String, mount_path: Option<String>) -> Self {
        Self {
            workspace_id,
            workspace_name,
            mount_path,
            tabs: Vec::new(),
            active_tab: 0,
            tab_scroll: ScrollHandle::new(),
        }
    }

    pub fn add_tab(&mut self, tab: TerminalTab, cx: &mut Context<Self>) {
        let tab_id = tab.tab_id;
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        cx.emit(SessionEvent::TabAdded { tab_id });
        cx.notify();
    }

    pub fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() { return; }
        self.active_tab = index;
        if let Some(tab) = self.tabs.get(index) {
            cx.emit(SessionEvent::TabActivated { tab_id: tab.tab_id });
        }
        cx.notify();
    }

    pub fn next_tab(&mut self, cx: &mut Context<Self>) {
        if self.tabs.is_empty() { return; }
        self.active_tab = (self.active_tab + 1) % self.tabs.len();
        if let Some(tab) = self.tabs.get(self.active_tab) {
            cx.emit(SessionEvent::TabActivated { tab_id: tab.tab_id });
        }
        cx.notify();
    }

    pub fn prev_tab(&mut self, cx: &mut Context<Self>) {
        if self.tabs.is_empty() { return; }
        self.active_tab = if self.active_tab == 0 {
            self.tabs.len() - 1
        } else {
            self.active_tab - 1
        };
        if let Some(tab) = self.tabs.get(self.active_tab) {
            cx.emit(SessionEvent::TabActivated { tab_id: tab.tab_id });
        }
        cx.notify();
    }

    pub fn remove_tab(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        let is_agent = self.tabs.iter()
            .find(|t| t.tab_id == tab_id)
            .map_or(false, |t| matches!(&t.kind, TabKind::Agent { .. }));

        if is_agent {
            // Remove agent tab and all its child shell tabs
            self.tabs.retain(|t| {
                if t.tab_id == tab_id { return false; }
                match &t.kind {
                    TabKind::Shell { parent_agent_tab_id, .. } => *parent_agent_tab_id != tab_id,
                    _ => true,
                }
            });
        } else {
            self.tabs.retain(|t| t.tab_id != tab_id);
        }

        if self.tabs.is_empty() {
            self.active_tab = 0;
        } else {
            self.active_tab = self.tabs.len() - 1;
        }

        cx.emit(SessionEvent::TabRemoved { tab_id });

        // Check if any sandbox remains
        let has_sandbox = self.tabs.iter().any(|t| {
            matches!(&t.kind, TabKind::Agent { sandbox: Some(_), .. })
        });
        if !has_sandbox {
            cx.emit(SessionEvent::NoSandboxRemaining);
        }

        cx.notify();
    }

    pub fn find_tab(&self, tab_id: u64) -> Option<&TerminalTab> {
        self.tabs.iter().find(|t| t.tab_id == tab_id)
    }

    pub fn find_tab_mut(&mut self, tab_id: u64) -> Option<&mut TerminalTab> {
        self.tabs.iter_mut().find(|t| t.tab_id == tab_id)
    }

    pub fn active_tab_ref(&self) -> Option<&TerminalTab> {
        self.tabs.get(self.active_tab)
    }

    /// Get the sandbox from the active tab, or fall back to any agent tab.
    /// Returns None if the active tab is a host terminal.
    pub fn active_sandbox(&self) -> Option<Arc<AsyncSandbox>> {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            match &tab.kind {
                TabKind::Agent { sandbox: Some(sb), .. } => return Some(sb.clone()),
                TabKind::Shell { sandbox, .. } => return Some(sandbox.clone()),
                TabKind::HostShell { .. } => return None,
                _ => {}
            }
        }
        // Fall back to any agent tab with a ready sandbox
        for tab in &self.tabs {
            if let TabKind::Agent { sandbox: Some(sb), .. } = &tab.kind {
                return Some(sb.clone());
            }
        }
        None
    }

    /// Notify that a sandbox is ready on a tab.
    pub fn sandbox_ready(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        cx.emit(SessionEvent::SandboxReady { tab_id });
        cx.notify();
    }

    // --- Setup step helpers ---

    pub fn set_step(&mut self, tab_id: u64, idx: usize, status: StepStatus) {
        if let Some(tab) = self.find_tab_mut(tab_id) {
            if let Some(ref mut steps) = tab.setup_steps {
                if let Some(step) = steps.get_mut(idx) {
                    step.status = status;
                }
            }
        }
    }

    pub fn update_step_label(&mut self, tab_id: u64, step_idx: usize, label: SharedString) {
        if let Some(tab) = self.find_tab_mut(tab_id) {
            if let Some(ref mut steps) = tab.setup_steps {
                if let Some(step) = steps.get_mut(step_idx) {
                    step.label = label;
                }
            }
        }
    }

    pub fn advance_step(&mut self, tab_id: u64, done_idx: usize, next_idx: usize) {
        self.set_step(tab_id, done_idx, StepStatus::Done);
        self.set_step(tab_id, next_idx, StepStatus::Active);
    }

    pub fn fail_setup(&mut self, tab_id: u64, step_idx: usize, error: String) {
        self.set_step(tab_id, step_idx, StepStatus::Failed);
        if let Some(tab) = self.find_tab_mut(tab_id) {
            tab.setup_error = Some(error);
        }
    }
}
