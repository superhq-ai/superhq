//! Per-connection state and the control-stream JSON-RPC dispatch loop.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use superhq_remote_proto::{
    decode, encode_response,
    methods::{self, Ack},
    stream::{StreamInit, STREAM_INIT},
    Message, Notification, Request, Response, RpcError,
};
use tokio::io::{AsyncBufRead, BufReader};
use tracing::{debug, info, warn};

/// Max size of a single control-stream JSON-RPC frame (one newline-
/// terminated message). The real payloads we care about (session.hello
/// with a resume token, a snapshot.invalidated push, etc.) are well
/// under 64 KiB; 1 MiB leaves headroom for a future large-notification
/// case without letting a misbehaving client OOM the host.
const MAX_CONTROL_FRAME_BYTES: usize = 1 << 20;

/// How long to wait for the peer to open the control stream after the
/// connection is accepted. A reachable peer that never opens it would
/// otherwise keep the detached session task alive indefinitely.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a subordinate data stream has to send its `stream.init`
/// line. Read-until-newline would otherwise block forever on a
/// half-open stream.
const STREAM_INIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Soft cap on the number of subordinate data streams a single
/// connection is allowed to keep open concurrently. Keeps a slowloris
/// style attack from accumulating detached stream tasks forever.
const MAX_DATA_STREAMS_PER_CONN: usize = 64;

use crate::handler::RemoteHandler;

/// Per-connection state shared between control and data streams.
///
/// `authenticated` flips to `true` after `session.hello` succeeds. The
/// device id is populated from the hello params' `auth.device_id` at the
/// same time — used by the audit hook to tie requests back to a device.
pub struct SessionState {
    pub authenticated: AtomicBool,
    pub device_id: Mutex<Option<String>>,
    /// Number of live subordinate data streams on this connection.
    /// Incremented when a stream task starts, decremented on exit.
    /// Gates acceptance so a single client can't open unbounded streams.
    pub data_streams: AtomicUsize,
    /// Fresh 32-byte nonce issued by `session.challenge`. Consumed
    /// (set to None) on the first `session.hello` attempt. Clients
    /// must challenge again after any failed hello.
    pub pending_challenge: Mutex<Option<[u8; 32]>>,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            authenticated: AtomicBool::new(false),
            device_id: Mutex::new(None),
            data_streams: AtomicUsize::new(0),
            pending_challenge: Mutex::new(None),
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

