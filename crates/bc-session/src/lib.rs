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
//!   ever sent (login, preview start and stop, keepalive, and the read-only
//!   info queries behind [`DeviceSession::query`]).
//! - **Bounded memory**: each [`Subscription`] has a fixed-capacity queue; a
//!   slow consumer loses frames up to the next keyframe, and the loss is
//!   reported ([`Item::Discontinuity`], [`Subscription::dropped`]).
//! - **No retry storm on bad credentials**: a refused login ends the session
//!   in [`ConnState::LoginFailed`] and is not retried; devices lock out after
//!   a few failures.
//! - **Quiet logs**: repeated connection failures are logged at most about
//!   once every few seconds, with a count of the suppressed ones. Credentials
//!   are never logged; addresses are not logged at all.

mod config;
mod queue;
mod supervisor;
mod transport;

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
