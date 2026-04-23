mod agents;
mod assets;
mod avatar_cache;
mod db;
mod git;
mod oauth;
mod runtime;
mod sandbox;
mod ui;

use anyhow::Result;
use db::Database;
use gpui::*;
use shuru_sdk::AsyncSandbox;
use gpui::prelude::FluentBuilder as _;
use std::sync::Arc;
use ui::dialogs::new_workspace::NewWorkspaceDialog;
use ui::dialogs::ports::PortsDialog;

actions!(
    superhq,
    [
        NewWorkspaceAction,
        OpenSettingsAction,
        ActivateWorkspace1, ActivateWorkspace2, ActivateWorkspace3,
        ActivateWorkspace4, ActivateWorkspace5, ActivateWorkspace6,
        ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9,
        ActivateTab1, ActivateTab2, ActivateTab3,
        ActivateTab4, ActivateTab5, ActivateTab6,
        ActivateTab7, ActivateTab8, ActivateTab9,
        ToggleRightDock,
        ToggleLeftSidebar,
        OpenPortsDialog,
    ]
);
/// Drag payload for resizing panels.
#[derive(Clone)]
enum PanelResize {
    Sidebar,
    RightDock,
}

/// Invisible drag view rendered while resizing.
struct ResizeDragView;

impl Render for ResizeDragView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

use ui::components::Toast;
use ui::dock::{Dock, DockPosition};
use ui::review::SidePanel;
use ui::settings::SettingsPanel;
use ui::setup::{SetupComplete, SetupScreen};
use ui::sidebar::workspace_list::WorkspaceListView;
use ui::terminal::TerminalPanel;

/// Root application view: 3-panel layout (sidebar | terminal | review).
struct AppView {
    db: Arc<Database>,
    sidebar: Entity<WorkspaceListView>,
    terminal: Entity<TerminalPanel>,
    right_dock: Entity<Dock>,
    toast: Entity<Toast>,
    dialog: Option<Entity<NewWorkspaceDialog>>,
    ports_dialog: Option<Entity<PortsDialog>>,
    settings: Option<Entity<SettingsPanel>>,
    setup: Option<Entity<SetupScreen>>,
    sidebar_size: Pixels,
    sidebar_collapsed: bool,
    cmd_held: bool,
    ctrl_held: bool,
    focus_handle: FocusHandle,
    /// Kept alive to keep the keystroke interceptor registered.
    _keystroke_sub: Option<gpui::Subscription>,
    /// Remote-access server (off-by-default; enabled manually via UI).
    remote_access: ui::remote::RemoteAccess,
    /// Kept so we can restart the server after a runtime disable/enable.
    remote_handler: Arc<ui::remote::AppHandler>,
    /// Persistent state for the title-bar remote-control popover.
    remote_popover_state: ui::remote::RemotePopoverState,
    /// Currently-visible pairing approval request (modal). We only ever
    /// display one at a time — a second arriving request is rejected
    /// immediately so the UI doesn't queue surprise modals at the user.
    pending_pairing: Option<PendingPairing>,
}

/// Pending pairing modal state. The `response` sender is shared via
/// `Arc<Mutex<_>>` so a watchdog task (see `on_pairing_approval_request`)
/// can observe whether the client dropped its `Receiver` mid-dialog
/// and auto-dismiss the modal in that case.
struct PendingPairing {
    device_label: String,
    response: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<bool>>>>,
}

impl Drop for PendingPairing {
    fn drop(&mut self) {
        if let Ok(mut g) = self.response.lock() {
            if let Some(tx) = g.take() {
                let _ = tx.send(false);
            }
        }
    }
}

