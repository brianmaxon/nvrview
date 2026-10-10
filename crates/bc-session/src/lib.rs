//! Async device sessions for the Baichuan protocol, on top of the sans-I/O
//! [`bc_protocol::session::Session`].
//!
//! One [`DeviceSession`] is one device: a single supervisor task owns the one
//! TCP connection, logs in, sends keepalives, carries every channel's preview,
//! and reconnects with exponential backoff when the connection fails. Because
//! only that task ever connects, attempts cannot stack.
//!
//! Guarantees:
//!
//! - **One connection per device**, closed on every exit path: [`DeviceSession::close`],
//!   dropping the handle, and a failed connection all end with the previous
//!   socket gone before any new one opens. A graceful close sends a preview
//!   stop for every active stream first.
//! - **One preview per stream**: subscriptions to the same channel and tier
//!   share a single device preview, started by the first subscriber and
//!   stopped when the last one is dropped. The stream is demultiplexed once;
//!   each subscriber has its own queue, keyframe gating, and drop counters, so
//!   a slow one never affects another. A subscriber that attaches mid-stream
//!   gets the stream info, then video from the next keyframe.
//! - **Read-only**: only commands the protocol crate's allowlist can build are
//!   ever sent (login, preview start and stop, keepalive, the read-only
//!   info queries behind [`DeviceSession::query`], and the recording
//!   searches, cover images, and downloads of [`DeviceSession::search_files`]
//!   and its siblings).
//! - **Recordings share the session**: searches, cover images, and downloads
//!   run on the same connection as live view, routed by message ID; at most
//!   [`SessionConfig::max_searches`] searches and
//!   [`SessionConfig::max_downloads`] downloads run at once (1 each by
//!   default), and dropping or cancelling a [`Download`] sends its stop (144).
//! - **Bounded memory**: each [`Subscription`] has a fixed-capacity queue; a
//!   slow consumer loses frames up to the next keyframe, and the loss is
//!   reported ([`Item::Discontinuity`], [`Subscription::dropped`]).
//! - **No retry storm on bad credentials**: a refused login ends the session
//!   in [`ConnState::LoginFailed`] and is not retried; devices lock out after
//!   a few failures.
//! - **Quiet logs**: repeated connection failures are logged at most about
//!   once every few seconds, with a count of the suppressed ones. Credentials
//!   are never logged; addresses are not logged at all.

mod archive;
mod config;
mod queue;
mod supervisor;
mod transport;

pub use archive::{CoverImage, Download, DownloadError, DownloadEvent, SearchError};
pub use config::{BackoffConfig, SessionConfig};
pub use queue::{Item, Subscription};
pub use supervisor::{ConnState, DeviceSession, QueryError, QueryReply, SessionClosed};
pub use transport::{Connector, TcpConnector};

use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

/// State shared between the handle, subscriptions, and the supervisor.
pub(crate) struct Shared {
    pub(crate) closing: AtomicBool,
    pub(crate) wake: Notify,
}

impl Shared {
    pub(crate) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }
}
