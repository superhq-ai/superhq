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
use gpui::prelude::FluentBuilder as _;
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::{Root, Theme, ThemeMode};
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
    ]
);
use ui::components::Toast;
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
    review: Entity<SidePanel>,
    toast: Entity<Toast>,
    dialog: Option<Entity<NewWorkspaceDialog>>,
    ports_dialog: Option<Entity<PortsDialog>>,
    settings: Option<Entity<SettingsPanel>>,
    setup: Option<Entity<SetupScreen>>,
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
            panel.set_on_open_port_dialog(move |ws_id, window, cx| {
                let _ = this_for_ports.update(cx, |this: &mut Self, cx| {
                    this.open_ports_dialog(ws_id, window, cx);
                });
            });
            panel
        });
        let review = cx.new(|_| SidePanel::new());
        // Wire review panel into terminal so it gets sandbox-ready notifications
        terminal.update(cx, |panel, _| {
            panel.set_side_panel(review.clone());
        });
        let this = cx.entity().downgrade();
        let this2 = cx.entity().downgrade();
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
                move |cx| {
                    this2.update(cx, |app, cx| {
                        app.settings = None;
                        cx.notify();
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
            review,
            toast,
            dialog: None,
            ports_dialog: None,
            settings: None,
            setup,
            cmd_held: false,
            ctrl_held: false,
            focus_handle: cx.focus_handle(),
            _keystroke_sub: None,
        }
    }

    fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings = None;
        // Restore active workspace highlight in sidebar
        let active_ws = self.terminal.read(cx).active_workspace_id;
        if let Some(ws_id) = active_ws {
            self.sidebar.update(cx, |view, cx| {
                view.active_workspace_id = Some(ws_id);
                view.refresh(cx);
            });
        }
        cx.notify();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            return;
        }
        self.sidebar.update(cx, |view, cx| view.clear_active(cx));
        let this = cx.entity().downgrade();
        let terminal = self.terminal.clone();
        let db = self.db.clone();
        let toast = self.toast.clone();
        let view = cx.new(|cx| {
            SettingsPanel::new(
                db,
                toast,
                move |_window, cx| {
                    this.update(cx, |app, cx| {
                        app.close_settings(cx);
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

    fn open_ports_dialog(&mut self, ws_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let db = self.db.clone();
        let this = cx.entity().downgrade();

        let view = cx.new(|cx| {
            PortsDialog::new(
                db,
                ws_id,
                move |_window, cx| {
                    this.update(cx, |app, cx| {
                        app.ports_dialog = None;
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

    fn open_new_workspace_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let db = self.db.clone();
        let sidebar = self.sidebar.clone();
        let this = cx.entity().downgrade();
        let this2 = cx.entity().downgrade();

        let view = cx.new(|cx| {
            NewWorkspaceDialog::new(
                db,
                move |_window, cx| {
                    sidebar.update(cx, |view: &mut WorkspaceListView, cx| view.refresh(cx));
                    this.update(cx, |app, cx| {
                        app.dialog = None;
                        cx.notify();
                    }).ok();
                },
                move |_window, cx| {
                    this2.update(cx, |app, cx| {
                        app.dialog = None;
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        let show_review = self.review.read(cx).visible;
        let show_settings = self.settings.is_some();

        div()
            .id("app-root")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(t::bg_base())
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _window, cx| {
                let cmd = event.modifiers.platform;
                let ctrl = event.modifiers.control;
                if this.cmd_held != cmd || this.ctrl_held != ctrl {
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
            .child(
                h_resizable("outer-layout")
                    .child(
                        resizable_panel()
                            .size(px(240.0))
                            .size_range(px(180.0)..px(400.0))
                            .child(
                                div()
                                    .id("sidebar-container")
                                    .size_full()
                                    .bg(t::bg_surface())
                                    .border_r_1()
                                    .border_color(t::border_strong())
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div().flex_grow().child(self.sidebar.clone()),
                                    )
                                    // Gear button at bottom of sidebar
                                    .child(
                                        div()
                                            .border_t_1()
                                            .border_color(t::border_subtle())
                                            .child(
                                                div()
                                                    .id("settings-btn")
                                                    .px_2p5()
                                                    .py_2()
                                                    .cursor_pointer()
                                                    .text_xs()
                                                    .text_color(if show_settings {
                                                        t::text_tertiary()
                                                    } else {
                                                        t::text_dim()
                                                    })
                                                    .when(show_settings, |el: Stateful<Div>| {
                                                        el.bg(t::bg_selected())
                                                    })
                                                    .hover(|s: StyleRefinement| {
                                                        s.bg(t::border_subtle())
                                                            .text_color(t::text_tertiary())
                                                    })
                                                    .on_click(
                                                        cx.listener(|this, _, window, cx| {
                                                            if this.settings.is_some() {
                                                                this.close_settings(cx);
                                                            } else {
                                                                this.open_settings(window, cx);
                                                            }
                                                        }),
                                                    )
                                                    .relative()
                                                    .child("Settings")
                                                    .when(self.cmd_held, |el: Stateful<Div>| {
                                                        el.child(
                                                            div()
                                                                .absolute()
                                                                .right(px(8.0))
                                                                .top(px(6.0))
                                                                .px(px(5.0))
                                                                .py(px(1.0))
                                                                .rounded(px(4.0))
                                                                .bg(t::bg_selected())
                                                                .text_xs()
                                                                .text_color(t::text_muted())
                                                                .child("\u{2318},"),
                                                        )
                                                    }),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        h_resizable("inner-layout")
                            .child(
                                resizable_panel().child(
                                    div()
                                        .size_full()
                                        .bg(t::bg_base())
                                        .child(self.terminal.clone()),
                                ),
                            )
                            .child(
                                resizable_panel()
                                    .visible(show_review)
                                    .size(px(340.0))
                                    .size_range(px(260.0)..px(500.0))
                                    .child(
                                        div()
                                            .size_full()
                                            .bg(t::bg_surface())
                                            .border_l_1()
                                            .border_color(t::border_strong())
                                            .child(self.review.clone()),
                                    ),
                            ),
                    ),
            )
            .children(self.settings.as_ref().map(|s| s.clone()))
            .children(self.dialog.as_ref().map(|d| d.clone()))
            .children(self.ports_dialog.as_ref().map(|d| d.clone()))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_sheet_layer(window, cx))
            .child(self.toast.clone())
            .into_any_element()
    }
}

fn main() -> Result<()> {
    let db = Arc::new(Database::open()?);

    let app = Application::new().with_assets(assets::Assets);

    app.run(move |cx| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);

        // Override gpui_component theme colors to match our dark palette
        {
            let theme = cx.global_mut::<Theme>();
            // Popover/menu colors — match our bg_surface (#1a1a1a) instead of #0a0a0a
            theme.popover = gpui::hsla(0.0, 0.0, 0.11, 1.0);         // ~#1c1c1c
            theme.popover_foreground = gpui::hsla(0.0, 0.0, 0.78, 1.0); // ~#c7c7c7
            theme.border = gpui::hsla(0.0, 0.0, 0.16, 1.0);          // ~#292929
            theme.accent_foreground = gpui::hsla(0.0, 0.0, 0.78, 1.0);
        }

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
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| AppView::new(db, cx));

                // Ensure the app root has focus so keystrokes work before a terminal is opened
                view.read(cx).focus_handle.focus(window);

                // Global keystroke interceptor — fires before all element handlers
                let sidebar = view.read(cx).sidebar.clone();
                let terminal = view.read(cx).terminal.clone();
                let app_view = view.clone();
                let sub = cx.intercept_keystrokes({
                    let sidebar = sidebar.clone();
                    let terminal = terminal.clone();
                    move |event, window, cx| {
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
                                sidebar.update(cx, |v, cx| v.activate_by_index(n, window, cx));
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
                        // ctrl+cmd+] → next workspace, ctrl+cmd+[ → prev workspace
                        if m.platform && m.control && !m.alt && !m.shift {
                            match key {
                                "]" => {
                                    sidebar.update(cx, |v, cx| v.next_workspace(window, cx));
                                    cx.stop_propagation();
                                }
                                "[" => {
                                    sidebar.update(cx, |v, cx| v.prev_workspace(window, cx));
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
                                    app.close_settings(cx);
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

                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("Failed to open window");
    });

    Ok(())
}