impl AppView {
    fn new(db: Arc<Database>, cx: &mut Context<Self>) -> Self {
        let this_for_settings = cx.entity().downgrade();
        let this_for_ports = cx.entity().downgrade();
        let terminal = cx.new(|cx| {
            let mut panel = TerminalPanel::new(db.clone(), cx);
            let app = this_for_settings.clone();
            panel.set_on_open_settings(move |window, cx| {
                let _ = app.update(cx, |this: &mut Self, cx| {
                    this.open_settings(window, cx);
                });
            });
            panel.set_on_open_port_dialog(move |ws_id, sandbox, tokio_handle, window, cx| {
                let _ = this_for_ports.update(cx, |this: &mut Self, cx| {
                    this.open_ports_dialog(ws_id, sandbox, tokio_handle, window, cx);
                });
            });
            panel
        });
        let review = cx.new(|_| SidePanel::new());
        let right_dock = cx.new(|cx| {
            let mut dock = Dock::new(DockPosition::Right);
            dock.set_size(px(340.0));
            dock.add_panel(review.clone(), cx);
            dock
        });
        // Wire review panel into terminal so it gets sandbox-ready notifications
        terminal.update(cx, |panel, _| {
            panel.set_side_panel(review.clone());
        });
        // Wire "Ask Agent": review panel sends text to the active terminal
        {
            let terminal_for_ask = terminal.clone();
            review.update(cx, |sp, _| {
                sp.set_on_ask_agent(move |msg, cx| {
                    terminal_for_ask.update(cx, |panel, cx| {
                        panel.send_to_active_terminal(&msg, cx);
                    });
                });
            });
        }
        let this = cx.entity().downgrade();
        let sidebar = cx.new(|cx| {
            WorkspaceListView::new(
                db.clone(),
                terminal.clone(),
                review.clone(),
                move |window, cx| {
                    this.update(cx, |app, cx| {
                        app.open_new_workspace_dialog(window, cx);
                    }).ok();
                },
                cx,
            )
        });
        let toast = cx.new(|_| Toast::new());

        let setup = if runtime::is_ready() {
            None
        } else {
            let view = cx.new(|cx| SetupScreen::new(cx));
            cx.subscribe(&view, |this: &mut Self, _, _: &SetupComplete, cx| {
                this.setup = None;
                cx.notify();
            })
            .detach();
            Some(view)
        };

        // Observe the TerminalPanel so any change inside it (tab added,
        // pty_map flipped to ready, agent status update, …) re-runs
        // AppView::render, which is what rebuilds and pushes the
        // remote snapshot. Without this, a TerminalPanel-local
        // `cx.notify()` only re-renders the panel itself — the remote
        // client would never learn the tab is booted.
        cx.observe(&terminal, |_, _, cx| cx.notify()).detach();

        let tokio_handle = terminal.read(cx).tokio_handle().clone();
        let pty_map: ui::remote::PtyMap = terminal.read(cx).pty_map().clone();
        let pairings =
            std::sync::Arc::new(ui::remote::PairingStore::new(db.clone()));
        let mut remote_access =
            ui::remote::RemoteAccess::new(tokio_handle, db.clone());
        // Fail closed: if the settings read fails (corrupt DB, partial
        // migration), default to remote control disabled instead of
        // silently enabling the iroh endpoint.
        let remote_enabled = db
            .get_settings()
            .map(|s| s.remote_control_enabled)
            .unwrap_or(false);
        let (approver, approval_rx) = ui::remote::PairingApprover::new();
        let (commands, command_rx) = ui::remote::HostCommandDispatcher::new();
        let remote_handler = std::sync::Arc::new(ui::remote::AppHandler::new(
            remote_access.state_handle(),
            pty_map,
            pairings,
            ui::remote::AuditLog::open(),
            approver,
            commands,
            remote_access.notifications_sender(),
        ));
        if remote_enabled {
            if let Err(e) = remote_access.start(remote_handler.clone()) {
                tracing::warn!(error = %e, "remote-control: failed to start");
            }
        }

        // Bridge pairing.request approvals from the tokio handler to the
        // GPUI main thread. One in-flight modal at a time — overflow is
        // rejected in `on_pairing_approval_request`.
        {
            let weak_this = cx.entity().downgrade();
            cx.spawn(async move |_, cx| {
                while let Ok(req) = approval_rx.recv_async().await {
                    let _ = cx.update(|cx| {
                        let _ = weak_this.update(cx, |app: &mut Self, cx| {
                            app.on_pairing_approval_request(req, cx);
                        });
                    });
                }
            })
            .detach();
        }

        // Bridge workspace.activate / tabs.create commands from the
        // remote handler to the main thread. Each command carries its
        // own oneshot response channel; we dispatch into TerminalPanel
        // and reply.
        {
            let weak_this = cx.entity().downgrade();
            cx.spawn(async move |_, cx| {
                while let Ok(cmd) = command_rx.recv_async().await {
                    let _ = cx.update(|cx| {
                        let _ = weak_this.update(cx, |app: &mut Self, cx| {
                            app.on_host_command(cmd, cx);
                        });
                    });
                }
            })
            .detach();
        }

        Self {
            db,
            sidebar,
            terminal,
            right_dock,
            toast,
            dialog: None,
            ports_dialog: None,
            settings: None,
            setup,
            sidebar_size: px(240.0),
            sidebar_collapsed: false,
            cmd_held: false,
            ctrl_held: false,
            focus_handle: cx.focus_handle(),
            _keystroke_sub: None,
            remote_access,
            remote_handler,
            remote_popover_state: ui::remote::RemotePopoverState::default(),
            pending_pairing: None,
        }
    }

