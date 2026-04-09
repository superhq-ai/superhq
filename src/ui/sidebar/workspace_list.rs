use crate::db::{Database, Workspace};
use crate::ui::review::SidePanel;
use crate::ui::terminal::TerminalPanel;
use crate::ui::theme as t;
use gpui::*;
use gpui::prelude::FluentBuilder as _;
use std::sync::Arc;

use super::workspace_item::WorkspaceItemView;

/// The sidebar workspace list component.
pub struct WorkspaceListView {
    db: Arc<Database>,
    workspace_views: Vec<Entity<WorkspaceItemView>>,
    terminal_panel: Entity<TerminalPanel>,
    review_panel: Entity<SidePanel>,
    pub active_workspace_id: Option<i64>,
    pub cmd_held: bool,
    on_new_workspace: std::rc::Rc<dyn Fn(&mut Window, &mut App) + 'static>,
    on_workspace_activated: std::rc::Rc<dyn Fn(&mut App) + 'static>,
}

impl WorkspaceListView {
    pub fn new(
        db: Arc<Database>,
        terminal_panel: Entity<TerminalPanel>,
        review_panel: Entity<SidePanel>,
        on_new_workspace: impl Fn(&mut Window, &mut App) + 'static,
        on_workspace_activated: impl Fn(&mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspaces = db.list_workspaces().unwrap_or_default();
        let active_workspace_id = None;
        let on_new_workspace = std::rc::Rc::new(on_new_workspace);
        let on_workspace_activated = std::rc::Rc::new(on_workspace_activated);
        let workspace_views =
            Self::build_views(&db, &workspaces, &terminal_panel, &review_panel, active_workspace_id, cx);
        Self {
            db,
            workspace_views,
            terminal_panel,
            review_panel,
            active_workspace_id,
            cmd_held: false,
            on_new_workspace,
            on_workspace_activated,
        }
    }

    /// Activate workspace at the given 0-based index.
    pub fn activate_by_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let workspaces = self.db.list_workspaces().unwrap_or_default();
        if let Some(ws) = workspaces.get(index) {
            let ws_id = ws.id;
            if self.active_workspace_id == Some(ws_id) {
                return;
            }
            let ws = ws.clone();
            self.terminal_panel.update(cx, |panel, cx| {
                panel.activate_workspace(&ws, window, cx);
            });
            let review = self.review_panel.clone();
            review.update(cx, |panel, cx| {
                panel.show_waiting(ws.id, "/workspace".to_string(), ws.mount_path.clone(), cx);
            });
            self.terminal_panel.update(cx, |panel, cx| {
                panel.notify_side_panel_pub(ws_id, cx);
            });
            self.active_workspace_id = Some(ws_id);
            (self.on_workspace_activated)(cx);
            self.refresh(cx);
        }
    }

    pub fn set_show_badges(&mut self, show: bool, cx: &mut Context<Self>) {
        self.cmd_held = show;
        for (i, view) in self.workspace_views.iter().enumerate() {
            view.update(cx, |item, _| {
                item.badge_index = if show && i < 9 { Some(i + 1) } else { None };
            });
        }
        cx.notify();
    }

    pub fn clear_active(&mut self, cx: &mut Context<Self>) {
        self.active_workspace_id = None;
        self.refresh(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let workspaces = self.db.list_workspaces().unwrap_or_default();
        self.workspace_views = Self::build_views(
            &self.db,
            &workspaces,
            &self.terminal_panel,
            &self.review_panel,
            self.active_workspace_id,
            cx,
        );
        cx.notify();
    }

    fn build_views(
        db: &Arc<Database>,
        workspaces: &[Workspace],
        terminal_panel: &Entity<TerminalPanel>,
        review_panel: &Entity<SidePanel>,
        active_workspace_id: Option<i64>,
        cx: &mut Context<WorkspaceListView>,
    ) -> Vec<Entity<WorkspaceItemView>> {
        let this = cx.entity().downgrade();
        let on_refresh = std::rc::Rc::new({
            let this = this.clone();
            move |cx: &mut App| {
                this.update(cx, |view: &mut WorkspaceListView, cx| view.refresh(cx))
                    .ok();
            }
        });
        let on_activate = std::rc::Rc::new({
            let this = this.clone();
            move |id: i64, cx: &mut App| {
                this.update(cx, |view: &mut WorkspaceListView, cx| {
                    view.active_workspace_id = Some(id);
                    (view.on_workspace_activated)(cx);
                    view.refresh(cx);
                })
                .ok();
            }
        });

        workspaces
            .iter()
            .map(|ws| {
                let cloned_from_name = ws
                    .cloned_from_id
                    .and_then(|id| db.get_cloned_from_name(id).ok().flatten());
                let is_active = active_workspace_id == Some(ws.id);
                cx.new(|_| WorkspaceItemView::new(
                    ws.clone(),
                    cloned_from_name,
                    terminal_panel.clone(),
                    review_panel.clone(),
                    is_active,
                    db.clone(),
                    on_refresh.clone(),
                    on_activate.clone(),
                ))
            })
            .collect()
    }
}

impl Render for WorkspaceListView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let on_new = self.on_new_workspace.clone();
        div()
            .id("workspace-list")
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("new-workspace-btn")
                    .px_2p5()
                    .py_2()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(t::text_dim())
                    .hover(|s| s.bg(t::border_subtle()))
                    .on_click(move |_event, window, cx| {
                        on_new(window, cx);
                    })
                    .relative()
                    .child("+ New Workspace")
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
                                .child("\u{2318}N"),
                        )
                    }),
            )
            .child(div().h(px(1.0)).mx_2p5().bg(t::border_subtle()))
            .child(
                div()
                    .flex_grow()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .py_1()
                    .px(px(6.0))
                    .children(self.workspace_views.iter().map(|view| view.clone())),
            )
    }
}