    // Control stream is the first bidirectional stream opened by the
    // peer. Wrap in a short timeout — otherwise a connected-but-silent
    // peer would keep the detached session task alive indefinitely.
    let (ctrl_send, ctrl_recv) =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, connection.accept_bi()).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => return Err(anyhow!("accept control stream: {e}")),
            Err(_) => return Err(anyhow!("handshake timed out waiting for control stream")),
        };

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
                    Ok((mut send, recv)) => {
                        // Gate: soft cap on concurrent data streams per
                        // connection. A compliant client never opens
                        // more than a handful; anything above the cap
                        // is rejected with an RPC error so the peer
                        // sees a clean failure instead of a stall.
                        let prior = session.data_streams.fetch_add(1, Ordering::AcqRel);
                        if prior >= MAX_DATA_STREAMS_PER_CONN {
                            session.data_streams.fetch_sub(1, Ordering::AcqRel);
                            warn!(
                                cap = MAX_DATA_STREAMS_PER_CONN,
                                "remote-host: rejecting data stream over cap"
                            );
                            let err = Response::error(
                                superhq_remote_proto::RequestId::Null,
                                RpcError::new(
                                    superhq_remote_proto::error_code::INTERNAL_ERROR,
                                    format!(
                                        "server refused stream: open stream cap of \
                                         {MAX_DATA_STREAMS_PER_CONN} reached"
                                    ),
                                ),
                            );
                            if let Ok(wire) = encode_response(&err) {
                                let _ = send.write_all(wire.as_bytes()).await;
                                let _ = send.write_all(b"\n").await;
                            }
                            let _ = send.finish();
                            continue;
                        }
                        let handler = handler.clone();
                        let session = session.clone();
                        tokio::spawn(async move {
                            let result = drive_data_stream(send, recv, handler, session.clone()).await;
                            session.data_streams.fetch_sub(1, Ordering::AcqRel);
                            if let Err(e) = result {
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
    //
    // Deliberately deferred until the session is authenticated. A
    // previous version subscribed eagerly, which meant any peer that
    // opened a control stream started receiving host notifications
    // before they had proven pairing. `None` means "don't push yet".
    let mut notifications: Option<
        tokio::sync::broadcast::Receiver<std::sync::Arc<String>>,
    > = None;

    loop {
        // One-shot subscription the instant the session is authenticated.
        // Polled before every select so we catch the transition even if
        // auth and the next iteration race.
        if notifications.is_none() && session.authenticated.load(Ordering::Acquire) {
            notifications = handler.subscribe_notifications();
        }
        tokio::select! {
            // Client → host request / notification
            read = read_until_capped(&mut reader, &mut buf, MAX_CONTROL_FRAME_BYTES) => {
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
                        warn!(id = %resp.id, "remote-host: unexpected response from client");
                    }
                    Err(e) => {
                        // Never log attacker-controlled bodies verbatim —
                        // lets a malicious peer balloon host logs with
                        // arbitrary content.
                        warn!(
                            error = %e,
                            len = text.len(),
                            "remote-host: decode failed; dropping"
                        );
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
        methods::SESSION_HELLO
            | methods::SESSION_CHALLENGE
            | methods::PAIRING_REQUEST
            | methods::SESSION_PING
    );
    if !pre_auth_ok && !session.authenticated.load(Ordering::Acquire) {
        let err = RpcError::new(
            superhq_remote_proto::error_code::AUTH_REQUIRED,
            format!("{method} requires an authenticated session"),
        );
        handler.audit_rpc(&method, false, session.device_id()).await;
        return Response::error(id, err);
    }

    if !pre_auth_ok {
        if let Some(device) = session.device_id() {
            if !handler.is_device_authorized(&device).await {
                let err = RpcError::new(
                    superhq_remote_proto::error_code::AUTH_INVALID,
                    "device access has been revoked",
                );
                handler.audit_rpc(&method, false, Some(device)).await;
                return Response::error(id, err);
            }
        }
    }

    let result = match method.as_str() {
        methods::SESSION_CHALLENGE => {
            // Generate a fresh one-shot nonce and stash it on this
            // connection. The next session.hello's HMAC must bind
            // to this exact value.
            let nonce = crate::auth::generate_challenge();
            if let Ok(mut g) = session.pending_challenge.lock() {
                *g = Some(nonce);
            }
            use base64::{engine::general_purpose::STANDARD, Engine};
            let result = methods::SessionChallengeResult {
                nonce: STANDARD.encode(nonce),
            };
            serde_json::to_value(result)
                .map_err(|e| RpcError::internal(format!("encode result: {e}")))
        }
        methods::SESSION_HELLO => {
            // Consume (take) the pending challenge before handing
            // off. Any failure of this hello invalidates it too, so
            // a retry needs a fresh session.challenge.
            let challenge = session
                .pending_challenge
                .lock()
                .ok()
                .and_then(|mut g| g.take());
            // Preserve the raw params so that on a successful hello we
            // can stash the authenticated device id in the session state.
            let params_raw = req.params.clone();
            let r = call_method(req.params, |p| handler.session_hello(p, challenge))
                .await;
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
        methods::PTY_ATTACH => {
            let device = session.device_id();
            call_method(req.params, |p| handler.pty_attach(p, device)).await
        }
        methods::PTY_DETACH => {
            let device = session.device_id();
            call_method_unit(req.params, |p| handler.pty_detach(p, device)).await
        }
        methods::PTY_RESIZE => {
            let device = session.device_id();
            call_method_unit(req.params, |p| handler.pty_resize(p, device)).await
        }
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
    // raw data region that follows the newline. Bounded timeout — a
    // half-open stream that never sends init would otherwise block.
    let init_text =
        match tokio::time::timeout(STREAM_INIT_TIMEOUT, read_line(&mut recv)).await {
            Ok(Ok(line)) => line,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(anyhow!(
                    "stream.init timed out after {:?}",
                    STREAM_INIT_TIMEOUT
                ))
            }
        };
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

    if let Some(device) = session.device_id() {
        if !handler.is_device_authorized(&device).await {
            let err = Response::error(
                req.id,
                RpcError::new(
                    superhq_remote_proto::error_code::AUTH_INVALID,
                    "device access has been revoked",
                ),
            );
            let wire = encode_response(&err)?;
            send.write_all(wire.as_bytes()).await?;
            send.write_all(b"\n").await?;
            let _ = send.finish();
            return Ok(());
        }
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
            let device = session.device_id();
            handler
                .pty_stream(workspace_id, tab_id, cols, rows, device, send, recv)
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

/// Read from `reader` into `buf` until the first `\n` (inclusive) or
/// cap is exceeded. Equivalent to `AsyncBufReadExt::read_until` but
/// fails closed instead of growing the buffer unbounded — a single
/// newline-free frame from a malicious peer would otherwise drive
/// process memory up until the kernel OOM-killed the host.
async fn read_until_capped<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    cap: usize,
) -> Result<usize> {
    use tokio::io::AsyncBufReadExt;
    let start_len = buf.len();
    loop {
        let chunk = match reader.fill_buf().await {
            Ok(c) => c,
            Err(e) => return Err(e.into()),
        };
        if chunk.is_empty() {
            return Ok(buf.len() - start_len);
        }
        let nl_pos = chunk.iter().position(|&b| b == b'\n');
        let take = match nl_pos {
            Some(i) => i + 1,
            None => chunk.len(),
        };
        if buf.len() + take > cap {
            return Err(anyhow!(
                "control frame exceeds cap of {cap} bytes; closing stream"
            ));
        }
        buf.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        if nl_pos.is_some() {
            return Ok(buf.len() - start_len);
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