    fn on_pairing_approval_request(
        &mut self,
        req: ui::remote::PairingApprovalRequest,
        cx: &mut Context<Self>,
    ) {
        if self.pending_pairing.is_some() {
            let _ = req.response.send(false);
            return;
        }
        let response = Arc::new(std::sync::Mutex::new(Some(req.response)));
        self.pending_pairing = Some(PendingPairing {
            device_label: req.device_label,
            response: response.clone(),
        });
        self.remote_popover_state.open.set(false);
        cx.notify();

        // Watchdog: the client may cancel the pairing (close its iroh
        // connection) while the user is still staring at the modal. In
        // that case the handler-side `oneshot::Receiver` is dropped and
        // `Sender::is_closed` flips true. Poll for it so we can
        // dismiss the modal without leaving a dangling dialog.
        let weak = cx.entity().downgrade();
        let watcher = response.clone();
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(300))
                    .await;
                let (still_pending, receiver_dropped) = match watcher.lock() {
                    Ok(g) => match g.as_ref() {
                        Some(tx) => (true, tx.is_closed()),
                        None => (false, false),
                    },
                    Err(_) => (false, false),
                };
                if !still_pending {
                    // User already hit Approve / Reject — nothing to do.
                    break;
                }
                if receiver_dropped {
                    let _ = weak.update(cx, |app: &mut Self, cx| {
                        if let Some(p) = app.pending_pairing.as_ref() {
                            // Guard against a newer request having
                            // replaced this one between ticks.
                            if Arc::ptr_eq(&p.response, &watcher) {
                                app.pending_pairing = None;
                                cx.notify();
                            }
                        }
                    });
                    break;
                }
            }
        })
        .detach();
    }

    fn on_host_command(
        &mut self,
        cmd: ui::remote::HostCommand,
        cx: &mut Context<Self>,
    ) {
        use superhq_remote_proto::{
            error_code,
            methods::{TabsCreateResult, WorkspaceActivateResult},
            RpcError,
        };
        match cmd {
            ui::remote::HostCommand::ActivateWorkspace {
                workspace_id,
                response,
            } => {
                let Ok(workspaces) = self.db.list_workspaces() else {
                    let _ = response.send(Err(RpcError::internal(
                        "failed to read workspace list",
                    )));
                    return;
                };
                let Some(workspace) =
                    workspaces.into_iter().find(|w| w.id == workspace_id)
                else {
                    let _ = response.send(Err(RpcError::new(
                        error_code::NOT_FOUND,
                        format!("workspace {workspace_id} not found"),
                    )));
                    return;
                };
                self.terminal.update(cx, |panel, cx| {
                    panel.activate_workspace(&workspace, None, cx);
                });
                // Rebuild the snapshot after activation and respond with
                // just the rows for this workspace. The render cycle
                // will push the same snapshot to the server shortly,
                // so the client sees a consistent view.
                let snapshot =
                    ui::remote::build_snapshot(&self.terminal, &self.db, cx);
                let ws_info = snapshot
                    .workspaces
                    .iter()
                    .find(|w| w.workspace_id == workspace_id)
                    .cloned();
                let tabs = snapshot
                    .tabs
                    .iter()
                    .filter(|t| t.workspace_id == workspace_id)
                    .cloned()
                    .collect();
                self.remote_access.push_snapshot(snapshot);
                let _ = match ws_info {
                    Some(workspace) => response
                        .send(Ok(WorkspaceActivateResult { workspace, tabs })),
                    None => response.send(Err(RpcError::internal(
                        "workspace activation did not surface in snapshot",
                    ))),
                };
            }
            ui::remote::HostCommand::CreateTab {
                workspace_id,
                spec,
                response,
            } => {
                use superhq_remote_proto::methods::TabCreateSpec;
                // Ensure the workspace is active first — all three
                // openers are no-ops when `active_workspace_id` is None.
                if self.terminal.read(cx).active_workspace_id != Some(workspace_id) {
                    let Ok(workspaces) = self.db.list_workspaces() else {
                        let _ = response.send(Err(RpcError::internal(
                            "failed to read workspace list",
                        )));
                        return;
                    };
                    let Some(workspace) =
                        workspaces.into_iter().find(|w| w.id == workspace_id)
                    else {
                        let _ = response.send(Err(RpcError::new(
                            error_code::NOT_FOUND,
                            format!("workspace {workspace_id} not found"),
                        )));
                        return;
                    };
                    self.terminal.update(cx, |panel, cx| {
                        panel.activate_workspace(&workspace, None, cx);
                    });
                }

                let new_tab_id: Option<u64> = match spec {
                    TabCreateSpec::HostShell => {
                        // Host-shell is off by default — the user must
                        // flip the toggle in Settings > Remote control.
                        let allowed = self
                            .db
                            .get_settings()
                            .map(|s| s.remote_host_shell_enabled)
                            .unwrap_or(false);
                        if !allowed {
                            let _ = response.send(Err(RpcError::new(
                                error_code::PERMISSION_DENIED,
                                "host-shell access is disabled; enable it in \
                                 Settings > Remote control on the desktop",
                            )));
                            return;
                        }
                        self.terminal
                            .update(cx, |panel, cx| panel.open_host_shell_tab(cx))
                    }
                    TabCreateSpec::GuestShell { parent_tab_id } => self
                        .terminal
                        .update(cx, |panel, cx| panel.open_shell_tab(parent_tab_id, cx)),
                    TabCreateSpec::Agent { agent_id } => {
                        let agents = self.db.list_agents().unwrap_or_default();
                        let resolved = agent_id
                            .and_then(|id| {
                                agents.iter().find(|a| a.id == id).cloned()
                            })
                            .or_else(|| {
                                let default_id = self
                                    .db
                                    .get_settings()
                                    .ok()
                                    .and_then(|s| s.default_agent_id);
                                default_id
                                    .and_then(|id| {
                                        agents
                                            .iter()
                                            .find(|a| a.id == id)
                                            .cloned()
                                    })
                                    .or_else(|| agents.first().cloned())
                            });
                        let Some(agent) = resolved else {
                            let _ = response.send(Err(RpcError::new(
                                error_code::NOT_FOUND,
                                "no agent configured to create a tab with",
                            )));
                            return;
                        };
                        let color = agent
                            .color
                            .as_ref()
                            .and_then(|c| ui::theme::parse_hex_color(c));
                        let icon = agent
                            .icon
                            .as_ref()
                            .map(|i| gpui::SharedString::from(i.clone()));
                        self.terminal.update(cx, |panel, cx| {
                            panel.open_agent_tab(
                                agent.id,
                                agent.display_name.clone(),
                                agent.command.clone(),
                                color,
                                icon,
                                None,
                                None,
                                cx,
                            )
                        })
                    }
                };

                let snapshot =
                    ui::remote::build_snapshot(&self.terminal, &self.db, cx);
                self.remote_access.push_snapshot(snapshot);
                let _ = match new_tab_id {
                    Some(tab_id) => response.send(Ok(TabsCreateResult {
                        workspace_id,
                        tab_id,
                    })),
                    None => response.send(Err(RpcError::internal(
                        "tab creation did not yield an id \
                         (wrong spec or inactive workspace?)",
                    ))),
                };
            }
            ui::remote::HostCommand::CloseTab {
                workspace_id,
                tab_id,
                mode,
                response,
            } => {
                use superhq_remote_proto::methods::TabCloseMode;
                // Ensure the workspace is active — close_tab needs the
                // session loaded. If it isn't, the tab isn't present
                // from the remote's perspective either.
                if self.terminal.read(cx).active_workspace_id != Some(workspace_id) {
                    let _ = response.send(Err(RpcError::new(
                        error_code::NOT_FOUND,
                        format!("workspace {workspace_id} is not active"),
                    )));
                    return;
                }
                self.terminal.update(cx, |panel, cx| match mode {
                    TabCloseMode::Checkpoint => panel.checkpoint_tab(workspace_id, tab_id, cx),
                    TabCloseMode::Force => panel.force_close_tab(workspace_id, tab_id, cx),
                });
                let snapshot =
                    ui::remote::build_snapshot(&self.terminal, &self.db, cx);
                self.remote_access.push_snapshot(snapshot);
                let _ = response.send(Ok(()));
            }
            ui::remote::HostCommand::WriteAttachment {
                workspace_id,
                tab_id,
                name,
                bytes,
                response,
            } => {
                let result = self.write_attachment(workspace_id, tab_id, &name, &bytes, cx);
                let _ = response.send(result);
            }
        }
    }

    /// Resolve the destination for a remote-uploaded file + write it.
    /// Agent/guest-shell tabs land inside `/workspace/.attachments/`
    /// in the sandbox (where Claude Code and friends can read them by
    /// path). Host-shell tabs save to `~/Downloads/superhq-attachments/`
    /// on the host OS. Returns the absolute path the user can refer to.
    fn write_attachment(
        &self,
        workspace_id: i64,
        tab_id: u64,
        name: &str,
        bytes: &[u8],
        cx: &Context<Self>,
    ) -> std::result::Result<String, superhq_remote_proto::RpcError> {
        use superhq_remote_proto::{error_code, RpcError};
        use ui::terminal::session::TabKind;

        // Strip any directory separators the client might have sent.
        let sanitized = std::path::Path::new(name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("attachment.bin")
            .to_string();

        let Some(session) = self.terminal.read(cx).sessions().get(&workspace_id) else {
            return Err(RpcError::new(
                error_code::NOT_FOUND,
                format!("workspace {workspace_id} is not active"),
            ));
        };

        let session_ref = session.read(cx);
        let tab = session_ref
            .tabs
            .iter()
            .find(|t| t.tab_id == tab_id)
            .ok_or_else(|| {
                RpcError::new(
                    error_code::NOT_FOUND,
                    format!("tab {tab_id} not found"),
                )
            })?;

        // Resolve the sandbox for sandbox-bound tabs.
        let sandbox = match &tab.kind {
            TabKind::Agent { sandbox: Some(sb), .. } => Some(sb.clone()),
            TabKind::Shell { sandbox, .. } => Some(sandbox.clone()),
            TabKind::Agent { sandbox: None, .. } => {
                return Err(RpcError::new(
                    error_code::NOT_FOUND,
                    "agent tab is not running",
                ));
            }
            TabKind::HostShell { .. } => None,
        };

        // Write + block on completion. Reasonable since the upload's
        // already in the critical path of a blocking RPC response.
        let bytes = bytes.to_vec();
        let file_name = sanitized.clone();
        let tokio_handle = self.terminal.read(cx).tokio_handle().clone();

        if let Some(sb) = sandbox {
            let guest_path = format!("/workspace/.attachments/{file_name}");
            let sb_for_mkdir = sb.clone();
            let mkdir = tokio_handle.block_on(async move {
                sb_for_mkdir
                    .exec_in("bash", "mkdir -p /workspace/.attachments")
                    .await
            });
            if let Err(e) = mkdir {
                return Err(RpcError::internal(format!("mkdir: {e}")));
            }
            let sb_for_write = sb.clone();
            let path_for_write = guest_path.clone();
            let write = tokio_handle.block_on(async move {
                sb_for_write.write_file(&path_for_write, &bytes).await
            });
            if let Err(e) = write {
                return Err(RpcError::internal(format!("write_file: {e}")));
            }
            Ok(guest_path)
        } else {
            // Host shell: save under ~/Downloads/superhq-attachments/.
            let home = std::env::var("HOME").map_err(|_| {
                RpcError::internal("no HOME for host-shell attachment")
            })?;
            let dir = std::path::PathBuf::from(home)
                .join("Downloads")
                .join("superhq-attachments");
            std::fs::create_dir_all(&dir)
                .map_err(|e| RpcError::internal(format!("mkdir: {e}")))?;
            let path = dir.join(&file_name);
            std::fs::write(&path, &bytes)
                .map_err(|e| RpcError::internal(format!("write: {e}")))?;
            Ok(path.display().to_string())
        }
    }

    fn resolve_pairing(&mut self, approved: bool, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending_pairing.take() {
            if let Ok(mut g) = pending.response.lock() {
                if let Some(tx) = g.take() {
                    let _ = tx.send(approved);
                }
            }
            if approved {
                self.toast.update(cx, |t, cx| t.show("Device paired", cx));
            } else {
                self.toast
                    .update(cx, |t, cx| t.show("Pairing rejected", cx));
            }
        }
        cx.notify();
    }

    fn set_remote_control_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if enabled == self.remote_access.is_running() {
            return;
        }
        if enabled {
            if let Err(e) = self.remote_access.start(self.remote_handler.clone()) {
                tracing::warn!(error = %e, "remote-control: failed to start");
                self.toast.update(cx, |t, cx| {
                    t.show(format!("Failed to start remote control: {e}"), cx)
                });
                // Rollback the Settings switch (if open) so the UI
                // matches the real server state.
                self.sync_remote_control_switch(false, cx);
                return;
            }
            self.toast.update(cx, |t, cx| t.show("Remote control enabled", cx));
        } else {
            self.remote_access.stop();
            self.toast.update(cx, |t, cx| t.show("Remote control disabled", cx));
        }
        if let Err(e) = self.db.update_remote_control_enabled(enabled) {
            tracing::warn!(error = %e, "remote-control: failed to persist toggle");
        }
        // Keep the Settings switch in sync when the toggle came from
        // the title-bar popover (or any other entry point).
        self.sync_remote_control_switch(enabled, cx);
        cx.notify();
    }

    fn rotate_host_id(&mut self, cx: &mut Context<Self>) {
        let was_running = self.remote_access.is_running();
        let restart = if was_running {
            Some(self.remote_handler.clone())
        } else {
            None
        };
        let result = self.remote_access.rotate_endpoint_secret(restart);
        let new_id = match result {
            Ok(id) => id.or_else(|| self.remote_access.host_id()),
            Err(e) => {
                tracing::warn!(error = %e, "remote-control: rotate failed");
                self.toast.update(cx, |t, cx| {
                    t.show(format!("Failed to rotate host id: {e}"), cx)
                });
                return;
            }
        };
        self.toast
            .update(cx, |t, cx| t.show("Host id rotated. Paired devices cleared.", cx));
        if let Some(settings) = self.settings.clone() {
            settings.update(cx, |panel, cx| {
                panel.host_id = new_id;
                panel.rotate_confirming = false;
                cx.notify();
            });
        }
        cx.notify();
    }

    fn sync_remote_control_switch(&self, enabled: bool, cx: &mut Context<Self>) {
        // Defer so we don't re-enter SettingsPanel's update when the
        // toggle originated from the Switch inside the settings panel
        // itself. When the source is the popover, the settings panel is
        // not in an update and the deferred call runs immediately after.
        // `Switch::set_value` is a no-op when the value already matches,
        // so syncing is always safe to schedule.
        let settings = self.settings.clone();
        cx.defer(move |cx| {
            if let Some(settings) = settings {
                settings.update(cx, |panel, cx| {
                    panel.remote_control_switch.update(cx, |sw, cx| {
                        sw.set_value(enabled, cx);
                    });
                });
            }
        });
    }

    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        let active_ws = self.terminal.read(cx).active_workspace_id;
        if let Some(ws_id) = active_ws {
            self.sidebar.update(cx, |view, cx| {
                view.active_workspace_id = Some(ws_id);
                view.refresh(cx);
            });
        }
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings_to_tab(None, window, cx);
    }

    fn open_settings_to_tab(
        &mut self,
        tab: Option<ui::settings::SettingsTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(existing) = self.settings.as_ref() {
            if let Some(tab) = tab {
                existing.update(cx, |panel, cx| panel.set_active_tab(tab, cx));
            }
            return;
        }
        self.clear_badges(cx);
        self.sidebar.update(cx, |view, cx| view.clear_active(cx));
        let this = cx.entity().downgrade();
        let this_for_close = this.clone();
        let terminal = self.terminal.clone();
        let db = self.db.clone();
        let toast = self.toast.clone();
        let audit_log_path = self.remote_handler.audit_log_path();
        let host_id = self.remote_access.host_id();
        let on_remote_control_toggled: ui::settings::RemoteControlToggle = {
            let this = this.clone();
            Arc::new(move |enabled, cx| {
                let _ = this.update(cx, |app: &mut Self, cx| {
                    app.set_remote_control_enabled(enabled, cx);
                });
            })
        };
        let on_rotate_host_id: ui::settings::RotateHostIdCallback = {
            let this = this.clone();
            Arc::new(move |cx| {
                let _ = this.update(cx, |app: &mut Self, cx| {
                    app.rotate_host_id(cx);
                });
            })
        };
        let view = cx.new(|cx| {
            let mut panel = SettingsPanel::new(
                db,
                toast,
                on_remote_control_toggled,
                on_rotate_host_id,
                audit_log_path,
                host_id,
                move |window, cx| {
                    this_for_close
                        .update(cx, |app, cx| {
                            app.close_settings(window, cx);
                        })
                        .ok();
                    // Notify terminal panel to re-check missing secrets
                    let _ = terminal.update(cx, |panel, cx| {
                        panel.on_settings_closed(cx);
                    });
                },
                window,
                cx,
            );
            if let Some(tab) = tab {
                panel.set_active_tab(tab, cx);
            }
            panel
        });
        self.settings = Some(view);
        cx.notify();
    }

    fn open_ports_dialog(&mut self, ws_id: i64, sandbox: Option<Arc<AsyncSandbox>>, tokio_handle: tokio::runtime::Handle, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_badges(cx);
        let db = self.db.clone();
        let this = cx.entity().downgrade();

        let view = cx.new(|cx| {
            PortsDialog::new(
                db,
                ws_id,
                sandbox,
                tokio_handle,
                move |window, cx| {
                    this.update(cx, |app, cx| {
                        app.ports_dialog = None;
                        app.focus_handle.focus(window);
                        cx.notify();
                    }).ok();
                },
                window,
                cx,
            )
        });
        self.ports_dialog = Some(view);
        cx.notify();
    }

    fn clear_badges(&mut self, cx: &mut Context<Self>) {
        self.cmd_held = false;
        self.ctrl_held = false;
        self.sidebar.update(cx, |view, cx| view.set_show_badges(false, cx));
        self.terminal.update(cx, |panel, cx| {
            panel.show_tab_badges = false;
            cx.notify();
        });
    }

    fn open_new_workspace_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_badges(cx);
        let db = self.db.clone();
        let sidebar = self.sidebar.clone();
        let this = cx.entity().downgrade();
        let this2 = cx.entity().downgrade();

        let view = cx.new(|cx| {
            NewWorkspaceDialog::new(
                db,
                move |window, cx| {
                    sidebar.update(cx, |view: &mut WorkspaceListView, cx| {
                        view.refresh(cx);
                        // Activate the newly created workspace (last in list)
                        let count = view.workspace_count();
                        if count > 0 {
                            view.activate_by_index(count - 1, window, cx);
                        }
                    });
                    this.update(cx, |app, cx| {
                        app.dialog = None;
                        app.sidebar_collapsed = false;
                        app.focus_handle.focus(window);
                        cx.notify();
                    }).ok();
                },
                move |window, cx| {
                    this2.update(cx, |app, cx| {
                        app.dialog = None;
                        app.focus_handle.focus(window);
                        cx.notify();
                    }).ok();
                },
                window,
                cx,
            )
        });
        self.dialog = Some(view);
        cx.notify();
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use ui::theme as t;

        // Push current workspace/tab snapshot to the remote-control server.
        // Cheap — RwLock write with a brief-held guard.
        let snapshot = ui::remote::build_snapshot(&self.terminal, &self.db, cx);
        self.remote_access.push_snapshot(snapshot);
        // Mirror the configured agents list (with inlined SVG icons)
        // so remote clients can render the same new-tab menu as the
        // desktop.
        let agents = ui::remote::build_agent_infos(&self.db);
        self.remote_handler.set_agents(agents);

        // First-run setup — full screen, nothing else visible
        if let Some(setup) = &self.setup {
            return div()
                .id("app-root")
                .size_full()
                .bg(t::bg_base())
                .child(setup.clone())
                .into_any_element();
        }

        let show_review = self.right_dock.read(cx).is_visible();
        let show_settings = self.settings.is_some();

        let dock_is_active = self.right_dock.read(cx).visible;

        div()
            .id("app-root")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(t::bg_base())
            .flex()
            .flex_col()
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _window, cx| {
                let has_dialog = this.dialog.is_some() || this.ports_dialog.is_some() || this.settings.is_some();
                let cmd = !has_dialog && event.modifiers.platform;
                let ctrl = !has_dialog && event.modifiers.control;
                // Always update when a dialog is open to clear stale badges
                if this.cmd_held != cmd || this.ctrl_held != ctrl || has_dialog {
                    this.cmd_held = cmd;
                    this.ctrl_held = ctrl;
                    this.sidebar.update(cx, |view, cx| view.set_show_badges(cmd, cx));
                    this.terminal.update(cx, |panel, cx| {
                        panel.show_tab_badges = ctrl;
                        cx.notify();
                    });
                }
            }))
            .on_action(cx.listener(|this, _: &NewWorkspaceAction, window, cx| {
                this.open_new_workspace_dialog(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSettingsAction, window, cx| {
                this.open_settings(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRightDock, _, cx| {
                this.right_dock.update(cx, |dock, cx| {
                    dock.toggle_collapsed();
                    cx.notify();
                });
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleLeftSidebar, _, cx| {
                this.sidebar_collapsed = !this.sidebar_collapsed;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenPortsDialog, window, cx| {
                let ws_id = match this.terminal.read(cx).active_workspace_id {
                    Some(id) => id,
                    None => return,
                };
                let sb_info = this.terminal.read(cx).get_active_sandbox(ws_id, cx);
                let (sb, th) = match sb_info {
                    Some((sb, th)) => (Some(sb), th),
                    None => return,
                };
                this.open_ports_dialog(ws_id, sb, th, window, cx);
            }))
            // Custom titlebar — empty area is the OS drag region (appears_transparent).
            // Only interactive children (with on_click) eat mouse events.
            .child(
                div()
                    .id("titlebar")
                    .h(px(36.0))
                    .flex_shrink_0()
                    .w_full()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(t::border_subtle())
                    .on_click(|event, window, _cx| {
                        if event.click_count() == 2 {
                            window.titlebar_double_click();
                        }
                    })
                    // Left: traffic lights spacer + sidebar toggle
                    .child(div().w(px(72.0)).flex_shrink_0())
                    .child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .mt(px(1.0))
                            .child(
                                div()
                                    .id("toggle-left-sidebar")
                                    .p(px(5.0))
                                    .rounded(px(4.0))
                                    .cursor_pointer()
                                    .hover(|s: StyleRefinement| s.bg(t::bg_hover()))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sidebar_collapsed = !this.sidebar_collapsed;
                                        cx.notify();
                                    }))
                                    .child(
                                        svg()
                                            .path(SharedString::from("icons/sidebar-left.svg"))
                                            .size(px(14.0))
                                            .text_color(if self.sidebar_collapsed {
                                                t::text_ghost()
                                            } else {
                                                t::text_secondary()
                                            }),
                                    ),
                            )
                            .child({
                                let this = cx.entity().downgrade();
                                let on_manage_devices: ui::remote::ManageDevicesCallback = {
                                    let this = this.clone();
                                    Arc::new(move |window, cx| {
                                        let _ = this.update(cx, |app: &mut Self, cx| {
                                            app.open_settings_to_tab(
                                                Some(ui::settings::SettingsTab::RemoteControl),
                                                window,
                                                cx,
                                            );
                                        });
                                    })
                                };
                                let on_toggle_enabled: ui::remote::ToggleEnabledCallback = {
                                    let this = this.clone();
                                    Arc::new(move |enabled, cx| {
                                        let _ = this.update(cx, |app: &mut Self, cx| {
                                            app.set_remote_control_enabled(enabled, cx);
                                        });
                                    })
                                };
                                ui::remote::render_titlebar_button(
                                    &self.remote_access,
                                    &self.db,
                                    &self.remote_popover_state,
                                    &self.toast,
                                    on_manage_devices,
                                    on_toggle_enabled,
                                    cx,
                                )
                            }),
                    )
                    // Center: title
                    .child(
                        div()
                            .flex_grow()
                            .flex()
                            .justify_center()
                            .mt(px(1.0))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(t::text_ghost())
                                    .child("superhq"),
                            ),
                    )
                    // Right: layout icons
                    .child(
                        div()
                            .w(px(78.0))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .pr_1()
                            .gap(px(2.0))
                            .mt(px(1.0))
                            // Dock toggle
                            .when(dock_is_active, |el| {
                                el.child(
                                    div()
                                        .id("toggle-right-dock")
                                        .p(px(5.0))
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .hover(|s: StyleRefinement| s.bg(t::bg_hover()))
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.right_dock.update(cx, |dock, cx| {
                                                dock.toggle_collapsed();
                                                cx.notify();
                                            });
                                            cx.notify();
                                        }))
                                        .child(
                                            svg()
                                                .path(SharedString::from("icons/sidebar-right.svg"))
                                                .size(px(14.0))
                                                .text_color(if show_review {
                                                    t::text_secondary()
                                                } else {
                                                    t::text_ghost()
                                                }),
                                        ),
                                )
                            })
                            // Settings
                            .child(
                                div()
                                    .id("titlebar-settings")
                                    .p(px(5.0))
                                    .rounded(px(4.0))
                                    .cursor_pointer()
                                    .hover(|s: StyleRefinement| s.bg(t::bg_hover()))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if this.settings.is_some() {
                                            this.close_settings(window, cx);
                                        } else {
                                            this.open_settings(window, cx);
                                        }
                                    }))
                                    .child(
                                        svg()
                                            .path(SharedString::from("icons/settings.svg"))
                                            .size(px(14.0))
                                            .text_color(if show_settings {
                                                t::text_secondary()
                                            } else {
                                                t::text_ghost()
                                            }),
                                    ),
                            ),
                    ),
            )
            .child({
                let dock_size = self.right_dock.read(cx).size();
                div()
                    .id("workspace-layout")
                    .w_full()
                    .flex_grow()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .on_drag_move::<PanelResize>({
                        let entity = cx.entity().downgrade();
                        let right_dock = self.right_dock.clone();
                        move |event, _window, cx| {
                            let resize = event.drag(cx).clone();
                            let x = event.event.position.x;
                            let bounds = event.bounds;
                            let _ = entity.update(cx, |this, cx| {
                                match resize {
                                    PanelResize::Sidebar => {
                                        this.sidebar_size = (x - bounds.left())
                                            .max(px(180.0))
                                            .min(px(400.0));
                                    }
                                    PanelResize::RightDock => {
                                        let size = (bounds.right() - x)
                                            .max(px(260.0))
                                            .min(px(500.0));
                                        right_dock.update(cx, |dock, _| dock.set_size(size));
                                    }
                                }
                                cx.notify();
                            });
                        }
                    })
                    // Sidebar (collapsible)
                    .when(!self.sidebar_collapsed, |el| el.child(
                        div()
                            .id("sidebar-container")
                            .h_full()
                            .w(self.sidebar_size)
                            .flex_shrink_0()
                            .bg(t::bg_surface())
                            .flex()
                            .flex_col()
                            .child(
                                div().flex_grow().min_h_0().child(self.sidebar.clone()),
                            ),
                    )
                    // Sidebar resize handle
                    .child(
                        div()
                            .id("sidebar-resize")
                            .w(px(6.0))
                            .ml(px(-3.0))
                            .h_full()
                            .flex_shrink_0()
                            .cursor(CursorStyle::ResizeLeftRight)
                            .flex()
                            .justify_center()
                            .on_drag(PanelResize::Sidebar, |_, _, _, cx| {
                                cx.new(|_| ResizeDragView)
                            })
                            .child(
                                div()
                                    .w(px(1.0))
                                    .h_full()
                                    .bg(t::border()),
                            ),
                    ))
                    // Center terminal
                    .child(
                        div()
                            .flex_grow()
                            .min_w_0()
                            .h_full()
                            .bg(t::bg_base())
                            .child(self.terminal.clone()),
                    )
                    // Right dock resize handle + panel (when expanded)
                    .when(show_review, |el| {
                        el.child(
                            div()
                                .id("dock-resize")
                                .w(px(6.0))
                                .mr(px(-3.0))
                                .h_full()
                                .flex_shrink_0()
                                .cursor(CursorStyle::ResizeLeftRight)
                                .flex()
                                .justify_center()
                                .on_drag(PanelResize::RightDock, |_, _, _, cx| {
                                    cx.new(|_| ResizeDragView)
                                })
                                .child(
                                    div()
                                        .w(px(1.0))
                                        .h_full()
                                        .bg(t::border()),
                                ),
                        )
                        .child(
                            div()
                                .w(dock_size)
                                .h_full()
                                .flex_shrink_0()
                                .bg(t::bg_surface())
                                .child(self.right_dock.clone()),
                        )
                    })
            })
            .children(self.settings.as_ref().map(|s| s.clone()))
            .children(self.dialog.as_ref().map(|d| d.clone()))
            .children(self.ports_dialog.as_ref().map(|d| d.clone()))
            .children(self.render_pairing_modal(cx))
            .child(self.toast.clone())
            .into_any_element()
    }
}

