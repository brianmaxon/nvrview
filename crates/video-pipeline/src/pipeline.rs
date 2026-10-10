//! The per-tile decoder state machine described in `docs/RENDERING.md`.
//!
//! One [`TilePipeline`] per displayed stream. The caller feeds it inputs
//! ([`TilePipeline::push`], [`TilePipeline::decoded`],
//! [`TilePipeline::decode_failed`], [`TilePipeline::tick`],
//! [`TilePipeline::discontinuity`]) and after each input drains
//! [`TilePipeline::next_action`] until it returns `None`, carrying every
//! action out synchronously and in order.
//!
//! # Policy
//!
//! - **Gating.** Nothing is decoded until a random access point, judged by
//!   NAL types (H.264 IDR or exact recovery point SEI; H.265 IDR, CRA, BLA).
//!   After a CRA or BLA starts decoding, its RASL pictures are skipped. The
//!   gate closes again after a flush, reset, overflow, discontinuity, codec
//!   switch, or a picture the decoder could not take.
//! - **Format changes.** A random access point whose parameter sets (or
//!   codec) differ from the configured ones produces `Reset` (when a decoder
//!   exists) and `Configure` right before its `Decode`. `Decode` never comes
//!   before a `Configure`, nor after a `Reset` without a new `Configure`.
//! - **Backpressure.** At most `max_in_flight` pictures are submitted without
//!   a result; the rest wait in a queue bounded by count and bytes. On
//!   overflow the queue is dropped and the gate closes until the next random
//!   access point.
//! - **Recovery.** A corrupt picture leads to `Flush` and gating. After
//!   `failures_before_reset` failures with no picture decoded in between,
//!   each further failure resets the decoder, after a cooldown that doubles
//!   each time (capped); at most `max_resets_per_window` resets per window.
//!   Beyond that, or when the decoder rejects the parameter sets, the
//!   tile releases its decoder, clears, and is `Undecodable` until the
//!   stream's parameter sets change, or until `undecodable_retry_us` has
//!   passed: then the next random access point tries the same parameter
//!   sets again (so a decoder that failed for a transient reason, such as
//!   the system running out of hardware sessions, comes back on its own).
//!   Every decoder creation (including
//!   format changes) is also capped per window, so no input can make the
//!   pipeline rebuild decoders in a tight loop. A failed `Configure` is
//!   reported through `decode_failed` like a picture failure.
//! - **Results.** Every `Decode` gets exactly one `decoded` or
//!   `decode_failed`, except pictures still in flight when a `Flush`,
//!   `Reset`, or `Configure` runs: the platform discards those silently, and
//!   the pipeline stops waiting for them at that point.
//! - **Logging.** Warnings go through `tracing`, at most `log_burst` per
//!   `log_window_us` per tile; the next allowed line reports how many were
//!   suppressed. Everything else is in [`TileStats`].
//!
//! # Time
//!
//! The pipeline never reads a clock. [`TilePipeline::tick`] supplies a
//! monotonic time in microseconds from any fixed origin; call it regularly
//! (on every push, or every display refresh). Cooldowns and stall detection
//! only advance when it is called.
//!
//! # Memory
//!
//! Access units are moved in, never copied: [`Action::Decode`] borrows the
//! bytes from the pipeline until the next call that takes `&mut self`. The
//! only copies are parameter sets (a few small NAL units per format change),
//! kept so a reset decoder can be configured again without waiting.

use std::collections::VecDeque;

use tracing::{info, warn};

use crate::color::ColorInfo;
use crate::format::{ParameterSets, VideoFormat};
use crate::geometry::{Rect, Size};
use crate::nal::{Codec, RandomAccess, analyze};

/// One picture as received from the device: Annex-B, as sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessUnit {
    /// Codec the device declared for this stream.
    pub codec: Codec,
    /// The device's keyframe flag. Gating trusts the NAL types instead; a
    /// disagreement is only counted.
    pub keyframe: bool,
    /// Presentation timestamp in microseconds. Should be unique among the
    /// pictures in flight; decode results are matched by it.
    pub pts: u64,
    /// The Annex-B bytes.
    pub data: Vec<u8>,
}

