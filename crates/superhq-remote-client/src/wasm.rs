//! WASM bindings for the browser PWA / demo page.

use std::sync::Arc;

use iroh::{
    endpoint::{RecvStream, SendStream},
    Endpoint, EndpointId,
};
use js_sys::Uint8Array;
use superhq_remote_proto::Notification;
use tokio::sync::{mpsc, Mutex};
use tracing::level_filters::LevelFilter;
use tracing_subscriber_wasm::MakeConsoleWriter;
use wasm_bindgen::{prelude::wasm_bindgen, JsValue};

use crate::client::RemoteClient;

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .with_writer(MakeConsoleWriter::default().map_trace_level_to(tracing::Level::DEBUG))
        .without_time()
        .with_ansi(false)
        .init();
    tracing::info!("superhq-remote-client wasm loaded");
}

#[wasm_bindgen]
pub struct ClientHandle {
    endpoint: Endpoint,
    client: RemoteClient,
    peer_id: String,
    /// Host-push notification receiver. Exposed to JS via
    /// `next_notification()`; taken out of the Option by the first
    /// consumer so we don't accidentally split the stream.
    notifications: Arc<Mutex<Option<mpsc::UnboundedReceiver<Notification>>>>,
}

#[wasm_bindgen]
#[derive(Clone)]
pub struct DeviceCredential {
    device_id: String,
    device_key_b64: String,
}

#[wasm_bindgen]
impl DeviceCredential {
    #[wasm_bindgen(constructor)]
    pub fn new(device_id: String, device_key_b64: String) -> Self {
        Self { device_id, device_key_b64 }
    }

    #[wasm_bindgen(getter)]
    pub fn device_id(&self) -> String {
        self.device_id.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn device_key(&self) -> String {
        self.device_key_b64.clone()
    }
}

#[wasm_bindgen]
impl ClientHandle {
    /// Connect to a remote host by its EndpointId.
    pub async fn connect(peer: String) -> Result<ClientHandle, JsValue> {
        let endpoint = Endpoint::bind()
            .await
            .map_err(|e| js_err_msg(&format!("bind: {e}")))?;
        let peer_id: EndpointId = peer
            .parse()
            .map_err(|e: iroh::KeyParsingError| js_err_msg(&format!("parse peer: {e}")))?;
        let (client, notifications) = RemoteClient::connect(&endpoint, peer_id)
            .await
            .map_err(|e| js_err_msg(&format!("connect: {e}")))?;
        Ok(ClientHandle {
            endpoint,
            client,
            peer_id: peer_id.to_string(),
            notifications: Arc::new(Mutex::new(Some(notifications))),
        })
    }

    /// Await the next host-pushed notification. Resolves with a JSON
    /// string shaped like `{"method":"snapshot.invalidated","params":{}}`
    /// or `null` once the stream closes (the caller should stop
    /// looping at that point). Single-consumer: the first caller
    /// takes ownership of the receiver.
    pub async fn next_notification(&self) -> Result<Option<String>, JsValue> {
        let mut slot = self.notifications.lock().await;
        let Some(rx) = slot.as_mut() else {
            return Ok(None);
        };
        match rx.recv().await {
            Some(note) => {
                let v = serde_json::json!({
                    "method": note.method,
                    "params": note.params,
                });
                Ok(Some(v.to_string()))
            }
            None => {
                *slot = None;
                Ok(None)
            }
        }
    }

    /// Request pairing — returns credentials the caller should persist
    /// and supply on subsequent connections via `session_hello_auth`.
    pub async fn pairing_request(
        &self,
        device_label: String,
    ) -> Result<DeviceCredential, JsValue> {
        use superhq_remote_proto::methods::PairingRequestParams;
        let result = self
            .client
            .pairing_request(PairingRequestParams {
                device_label,
                totp_code: None,
            })
            .await
            .map_err(js_err)?;
        Ok(DeviceCredential {
            device_id: result.device_id,
            device_key_b64: result.device_key,
        })
    }

