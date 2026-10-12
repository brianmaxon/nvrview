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
//! - **Read-only, but for talk-back**: only commands the protocol crate's allowlist can build are
//!   ever sent (login, preview start and stop, keepalive, the detection
//!   subscription (192) behind [`DeviceSession::alarms`], the read-only
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
//! - **A 400 to a preview is waited out, boundedly**: a third-party camera
//!   behind an NVR answers its preview with 400 and may stream afterwards on
//!   that message ID, so the preview stays open, is restarted (stop, then a
//!   new start: never two outstanding) every
//!   [`SessionConfig::preview_retry_interval`] while no media has arrived,
//!   and is given up after [`SessionConfig::preview_wait_limit`]
//!   ([`Item::WaitingForDevice`], then media or [`Item::StartRejected`]).
//! - **Second-lens probes stop**: [`DeviceSession::probe_second_lens`]
//!   opens one short `externStream` preview on the existing connection and
//!   stops it on every path; a 400 ends it at once (no wait).
//! - **No retry storm on bad credentials**: a refused login ends the session
//!   in [`ConnState::LoginFailed`] and is not retried; devices lock out after
//!   a few failures.
//! - **Talk-back only where allowed**: [`DeviceSession::talk`] sends sound
//!   to a camera's speaker (camera motion control) only on a channel the
//!   caller enabled with [`DeviceSession::set_motion_allowed`], at most
//!   [`SessionConfig::talk_starts_per_minute`] starts per channel, paced in
//!   real time with a bounded buffer, logged at every start and stop, and
//!   closed (command 11) on every exit path: stop, drop, session close, a
//!   withdrawn permission, an idle or overlong talk, and device errors.
//! - **Cameras stop**: camera motion control ([`DeviceSession::ptz_move`],
//!   [`DeviceSession::ptz_recall`], [`DeviceSession::ptz_lens`]) names its
//!   origin: the user's own controls need no enable, automation needs a
//!   per-camera grant ([`DeviceSession::set_motion_grant`]) and yields to
//!   the user. Only what the channel's capabilities offer is sent, from
//!   fixed templates; moves and recalls are rate-limited per camera and per
//!   device, every command is logged, and a move is always followed by
//!   `stop`: on release, a dead-man timer, a maximum hold, a device refusal,
//!   a withdrawn grant, close and drop, and after a reconnect for a move
//!   the lost connection could not stop.
//! - **Quiet logs**: repeated connection failures are logged at most about
//!   once every few seconds, with a count of the suppressed ones. Credentials
//!   are never logged; addresses are not logged at all.

mod archive;
mod config;
mod lens;
mod play;
mod ptz;
mod queue;
mod supervisor;
mod talk;
mod transport;

pub use archive::{CoverImage, Download, DownloadError, DownloadEvent, SearchError};
pub use config::{BackoffConfig, PtzConfig, RateLimit, SessionConfig};
pub use lens::LensProbe;
pub use play::{Play, PlayStart};
pub use ptz::{
    MotionOrigin, PtzCommand, PtzInfo, PtzNotice, PtzOutcome, PtzPending, PtzRefusal, StopCause,
};
pub use queue::{Item, Subscription};
pub use supervisor::{
    AlarmPush, ConnState, DeviceSession, MANUAL_RETRY_FLOOR, QueryError, QueryReply, SessionClosed,
};
pub use talk::{Refusal, SentRecord, StopReason, Talk, TalkObserver, TalkState, TalkStats};
pub use transport::{Connector, FnConnector, TcpConnector};

use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

/// State shared between the handle, subscriptions, and the supervisor.
pub(crate) struct Shared {
    pub(crate) closing: AtomicBool,
    pub(crate) wake: Notify,
    /// Streams the user asked to retry by hand ([`DeviceSession::retry_stream`]);
    /// bounded, deduplicated.
    pub(crate) retry: std::sync::Mutex<Vec<(u8, bc_protocol::stream::StreamTier)>>,
}

impl Shared {
    pub(crate) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }
}