/// State of a tile, for the user interface.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileStatus {
    /// Waiting for the first decodable picture (after start, a discontinuity,
    /// a new format, or a long gap without pictures).
    Waiting = 0,
    /// Pictures are being decoded.
    Playing = 1,
    /// A decode failure, overflow, or stall happened; waiting for the next
    /// random access point, possibly after a reset cooldown.
    Recovering = 2,
    /// Gave up on the current parameter sets; waits for them to change.
    Undecodable = 3,
}

/// Why the platform could not configure its decoder or decode a picture.
/// Shared by every platform; each maps its own error codes onto these.
///
/// The first two answer a `Configure`; the rest answer the oldest `Decode`
/// in flight.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodeFailureKind {
    /// `Configure`: the decoder rejected the parameter sets (macOS: creating
    /// the format description failed). The tile is `Undecodable` until the
    /// parameter sets change.
    InvalidParameterSets = 0,
    /// `Configure`: the decoder could not be created (macOS: creating the
    /// decompression session failed; Windows: no decoder or media types
    /// refused). Retried through bounded resets, then `Undecodable`.
    SessionCreateFailed = 1,
    /// `Decode` arrived with no decoder. Leads to a reset.
    NotConfigured = 2,
    /// The access unit could not be wrapped for the decoder (malformed
    /// NAL units). Handled like a decode error.
    InvalidAccessUnit = 3,
    /// Submitting the picture failed synchronously. Handled like a decode
    /// error: flush, then resets if it keeps happening (which also covers a
    /// session the system invalidated).
    SubmitFailed = 4,
    /// The decoder reported an error for the picture. Flush and gate; after
    /// repeated failures, reset.
    DecodeError = 5,
    /// The decoder finished the picture without producing one (for example
    /// dropped it). Not an error by itself: nothing is flushed, but a long
    /// run of these with no picture counts as a decode error.
    NoPicture = 6,
}

impl DecodeFailureKind {
    /// True for the kinds that answer a `Configure` rather than a `Decode`.
    pub const fn is_configure_failure(self) -> bool {
        matches!(
            self,
            DecodeFailureKind::InvalidParameterSets | DecodeFailureKind::SessionCreateFailed
        )
    }
}

/// Tunables; all times in microseconds. [`Default`] gives the documented
/// values.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PipelineConfig {
    /// Access units waiting for submission (default 8; at least 1).
    pub max_queued_units: u32,
    /// Bytes waiting for submission (default 8 MiB). A single access unit
    /// larger than this is still accepted into an empty queue.
    pub max_queued_bytes: u32,
    /// Pictures submitted without a result yet (default 4; at least 1).
    /// Decoders that hold several inputs before producing output need this
    /// at least as large as their latency.
    pub max_in_flight: u32,
    /// Failures without a decoded picture in between before the first reset
    /// (default 3; at least 1).
    pub failures_before_reset: u32,
    /// `NoPicture` results in a row that count as one decode error
    /// (default 40, two seconds at 20 fps; at least 1).
    pub no_picture_limit: u32,
    /// Resets allowed per window before giving up (default 3; at most 64).
    pub max_resets_per_window: u32,
    /// Decoder creations of any cause per window (default 10; 1 to 64).
    pub max_configures_per_window: u32,
    /// Window for the two limits above (default 60 s).
    pub window_us: u64,
    /// Cooldown before the first reset's decoder is used (default 1 s);
    /// doubles with each further reset until a picture decodes.
    pub reset_cooldown_us: u64,
    /// Cooldown cap (default 30 s).
    pub max_reset_cooldown_us: u64,
    /// A submitted picture with no result after this long counts as a lost
    /// decoder (default 5 s; 0 disables).
    pub stall_timeout_us: u64,
    /// With no picture decoded for this long, the held picture is cleared so
    /// a frozen image is not mistaken for live video (default 10 s; 0
    /// disables).
    pub clear_after_us: u64,
    /// Warnings allowed per log window (default 5).
    pub log_burst: u32,
    /// Log window (default 60 s).
    pub log_window_us: u64,
    /// An `Undecodable` tile tries its current parameter sets again at the
    /// first random access point after this long (default 5 minutes; 0
    /// never retries). A retry goes through the same recovery, so it is
    /// bounded by the limits above and gives up again if it keeps failing.
    pub undecodable_retry_us: u64,
}

