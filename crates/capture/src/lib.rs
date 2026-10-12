//! The pure base of local capture (docs/CAPTURE.md): the decisions and
//! arithmetic behind recording a live tile or a doorbell call, with no
//! threads, clocks, devices, or I/O of its own. Every function takes the
//! numbers it needs (timestamps, free bytes, sizes, existing names) and
//! answers; the recorder that wires it to a session, the muxer, and the
//! file system is separate.
//!
//! - [`epoch`]: one recording timeline. Maps video and camera-audio
//!   timestamps (with wrap, restarts, and jumps) and our sent audio's host
//!   times onto one file clock, and places audio chunks at exact sample
//!   positions.
//! - [`policy`]: the segment and gap policy: keyframe-gated starts, gaps
//!   that continue or cut, format changes, caps, and the end of a stream.
//! - [`mixer`]: the call mixer (Visitor, You, and their sum through a soft
//!   limiter) in 48 kHz AAC-ready frames, and the bounded queue to the
//!   app's encoder.
//! - [`caps`]: duration and size caps, the disk guard, finalize planning,
//!   and admission against the count limits.
//! - [`partials`]: the registry of partial files and the recovery plan.
//! - [`naming`]: picture, live, and call file names.
//! - [`tracks`]: which tracks a recording holds and how audio is produced.
//! - [`reason`]: the reason and limit codes the interface reports.
//!
//! Decisions where the contract is silent are written in each module's
//! documentation under "Rules". The crate builds and tests on Linux,
//! macOS, and Windows.

pub mod caps;
pub mod epoch;
pub mod mixer;
pub mod naming;
pub mod partials;
pub mod policy;
pub mod reason;
pub mod tracks;

pub use reason::{LimitKind, Reason};
