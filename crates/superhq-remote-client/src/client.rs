//! `RemoteClient` — connects to a remote-host via iroh + ALPN, drives the
//! control stream, multiplexes RPC calls and notifications.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{anyhow, Result};
use iroh::{
    endpoint::{Connection, RecvStream, SendStream},
    Endpoint, EndpointId,
};
use superhq_remote_proto::{
    decode, encode_request,
    methods::{self, Ack},
    stream::{StreamInit, STREAM_INIT},
    Message, Notification, Request, Response, RpcError, ALPN,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{mpsc, oneshot, Mutex as AsyncMutex},
};
use tracing::{debug, warn};

/// Upper bound on how long a single RPC waits before giving up. A
/// responsive host answers in milliseconds; 60s covers genuinely slow
/// operations like workspace activation / agent sandbox boot.
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(60);

/// Cross-platform async sleep. `tokio::time` isn't supported on wasm
/// (no timer driver), so we fall through to gloo-timers there.
async fn sleep(dur: Duration) {
    #[cfg(not(target_family = "wasm"))]
    {
        tokio::time::sleep(dur).await;
    }
    #[cfg(target_family = "wasm")]
    {
        gloo_timers::future::sleep(dur).await;
    }
}

/// Error calling an RPC method.
#[derive(Debug, thiserror::Error)]
pub enum RpcCallError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("rpc error (code {}): {}", .0.code, .0.message)]
    Rpc(RpcError),
    #[error("encode/decode: {0}")]
    Codec(String),
    #[error("connection closed before response")]
    Closed,
    #[error("rpc timed out after {0:?}")]
    Timeout(Duration),
}

#[derive(Debug, thiserror::Error)]
pub enum PendingError {
    #[error("unknown request id")]
    UnknownId,
}

type ResponseTx = oneshot::Sender<Response>;
type PendingMap = Arc<Mutex<HashMap<u64, ResponseTx>>>;

/// A connected remote-control client.
///
/// Cheap to clone; shares the underlying control stream, request map, and
/// notification receiver across clones.
#[derive(Clone)]
pub struct RemoteClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    conn: Connection,
    ctrl_send: AsyncMutex<SendStream>,
    next_id: AtomicU64,
    pending: PendingMap,
    // Held to keep the recv-loop alive for the lifetime of the client.
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    _ctrl_task: tokio::task::JoinHandle<()>,
}

impl RemoteClient {
    /// Connect to a host at `peer` via the `superhq/remote/1` ALPN. Opens
    /// the control stream and starts the response-dispatch loop.
    pub async fn connect(
        endpoint: &Endpoint,
        peer: EndpointId,
    ) -> Result<(Self, mpsc::UnboundedReceiver<Notification>)> {
        let conn = endpoint.connect(peer, ALPN).await?;
        let (ctrl_send, ctrl_recv) = conn.open_bi().await?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (notifications_tx, notifications_rx) = mpsc::unbounded_channel();

        let pending_for_task = pending.clone();
        let recv_loop_fut = async move {
            if let Err(e) =
                run_control_recv_loop(ctrl_recv, pending_for_task, notifications_tx).await
            {
                debug!(error = %e, "remote-client: control recv loop exited");
            }
        };

        #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
        let _ctrl_task = tokio::spawn(recv_loop_fut);
        #[cfg(all(target_family = "wasm", target_os = "unknown"))]
        wasm_bindgen_futures::spawn_local(recv_loop_fut);

        Ok((
            Self {
                inner: Arc::new(ClientInner {
                    conn,
                    ctrl_send: AsyncMutex::new(ctrl_send),
                    next_id: AtomicU64::new(1),
                    pending,
                    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
                    _ctrl_task,
                }),
            },
            notifications_rx,
        ))
    }