impl Default for PipelineConfig {
    fn default() -> PipelineConfig {
        PipelineConfig {
            max_queued_units: 8,
            max_queued_bytes: 8 << 20,
            max_in_flight: 4,
            failures_before_reset: 3,
            no_picture_limit: 40,
            max_resets_per_window: 3,
            max_configures_per_window: 10,
            window_us: 60_000_000,
            reset_cooldown_us: 1_000_000,
            max_reset_cooldown_us: 30_000_000,
            stall_timeout_us: 5_000_000,
            clear_after_us: 10_000_000,
            log_burst: 5,
            log_window_us: 60_000_000,
            undecodable_retry_us: 300_000_000,
        }
    }
}

impl PipelineConfig {
    fn sanitized(mut self) -> PipelineConfig {
        self.max_queued_units = self.max_queued_units.max(1);
        self.max_in_flight = self.max_in_flight.max(1);
        self.failures_before_reset = self.failures_before_reset.max(1);
        self.no_picture_limit = self.no_picture_limit.max(1);
        self.max_resets_per_window = self.max_resets_per_window.min(64);
        self.max_configures_per_window = self.max_configures_per_window.clamp(1, 64);
        self.max_reset_cooldown_us = self.max_reset_cooldown_us.max(self.reset_cooldown_us);
        self
    }
}

/// Counters per tile, exposed instead of per-frame logging.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileStats {
    /// Access units pushed.
    pub received: u64,
    /// Pictures the decoder reported decoded.
    pub decoded: u64,
    /// Access units dropped by gating (waiting for a random access point,
    /// RASL skipping, cooldown, undecodable, or no decoder).
    pub gated: u64,
    /// Access units dropped by backpressure.
    pub dropped: u64,
    /// Configure and decode failures reported, and stalls detected
    /// (`NoPicture` results are counted separately).
    pub failures: u64,
    /// `NoPicture` results.
    pub no_pictures: u64,
    /// Decoder resets decided by recovery.
    pub resets: u64,
    /// Changes of parameter sets or codec that rebuilt the decoder.
    pub format_changes: u64,
    /// `Flush` actions issued.
    pub flushes: u64,
    /// `Configure` actions issued.
    pub configures: u64,
    /// Submitted pictures that timed out.
    pub stalls: u64,
    /// Results that matched no picture in flight (late, after a flush).
    pub stale_results: u64,
    /// Access units without any picture or parameter set, and parameter
    /// sets that could not be parsed.
    pub invalid: u64,
    /// `Clear` actions issued.
    pub clears: u64,
    /// Access units whose keyframe flag disagreed with their NAL types.
    pub keyframe_flag_mismatches: u64,
}

/// What the platform must do next. Borrowed data stays valid until the next
/// call into the pipeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action<'a> {
    /// Create a decoder for this format, replacing any existing one (whose
    /// pictures in flight are dropped without results). Ask the decoder for
    /// output in `color.full_range`, so its samples match
    /// `color.ycbcr_to_rgb`. A failure is reported with `decode_failed`.
    Configure {
        /// Codec.
        codec: Codec,
        /// Parameter sets in VPS, SPS, PPS order, each a NAL unit without
        /// start code.
        parameter_sets: &'a ParameterSets,
        /// Decoded picture size before cropping.
        coded_size: Size,
        /// Region of the decoded picture to show.
        visible_rect: Rect,
        /// YCbCr to RGB conversion.
        color: ColorInfo,
    },
    /// Submit one picture (Annex-B, as received); must return quickly.
    Decode {
        /// The access unit bytes.
        access_unit: &'a [u8],
        /// The picture is a random access point (by NAL type).
        keyframe: bool,
        /// Presentation timestamp in microseconds; report it back through
        /// `decoded`.
        pts: u64,
    },
    /// Drop everything in flight without reporting results; keep the
    /// decoder.
    Flush,
    /// Destroy the decoder, dropping everything in flight without reporting
    /// results. A `Configure` comes before any further `Decode`.
    Reset,
    /// Show nothing (black) for this tile until a new picture decodes.
    Clear,
    /// The tile's status changed.
    Status(TileStatus),
}

/// Discriminant of [`Action`], for a C interface.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActionKind {
    /// [`Action::Configure`].
    Configure = 0,
    /// [`Action::Decode`].
    Decode = 1,
    /// [`Action::Flush`].
    Flush = 2,
    /// [`Action::Reset`].
    Reset = 3,
    /// [`Action::Clear`].
    Clear = 4,
    /// [`Action::Status`].
    Status = 5,
}

