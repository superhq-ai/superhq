//! Per-connection state and the control-stream JSON-RPC dispatch loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use superhq_remote_proto::{
    decode, encode_response,
    methods::{self, Ack},
    stream::{StreamInit, STREAM_INIT},
    Message, Notification, Request, Response, RpcError,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{debug, info, warn};

use crate::handler::RemoteHandler;

/// Per-connection state shared between control and data streams.
///
/// `authenticated` flips to `true` after `session.hello` succeeds. The
/// device id is populated from the hello params' `auth.device_id` at the
/// same time — used by the audit hook to tie requests back to a device.
pub struct SessionState {
    pub authenticated: AtomicBool,
    pub device_id: Mutex<Option<String>>,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            authenticated: AtomicBool::new(false),
            device_id: Mutex::new(None),
        }
    }

    pub fn device_id(&self) -> Option<String> {
        self.device_id.lock().ok().and_then(|g| g.clone())
    }

    fn set_device_id(&self, id: String) {
        if let Ok(mut guard) = self.device_id.lock() {
            *guard = Some(id);
        }
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

/// Drive a remote-host connection: accept control stream, then loop
/// accepting subordinate data streams (PTY, status, etc.) and route them
/// to the handler.
pub async fn drive_connection<H: RemoteHandler>(
    connection: Connection,
    handler: Arc<H>,
) -> Result<()> {
    info!(remote = %connection.remote_id(), "remote-host: connection accepted");

    // Per-connection state, shared with data-stream handlers so they can
    // reject until the session is established and audit log entries can
    // be correlated to the authenticated device.
    let session = Arc::new(SessionState::new());

    // Control stream is the first bidirectional stream opened by the peer.
    let (ctrl_send, ctrl_recv) = connection
        .accept_bi()
        .await
        .map_err(|e| anyhow!("accept control stream: {e}"))?;

    let control_handler = handler.clone();
    let control_session = session.clone();
    let control_task = tokio::spawn(async move {
        if let Err(e) =
            drive_control_stream(ctrl_send, ctrl_recv, control_handler, control_session).await
        {
            let s = e.to_string();
            if s.contains("connection lost") || s.contains("application closed") {
                debug!(error = %e, "remote-host: control stream closed");
            } else {
                warn!(error = %e, "remote-host: control stream exited with error");
            }
        }
    });

    // Subsequent bidirectional streams are data streams.
    loop {
        tokio::select! {
            bi = connection.accept_bi() => {
                match bi {
                    Ok((send, recv)) => {
                        let handler = handler.clone();
                        let session = session.clone();
                        tokio::spawn(async move {
                            if let Err(e) = drive_data_stream(send, recv, handler, session).await {
                                warn!(error = %e, "remote-host: data stream error");
                            }
                        });
                    }
                    Err(_) => break, // connection closed
                }
            }
            _ = connection.closed() => break,
        }
    }

    let _ = control_task.await;
    Ok(())
}

async fn drive_control_stream<H: RemoteHandler>(
    mut send: SendStream,
    recv: RecvStream,
    handler: Arc<H>,
    session: Arc<SessionState>,
) -> Result<()> {
    let mut reader = BufReader::new(recv);
    let mut buf = Vec::new();
    // Host-originated notifications — whatever the handler wants to
    // push. Each item on this channel is already-encoded JSON-RPC
    // notification text (no trailing newline).
    let mut notifications = handler.subscribe_notifications();

    loop {
        tokio::select! {
            // Client → host request / notification
            read = reader.read_until(b'\n', &mut buf) => {
                let n = read?;
                if n == 0 {
                    info!("remote-host: control stream closed by peer");
                    return Ok(());
                }
                while matches!(buf.last(), Some(b'\n' | b'\r')) {
                    buf.pop();
                }
                if buf.is_empty() {
                    buf.clear();
                    continue;
                }
                let text = match std::str::from_utf8(&buf) {
                    Ok(s) => s.to_string(),
                    Err(e) => {
                        warn!(error = %e, "remote-host: non-utf8 control message; dropping");
                        buf.clear();
                        continue;
                    }
                };
                buf.clear();
                match decode(&text) {
                    Ok(Message::Request(req)) => {
                        let resp = dispatch_control_request(&handler, req, &session).await;
                        let wire = encode_response(&resp)?;
                        send.write_all(wire.as_bytes()).await?;
                        send.write_all(b"\n").await?;
                    }
                    Ok(Message::Notification(note)) => {
                        handle_incoming_notification(note);
                    }
                    Ok(Message::Response(resp)) => {
                        warn!(id = resp.id, "remote-host: unexpected response from client");
                    }
                    Err(e) => {
                        warn!(error = %e, body = %text, "remote-host: decode failed; dropping");
                    }
                }
            }

            // Host → client push notification (snapshot.invalidated, etc).
            pushed = async {
                match notifications.as_mut() {
                    Some(rx) => rx.recv().await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                if let Some(line) = pushed {
                    send.write_all(line.as_bytes()).await?;
                    send.write_all(b"\n").await?;
                }
            }
        }
    }
}

fn handle_incoming_notification(note: Notification) {
    debug!(method = %note.method, "remote-host: received notification");
}

async fn dispatch_control_request<H: RemoteHandler>(
    handler: &Arc<H>,
    req: Request,
    session: &Arc<SessionState>,
) -> Response {
    let id = req.id;
    let method = req.method.clone();

    // Methods allowed pre-authentication: bootstrapping the session and
    // pairing with the host. Everything else requires a successful
    // session.hello first.
    let pre_auth_ok = matches!(
        method.as_str(),
        methods::SESSION_HELLO | methods::PAIRING_REQUEST | methods::SESSION_PING
    );
    if !pre_auth_ok && !session.authenticated.load(Ordering::Acquire) {
        let err = RpcError::new(
            superhq_remote_proto::error_code::AUTH_REQUIRED,
            format!("{method} requires an authenticated session"),
        );
        handler.audit_rpc(&method, false, session.device_id()).await;
        return Response::error(id, err);
    }

    let result = match method.as_str() {
        methods::SESSION_HELLO => {
            // Preserve the raw params so that on a successful hello we
            // can stash the authenticated device id in the session state.
            let params_raw = req.params.clone();
            let r = call_method(req.params, |p| handler.session_hello(p)).await;
            if r.is_ok() {
                session.authenticated.store(true, Ordering::Release);
                if let Some(device_id) = params_raw
                    .get("auth")
                    .and_then(|a| a.get("device_id"))
                    .and_then(|v| v.as_str())
                {
                    session.set_device_id(device_id.to_string());
                }
            }
            r
        }
        methods::PAIRING_REQUEST => {
            call_method(req.params, |p| handler.pairing_request(p)).await
        }
        methods::WORKSPACES_LIST => handler.workspaces_list().await.and_then(|r| {
            serde_json::to_value(r)
                .map_err(|e| RpcError::internal(format!("encode result: {e}")))
        }),
        methods::WORKSPACE_ACTIVATE => {
            call_method(req.params, |p| handler.workspace_activate(p)).await
        }
        methods::TABS_LIST => handler.tabs_list().await.and_then(|r| {
            serde_json::to_value(r)
                .map_err(|e| RpcError::internal(format!("encode result: {e}")))
        }),
        methods::TABS_CREATE => call_method(req.params, |p| handler.tabs_create(p)).await,
        methods::TABS_CLOSE => call_method_unit(req.params, |p| handler.tabs_close(p)).await,
        methods::PTY_ATTACH => call_method(req.params, |p| handler.pty_attach(p)).await,
        methods::PTY_DETACH => call_method_unit(req.params, |p| handler.pty_detach(p)).await,
        methods::PTY_RESIZE => call_method_unit(req.params, |p| handler.pty_resize(p)).await,
        _ => Err(RpcError::method_not_found(&method)),
    };

    let ok = result.is_ok();
    handler.audit_rpc(&method, ok, session.device_id()).await;

    match result {
        Ok(value) => Response::success(id, value),
        Err(err) => Response::error(id, err),
    }
}

/// Handle a subordinate data stream: PTY, status, or future stream types.
///
/// First message is a JSON-RPC `stream.init` request identifying the stream.
/// After the ack, the stream-specific protocol takes over.
async fn drive_data_stream<H: RemoteHandler>(
    mut send: SendStream,
    mut recv: RecvStream,
    handler: Arc<H>,
    session: Arc<SessionState>,
) -> Result<()> {
    // Read the init line byte-by-byte so we don't over-consume into the
    // raw data region that follows the newline.
    let init_text = read_line(&mut recv).await?;
    let Some(init_text) = init_text else {
        return Ok(()); // stream closed before init
    };

    let msg = decode(&init_text).map_err(|e| anyhow!("decode stream.init: {e}"))?;
    let req = match msg {
        Message::Request(r) if r.method == STREAM_INIT => r,
        _ => return Err(anyhow!("expected stream.init request as first message")),
    };

    // Data streams require the connection to already be authenticated
    // via the control stream's session.hello.
    if !session.authenticated.load(Ordering::Acquire) {
        let err = Response::error(
            req.id,
            RpcError::new(
                superhq_remote_proto::error_code::AUTH_REQUIRED,
                "data streams require an authenticated session (session.hello first)",
            ),
        );
        let wire = encode_response(&err)?;
        send.write_all(wire.as_bytes()).await?;
        send.write_all(b"\n").await?;
        let _ = send.finish();
        return Ok(());
    }

    let init: StreamInit = serde_json::from_value(req.params.clone())
        .map_err(|e| anyhow!("parse stream.init params: {e}"))?;

    // Ack the init.
    let ack = Response::success(
        req.id,
        serde_json::to_value(Ack::ok()).unwrap(),
    );
    let wire = encode_response(&ack)?;
    send.write_all(wire.as_bytes()).await?;
    send.write_all(b"\n").await?;

    match init {
        StreamInit::Pty { workspace_id, tab_id, cols, rows } => {
            // Hand the stream off to the handler. It owns the raw I/O
            // from this point.
            handler
                .pty_stream(workspace_id, tab_id, cols, rows, send, recv)
                .await
                .map_err(|e| anyhow!("pty_stream: {}", e.message))
        }
        StreamInit::Status => {
            // Not yet implemented; close the stream politely.
            let _ = send.finish();
            Ok(())
        }
    }
}

/// Read bytes from `recv` up to and including the first `\n`, returning
/// the line as a UTF-8 string (trimmed of trailing `\r\n`). Does not
/// over-consume: bytes after the newline stay in the stream.
async fn read_line(recv: &mut RecvStream) -> Result<Option<String>> {
    let mut out = Vec::with_capacity(256);
    let mut b = [0u8; 1];
    loop {
        match recv.read(&mut b).await? {
            None | Some(0) => {
                return Ok(if out.is_empty() {
                    None
                } else {
                    Some(String::from_utf8(out)?)
                });
            }
            Some(_) => {}
        }
        if b[0] == b'\n' {
            while matches!(out.last(), Some(b'\r')) {
                out.pop();
            }
            return Ok(Some(String::from_utf8(out)?));
        }
        out.push(b[0]);
        if out.len() > 1 << 16 {
            return Err(anyhow!("stream.init message too long"));
        }
    }
}

async fn call_method<P, R, Fut>(
    params: serde_json::Value,
    call: impl FnOnce(P) -> Fut,
) -> Result<serde_json::Value, RpcError>
where
    P: serde::de::DeserializeOwned,
    R: serde::Serialize,
    Fut: std::future::Future<Output = Result<R, RpcError>>,
{
    let p: P = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("parse params: {e}")))?;
    let r = call(p).await?;
    serde_json::to_value(r).map_err(|e| RpcError::internal(format!("encode result: {e}")))
}

async fn call_method_unit<P, Fut>(
    params: serde_json::Value,
    call: impl FnOnce(P) -> Fut,
) -> Result<serde_json::Value, RpcError>
where
    P: serde::de::DeserializeOwned,
    Fut: std::future::Future<Output = Result<(), RpcError>>,
{
    let p: P = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("parse params: {e}")))?;
    call(p).await?;
    Ok(serde_json::to_value(Ack::ok()).unwrap())
}