    /// Low-level typed RPC call. Serializes `params`, sends a request,
    /// awaits the matching response, deserializes the result.
    ///
    /// Cleans up its `pending` entry on *every* exit path (send failure,
    /// timeout, connection close) so the map never accumulates dead
    /// waiters. Enforces a per-call timeout so an unresponsive host
    /// can't strand a caller forever.
    pub async fn call<P, R>(&self, method: &str, params: P) -> Result<R, RpcCallError>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let params_value = serde_json::to_value(&params)
            .map_err(|e| RpcCallError::Codec(format!("encode params: {e}")))?;
        let req = Request::new(id.into(), method, params_value);
        let wire =
            encode_request(&req).map_err(|e| RpcCallError::Codec(format!("encode: {e}")))?;

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.inner.pending.lock().unwrap();
            pending.insert(id, tx);
        }

        // Helper: drop the pending entry. Called from every error path
        // so we never leak a dead sender into the map.
        let drop_pending = || {
            if let Ok(mut p) = self.inner.pending.lock() {
                p.remove(&id);
            }
        };

        {
            let mut send = self.inner.ctrl_send.lock().await;
            if let Err(e) = send.write_all(wire.as_bytes()).await {
                drop_pending();
                return Err(RpcCallError::Transport(e.to_string()));
            }
            if let Err(e) = send.write_all(b"\n").await {
                drop_pending();
                return Err(RpcCallError::Transport(e.to_string()));
            }
        }

        // Race the response against the timeout. Whichever wakes first
        // wins; the other is cancelled on drop.
        let resp = {
            use futures_util::{future::Either, pin_mut};
            let timer = sleep(DEFAULT_RPC_TIMEOUT);
            pin_mut!(rx);
            pin_mut!(timer);
            match futures_util::future::select(rx, timer).await {
                Either::Left((resp, _)) => match resp {
                    Ok(r) => r,
                    Err(_) => {
                        drop_pending();
                        return Err(RpcCallError::Closed);
                    }
                },
                Either::Right(_) => {
                    drop_pending();
                    return Err(RpcCallError::Timeout(DEFAULT_RPC_TIMEOUT));
                }
            }
        };
        if let Some(err) = resp.error {
            return Err(RpcCallError::Rpc(err));
        }
        let result_value = resp.result.ok_or_else(|| {
            RpcCallError::Codec("response had neither result nor error".into())
        })?;
        serde_json::from_value(result_value)
            .map_err(|e| RpcCallError::Codec(format!("decode result: {e}")))
    }

    // ── Typed wrappers over `call` ────────────────────────────────────

    pub async fn session_hello(
        &self,
        params: methods::SessionHelloParams,
    ) -> Result<methods::SessionHelloResult, RpcCallError> {
        self.call(methods::SESSION_HELLO, params).await
    }

    pub async fn session_challenge(
        &self,
    ) -> Result<methods::SessionChallengeResult, RpcCallError> {
        self.call(methods::SESSION_CHALLENGE, serde_json::json!({})).await
    }

    pub async fn pairing_request(
        &self,
        params: methods::PairingRequestParams,
    ) -> Result<methods::PairingRequestResult, RpcCallError> {
        self.call(methods::PAIRING_REQUEST, params).await
    }

    pub async fn workspaces_list(
        &self,
    ) -> Result<methods::WorkspacesListResult, RpcCallError> {
        self.call(methods::WORKSPACES_LIST, serde_json::json!({})).await
    }

    pub async fn workspace_activate(
        &self,
        params: methods::WorkspaceActivateParams,
    ) -> Result<methods::WorkspaceActivateResult, RpcCallError> {
        self.call(methods::WORKSPACE_ACTIVATE, params).await
    }

    pub async fn tabs_list(&self) -> Result<methods::TabsListResult, RpcCallError> {
        self.call(methods::TABS_LIST, serde_json::json!({})).await
    }

    pub async fn tabs_create(
        &self,
        params: methods::TabsCreateParams,
    ) -> Result<methods::TabsCreateResult, RpcCallError> {
        self.call(methods::TABS_CREATE, params).await
    }

    pub async fn tabs_close(
        &self,
        params: methods::TabsCloseParams,
    ) -> Result<(), RpcCallError> {
        self.call::<_, serde_json::Value>(methods::TABS_CLOSE, params)
            .await
            .map(|_| ())
    }

    pub async fn pty_attach(
        &self,
        params: methods::PtyAttachParams,
    ) -> Result<methods::PtyAttachResult, RpcCallError> {
        self.call(methods::PTY_ATTACH, params).await
    }

    pub async fn pty_detach(
        &self,
        params: methods::PtyDetachParams,
    ) -> Result<Ack, RpcCallError> {
        self.call(methods::PTY_DETACH, params).await
    }

    pub async fn pty_resize(
        &self,
        params: methods::PtyResizeParams,
    ) -> Result<Ack, RpcCallError> {
        self.call(methods::PTY_RESIZE, params).await
    }

    /// Open a new bidirectional data stream, send `stream.init`, await the
    /// ack. After this returns, the caller may send/receive raw bytes on
    /// the returned streams according to the stream's protocol (e.g. for
    /// PTY: bytes both ways).
    pub async fn open_pty_stream(
        &self,
        workspace_id: superhq_remote_proto::types::WorkspaceId,
        tab_id: superhq_remote_proto::types::TabId,
        cols: u16,
        rows: u16,
    ) -> Result<(SendStream, RecvStream)> {
        let (mut send, mut recv) = self.inner.conn.open_bi().await?;

        // First JSON-RPC request on this stream: stream.init
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let init_params =
            serde_json::to_value(StreamInit::Pty { workspace_id, tab_id, cols, rows })?;
        let init_req = Request::new(id.into(), STREAM_INIT, init_params);
        let wire = encode_request(&init_req)?;
        send.write_all(wire.as_bytes()).await?;
        send.write_all(b"\n").await?;

        // Read ack without over-consuming.
        let line = read_line(&mut recv)
            .await?
            .ok_or_else(|| anyhow!("stream closed before init ack"))?;
        let msg = decode(&line)?;
        match msg {
            Message::Response(resp) if resp.id.as_number() == Some(id) => {
                if let Some(err) = resp.error {
                    return Err(anyhow!("stream.init rejected: {} ({})", err.message, err.code));
                }
                let _ack: Ack =
                    serde_json::from_value(resp.result.unwrap_or(serde_json::Value::Null))?;
                Ok((send, recv))
            }
            other => Err(anyhow!("unexpected first message on data stream: {other:?}")),
        }
    }

    /// Open a bidi stream, send a `StreamInit::Attachment`, await the
    /// ack, write `bytes`, then read the JSON-encoded
    /// `AttachmentResult { path }` the server writes back.
    pub async fn upload_attachment(
        &self,
        workspace_id: superhq_remote_proto::types::WorkspaceId,
        tab_id: superhq_remote_proto::types::TabId,
        name: String,
        mime: Option<String>,
        bytes: Vec<u8>,
    ) -> Result<String> {
        let (mut send, mut recv) = self.inner.conn.open_bi().await?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let init_params = serde_json::to_value(StreamInit::Attachment {
            workspace_id,
            tab_id,
            name,
            mime,
            size: bytes.len() as u64,
        })?;
        let init_req = Request::new(id.into(), STREAM_INIT, init_params);
        let wire = encode_request(&init_req)?;
        send.write_all(wire.as_bytes()).await?;
        send.write_all(b"\n").await?;

        // Read ack.
        let line = read_line(&mut recv)
            .await?
            .ok_or_else(|| anyhow!("stream closed before attachment init ack"))?;
        match decode(&line)? {
            Message::Response(resp) if resp.id.as_number() == Some(id) => {
                if let Some(err) = resp.error {
                    return Err(anyhow!(
                        "attachment init rejected: {} ({})",
                        err.message,
                        err.code
                    ));
                }
            }
            other => {
                return Err(anyhow!(
                    "unexpected first message on attachment stream: {other:?}"
                ));
            }
        }

        // Stream the bytes.
        send.write_all(&bytes).await?;
        send.finish()?;

        // Read the single-line result the server writes after save.
        let result_line = read_line(&mut recv)
            .await?
            .ok_or_else(|| anyhow!("stream closed before attachment result"))?;
        let result: superhq_remote_proto::stream::AttachmentResult =
            serde_json::from_str(&result_line)
                .map_err(|e| anyhow!("decode attachment result: {e}"))?;
        Ok(result.path)
    }

    /// Politely close the control stream and the underlying connection.
    pub fn close(&self) {
        self.inner.conn.close(0u32.into(), b"client closed");
    }
}

