//! Sans-I/O implementation of the Reolink Baichuan protocol: framing, login, encryption, and media records.
//!
//! This crate never opens sockets, spawns threads, or reads a clock. The
//! caller moves bytes between the network and these types:
//!
//! - [`framer`]: split a TCP byte stream into [`framer::RawMessage`]s and
//!   encode outgoing messages (header layout in [`header`]).
//! - [`session`]: turn received messages into typed [`session::Event`]s and
//!   build the outgoing handshake, login, preview, and keepalive messages.
//!   Only commands on the read-only allowlist can be produced.
//! - [`media`]: demultiplex one stream's payload bytes into video, audio, and
//!   info records.
//! - [`archive`]: recorded video on the device: day, file, and event
//!   searches, cover images, and download by time range (bodies and reply
//!   parsers; the session builds the messages).
//! - [`recording`]: a file format for a received stream with arrival times,
//!   for replaying real streams in development.
//! - [`crypto`], [`xml`], [`stream`]: the building blocks used by the above.
//!
//! The protocol facts implemented here are documented in `docs/protocol.md`.

pub mod archive;
pub mod crypto;
pub mod framer;
pub mod header;
pub mod media;
pub mod recording;
pub mod session;
pub mod stream;
pub mod xml;
