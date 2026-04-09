use gpui::*;
use gpui::prelude::FluentBuilder as _;
use gpui_component::input::{Input, InputState};
use gpui_component::{Sizable as _};

use crate::db::{CreateWorkspaceParams, Database};
use crate::ui::theme as t;
use std::sync::Arc;

pub struct NewWorkspaceDialog {
    db: Arc<Database>,
    name_input: Entity<InputState>,
    mount_path: Option<String>,
    on_created: Box<dyn Fn(&mut Window, &mut App) + 'static>,
    on_dismiss: Box<dyn Fn(&mut Window, &mut App) + 'static>,
    focus_handle: FocusHandle,
}

impl NewWorkspaceDialog {
    pub fn new(
        db: Arc<Database>,
        on_created: impl Fn(&mut Window, &mut App) + 'static,
        on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Workspace name", window, cx);
            state.focus(window, cx);
            state
        });
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        Self {
            db,
            name_input,
            mount_path: None,
            on_created: Box::new(on_created),
            on_dismiss: Box::new(on_dismiss),
            focus_handle,
        }
    }

    fn browse_folder(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select folder to mount".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.first() {
                    let path_str = path.to_string_lossy().to_string();
                    cx.update(|cx| {
                        this.update(cx, |this, cx| {
                            this.mount_path = Some(path_str);
                            cx.notify();
                        })
                        .ok();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn submit(&self, window: &mut Window, cx: &mut App) {
        let name = self.name_input.read(cx).value().to_string();
        if name.is_empty() {
            return;
        }

        let mount_path = self.mount_path.clone();
        let is_git_repo = mount_path
            .as_ref()
            .map_or(false, |p| std::path::Path::new(p).join(".git").exists());

        let settings = self.db.get_settings().ok();
        let _id = self.db.create_workspace(CreateWorkspaceParams {
            name,
            mount_path,
            mount_read_only: true,
            is_git_repo,
            branch_name: None,
            base_branch: None,
            initial_prompt: None,
            sandbox_cpus: settings.as_ref().map(|s| s.sandbox_cpus).unwrap_or(2),
            sandbox_memory_mb: settings.as_ref().map(|s| s.sandbox_memory_mb).unwrap_or(8192),
            sandbox_disk_mb: settings.as_ref().map(|s| s.sandbox_disk_mb).unwrap_or(16384),
            allowed_hosts: None,
            secrets_config: None,
            cloned_from_id: None,
        });

        (self.on_created)(window, cx);
    }

    fn dismiss(&self, window: &mut Window, cx: &mut App) {
        (self.on_dismiss)(window, cx);
    }
}

impl Render for NewWorkspaceDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mount_display = self
            .mount_path
            .as_ref()
            .map(|p| p.split('/').last().unwrap_or(p).to_string())
            .unwrap_or_else(|| "None (scratch sandbox)".to_string());
        let has_mount = self.mount_path.is_some();

        // Full-screen backdrop
        div()
            .id("dialog-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000088))
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                this.dismiss(window, cx);
            }))
            .child(
                div()
                    .id("dialog-card")
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            this.dismiss(window, cx);
                        }
                    }))
                    .w(px(360.0))
                    .bg(t::bg_surface())
                    .border_1()
                    .border_color(t::border())
                    .rounded(px(10.0))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    // Header
                    .child(
                        div()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(t::border_subtle())
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t::text_secondary())
                                    .child("New Workspace"),
                            ),
                    )
                    // Body
                    .child(
                        div()
                            .px_4()
                            .py_3()
                            .flex()
                            .flex_col()
                            .gap_3()
                            // Name field
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(t::text_dim())
                                            .child("Name"),
                                    )
                                    .child(Input::new(&self.name_input).small()),
                            )
                            // Mount folder field
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(t::text_dim())
                                            .child("Mount folder"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_grow()
                                                    .text_xs()
                                                    .text_color(if has_mount {
                                                        t::text_secondary()
                                                    } else {
                                                        t::text_ghost()
                                                    })
                                                    .child(mount_display),
                                            )
                                            .child(
                                                div()
                                                    .id("browse-btn")
                                                    .text_xs()
                                                    .text_color(t::text_dim())
                                                    .cursor_pointer()
                                                    .hover(|s| s.text_color(t::text_tertiary()))
                                                    .on_click(cx.listener(|this, _, _window, cx| {
                                                        this.browse_folder(cx);
                                                    }))
                                                    .child("Browse"),
                                            )
                                            .when(has_mount, |el| {
                                                el.child(
                                                    div()
                                                        .id("clear-mount")
                                                        .text_xs()
                                                        .text_color(t::text_ghost())
                                                        .cursor_pointer()
                                                        .hover(|s| s.text_color(t::text_dim()))
                                                        .on_click(cx.listener(|this, _, _window, cx| {
                                                            this.mount_path = None;
                                                            cx.notify();
                                                        }))
                                                        .child("Clear"),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(t::text_faint())
                                            .child("Leave empty for a scratch sandbox"),
                                    ),
                            ),
                    )
                    // Footer
                    .child(
                        div()
                            .px_4()
                            .py_3()
                            .border_t_1()
                            .border_color(t::border_subtle())
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("cancel-btn")
                                    .px_3()
                                    .py_1()
                                    .rounded(px(6.0))
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(t::text_dim())
                                    .hover(|s| s.bg(t::bg_hover()).text_color(t::text_tertiary()))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.dismiss(window, cx);
                                    }))
                                    .child("Cancel"),
                            )
                            .child(
                                div()
                                    .id("create-btn")
                                    .px_3()
                                    .py_1()
                                    .rounded(px(6.0))
                                    .cursor_pointer()
                                    .text_xs()
                                    .bg(t::bg_selected())
                                    .text_color(t::text_secondary())
                                    .hover(|s| s.bg(t::bg_hover()).text_color(t::text_primary()))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit(window, cx);
                                    }))
                                    .child("Create"),
                            ),
                    ),
            )
    }
}

