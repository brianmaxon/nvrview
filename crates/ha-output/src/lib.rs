//! Home Assistant output for smart notifications (docs/NOTIFICATIONS.md,
//! "Home Assistant output").
//!
//! The host (viewer-core) builds a [`HaOutput`] from a [`Config`] and a
//! [`Token`] it read from the keychain, then calls [`HaOutput::deliver`] with
//! an [`Alert`] (blocking; it sleeps for the pulse gap) or hosts it on a
//! [`Worker`] thread with a bounded queue. The result is a structured
//! [`OutputResult`] the host records as a `delivery` trace step.
//!
//! This crate has no dependency on viewer-core. Time and sleeping go through
//! [`Clock`], and the network through [`Http`], so tests run without waiting.
//! The token lives in memory only, prints as redacted, and never appears in
//! results or logs.

mod config;
mod http;
mod output;
mod template;
mod worker;

pub use config::{Action, Config, ConfigError, MatchMode, Rule};
pub use http::{Http, HttpRequest, HttpResponse, Method, NetError, Token, UreqHttp, host_is_local};
pub use output::{
    Alert, Attempt, Clock, DecisionKind, EntityResult, EntityStatus, ErrorKind, HaOutput, LogSink,
    OutputResult, Step, SystemClock,
};
pub use worker::{SubmitError, Worker};