    /// `session.hello` with a device credential. Calls
    /// `session.challenge` first to get a nonce, then HMACs the nonce
    /// with the device key and sends the proof in `session.hello`.
    pub async fn session_hello_auth(
        &self,
        device_label: String,
        credential: &DeviceCredential,
    ) -> Result<String, JsValue> {
        use crate::auth;
        use superhq_remote_proto::{
            methods::{SessionAuth, SessionHelloParams},
            PROTOCOL_VERSION,
        };
        let key = auth::decode_device_key(&credential.device_key_b64)
            .map_err(|e| js_err_msg(e))?;
        let challenge = self.client.session_challenge().await.map_err(js_err)?;
        let nonce = auth::decode_nonce(&challenge.nonce).map_err(|e| js_err_msg(e))?;
        let proof =
            auth::compute_proof(&key, &self.peer_id, &credential.device_id, &nonce)
                .map_err(|e| js_err_msg(e))?;
        let result = self
            .client
            .session_hello(SessionHelloParams {
                protocol_version: PROTOCOL_VERSION,
                device_label,
                resume_token: None,
                auth: Some(SessionAuth {
                    device_id: credential.device_id.clone(),
                    proof,
                }),
            })
            .await
            .map_err(js_err)?;
        serde_json::to_string(&result).map_err(|e| js_err_msg(&e.to_string()))
    }

    /// Our local endpoint id (for display only).
    pub fn endpoint_id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Call `session.hello` without auth. Only works if the host has
    /// `SUPERHQ_REMOTE_REQUIRE_AUTH` unset.
    pub async fn session_hello(&self, device_label: String) -> Result<String, JsValue> {
        use superhq_remote_proto::{methods::SessionHelloParams, PROTOCOL_VERSION};
        let result = self
            .client
            .session_hello(SessionHelloParams {
                protocol_version: PROTOCOL_VERSION,
                device_label,
                resume_token: None,
                auth: None,
            })
            .await
            .map_err(js_err)?;
        serde_json::to_string(&result).map_err(|e| js_err_msg(&e.to_string()))
    }

    /// Call `workspaces.list`. Returns JSON string of the workspace list.
    pub async fn workspaces_list(&self) -> Result<String, JsValue> {
        let ws = self.client.workspaces_list().await.map_err(js_err)?;
        serde_json::to_string(&ws).map_err(|e| js_err_msg(&e.to_string()))
    }

    /// Call `workspace.activate`. Returns JSON string of the activated
    /// workspace plus its tabs right after the sandbox spins up.
    pub async fn workspace_activate(
        &self,
        workspace_id: i64,
    ) -> Result<String, JsValue> {
        use superhq_remote_proto::methods::WorkspaceActivateParams;
        let result = self
            .client
            .workspace_activate(WorkspaceActivateParams { workspace_id })
            .await
            .map_err(js_err)?;
        serde_json::to_string(&result).map_err(|e| js_err_msg(&e.to_string()))
    }

    /// Call `tabs.list`. Returns JSON string of the tab list.
    pub async fn tabs_list(&self) -> Result<String, JsValue> {
        let tabs = self.client.tabs_list().await.map_err(js_err)?;
        serde_json::to_string(&tabs).map_err(|e| js_err_msg(&e.to_string()))
    }

    /// Call `tabs.create`. `spec_json` is a JSON string encoding a
    /// `TabCreateSpec` value — we take it as JSON so wasm-bindgen
    /// doesn't have to project every enum variant. Shapes:
    ///   `{"kind":"host_shell"}`
    ///   `{"kind":"guest_shell","parent_tab_id":<u64>}`
    ///   `{"kind":"agent"}` or `{"kind":"agent","agent_id":<i64>}`
    pub async fn tabs_create(
        &self,
        workspace_id: i64,
        spec_json: String,
    ) -> Result<String, JsValue> {
        use superhq_remote_proto::methods::{TabCreateSpec, TabsCreateParams};
        let spec: TabCreateSpec = serde_json::from_str(&spec_json)
            .map_err(|e| js_err_msg(&format!("parse spec: {e}")))?;
        let result = self
            .client
            .tabs_create(TabsCreateParams {
                workspace_id,
                spec,
            })
            .await
            .map_err(js_err)?;
        serde_json::to_string(&result).map_err(|e| js_err_msg(&e.to_string()))
    }