impl Action<'_> {
    /// The action's discriminant.
    pub fn kind(&self) -> ActionKind {
        match self {
            Action::Configure { .. } => ActionKind::Configure,
            Action::Decode { .. } => ActionKind::Decode,
            Action::Flush => ActionKind::Flush,
            Action::Reset => ActionKind::Reset,
            Action::Clear => ActionKind::Clear,
            Action::Status(_) => ActionKind::Status,
        }
    }
}

struct Entry {
    au: AccessUnit,
    random_access: RandomAccess,
    has_parameter_sets: bool,
    /// Out-of-band parameter sets from a preceding access unit.
    stashed: Option<ParameterSets>,
}

enum Op {
    Configure,
    Decode(Entry),
    Flush,
    Reset,
    Clear,
    Status,
}

struct InFlight {
    pts: u64,
    submitted_at: u64,
}

/// Timestamps of recent events, bounded by `limit`.
struct Window {
    times: VecDeque<u64>,
    limit: usize,
}

impl Window {
    fn new(limit: u32) -> Window {
        Window {
            times: VecDeque::with_capacity(limit as usize),
            limit: limit as usize,
        }
    }

    fn prune(&mut self, now: u64, span: u64) {
        while self
            .times
            .front()
            .is_some_and(|&t| t.saturating_add(span) <= now)
        {
            self.times.pop_front();
        }
    }

    fn full(&mut self, now: u64, span: u64) -> bool {
        self.prune(now, span);
        self.times.len() >= self.limit
    }

    fn record(&mut self, now: u64, span: u64) {
        self.prune(now, span);
        if self.limit == 0 {
            return;
        }
        if self.times.len() >= self.limit {
            self.times.pop_front();
        }
        self.times.push_back(now);
    }

    /// When the oldest entry leaves the window.
    fn next_free(&self, span: u64) -> u64 {
        self.times.front().map_or(0, |&t| t.saturating_add(span))
    }
}

/// Allows a burst of log lines per window and counts the rest.
struct LogLimiter {
    window_start: u64,
    in_window: u32,
    suppressed: u64,
}

impl LogLimiter {
    /// `Some(suppressed since the last allowed line)` when a line may be
    /// written now.
    fn allow(&mut self, now: u64, burst: u32, window: u64) -> Option<u64> {
        if now >= self.window_start.saturating_add(window) {
            self.window_start = now;
            self.in_window = 0;
        }
        if self.in_window < burst {
            self.in_window += 1;
            Some(std::mem::take(&mut self.suppressed))
        } else {
            self.suppressed += 1;
            None
        }
    }
}

/// Decoder decisions for one tile. See the module documentation.
pub struct TilePipeline {
    cfg: PipelineConfig,
    tile: u32,
    now: u64,
    status: TileStatus,
    reported_status: TileStatus,
    stats: TileStats,

    // Input side.
    queue: VecDeque<Entry>,
    queued_bytes: usize,
    gate_open: bool,
    skip_rasl: bool,
    stream_codec: Option<Codec>,
    stash: Option<ParameterSets>,

    // Output side.
    ops: VecDeque<Op>,
    current: Option<AccessUnit>,
    in_flight: VecDeque<InFlight>,

    // Decoder as the platform holds it.
    format: Option<VideoFormat>,
    decoder_configured: bool,
    decoder_dirty: bool,
    picture_held: bool,
    last_picture_at: u64,

    // Recovery.
    consecutive_failures: u32,
    consecutive_no_pictures: u32,
    resets_since_success: u32,
    resets: Window,
    configures: Window,
    cooldown_until: u64,
    undecodable: Option<ParameterSets>,
    /// When the tile last became `Undecodable`.
    gave_up_at: u64,

    log: LogLimiter,
}

impl TilePipeline {
    /// A pipeline for one tile. `tile` only labels log lines.
    pub fn new(tile: u32, config: PipelineConfig) -> TilePipeline {
        let cfg = config.sanitized();
        TilePipeline {
            tile,
            now: 0,
            status: TileStatus::Waiting,
            reported_status: TileStatus::Waiting,
            stats: TileStats::default(),
            queue: VecDeque::with_capacity(cfg.max_queued_units as usize),
            queued_bytes: 0,
            gate_open: false,
            skip_rasl: false,
            stream_codec: None,
            stash: None,
            ops: VecDeque::new(),
            current: None,
            in_flight: VecDeque::with_capacity(cfg.max_in_flight as usize),
            format: None,
            decoder_configured: false,
            decoder_dirty: false,
            picture_held: false,
            last_picture_at: 0,
            consecutive_failures: 0,
            consecutive_no_pictures: 0,
            resets_since_success: 0,
            resets: Window::new(cfg.max_resets_per_window),
            configures: Window::new(cfg.max_configures_per_window),
            cooldown_until: 0,
            undecodable: None,
            gave_up_at: 0,
            log: LogLimiter {
                window_start: 0,
                in_window: 0,
                suppressed: 0,
            },
            cfg,
        }
    }

