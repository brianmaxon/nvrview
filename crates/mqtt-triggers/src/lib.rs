//! MQTT input for smart notifications (docs/NOTIFICATIONS.md, "MQTT input").
//!
//! A host starts one [`Handle`] per configured source with [`start`]. Each
//! runs a subscribe-only MQTT 3.1.1 client on its own thread: it connects to
//! the user's broker (TCP or TLS), subscribes to one topic, maps each message
//! to a [`Trigger`] by the source's [`Mapping`], and calls the host's sink.
//! It reconnects with exponential backoff and jitter, and gives up only on
//! an authentication failure (retrying a wrong password locks accounts).
//!
//! The crate knows nothing about the host: the password comes from the host
//! at start (kept in memory only, never in the config), the clock can be
//! replaced, and the only output is the sink, [`Stats`] and an optional,
//! rate-limited log line callback.
//!
//! Payloads are untrusted data. They are size-capped, rate-limited, read
//! only to decide on/off, and never copied into a trigger or a log line.

mod client;
pub mod config;
pub mod mapping;
pub mod messages;
pub mod net;
pub mod packet;

pub use client::{
    Clock, Credentials, ErrorKind, Handle, LogFn, Options, Sink, StartError, Stats, Status,
    SystemClock, Trigger, backoff_ms, start,
};
pub use config::{
    Broker, ConfigError, Limits, Mapping, Mode, Reconnect, RetainedPolicy, SourceConfig, TlsConfig,
};
pub use mapping::TriggerState;
pub use messages::{MessageEntry, MessageOutcome, MessagePage};
