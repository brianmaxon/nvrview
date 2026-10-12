//! Smart notifications for nvrview: the rules engine.
//!
//! Sans-I/O: no clocks, threads, sockets, or files. The host passes in
//! every time (UTC milliseconds and its current UTC offset), every picture
//! check result, and every learned input, and gets back decisions with a
//! step-by-step trace and an explanation. The design is in
//! `docs/NOTIFICATIONS.md`.
//!
//! - [`trigger`]: intake and normalization (device AI and status kinds,
//!   local audio, external sensors; unknown kinds pass through).
//! - [`engine`]: the gate, check bookkeeping, rules, timeliness, and the
//!   decision.
//! - [`rules`]: night, schedules, severity, always-alert rules.
//! - [`sun`]: sunrise and sunset (NOAA equations) and "is it night".
//! - [`episode`]: episodes per camera and class, cooldowns, updates.
//! - [`verify`]: k-of-n counting over sampled pictures; where detector
//!   results and learned rarity plug in.
//! - [`external`]: confirmation windows and per-source health.
//! - [`trace`], [`explain`], [`decision`]: what the app shows and logs.
//! - [`settings`]: the settings document, defaults, and validation.

pub mod decision;
pub mod engine;
pub mod episode;
pub mod explain;
pub mod external;
pub mod model;
pub mod rules;
pub mod settings;
pub mod sun;
pub mod time;
pub mod trace;
pub mod trigger;
pub mod verify;
pub mod zone;

pub use decision::{Decision, Outcome, QuietReason, SuppressReason};
pub use engine::{DecideInputs, Engine, Intake, Next, Now, SampleProgress, Tick};
pub use model::{CameraId, Class, Exposure, Kind, Mode, Severity, Source};
pub use settings::AlertSettings;
pub use trace::{Step, StepKind, Trace};
pub use trigger::{Trigger, TriggerState};
pub use verify::{Detection, Rarity, RarityModel, Sample};