impl AppView {
    fn render_pairing_modal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use ui::theme as t;
        let pending = self.pending_pairing.as_ref()?;
        let label: SharedString = pending.device_label.clone().into();
        Some(
            div()
                .id("pairing-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000088))
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| {
                    // Clicking the backdrop is a soft cancel = reject.
                    cx.stop_propagation();
                }))
                .child(
                    div()
                        .w(px(400.0))
                        .bg(t::bg_surface())
                        .border_1()
                        .border_color(t::border())
                        .rounded(px(10.0))
                        .shadow_lg()
                        .p_5()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t::text_secondary())
                                .child("Allow this device to connect?"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(t::text_ghost())
                                .child(
                                    "A remote client is requesting pairing with this host. \
                                     Approve only if you initiated it on a device you control.",
                                ),
                        )
                        .child(
                            div()
                                .mt_1()
                                .px_3()
                                .py_2()
                                .rounded(px(6.0))
                                .bg(t::bg_elevated())
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(t::text_ghost())
                                        .child("Device label"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_family("monospace")
                                        .text_color(t::text_secondary())
                                        .child(label),
                                ),
                        )
                        .child(
                            div()
                                .mt_2()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    div()
                                        .id("pairing-reject")
                                        .px_3()
                                        .py_1p5()
                                        .rounded(px(6.0))
                                        .text_xs()
                                        .text_color(t::text_secondary())
                                        .bg(t::bg_elevated())
                                        .cursor_pointer()
                                        .hover(|s: StyleRefinement| s.bg(t::bg_hover()))
                                        .on_click(cx.listener(|app, _, _, cx| {
                                            app.resolve_pairing(false, cx);
                                        }))
                                        .child("Reject"),
                                )
                                .child(
                                    div()
                                        .id("pairing-approve")
                                        .px_3()
                                        .py_1p5()
                                        .rounded(px(6.0))
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(0xffffff))
                                        .bg(t::accent())
                                        .cursor_pointer()
                                        .hover(|s: StyleRefinement| s.opacity(0.9))
                                        .on_click(cx.listener(|app, _, _, cx| {
                                            app.resolve_pairing(true, cx);
                                        }))
                                        .child("Approve"),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}