    /// Close a tab. `mode` is `"checkpoint"` (snapshot the sandbox and
    /// leave a stopped row) or `"force"` (tear the tab down).
    pub async fn tabs_close(
        &self,
        workspace_id: i64,
        tab_id: u64,
        mode: String,
    ) -> Result<(), JsValue> {
        use superhq_remote_proto::methods::{TabCloseMode, TabsCloseParams};
        let mode = match mode.as_str() {
            "checkpoint" => TabCloseMode::Checkpoint,
            "force" => TabCloseMode::Force,
            other => {
                return Err(js_err_msg(&format!(
                    "unknown close mode: {other}"
                )))
            }
        };
        self.client
            .tabs_close(TabsCloseParams {
                workspace_id,
                tab_id,
                mode,
            })
            .await
            .map_err(js_err)
    }

    /// End-to-end PTY test: attach, open data stream, send bytes, read echo,
    /// return what came back. Used by the demo page to prove the full
    /// stream lifecycle works from the browser.
    pub async fn pty_echo_test(
        &self,
        workspace_id: i64,
        tab_id: u64,
        payload: Uint8Array,
    ) -> Result<Uint8Array, JsValue> {
        use superhq_remote_proto::methods::PtyAttachParams;
        let attach = self
            .client
            .pty_attach(PtyAttachParams {
                workspace_id,
                tab_id,
                cols: None,
                rows: None,
            })
            .await
            .map_err(js_err)?;

        let (mut send, mut recv) = self
            .client
            .open_pty_stream(workspace_id, tab_id, attach.cols, attach.rows)
            .await
            .map_err(|e| js_err_msg(&format!("open pty stream: {e}")))?;

        let payload_bytes = uint8array_to_vec(&payload);
        send.write_all(&payload_bytes)
            .await
            .map_err(|e| js_err_msg(&format!("write: {e}")))?;
        send.finish()
            .map_err(|e| js_err_msg(&format!("finish: {e}")))?;

        let mut got = Vec::with_capacity(payload_bytes.len());
        let mut tmp = [0u8; 4096];
        while got.len() < payload_bytes.len() {
            match recv.read(&mut tmp).await {
                Ok(Some(0)) | Ok(None) => break,
                Ok(Some(n)) => got.extend_from_slice(&tmp[..n]),
                Err(e) => return Err(js_err_msg(&format!("read: {e}"))),
            }
        }
        Ok(vec_to_uint8array(&got))
    }

    /// Open a persistent PTY stream for the given tab. Returns a handle
    /// whose `read_chunk` / `write` / `resize` methods can be driven from
    /// JS (e.g. wired to xterm.js).
    pub async fn open_pty(
        &self,
        workspace_id: i64,
        tab_id: u64,
        cols: u16,
        rows: u16,
    ) -> Result<PtyStreamHandle, JsValue> {
        use superhq_remote_proto::methods::PtyAttachParams;
        // Advertise our xterm size on attach. The host aggregates per
        // client and sizes the PTY to the minimum across all attached
        // clients, so we may get back different effective dims.
        let attach = self
            .client
            .pty_attach(PtyAttachParams {
                workspace_id,
                tab_id,
                cols: Some(cols),
                rows: Some(rows),
            })
            .await
            .map_err(js_err)?;
        let (send, recv) = self
            .client
            .open_pty_stream(workspace_id, tab_id, attach.cols, attach.rows)
            .await
            .map_err(|e| js_err_msg(&format!("open pty stream: {e}")))?;
        let client = self.client.clone();
        Ok(PtyStreamHandle {
            send: Arc::new(Mutex::new(send)),
            recv: Arc::new(Mutex::new(recv)),
            workspace_id,
            tab_id,
            client,
        })
    }

    /// Close the connection.
    pub fn close(&self) {
        self.client.close();
    }