    /// The configuration in use (after bounds were applied).
    pub fn config(&self) -> &PipelineConfig {
        &self.cfg
    }

    /// Current status.
    pub fn status(&self) -> TileStatus {
        self.status
    }

    /// Counters.
    pub fn stats(&self) -> TileStats {
        self.stats
    }

    /// The format of the current (or pending) decoder, if any.
    pub fn format(&self) -> Option<&VideoFormat> {
        self.format.as_ref()
    }

    /// Access units waiting for submission.
    pub fn queued_units(&self) -> usize {
        self.queue.len()
    }

    /// Pictures submitted without a result.
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// Advances time (microseconds, monotonic; earlier values are ignored)
    /// and applies time-based policy: stall detection and clearing a stale
    /// picture.
    pub fn tick(&mut self, now: u64) {
        self.now = self.now.max(now);
        let stall = self.cfg.stall_timeout_us;
        if stall > 0
            && self
                .in_flight
                .front()
                .is_some_and(|f| self.now.saturating_sub(f.submitted_at) >= stall)
        {
            self.stats.stalls += 1;
            self.stats.failures += 1;
            self.escalate("decoder returned no result in time");
        }
        let clear = self.cfg.clear_after_us;
        if clear > 0 && self.picture_held && self.now.saturating_sub(self.last_picture_at) >= clear
        {
            self.clear_picture();
            if self.status == TileStatus::Playing {
                self.set_status(TileStatus::Waiting);
            }
        }
    }

    /// Accepts one access unit from the session.
    pub fn push(&mut self, au: AccessUnit) {
        self.stats.received += 1;
        let info = analyze(au.codec, &au.data);
        if !info.has_picture {
            if info.parameter_sets > 0 {
                // Parameter sets sent on their own: keep them for the next
                // random access point that comes without any.
                self.stash = Some(ParameterSets::from_access_unit(au.codec, &au.data));
            } else {
                self.stats.invalid += 1;
            }
            return;
        }
        let ra = info.random_access;
        if au.keyframe != ra.is_random_access() {
            self.stats.keyframe_flag_mismatches += 1;
        }
        let in_band = info.parameter_sets > 0;
        let stashed = if !ra.is_random_access() {
            None
        } else if in_band {
            self.stash = None;
            None
        } else {
            self.stash.clone().filter(|s| s.codec() == au.codec)
        };

        if let Some(given_up) = &self.undecodable {
            let changed = ra.is_random_access()
                && if in_band {
                    !given_up.matches_access_unit(au.codec, &au.data)
                } else {
                    stashed.as_ref().is_some_and(|s| s != given_up)
                };
            let retry = self.cfg.undecodable_retry_us;
            let retry_due = ra.is_random_access()
                && retry > 0
                && self.now >= self.gave_up_at.saturating_add(retry);
            if !changed && !retry_due {
                self.stats.gated += 1;
                return;
            }
            self.revive(changed);
        }

        if self.now < self.cooldown_until {
            self.gate_open = false;
            self.stats.gated += 1;
            return;
        }
        if self.stream_codec.is_some_and(|c| c != au.codec) && !ra.is_random_access() {
            self.gate_open = false;
        }
        if !self.gate_open {
            if !ra.is_random_access() {
                self.stats.gated += 1;
                return;
            }
            self.open_gate(ra);
        } else if ra.is_random_access() {
            // Mid-stream random access: its leading pictures are decodable.
            self.skip_rasl = false;
        } else if self.skip_rasl {
            if info.rasl {
                self.stats.gated += 1;
                return;
            }
            if info.trailing {
                self.skip_rasl = false;
            }
        }
        self.stream_codec = Some(au.codec);

        let len = au.data.len();
        if !self.queue.is_empty()
            && (self.queue.len() >= self.cfg.max_queued_units as usize
                || self.queued_bytes.saturating_add(len) > self.cfg.max_queued_bytes as usize)
        {
            self.overflow();
            if !ra.is_random_access() {
                self.stats.dropped += 1;
                return;
            }
            self.open_gate(ra);
        }
        self.queued_bytes += len;
        self.queue.push_back(Entry {
            au,
            random_access: ra,
            has_parameter_sets: in_band,
            stashed,
        });
    }

