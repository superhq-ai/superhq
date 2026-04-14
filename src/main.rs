mod agents;
mod assets;
mod db;
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
        }
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
        if self.settings.is_some() {
            return;
        }
        self.clear_badges(cx);
        self.sidebar.update(cx, |view, cx| view.clear_active(cx));
        let this = cx.entity().downgrade();
        let terminal = self.terminal.clone();
        let db = self.db.clone();
        let toast = self.toast.clone();
        let view = cx.new(|cx| {
            SettingsPanel::new(
                db,
                toast,
                move |window, cx| {
                    this.update(cx, |app, cx| {
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
            )
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
                            ),
                    )
                    // Center: title
                    .child(
                        div()
                            .flex_grow()
                            .flex()
                            .justify_center()
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
            .child(self.toast.clone())
            .into_any_element()
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
                    traffic_light_position: Some(point(px(8.0), px(8.0))),
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