    /// Upload a binary attachment (typically an image) to the host
    /// and have it type the resulting path into the tab's PTY.
    /// Returns the host-side absolute path as a string.
    pub async fn upload_attachment(
        &self,
        workspace_id: i64,
        tab_id: u64,
        name: String,
        mime: Option<String>,
        bytes: Uint8Array,
    ) -> Result<String, JsValue> {
        let raw = uint8array_to_vec(&bytes);
        self.client
            .upload_attachment(workspace_id, tab_id, name, mime, raw)
            .await
            .map_err(|e| js_err_msg(&format!("upload_attachment: {e}")))
    }
}

/// JS-facing handle for a live PTY stream. Read chunks incrementally,
/// write input bytes, push resize events.
#[wasm_bindgen]
pub struct PtyStreamHandle {
    send: Arc<Mutex<SendStream>>,
    recv: Arc<Mutex<RecvStream>>,
    workspace_id: i64,
    tab_id: u64,
    client: RemoteClient,
}

#[wasm_bindgen]
impl PtyStreamHandle {
    /// Await the next chunk of bytes from the PTY. Returns a `Uint8Array`;
    /// an empty array means the stream closed (EOF).
    pub async fn read_chunk(&self) -> Result<Uint8Array, JsValue> {
        let mut recv = self.recv.lock().await;
        let mut buf = [0u8; 8192];
        match recv.read(&mut buf).await {
            Ok(Some(0)) | Ok(None) => Ok(Uint8Array::new_with_length(0)),
            Ok(Some(n)) => {
                let arr = Uint8Array::new_with_length(n as u32);
                arr.copy_from(&buf[..n]);
                Ok(arr)
            }
            Err(e) => Err(js_err_msg(&format!("read: {e}"))),
        }
    }

    /// Write input bytes into the PTY.
    pub async fn write(&self, data: Uint8Array) -> Result<(), JsValue> {
        let bytes = uint8array_to_vec(&data);
        let mut send = self.send.lock().await;
        send.write_all(&bytes)
            .await
            .map_err(|e| js_err_msg(&format!("write: {e}")))?;
        Ok(())
    }

    /// Tell the host to resize the PTY to `cols × rows`.
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), JsValue> {
        use superhq_remote_proto::methods::PtyResizeParams;
        self.client
            .pty_resize(PtyResizeParams {
                workspace_id: self.workspace_id,
                tab_id: self.tab_id,
                cols,
                rows,
            })
            .await
            .map_err(js_err)?;
        Ok(())
    }
}

/// Plain-text JS Error helper — wraps a string message into a
/// throwable JS `Error`. Equivalent to the old `js_err_msg(msg)`
/// but typed as `JsValue` so it slots into methods that now return
/// `Result<T, JsValue>`.
fn js_err_msg(msg: impl AsRef<str>) -> JsValue {
    js_sys::Error::new(msg.as_ref()).into()
}

/// Map an `RpcCallError` to a JS `Error` with extra machine-readable
/// properties so callers don't have to string-match:
///
///   err.kind    — "transport" | "rpc" | "codec" | "closed" | "timeout"
///   err.code    — numeric JSON-RPC code (only present when kind="rpc")
///   err.message — human-readable summary (same as err.toString())
///
/// Previously this returned a plain `JsError` built from `e.to_string()`,
/// which threw away `RpcError.code` and forced the JS side to parse the
/// message to distinguish auth failures from transport drops.
fn js_err(e: crate::RpcCallError) -> JsValue {
    let err = js_sys::Error::new(&e.to_string());
    let (kind, code) = match &e {
        crate::RpcCallError::Transport(_) => ("transport", None),
        crate::RpcCallError::Rpc(r) => ("rpc", Some(r.code)),
        crate::RpcCallError::Codec(_) => ("codec", None),
        crate::RpcCallError::Closed => ("closed", None),
        crate::RpcCallError::Timeout(_) => ("timeout", None),
    };
    let _ = js_sys::Reflect::set(
        &err,
        &JsValue::from_str("kind"),
        &JsValue::from_str(kind),
    );
    if let Some(c) = code {
        let _ = js_sys::Reflect::set(
            &err,
            &JsValue::from_str("code"),
            &JsValue::from_f64(c as f64),
        );
    }
    err.into()
}

fn uint8array_to_vec(a: &Uint8Array) -> Vec<u8> {
    let mut v = vec![0u8; a.length() as usize];
    a.copy_to(&mut v[..]);
    v
}

fn vec_to_uint8array(bytes: &[u8]) -> Uint8Array {
    let a = Uint8Array::new_with_length(bytes.len() as u32);
    a.copy_from(bytes);
    a
}
