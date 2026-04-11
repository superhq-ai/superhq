//! Agent lifecycle event service.
//!
//! Watches `/root/.superhq/events.jsonl` inside the sandbox for JSONL lines
//! written by agent hooks. Parses raw events, maps them to `AgentStatus`,
//! and sends status updates over a flume channel for the UI to consume.
//!
//! The UI layer only receives `AgentStatus` values — all event parsing and
//! mapping logic stays here in the service layer.

use crate::ui::terminal::session::AgentStatus;
use serde::Deserialize;
use shuru_sdk::AsyncSandbox;
use std::sync::Arc;
use tokio::sync::Notify;

/// Path inside the sandbox where hooks append events.
const EVENTS_DIR: &str = "/root/.superhq";
const EVENTS_FILE: &str = "/root/.superhq/events.jsonl";

/// Raw event from the JSONL file (internal to this module).
#[derive(Debug, Deserialize)]
struct RawEvent {
    event: String,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

impl RawEvent {
    /// Map a raw event to an AgentStatus.
    fn into_status(self) -> Option<AgentStatus> {
        match self.event.as_str() {
            "session_start" => Some(AgentStatus::Idle),
            "running" => Some(AgentStatus::Running { tool: self.tool }),
            "needs_input" => Some(AgentStatus::NeedsInput {
                message: self.message.or(self.title),
            }),
            "idle" | "stop" => Some(AgentStatus::Idle),
            "session_end" => Some(AgentStatus::Unknown),
            _ => None,
        }
    }
}

/// Agent lifecycle event service.
///
/// Watches the sandbox filesystem for hook events and emits `AgentStatus`
/// updates. Drop to stop the watcher thread.
pub struct AgentEventService {
    stop: Arc<Notify>,
    _handle: std::thread::JoinHandle<()>,
}

impl Drop for AgentEventService {
    fn drop(&mut self) {
        self.stop.notify_one();
    }
}

impl AgentEventService {
    /// Start the service. Returns the handle and a receiver for status updates.
    /// Returns None if the watch can't be opened.
    pub fn start(
        sandbox: Arc<AsyncSandbox>,
        tokio_handle: tokio::runtime::Handle,
    ) -> Option<(Self, flume::Receiver<AgentStatus>)> {
        let (tx, rx) = flume::unbounded::<AgentStatus>();
        let stop = Arc::new(Notify::new());
        let stop_notify = stop.clone();

        let handle = std::thread::Builder::new()
            .name("superhq-event-service".into())
            .spawn(move || {
                tokio_handle.block_on(async move {
                    // Ensure the events directory exists
                    let _ = sandbox
                        .exec_in("sh", &format!("mkdir -p {EVENTS_DIR}"))
                        .await;

                    let mut watch = match sandbox.open_watch(EVENTS_DIR, false).await {
                        Ok(w) => w,
                        Err(e) => {
                            eprintln!("[event_service] failed to open watch: {e}");
                            return;
                        }
                    };

                    loop {
                        // Wait for a filesystem event or stop signal
                        tokio::select! {
                            e = watch.receiver.recv() => match e {
                                Some(_) => {},
                                None => break,
                            },
                            _ = stop_notify.notified() => break,
                        };

                        // Settle: drain pending events (50ms)
                        loop {
                            while watch.receiver.try_recv().is_ok() {}
                            match tokio::time::timeout(
                                std::time::Duration::from_millis(50),
                                watch.receiver.recv(),
                            )
                            .await
                            {
                                Ok(Some(_)) => continue,
                                Ok(None) => return,
                                Err(_) => break,
                            }
                        }

                        // Read and truncate — avoids re-reading old events
                        let content = match sandbox.read_file(EVENTS_FILE).await {
                            Ok(bytes) => bytes,
                            Err(_) => continue,
                        };
                        if content.is_empty() {
                            continue;
                        }
                        let _ = sandbox.write_file(EVENTS_FILE, b"").await;

                        let text = String::from_utf8_lossy(&content);
                        for line in text.lines() {
                            if line.is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<RawEvent>(line) {
                                Ok(raw) => {
                                    if let Some(status) = raw.into_status() {
                                        if tx.send(status).is_err() {
                                            return;
                                        }
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[event_service] bad event line: {e}");
                                }
                            }
                        }
                    }
                });
            })
            .ok()?;

        Some((
            AgentEventService {
                stop,
                _handle: handle,
            },
            rx,
        ))
    }
}
