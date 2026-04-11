#[allow(dead_code)]
pub mod session;
mod boot;
mod lifecycle;
pub(crate) mod panel;
mod setup_view;
mod tab_bar;
mod workspace;

pub use panel::TerminalPanel;

use gpui::*;
use gpui::prelude::FluentBuilder as _;

use tab_bar::{DraggedTab, DraggedTabView};
use crate::ui::theme as t;

actions!(terminal_panel, [
    CloseActiveTab,
    NextTab,
    PrevTab,
]);

impl Render for TerminalPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active: Option<&Entity<session::WorkspaceSession>> = self
            .active_workspace_id
            .and_then(|id| self.sessions.get(&id));

        match active {
            Some(session) => {
                let s = session.read(cx);
                let active_tab_idx = s.active_tab;
                let ws_id = self.active_workspace_id.unwrap();
                let show_menu = self.show_agent_menu;
                let has_tabs = !s.tabs.is_empty();

                // Capture active tab's state for content rendering
                let active_tab_terminal = s.tabs.get(active_tab_idx)
                    .and_then(|t| t.terminal.clone());
                let active_tab_setup = s.tabs.get(active_tab_idx)
                    .filter(|t| t.is_setting_up())
                    .map(|t| (
                        t.setup_steps.clone().unwrap_or_default(),
                        t.setup_error.clone(),
                        t.agent_color,
                        t.icon_path.clone(),
                    ));
                let active_tab_stopped = s.tabs.get(active_tab_idx)
                    .filter(|t| t.is_stopped())
                    .map(|t| t.tab_id);
                let active_tab_checkpointing = s.tabs.get(active_tab_idx)
                    .filter(|t| t.checkpointing)
                    .is_some();

                let tab_scroll = s.tab_scroll.clone();

                // Snapshot tab data for rendering (avoids holding session borrow across closures)
                struct TabSnapshot {
                    is_active: bool,
                    is_stopped: bool,
                    icon_path: Option<SharedString>,
                    color: Option<Rgba>,
                    tab_id: u64,
                    display_label: SharedString,
                    agent_status: session::AgentStatus,
                }
                let tab_snapshots: Vec<TabSnapshot> = s.tabs.iter().enumerate().map(|(i, tab)| {
                    let is_setup = tab.is_setting_up();
                    let is_stopped = tab.is_stopped();
                    TabSnapshot {
                        is_active: i == active_tab_idx,
                        is_stopped,
                        icon_path: tab.icon_path.clone(),
                        color: if is_stopped { None } else { tab.agent_color },
                        tab_id: tab.tab_id,
                        display_label: if is_setup {
                            SharedString::from(format!("{} (initializing...)", tab.label))
                        } else {
                            tab.label.clone()
                        },
                        agent_status: tab.agent_status.clone(),
                    }
                }).collect();

                // Note: `s` (the read borrow) goes out of scope here before closures are built.

                // Build tab bar items
                let mut tab_elements: Vec<Stateful<Div>> = Vec::new();

                for (i, snap) in tab_snapshots.iter().enumerate() {
                    let is_active = snap.is_active;
                    let is_stopped = snap.is_stopped;
                    let tab_ws_id = ws_id;
                    let icon_path = snap.icon_path.clone();
                    let color = snap.color;
                    let close_tab_id = snap.tab_id;
                    let display_label = snap.display_label.clone();

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
                                if let Some(session) = this.sessions.get(&tab_ws_id) {
                                    session.update(cx, |s: &mut session::WorkspaceSession, cx: &mut gpui::Context<session::WorkspaceSession>| {
                                        s.activate_tab(i, cx);
                                    });
                                    let s = session.read(cx);
                                    if let Some(tab) = s.tabs.get(s.active_tab) {
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
                                if let Some(session) = this.sessions.get(&tab_ws_id) {
                                    session.update(cx, |s: &mut session::WorkspaceSession, _cx: &mut gpui::Context<session::WorkspaceSession>| {
                                        let from = dragged.tab_ix;
                                        let to = i;
                                        if from != to && from < s.tabs.len() && to < s.tabs.len() {
                                            let tab = s.tabs.remove(from);
                                            s.tabs.insert(to, tab);
                                            s.active_tab = to;
                                        }
                                    });
                                    cx.notify();
                                }
                            }))
                            .child(self.render_tab_icon(&icon_path, color, is_active))
                            .child(display_label)
                            .when_some({
                                match &snap.agent_status {
                                    session::AgentStatus::Running { .. } => Some(t::agent_running()),
                                    session::AgentStatus::NeedsInput { .. } => Some(t::agent_needs_input()),
                                    _ => None,
                                }
                            }, |el, dot_color| {
                                el.child(
                                    div()
                                        .size(px(6.0))
                                        .rounded_full()
                                        .flex_shrink_0()
                                        .bg(dot_color),
                                )
                            })
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
                    let running_agents = self.active_agent_tabs(cx);

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

                    let mut menu = t::popover()
                        .id("agent-menu-popup")
                        .track_focus(&self.agent_menu_focus)
                        .w(px(200.0))
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
                    .track_scroll(&tab_scroll)
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
                                let scroll = tab_scroll.clone();
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
                        this.close_active_tab(cx);
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
                        .and_then(|s| s.read(cx).find_tab(close_tab_id).map(|t| t.label.clone()))
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
                                t::button("Cancel")
                                    .id("close-cancel")
                                    .hover(|s| s.bg(t::bg_hover()).text_color(t::text_tertiary()))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.pending_close = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                t::button_primary("Checkpoint")
                                    .id("close-checkpoint")
                                    .hover(|s| s.bg(t::bg_hover()).text_color(t::text_primary()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.checkpoint_tab(close_ws_id, close_tab_id, cx);
                                    })),
                            )
                            .child(
                                t::button_danger("Close")
                                    .id("close-confirm")
                                    .hover(|s| s.bg(t::error_bg()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.force_close_tab(close_ws_id, close_tab_id, cx);
                                    })),
                            ),
                    );
                }

                // Missing secrets prompt bar — only show on the affected tab
                let active_tab_id = session.read(cx).active_tab_ref().map(|t| t.tab_id);
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
                                t::button("Skip")
                                    .id("secrets-skip")
                                    .hover(|s| s.bg(t::bg_hover()).text_color(t::text_tertiary()))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.dismiss_missing_secrets(cx);
                                    })),
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
                } else if active_tab_checkpointing {
                    content = content.child(
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(t::text_ghost())
                                    .child("Saving checkpoint\u{2026}"),
                            ),
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
                                    t::button_primary("Fork & Continue")
                                        .id("fork-tab")
                                        .hover(|s| s.bg(t::bg_hover()).text_color(t::text_primary()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.fork_tab(fork_ws, fork_tab, cx);
                                        })),
                                )
                                .child(
                                    t::button("Remove")
                                        .id("remove-tab")
                                        .hover(|s| s.bg(t::bg_hover()).text_color(t::text_tertiary()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.remove_stopped_tab(remove_ws, remove_tab, cx);
                                        })),
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
                                        let (sb, th) = this.get_active_sandbox(ws_id, cx)
                                            .map(|(sb, th)| (Some(sb), th))
                                            .unwrap_or_else(|| (None, this.tokio_handle.clone()));
                                        cb(ws_id, sb, th, window, cx);
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
