//! Adapters to bridge shuru-sdk's ShellWriter/ShellReader to std::io::Read/Write
//! so they can be used with gpui-terminal.

use bytes::Bytes;
use shuru_sdk::{ShellEvent, ShellReader, ShellWriter};
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle as TokioHandle;

/// Default cap on scrollback bytes kept for late-attaching remote clients.
const SCROLLBACK_CAP: usize = 256 * 1024;

/// Ring buffer of raw PTY output bytes. Drops oldest bytes when full.
pub struct ScrollbackRing {
    buffer: Vec<u8>,
    cap: usize,
}

impl ScrollbackRing {
    pub fn new(cap: usize) -> Self {
        Self { buffer: Vec::new(), cap }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > self.cap {
            let drop_n = self.buffer.len() - self.cap;
            self.buffer.drain(..drop_n);
        }
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.buffer.clone()
    }
}

/// Wraps ShellWriter to implement std::io::Write.
/// gpui-terminal writes keyboard input here → forwarded to shuru VM.
pub struct ShuruPtyWriter {
    inner: ShellWriter,
}

impl ShuruPtyWriter {
    pub fn new(writer: ShellWriter) -> Self {
        Self { inner: writer }
    }
}

impl Write for ShuruPtyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner
            .send_input(buf)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Wraps ShellReader to implement std::io::Read.
/// gpui-terminal reads terminal output from here ← received from shuru VM.
///
/// Since ShellReader::recv() is async, we use a background thread that
/// receives events and buffers them for synchronous reads.
///
/// Also exposes a `tap()` that returns a `broadcast::Sender<Bytes>` — every
/// output chunk is published to the broadcast before being put on the sync
/// channel, so remote-control subscribers can receive the same bytes the
/// local terminal sees.
pub struct ShuruPtyReader {
    buffer: Vec<u8>,
    buf_pos: usize,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    tap: tokio::sync::broadcast::Sender<Bytes>,
    scrollback: Arc<Mutex<ScrollbackRing>>,
}

impl ShuruPtyReader {
    /// Create a new reader that drains events from the ShellReader in a background task.
    pub fn new(mut reader: ShellReader, tokio_handle: TokioHandle) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        // Broadcast buffer of 1024: if a subscriber lags, old chunks are
        // dropped for them (they see `RecvError::Lagged`) but the source
        // keeps flowing. That's the right tradeoff — we never want the
        // local terminal to stall because a remote client is slow.
        let (tap_tx, _tap_rx) = tokio::sync::broadcast::channel(1024);
        let tap_for_thread = tap_tx.clone();

        let scrollback = Arc::new(Mutex::new(ScrollbackRing::new(SCROLLBACK_CAP)));
        let sb_for_thread = scrollback.clone();

        // Spawn a tokio task to read from the async ShellReader and
        // forward output bytes to: (1) the synchronous channel for the
        // local terminal, (2) the scrollback ring, and (3) the broadcast
        // tap for remote subscribers. Scrollback push and broadcast send
        // happen under the same mutex so an attach handler can snapshot
        // + subscribe atomically (no duplicates, no lost bytes).
        std::thread::Builder::new()
            .name("shuru-pty-bridge".into())
            .spawn(move || {
                tokio_handle.block_on(async move {
                    while let Some(event) = reader.recv().await {
                        match event {
                            ShellEvent::Output(data) => {
                                let bytes = Bytes::copy_from_slice(&data);
                                // Hold scrollback lock while broadcasting so
                                // an attach handler either sees this byte in
                                // the snapshot OR in the broadcast, never both.
                                if let Ok(mut sb) = sb_for_thread.lock() {
                                    sb.push(&data);
                                    let _ = tap_for_thread.send(bytes);
                                }
                                if tx.send(data).is_err() {
                                    break;
                                }
                            }
                            ShellEvent::Exit(_) | ShellEvent::Error(_) => {
                                break;
                            }
                        }
                    }
                });
            })
            .expect("Failed to spawn shuru PTY bridge thread");

        Self {
            buffer: Vec::new(),
            buf_pos: 0,
            rx,
            tap: tap_tx,
            scrollback,
        }
    }

    /// Returns a clone of the broadcast sender so callers can subscribe
    /// to the same byte stream the local terminal consumes.
    pub fn tap(&self) -> tokio::sync::broadcast::Sender<Bytes> {
        self.tap.clone()
    }

    /// Returns a clone of the scrollback handle. Callers that hold the
    /// lock + subscribe to `tap()` atomically can capture recent output
    /// for a late-attaching client with no duplicates or gaps.
    pub fn scrollback(&self) -> Arc<Mutex<ScrollbackRing>> {
        self.scrollback.clone()
    }
}

impl Read for ShuruPtyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // If we have buffered data, return it first
        if self.buf_pos < self.buffer.len() {
            let available = &self.buffer[self.buf_pos..];
            let to_copy = available.len().min(buf.len());
            buf[..to_copy].copy_from_slice(&available[..to_copy]);
            self.buf_pos += to_copy;
            if self.buf_pos >= self.buffer.len() {
                self.buffer.clear();
                self.buf_pos = 0;
            }
            return Ok(to_copy);
        }

        // Block waiting for next chunk from the VM
        match self.rx.recv() {
            Ok(data) => {
                let to_copy = data.len().min(buf.len());
                buf[..to_copy].copy_from_slice(&data[..to_copy]);
                if to_copy < data.len() {
                    self.buffer = data;
                    self.buf_pos = to_copy;
                }
                Ok(to_copy)
            }
            Err(_) => Ok(0), // Channel closed = EOF
        }
    }
}