    /// The platform decoded the picture with this timestamp.
    pub fn decoded(&mut self, pts: u64) {
        let Some(i) = self.in_flight.iter().position(|f| f.pts == pts) else {
            self.stats.stale_results += 1;
            return;
        };
        self.in_flight.remove(i);
        self.stats.decoded += 1;
        self.consecutive_failures = 0;
        self.consecutive_no_pictures = 0;
        self.resets_since_success = 0;
        self.picture_held = true;
        self.last_picture_at = self.now;
        if self.undecodable.is_none() {
            self.set_status(TileStatus::Playing);
        }
    }

    /// The platform could not carry out the last `Configure`
    /// ([`DecodeFailureKind::is_configure_failure`]) or decode the oldest
    /// picture in flight (every other kind). Reports that match nothing (no
    /// decoder configured, or nothing in flight because a `Flush`, `Reset`,
    /// or `Configure` discarded it) are ignored and counted as stale.
    pub fn decode_failed(&mut self, kind: DecodeFailureKind) {
        if kind.is_configure_failure() {
            if !self.decoder_configured {
                self.stats.stale_results += 1;
                return;
            }
            self.stats.failures += 1;
            // The platform has no decoder now.
            self.decoder_dirty = false;
            if kind == DecodeFailureKind::InvalidParameterSets {
                self.give_up_on_current("the decoder rejected the parameter sets");
            } else {
                self.escalate("the decoder could not be created");
            }
            return;
        }
        if self.in_flight.pop_front().is_none() {
            self.stats.stale_results += 1;
            return;
        }
        match kind {
            DecodeFailureKind::NoPicture => {
                self.stats.no_pictures += 1;
                self.consecutive_no_pictures = self.consecutive_no_pictures.saturating_add(1);
                if self.consecutive_no_pictures < self.cfg.no_picture_limit {
                    return;
                }
                self.consecutive_no_pictures = 0;
                self.stats.failures += 1;
                self.picture_failed("the decoder keeps producing no pictures");
            }
            DecodeFailureKind::NotConfigured => {
                self.stats.failures += 1;
                self.escalate("a picture reached the platform with no decoder");
            }
            _ => {
                self.stats.failures += 1;
                self.picture_failed("picture failed to decode");
            }
        }
    }

