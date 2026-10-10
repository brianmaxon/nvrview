//! Bounded, rate-limited file logging for nvrview: rotation capped by file count and total size.
//!
//! The rest of the program just uses `tracing::info!` and friends; call
//! [`init`] once at startup and keep the returned [`LogGuard`] alive.
//!
//! Why this exists: a vendor client once logged about 9,000 identical error
//! lines per second and rotated its logs by renaming with an ever-increasing
//! index without deleting any, filling tens of gigabytes within hours. Here:
//!
//! - [`RotatingFileWriter`] keeps `<stem>.log` plus `<stem>.1.log` ... and
//!   deletes the oldest files beyond a file-count cap and a total-size cap.
//!   Caps are also enforced at startup on leftovers from a previous run.
//! - [`RateLimitLayer`] allows a burst of events per call site, then a trickle,
//!   and emits one `suppressed N similar messages` line for the rest.
//! - Writing happens on a dedicated thread behind a bounded queue, so a video
//!   callback thread never waits on disk. When the queue is full, or the disk
//!   fails, lines are dropped and counted ([`LogStats`]); logging never panics.
//! - [`Redacted`] and [`RedactedTail`] keep secrets out of log lines.
//!
//! # What must never be logged
//!
//! Passwords, nonces, keys, and tokens must not be logged at any level; wrap
//! them in [`Redacted`] if they might end up in a formatted value. Device UIDs
//! should be shown with [`RedactedTail`]. Network addresses and hostnames
//! belong at `debug` level only, never `info` or above, because default logs
//! are routinely attached to bug reports.

mod nonblocking;
mod rotate;
mod stats;

pub use nonblocking::{NonBlockingWriter, WriterThread};
pub use rotate::{RotatingFileWriter, RotationConfig, total_bytes_on_disk};
pub use stats::{LogStats, LogStatsSnapshot};
