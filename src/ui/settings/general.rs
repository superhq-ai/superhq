use gpui::*;
use super::SettingsPanel;
use super::card::*;
use crate::ui::components::select::{Select, SelectItem, SelectEvent};

impl SettingsPanel {
    pub(super) fn init_agent_dropdown(
        agents: &[crate::db::Agent],
        selected: Option<i64>,
        cx: &mut Context<Self>,
    ) -> Entity<Select> {
        let items: Vec<SelectItem> = agents
            .iter()
            .map(|a| SelectItem {
                id: a.id,
                label: a.display_name.clone(),
                icon: a.icon.clone(),
            })
            .collect();

        let state = cx.new(|cx| Select::new(items, selected, cx));

        cx.subscribe(&state, |this: &mut Self, _, event: &SelectEvent, cx| {
            let SelectEvent::Change(value) = event;
            this.default_agent_id = *value;
            if let Err(e) = this.db.update_default_agent(*value) {
                eprintln!("Failed to save default agent: {e}");
            }
            cx.notify();
        })
        .detach();

        state
    }

    pub(super) fn render_general_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let auto_launch = self.auto_launch_agent;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .child(section_header("General"))
            .child(settings_card(vec![
                settings_row(
                    "Default Agent",
                    "Agent to open for new workspaces",
                    self.agent_dropdown.clone(),
                )
                .into_any_element(),
                settings_row(
                    "Auto-launch agent",
                    "Automatically start the default agent when opening a workspace",
                    div()
                        .id("auto-launch-toggle")
                        .px_2()
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_xs()
                        .text_color(if auto_launch {
                            crate::ui::theme::text_secondary()
                        } else {
                            crate::ui::theme::text_ghost()
                        })
                        .hover(|s| s.bg(crate::ui::theme::bg_hover()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.auto_launch_agent = !this.auto_launch_agent;
                            let _ = this.db.update_auto_launch_agent(this.auto_launch_agent);
                            cx.notify();
                        }))
                        .child(if auto_launch { "On" } else { "Off" }),
                )
                .into_any_element(),
            ]))
    }
}