    /// A picture failed: flush and gate, or reset after repeated failures.
    fn picture_failed(&mut self, reason: &str) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= self.cfg.failures_before_reset {
            self.escalate(reason);
        } else {
            self.flush_and_gate(reason);
        }
    }

    /// The stream restarted (reconnect, stream switch): flush and wait for a
    /// random access point.
    pub fn discontinuity(&mut self) {
        self.in_flight.clear();
        self.drop_queue();
        self.stash = None;
        self.stream_codec = None;
        self.flush_decoder();
        if self.undecodable.is_none() {
            self.set_status(TileStatus::Waiting);
        }
    }

    /// The next action to carry out, or `None` when there is nothing to do
    /// until the next input.
    pub fn next_action(&mut self) -> Option<Action<'_>> {
        loop {
            if let Some(op) = self.ops.pop_front() {
                match op {
                    Op::Configure => {
                        // Always set when a Configure is queued.
                        let f = self.format.as_ref()?;
                        return Some(Action::Configure {
                            codec: f.codec,
                            parameter_sets: &f.parameter_sets,
                            coded_size: f.coded_size,
                            visible_rect: f.visible_rect,
                            color: f.color,
                        });
                    }
                    Op::Decode(entry) => {
                        self.in_flight.push_back(InFlight {
                            pts: entry.au.pts,
                            submitted_at: self.now,
                        });
                        self.decoder_dirty = true;
                        let keyframe = entry.random_access.is_random_access();
                        let au = self.current.insert(entry.au);
                        return Some(Action::Decode {
                            access_unit: &au.data,
                            keyframe,
                            pts: au.pts,
                        });
                    }
                    Op::Flush => return Some(Action::Flush),
                    Op::Reset => return Some(Action::Reset),
                    Op::Clear => return Some(Action::Clear),
                    Op::Status => {
                        if self.status != self.reported_status {
                            self.reported_status = self.status;
                            return Some(Action::Status(self.status));
                        }
                        continue;
                    }
                }
            }
            // The previous Decode's bytes are no longer borrowed.
            self.current = None;
            if self.in_flight.len() >= self.cfg.max_in_flight as usize {
                return None;
            }
            let entry = self.queue.pop_front()?;
            self.queued_bytes -= entry.au.data.len();
            self.prepare(entry);
        }
    }

    /// Decides what a dequeued access unit needs before its `Decode`.
    fn prepare(&mut self, mut entry: Entry) {
        if !entry.random_access.is_random_access() {
            if !self.decoder_configured {
                self.stats.gated += 1;
                self.gate_open = false;
                return;
            }
            self.ops.push_back(Op::Decode(entry));
            return;
        }
        let codec = entry.au.codec;
        let differs = match &self.format {
            None => true,
            Some(f) if entry.has_parameter_sets => {
                !f.parameter_sets.matches_access_unit(codec, &entry.au.data)
            }
            Some(f) => match &entry.stashed {
                Some(p) => *p != f.parameter_sets,
                None => f.codec != codec,
            },
        };
        if differs {
            let sets = if entry.has_parameter_sets {
                ParameterSets::from_access_unit(codec, &entry.au.data)
            } else if let Some(p) = entry.stashed.take() {
                p
            } else {
                self.stats.gated += 1;
                self.drop_queue();
                self.warn("random access point without parameter sets; waiting for the next one");
                return;
            };
            let format = match VideoFormat::from_parameter_sets(sets.clone()) {
                Ok(f) => f,
                Err(e) => {
                    self.stats.invalid += 1;
                    let reason = format!("parameter sets could not be parsed: {e}");
                    self.give_up(sets, &reason);
                    return;
                }
            };
            if !self.allow_configure() {
                return;
            }
            if self.decoder_configured {
                self.push_reset();
            }
            if let Some(old) = &self.format {
                self.stats.format_changes += 1;
                let (o, n) = (old.visible_rect, format.visible_rect);
                info!(
                    tile = self.tile,
                    "format changed from {:?} {}x{} to {:?} {}x{}",
                    old.codec,
                    o.width,
                    o.height,
                    format.codec,
                    n.width,
                    n.height
                );
            }
            self.format = Some(format);
            self.push_configure();
        } else if !self.decoder_configured {
            // Same format, decoder released by a reset.
            if !self.allow_configure() {
                return;
            }
            self.push_configure();
        }
        self.ops.push_back(Op::Decode(entry));
    }

    fn allow_configure(&mut self) -> bool {
        let span = self.cfg.window_us;
        if self.configures.full(self.now, span) {
            self.stats.gated += 1;
            self.drop_queue();
            self.cooldown_until = self.configures.next_free(span);
            if self.undecodable.is_none() {
                self.set_status(TileStatus::Recovering);
            }
            self.warn("too many decoder rebuilds; pausing this tile");
            return false;
        }
        self.configures.record(self.now, span);
        true
    }

    /// Pictures in flight when a `Flush`, `Reset`, or `Configure` runs are
    /// discarded by the platform without a result.
    fn discard_in_flight(&mut self) {
        self.in_flight.clear();
    }

    /// Drops `Decode`s not yet handed out, so nothing reaches a decoder that
    /// is about to be reset or failed to be created.
    fn drop_pending_decodes(&mut self) {
        let before = self.ops.len();
        self.ops.retain(|o| !matches!(o, Op::Decode(_)));
        self.stats.gated += (before - self.ops.len()) as u64;
    }

    fn push_configure(&mut self) {
        self.discard_in_flight();
        self.ops.push_back(Op::Configure);
        self.stats.configures += 1;
        self.decoder_configured = true;
        self.decoder_dirty = false;
    }

    fn push_reset(&mut self) {
        if self.decoder_configured {
            self.discard_in_flight();
            self.ops.push_back(Op::Reset);
            self.decoder_configured = false;
            self.decoder_dirty = false;
        }
    }

    fn flush_decoder(&mut self) {
        self.discard_in_flight();
        if self.decoder_dirty {
            self.ops.push_back(Op::Flush);
            self.stats.flushes += 1;
            self.decoder_dirty = false;
        }
    }

    fn clear_picture(&mut self) {
        if self.picture_held {
            self.ops.push_back(Op::Clear);
            self.stats.clears += 1;
            self.picture_held = false;
        }
    }

    fn open_gate(&mut self, ra: RandomAccess) {
        self.gate_open = true;
        self.skip_rasl = ra.may_have_skipped_leading();
    }

    /// Drops every queued access unit and closes the gate.
    fn drop_queue(&mut self) {
        self.stats.gated += self.queue.len() as u64;
        self.queue.clear();
        self.queued_bytes = 0;
        self.gate_open = false;
        self.skip_rasl = false;
    }

    fn overflow(&mut self) {
        self.stats.dropped += self.queue.len() as u64;
        self.queue.clear();
        self.queued_bytes = 0;
        self.gate_open = false;
        self.skip_rasl = false;
        if self.status == TileStatus::Playing {
            self.set_status(TileStatus::Recovering);
        }
        self.warn("decoder is behind; dropping to the next keyframe");
    }

    fn flush_and_gate(&mut self, reason: &str) {
        self.in_flight.clear();
        self.drop_queue();
        self.flush_decoder();
        self.set_status(TileStatus::Recovering);
        let msg = format!("{reason}; flushing and waiting for the next keyframe");
        self.warn(&msg);
    }

    /// Resets the decoder, or gives up when resets are exhausted.
    fn escalate(&mut self, reason: &str) {
        self.in_flight.clear();
        self.drop_queue();
        self.drop_pending_decodes();
        let span = self.cfg.window_us;
        if self.resets.full(self.now, span) {
            self.give_up_on_current(reason);
            return;
        }
        self.resets.record(self.now, span);
        self.stats.resets += 1;
        self.push_reset();
        let shift = self.resets_since_success.min(32);
        let cooldown = self
            .cfg
            .reset_cooldown_us
            .saturating_mul(1u64 << shift)
            .min(self.cfg.max_reset_cooldown_us);
        self.resets_since_success = self.resets_since_success.saturating_add(1);
        self.cooldown_until = self.now.saturating_add(cooldown);
        self.set_status(TileStatus::Recovering);
        let msg = format!(
            "{reason}; resetting the decoder after {} ms",
            cooldown / 1000
        );
        self.warn(&msg);
    }

    fn give_up_on_current(&mut self, reason: &str) {
        let sets = match &self.format {
            Some(f) => f.parameter_sets.clone(),
            None => ParameterSets::new(self.stream_codec.unwrap_or(Codec::H264)),
        };
        self.give_up(sets, reason);
    }

    /// Releases the decoder and waits for different parameter sets.
    fn give_up(&mut self, sets: ParameterSets, reason: &str) {
        self.in_flight.clear();
        self.drop_queue();
        self.drop_pending_decodes();
        self.push_reset();
        self.clear_picture();
        self.undecodable = Some(sets);
        self.gave_up_at = self.now;
        self.set_status(TileStatus::Undecodable);
        let msg = format!("{reason}; tile is undecodable until the stream format changes");
        self.warn(&msg);
    }

    /// Leaves `Undecodable`: on new parameter sets with a clean slate, on a
    /// timed retry of the same ones keeping the recovery window, so the
    /// retry stays within the per-window reset cap.
    fn revive(&mut self, format_changed: bool) {
        self.undecodable = None;
        self.consecutive_failures = 0;
        self.resets_since_success = 0;
        self.cooldown_until = 0;
        self.set_status(TileStatus::Waiting);
        if format_changed {
            self.resets.times.clear();
            info!(tile = self.tile, "stream format changed; trying again");
        } else {
            info!(
                tile = self.tile,
                "retrying the same stream format after a long pause"
            );
        }
    }

    fn set_status(&mut self, status: TileStatus) {
        if self.status == status {
            return;
        }
        self.status = status;
        if !self.ops.iter().any(|o| matches!(o, Op::Status)) {
            self.ops.push_back(Op::Status);
        }
    }

    fn warn(&mut self, msg: &str) {
        if let Some(suppressed) =
            self.log
                .allow(self.now, self.cfg.log_burst, self.cfg.log_window_us)
        {
            if suppressed > 0 {
                warn!(
                    tile = self.tile,
                    suppressed, "{msg} ({suppressed} similar messages suppressed)"
                );
            } else {
                warn!(tile = self.tile, "{msg}");
            }
        }
    }
}
