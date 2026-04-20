//! End-to-end integration test: spawn a host, connect with a raw iroh
//! client in the same process, exercise the control-stream RPC surface
//! against the `StubHandler`.

use anyhow::Result;
use iroh::{discovery::static_provider::StaticProvider, Endpoint};
use superhq_remote_proto::{
    decode, encode_request,
    methods::{self, Ack},
    stream::{StreamInit, STREAM_INIT},
    Message, Request, ALPN, PROTOCOL_VERSION,
};
use superhq_remote_host::{RemoteServer, StubHandler};
use tokio::io::{AsyncBufReadExt, BufReader};

/// Build a server and a client endpoint that know how to reach each other
/// directly via `StaticProvider`, so tests don't depend on DNS publishing.
async fn setup() -> Result<(RemoteServer, Endpoint)> {
    let server = RemoteServer::spawn(StubHandler::default()).await?;
    // Wait until the server has at least one reachable address.
    server.endpoint().online().await;
    let server_addr = server.endpoint().addr();

    let static_disco = StaticProvider::new();
    static_disco.add_endpoint_info(server_addr);

    let client = Endpoint::builder().discovery(static_disco).bind().await?;
    Ok((server, client))
}

/// Send a `session.hello` (no auth) and wait for its response, so the
/// connection is considered authenticated for subsequent calls.
async fn hello<R>(send: &mut iroh::endpoint::SendStream, reader: &mut BufReader<R>) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let req = Request::new(
        1000,
        methods::SESSION_HELLO,
        serde_json::to_value(methods::SessionHelloParams {
            protocol_version: PROTOCOL_VERSION,
            device_label: "test".into(),
            resume_token: None,
            auth: None,
        })?,
    );
    send.write_all(encode_request(&req)?.as_bytes()).await?;
    send.write_all(b"\n").await?;
    let mut buf = Vec::new();
    reader.read_until(b'\n', &mut buf).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_roundtrip() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,iroh=warn".into()),
        )
        .try_init();

    let (server, client) = setup().await?;
    let server_id = server.endpoint_id();

    let conn = client.connect(server_id, ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;

    // session.hello
    let hello = Request::new(
        1,
        methods::SESSION_HELLO,
        serde_json::to_value(methods::SessionHelloParams {
            protocol_version: PROTOCOL_VERSION,
            device_label: "test client".into(),
            resume_token: None,
            auth: None,
        })?,
    );
    send.write_all(encode_request(&hello)?.as_bytes()).await?;
    send.write_all(b"\n").await?;

    let mut reader = BufReader::new(recv);
    let mut buf = Vec::new();
    reader.read_until(b'\n', &mut buf).await?;
    let text = std::str::from_utf8(&buf)?.trim_end();
    let msg = decode(text)?;

    match msg {
        Message::Response(resp) => {
            assert_eq!(resp.id, 1);
            assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
            let result: methods::SessionHelloResult =
                serde_json::from_value(resp.result.unwrap())?;
            assert_eq!(result.protocol_version, PROTOCOL_VERSION);
            assert!(!result.session_id.is_empty());
            assert!(!result.resume_token.is_empty());
            assert_eq!(result.tabs.len(), 0);
        }
        other => panic!("expected response, got {other:?}"),
    }

    drop(send);
    drop(reader);
    conn.close(0u32.into(), b"done");
    server.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn tabs_list_is_empty() -> Result<()> {
    let (server, client) = setup().await?;
    let server_id = server.endpoint_id();

    let conn = client.connect(server_id, ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut reader = BufReader::new(recv);
    hello(&mut send, &mut reader).await?;

    let req = Request::new(7, methods::TABS_LIST, serde_json::json!({}));
    send.write_all(encode_request(&req)?.as_bytes()).await?;
    send.write_all(b"\n").await?;

    let mut buf = Vec::new();
    reader.read_until(b'\n', &mut buf).await?;
    let text = std::str::from_utf8(&buf)?.trim_end();
    let msg = decode(text)?;

    match msg {
        Message::Response(resp) => {
            assert_eq!(resp.id, 7);
            let tabs: methods::TabsListResult =
                serde_json::from_value(resp.result.unwrap())?;
            assert_eq!(tabs.len(), 0);
        }
        other => panic!("expected response, got {other:?}"),
    }

    drop(send);
    drop(reader);
    conn.close(0u32.into(), b"done");
    server.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn pty_echo_through_stream() -> Result<()> {
    let (server, client) = setup().await?;
    let server_id = server.endpoint_id();

    let conn = client.connect(server_id, ALPN).await?;

    // Control stream: open first, authenticate, then call pty.attach.
    let (mut ctrl_send, ctrl_recv) = conn.open_bi().await?;
    let mut ctrl_reader = BufReader::new(ctrl_recv);
    hello(&mut ctrl_send, &mut ctrl_reader).await?;

    let attach_req = Request::new(
        1,
        methods::PTY_ATTACH,
        serde_json::to_value(methods::PtyAttachParams { workspace_id: 1, tab_id: 42 })?,
    );
    ctrl_send.write_all(encode_request(&attach_req)?.as_bytes()).await?;
    ctrl_send.write_all(b"\n").await?;

    let mut line = Vec::new();
    ctrl_reader.read_until(b'\n', &mut line).await?;
    let text = std::str::from_utf8(&line)?.trim_end();
    let result: methods::PtyAttachResult = match decode(text)? {
        Message::Response(r) => {
            assert!(r.error.is_none(), "attach failed: {:?}", r.error);
            serde_json::from_value(r.result.unwrap())?
        }
        other => panic!("expected response, got {other:?}"),
    };
    assert_eq!(result.cols, 80);
    assert_eq!(result.rows, 24);

    // Open a second bidirectional stream for the PTY data.
    let (mut pty_send, mut pty_recv) = conn.open_bi().await?;

    // Send StreamInit::Pty as a JSON-RPC stream.init request.
    let init_req = Request::new(
        1,
        STREAM_INIT,
        serde_json::to_value(StreamInit::Pty { workspace_id: 1, tab_id: 42, cols: 80, rows: 24 })?,
    );
    pty_send.write_all(encode_request(&init_req)?.as_bytes()).await?;
    pty_send.write_all(b"\n").await?;

    // Read the init ack.
    let mut pty_reader = BufReader::new(&mut pty_recv);
    let mut ack_line = Vec::new();
    pty_reader.read_until(b'\n', &mut ack_line).await?;
    let ack_text = std::str::from_utf8(&ack_line)?.trim_end();
    match decode(ack_text)? {
        Message::Response(r) => {
            assert!(r.error.is_none(), "stream.init failed: {:?}", r.error);
            let ack: Ack = serde_json::from_value(r.result.unwrap())?;
            assert!(ack.ok);
        }
        other => panic!("expected response, got {other:?}"),
    }
    drop(pty_reader);

    // Now the stream is in raw-bytes mode. Write some bytes; server echoes.
    let payload = b"hello terminal\n";
    pty_send.write_all(payload).await?;
    pty_send.finish()?;

    // Read echo back. read_to_end here because the server closes send on EOF.
    let mut buf = Vec::new();
    while buf.len() < payload.len() {
        let mut tmp = [0u8; 1024];
        match pty_recv.read(&mut tmp).await? {
            None | Some(0) => break,
            Some(n) => buf.extend_from_slice(&tmp[..n]),
        }
    }
    assert_eq!(&buf, payload, "echo mismatch");

    conn.close(0u32.into(), b"done");
    server.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn tabs_list_pre_hello_is_rejected() -> Result<()> {
    let (server, client) = setup().await?;
    let server_id = server.endpoint_id();

    let conn = client.connect(server_id, ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;

    // Skip session.hello — go straight to tabs.list.
    let req = Request::new(42, methods::TABS_LIST, serde_json::json!({}));
    send.write_all(encode_request(&req)?.as_bytes()).await?;
    send.write_all(b"\n").await?;

    let mut reader = BufReader::new(recv);
    let mut buf = Vec::new();
    reader.read_until(b'\n', &mut buf).await?;
    let text = std::str::from_utf8(&buf)?.trim_end();
    let msg = decode(text)?;
    match msg {
        Message::Response(resp) => {
            assert_eq!(resp.id, 42);
            let err = resp.error.expect("expected error");
            assert_eq!(err.code, superhq_remote_proto::error_code::AUTH_REQUIRED);
        }
        other => panic!("expected response, got {other:?}"),
    }

    drop(send);
    drop(reader);
    conn.close(0u32.into(), b"done");
    server.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_method_returns_error() -> Result<()> {
    let (server, client) = setup().await?;
    let server_id = server.endpoint_id();

    let conn = client.connect(server_id, ALPN).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut reader = BufReader::new(recv);
    hello(&mut send, &mut reader).await?;

    let req = Request::new(99, "does.not.exist", serde_json::json!({}));
    send.write_all(encode_request(&req)?.as_bytes()).await?;
    send.write_all(b"\n").await?;

    let mut buf = Vec::new();
    reader.read_until(b'\n', &mut buf).await?;
    let text = std::str::from_utf8(&buf)?.trim_end();
    let msg = decode(text)?;

    match msg {
        Message::Response(resp) => {
            assert_eq!(resp.id, 99);
            let err = resp.error.expect("expected error");
            assert_eq!(
                err.code,
                superhq_remote_proto::error_code::METHOD_NOT_FOUND
            );
        }
        other => panic!("expected response, got {other:?}"),
    }

    drop(send);
    drop(reader);
    conn.close(0u32.into(), b"done");
    server.shutdown().await?;
    Ok(())
}