fn main() -> Result<()> {
    let db = Arc::new(Database::open()?);

    let app = Application::new().with_assets(assets::Assets);

    app.run(move |cx| {
        // Load saved theme before any UI renders.
        let saved_theme = db
            .get_settings()
            .ok()
            .map(|s| s.theme)
            .unwrap_or_else(|| "superhq-dark".into());
        let resolved = ui::theme::resolve_theme_id(&saved_theme, cx.window_appearance());
        ui::theme::load_theme(resolved);
        ui::review::load_syntax_theme(resolved);

        ui::components::actions::bind_keys(cx);
        ui::components::text_input::bind_keys(cx);

        // Disable Tab focus-cycling only inside the terminal, so Tab reaches
        // on_key_down for shell tab-completion. Root's Tab still works in dialogs/menus.
        cx.bind_keys([
            KeyBinding::new("tab", NoAction, Some("Terminal")),
            KeyBinding::new("shift-tab", NoAction, Some("Terminal")),
        ]);

        // Our shortcuts
        cx.bind_keys([
            KeyBinding::new("cmd-n", NewWorkspaceAction, None),
            KeyBinding::new("cmd-,", OpenSettingsAction, None),
            KeyBinding::new("cmd-b", ToggleRightDock, None),
            KeyBinding::new("cmd-shift-b", ToggleLeftSidebar, None),
            KeyBinding::new("cmd-shift-p", OpenPortsDialog, None),
            // Tab navigation
            KeyBinding::new("cmd-w", ui::terminal::CloseActiveTab, Some("Terminal")),
            // Workspace switching: cmd+1..9
            KeyBinding::new("cmd-1", ActivateWorkspace1, None),
            KeyBinding::new("cmd-2", ActivateWorkspace2, None),
            KeyBinding::new("cmd-3", ActivateWorkspace3, None),
            KeyBinding::new("cmd-4", ActivateWorkspace4, None),
            KeyBinding::new("cmd-5", ActivateWorkspace5, None),
            KeyBinding::new("cmd-6", ActivateWorkspace6, None),
            KeyBinding::new("cmd-7", ActivateWorkspace7, None),
            KeyBinding::new("cmd-8", ActivateWorkspace8, None),
            KeyBinding::new("cmd-9", ActivateWorkspace9, None),
            // Tab switching: ctrl+1..9
            KeyBinding::new("ctrl-1", ActivateTab1, None),
            KeyBinding::new("ctrl-2", ActivateTab2, None),
            KeyBinding::new("ctrl-3", ActivateTab3, None),
            KeyBinding::new("ctrl-4", ActivateTab4, None),
            KeyBinding::new("ctrl-5", ActivateTab5, None),
            KeyBinding::new("ctrl-6", ActivateTab6, None),
            KeyBinding::new("ctrl-7", ActivateTab7, None),
            KeyBinding::new("ctrl-8", ActivateTab8, None),
            KeyBinding::new("ctrl-9", ActivateTab9, None),
        ]);


        let db = db.clone();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1400.0), px(900.0)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("superhq".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(8.0), px(10.0))),
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| AppView::new(db.clone(), cx));

                // Ensure the app root has focus so keystrokes work before a terminal is opened
                view.read(cx).focus_handle.focus(window);

                // Follow system light/dark when theme is set to "auto".
                let db_for_appearance = db.clone();
                window
                    .observe_window_appearance(move |window, cx| {
                        let saved = db_for_appearance
                            .get_settings()
                            .ok()
                            .map(|s| s.theme)
                            .unwrap_or_default();
                        if saved != ui::theme::AUTO_THEME {
                            return;
                        }
                        let resolved =
                            ui::theme::resolve_theme_id(&saved, window.appearance());
                        if ui::theme::load_theme(resolved) {
                            ui::review::load_syntax_theme(resolved);
                            cx.refresh_windows();
                        }
                    })
                    .detach();

                // Global keystroke interceptor — fires before all element handlers
                let sidebar = view.read(cx).sidebar.clone();
                let terminal = view.read(cx).terminal.clone();
                let toast = view.read(cx).toast.clone();
                let app_view = view.clone();
                let sub = cx.intercept_keystrokes({
                    let sidebar = sidebar.clone();
                    let terminal = terminal.clone();
                    let toast = toast.clone();
                    move |event, window, cx| {
                        let show_workspace_toast = |cx: &mut App| {
                            if let Some(name) = terminal.read(cx).active_workspace_name(cx) {
                                toast.update(cx, |t, cx| t.show(format!("Switched to {name}"), cx));
                            }
                        };
                        let key = event.keystroke.key.as_str();
                        let m = &event.keystroke.modifiers;
                        // cmd+1..9 → switch workspace
                        if m.platform && !m.control && !m.alt && !m.shift {
                            if let Some(n) = match key {
                                "1" => Some(0), "2" => Some(1), "3" => Some(2),
                                "4" => Some(3), "5" => Some(4), "6" => Some(5),
                                "7" => Some(6), "8" => Some(7), "9" => Some(8),
                                _ => None,
                            } {
                                let prev = terminal.read(cx).active_workspace_id;
                                sidebar.update(cx, |v, cx| {
                                    v.activate_by_index(n, window, cx);
                                });
                                if terminal.read(cx).active_workspace_id != prev {
                                    show_workspace_toast(cx);
                                }
                                cx.stop_propagation();
                            }
                        }
                        // ctrl+1..9 → switch tab
                        if m.control && !m.platform && !m.alt && !m.shift {
                            if let Some(n) = match key {
                                "1" => Some(0), "2" => Some(1), "3" => Some(2),
                                "4" => Some(3), "5" => Some(4), "6" => Some(5),
                                "7" => Some(6), "8" => Some(7), "9" => Some(8),
                                _ => None,
                            } {
                                terminal.update(cx, |p, cx| p.activate_tab_by_index(n, window, cx));
                                cx.stop_propagation();
                            }
                        }
                        // cmd+w → close active tab
                        if m.platform && !m.shift && !m.control && !m.alt && key == "w" {
                            terminal.update(cx, |p, cx| p.close_active_tab(cx));
                            cx.stop_propagation();
                        }
                        // cmd+t → toggle agent picker
                        if m.platform && !m.shift && !m.control && !m.alt && key == "t" {
                            terminal.update(cx, |p, cx| {
                                p.show_agent_menu = !p.show_agent_menu;
                                p.agent_menu_index = 0;
                                cx.notify();
                            });
                            if terminal.read(cx).show_agent_menu {
                                terminal.read(cx).agent_menu_focus.focus(window);
                            }
                            cx.stop_propagation();
                        }
                        // cmd+shift+] → next tab, cmd+shift+[ → prev tab
                        // macOS reports key as "{" / "}" with shift=false
                        if m.platform && !m.control && !m.alt {
                            match key {
                                "}" => {
                                    terminal.update(cx, |p, cx| p.next_tab(window, cx));
                                    cx.stop_propagation();
                                }
                                "{" => {
                                    terminal.update(cx, |p, cx| p.prev_tab(window, cx));
                                    cx.stop_propagation();
                                }
                                _ => {}
                            }
                        }
                        // ctrl+cmd+] → next workspace, ctrl+cmd+[ → prev workspace
                        if m.platform && m.control && !m.alt {
                            match key {
                                "}" | "]" => {
                                    let prev = terminal.read(cx).active_workspace_id;
                                    sidebar.update(cx, |v, cx| v.next_workspace(window, cx));
                                    if terminal.read(cx).active_workspace_id != prev {
                                        show_workspace_toast(cx);
                                    }
                                    cx.stop_propagation();
                                }
                                "{" | "[" => {
                                    let prev = terminal.read(cx).active_workspace_id;
                                    sidebar.update(cx, |v, cx| v.prev_workspace(window, cx));
                                    if terminal.read(cx).active_workspace_id != prev {
                                        show_workspace_toast(cx);
                                    }
                                    cx.stop_propagation();
                                }
                                _ => {}
                            }
                        }
                        // cmd+n → new workspace
                        if m.platform && !m.shift && !m.control && !m.alt && key == "n" {
                            app_view.update(cx, |app, cx| {
                                app.open_new_workspace_dialog(window, cx);
                            });
                            cx.stop_propagation();
                        }
                        // cmd+, → settings
                        if m.platform && !m.shift && !m.control && !m.alt && key == "," {
                            app_view.update(cx, |app, cx| {
                                if app.settings.is_some() {
                                    app.close_settings(window, cx);
                                } else {
                                    app.open_settings(window, cx);
                                }
                            });
                            cx.stop_propagation();
                        }
                    }
                });
                // Store subscription in AppView so it stays alive
                view.update(cx, |app, _| { app._keystroke_sub = Some(sub); });

                view
            },
        )
        .expect("Failed to open window");
    });

    Ok(())
}