async fn run_control_recv_loop(
    recv: RecvStream,
    pending: PendingMap,
    notifications: mpsc::UnboundedSender<Notification>,
) -> Result<()> {
    // Drain every pending waiter on ANY exit (EOF or error). Previously
    // we only cleared on clean EOF, so a transport I/O error would
    // bubble up via `?` and leave every in-flight RPC hanging forever.
    // A Drop guard handles it uniformly.
    struct DrainPending(PendingMap);
    impl Drop for DrainPending {
        fn drop(&mut self) {
            if let Ok(mut p) = self.0.lock() {
                p.clear();
            }
        }
    }
    let _drain_on_exit = DrainPending(pending.clone());

    let mut reader = BufReader::new(recv);
    let mut buf = Vec::new();

    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf).await?;
        if n == 0 {
            debug!("remote-client: control stream closed by peer");
            return Ok(());
        }
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        if buf.is_empty() {
            continue;
        }
        let text = match std::str::from_utf8(&buf) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "remote-client: non-utf8 on control stream");
                continue;
            }
        };
        match decode(text) {
            Ok(Message::Response(resp)) => {
                // Our `pending` map is keyed by the u64 ids we generate.
                // Non-numeric / null ids from weird peers can't match.
                let Some(num_id) = resp.id.as_number() else {
                    warn!(
                        id = %resp.id,
                        "remote-client: response with non-numeric id; cannot match pending"
                    );
                    continue;
                };
                let mut p = pending.lock().unwrap();
                if let Some(tx) = p.remove(&num_id) {
                    let _ = tx.send(resp);
                } else {
                    warn!(id = num_id, "remote-client: response for unknown id");
                }
            }
            Ok(Message::Notification(note)) => {
                // Don't let a dropped notification consumer kill the
                // control-stream reader — that used to strand all
                // subsequent RPC responses. Just drop the notification
                // and keep looping.
                if notifications.send(note).is_err() {
                    debug!("remote-client: notifications receiver dropped; ignoring");
                }
            }
            Ok(Message::Request(req)) => {
                warn!(method = %req.method, "remote-client: unexpected request from server");
            }
            Err(e) => {
                warn!(error = %e, body = %text, "remote-client: decode failed");
            }
        }
    }
}

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
            return Err(anyhow!("ack line too long"));
        }
    }
}
