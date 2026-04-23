//! Stream initialization framing.
//!
//! Every non-control stream opens with a `StreamInit` message that
//! identifies what the stream carries. Sent as a single JSON-RPC request
//! (method `stream.init`) — server replies with an ack, then the
//! stream-specific protocol takes over (for PTY: raw bytes both ways).

use serde::{Deserialize, Serialize};

use crate::types::{TabId, WorkspaceId};

pub const STREAM_INIT: &str = "stream.init";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamInit {
    /// A terminal stream attached to a specific tab.
    Pty {
        workspace_id: WorkspaceId,
        tab_id: TabId,
        cols: u16,
        rows: u16,
    },
    /// A status event stream (host → client, unidirectional).
    Status,
    /// Upload a binary attachment (typically an image) that the host
    /// saves into the tab's workspace and then types the resulting
    /// path into the PTY. Client writes raw bytes after the ack and
    /// closes the send side; server replies with the final path.
    Attachment {
        workspace_id: WorkspaceId,
        tab_id: TabId,
        /// Base filename (no directory traversal). The host sanitizes
        /// and may adjust to avoid collisions.
        name: String,
        /// MIME hint, e.g. "image/png". Optional, used only for logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime: Option<String>,
        /// Byte length the client intends to send. Lets the server
        /// reject up front if it's over the limit.
        size: u64,
    },
}

/// Result written back on the attachment stream after the upload
/// completes. One line of JSON followed by the send side closing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttachmentResult {
    /// Absolute path the file was saved to.
    pub path: String,
}
