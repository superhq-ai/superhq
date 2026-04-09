use gpui::*;

use super::theme as t;
use crate::runtime;

/// First-run setup phase.
#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Welcome,
    Installing,
}

#[derive(Debug, Clone, PartialEq)]
enum StepStatus {
    Done,
    Active,
    Pending,
    Failed,
}

struct Step {
    label: String,
    status: StepStatus,
}

/// Messages from the download thread.
enum DownloadMsg {
    Progress(u64, Option<u64>),
    Extracting,
    Done(Result<(), String>),
}

/// Emitted when setup completes successfully.
pub struct SetupComplete;

impl EventEmitter<SetupComplete> for SetupScreen {}

/// Full-screen first-run setup overlay.
pub struct SetupScreen {
    phase: Phase,
    steps: Vec<Step>,
    error: Option<String>,
    completed: bool,
}

impl SetupScreen {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            phase: Phase::Welcome,
            steps: vec![
                Step { label: "Downloading Shuru Runtime".into(), status: StepStatus::Pending },
                Step { label: "Extracting image".into(), status: StepStatus::Pending },
                Step { label: "Verifying installation".into(), status: StepStatus::Pending },
            ],
            error: None,
            completed: false,
        }
    }

    fn start_setup(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.phase = Phase::Installing;
        self.error = None;
        self.steps[0].status = StepStatus::Active;
        self.steps[0].label = "Downloading Shuru Runtime (0 MB)".into();
        cx.notify();

        // Channel for download thread → UI thread communication
        let (tx, rx) = flume::unbounded::<DownloadMsg>();

        let tx_progress = tx.clone();
        let tx_extract = tx.clone();
        let tx_done = tx;

        // Spawn the download on a background thread
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(runtime::download_and_install(
                move |downloaded, total| {
                    let _ = tx_progress.send(DownloadMsg::Progress(downloaded, total));
                },
                move || {
                    let _ = tx_extract.send(DownloadMsg::Extracting);
                },
                move |result| {
                    let _ = tx_done.send(DownloadMsg::Done(
                        result.map_err(|e| e.to_string()),
                    ));
                },
            ));
        });

        // Poll the channel on the UI thread
        let this = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;

                let mut got_done = false;
                while let Ok(msg) = rx.try_recv() {
                    cx.update(|cx| {
                        this.update(cx, |s: &mut SetupScreen, cx| {
                            match msg {
                                DownloadMsg::Progress(downloaded, total) => {
                                    let dl_mb = downloaded / (1024 * 1024);
                                    s.steps[0].label = match total {
                                        Some(t) => {
                                            let total_mb = t / (1024 * 1024);
                                            format!("Downloading Shuru Runtime ({dl_mb} / {total_mb} MB)")
                                        }
                                        None => format!("Downloading Shuru Runtime ({dl_mb} MB)"),
                                    };
                                }
                                DownloadMsg::Extracting => {
                                    s.steps[0].status = StepStatus::Done;
                                    s.steps[1].status = StepStatus::Active;
                                }
                                DownloadMsg::Done(result) => {
                                    match result {
                                        Ok(()) => {
                                            s.steps[1].status = StepStatus::Done;
                                            s.steps[2].status = StepStatus::Done;
                                            s.steps[2].label = "Ready".into();
                                        }
                                        Err(e) => {
                                            for step in &mut s.steps {
                                                if step.status == StepStatus::Active {
                                                    step.status = StepStatus::Failed;
                                                    break;
                                                }
                                            }
                                            s.error = Some(e);
                                        }
                                    }
                                    got_done = true;
                                }
                            }
                            cx.notify();
                        }).ok();
                    }).ok();
                }

                if got_done {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(600))
                        .await;
                    let has_error = cx
                        .update(|cx| {
                            this.update(cx, |s: &mut SetupScreen, _| s.error.is_some())
                                .unwrap_or(true)
                        })
                        .unwrap_or(true);
                    if !has_error {
                        cx.update(|cx| {
                            this.update(cx, |s: &mut SetupScreen, cx| {
                                s.completed = true;
                                cx.notify();
                            }).ok();
                        }).ok();
                    }
                    break;
                }
            }
        })
        .detach();
    }

    fn render_icon(&self, size: f32) -> impl IntoElement {
        img(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/app-icon-128.png"
        )))
        .w(px(size))
        .h(px(size))
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(32.0))
            .child(self.render_icon(80.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(20.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t::text_primary())
                            .child("Welcome to SuperHQ"),
                    )
                    .child(
                        div()
                            .max_w(px(360.0))
                            .text_sm()
                            .text_color(t::text_dim())
                            .text_center()
                            .child(
                                "SuperHQ needs to download the sandbox runtime to run AI agents in isolated environments.",
                            ),
                    ),
            )
            .child(
                div()
                    .id("setup-btn")
                    .mt_2()
                    .px_3()
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .text_xs()
                    .text_color(t::text_dim())
                    .hover(|s| {
                        s.bg(gpui::rgba(0xffffff0d))
                            .text_color(t::text_secondary())
                    })
                    .active(|s| s.bg(gpui::rgba(0xffffff08)))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start_setup(window, cx);
                    }))
                    .child("Download & Set Up →"),
            )
    }

    fn render_installing(&self) -> impl IntoElement {
        let mut step_list = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .w(px(320.0));

        for (i, step) in self.steps.iter().enumerate() {
            let is_active = step.status == StepStatus::Active;

            let (indicator, indicator_color, label_color) = match step.status {
                StepStatus::Done => (
                    "\u{2713}",
                    t::status_green_dim(),
                    t::text_ghost(),
                ),
                StepStatus::Active => (
                    "\u{25CF}",
                    t::text_secondary(),
                    t::text_secondary(),
                ),
                StepStatus::Pending => (
                    "\u{25CB}",
                    t::text_invisible(),
                    t::text_invisible(),
                ),
                StepStatus::Failed => (
                    "\u{2717}",
                    t::error_text(),
                    t::error_text(),
                ),
            };

            let indicator_el = div()
                .w(px(14.0))
                .text_xs()
                .text_color(indicator_color)
                .flex()
                .justify_center()
                .child(indicator);

            let row = if is_active {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py(px(3.0))
                    .child(
                        indicator_el.with_animation(
                            SharedString::from(format!("setup-pulse-{i}")),
                            super::animation::breathing(2.0),
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
                    .py(px(3.0))
                    .child(indicator_el)
                    .child(
                        div()
                            .text_xs()
                            .text_color(label_color)
                            .child(step.label.clone()),
                    )
            };

            step_list = step_list.child(row);
        }

        let mut view = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(24.0))
            .child(self.render_icon(48.0))
            .child(step_list);

        if let Some(err) = &self.error {
            view = view.child(
                div()
                    .mt_4()
                    .max_w(px(400.0))
                    .px_3()
                    .py_2()
                    .rounded(px(6.0))
                    .bg(t::error_bg())
                    .border_1()
                    .border_color(t::error_border())
                    .text_xs()
                    .text_color(t::error_text())
                    .child(err.clone()),
            );
        }

        view
    }
}

impl Render for SetupScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.completed {
            self.completed = false;
            cx.emit(SetupComplete);
        }

        let content = match self.phase {
            Phase::Welcome => self.render_welcome(cx).into_any_element(),
            Phase::Installing => self.render_installing().into_any_element(),
        };

        div()
            .id("setup-screen")
            .size_full()
            .bg(t::bg_base())
            .flex()
            .items_center()
            .justify_center()
            .child(content)
    }
}
