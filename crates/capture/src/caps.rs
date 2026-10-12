//! Caps and disk-guard decisions (docs/CAPTURE.md, "Bounds").
//!
//! Everything is a function of numbers the caller reads: the free bytes
//! (through its own shim, `statvfs` or `GetDiskFreeSpaceExW`), the current
//! file's size, the counts of running recordings, and a timestamp. Nothing
//! here touches the file system or a clock.
//!
//! # Rules
//!
//! - **Defaults and clamps.** A live recording stops at 60 minutes or
//!   4 GB (decimal), whichever is first. Settings are clamped to 1 minute
//!   to 12 hours and 100 MB to 64 GB. A call recording has a safety cap of
//!   2 hours or 2 GB and is not configurable.
//! - **Counts.** At most 4 live and 2 call recordings per core, and 2
//!   main-stream recorders per device. A start over a count is refused
//!   with the limit's code, checked in that order, then the disk.
//! - **Start.** A recording does not start with under `min_free_bytes`
//!   (2 GB) free (`disk_low`).
//! - **While recording.** Free space is read every 5 s and at each
//!   fragment; the recording stops (`disk_low`) when free space is under
//!   `stop_free_bytes` (1 GB) plus the current file's size, which keeps
//!   room to finalize.
//! - **Finalizing.** The finalize step writes a temporary file about the
//!   size of the partial one. If free space cannot hold that plus a small
//!   margin, the fragmented file itself is published as the `.mp4` (it
//!   plays) instead of failing, and the event says so.

use crate::reason::{LimitKind, Reason};

/// One megabyte, decimal (the settings and the caps use decimal units).
pub const MB: u64 = 1_000_000;
/// One gigabyte, decimal.
pub const GB: u64 = 1_000_000_000;
/// Microseconds per minute.
const MINUTE_US: i64 = 60_000_000;

/// Smallest and largest duration a live-recording setting may be.
pub const DURATION_RANGE_US: (i64, i64) = (MINUTE_US, 12 * 60 * MINUTE_US);
/// Smallest and largest size a live-recording setting may be.
pub const SIZE_RANGE_BYTES: (u64, u64) = (100 * MB, 64 * GB);

/// A recording's duration and size caps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Longest recording, microseconds of media time.
    pub max_duration_us: i64,
    /// Most bytes written.
    pub max_bytes: u64,
}

impl Limits {
    /// Live caps, clamped into the allowed ranges.
    pub fn live(max_duration_us: i64, max_bytes: u64) -> Limits {
        Limits::exact(
            max_duration_us.clamp(DURATION_RANGE_US.0, DURATION_RANGE_US.1),
            max_bytes.clamp(SIZE_RANGE_BYTES.0, SIZE_RANGE_BYTES.1),
        )
    }

    /// Caps as given, unclamped (for tests and internal limits).
    pub const fn exact(max_duration_us: i64, max_bytes: u64) -> Limits {
        Limits {
            max_duration_us,
            max_bytes,
        }
    }

    /// The defaults: 60 minutes, 4 GB.
    pub fn live_default() -> Limits {
        Limits::live(60 * MINUTE_US, 4 * GB)
    }

    /// A call recording's safety cap: 2 hours, 2 GB.
    pub fn call() -> Limits {
        Limits::exact(120 * MINUTE_US, 2 * GB)
    }
}

/// The disk thresholds and the check interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskConfig {
    /// Under this much free space a recording does not start.
    pub min_free_bytes: u64,
    /// A recording stops when free space is under this plus its file size.
    pub stop_free_bytes: u64,
    /// Minimum time between free-space reads between fragments.
    pub check_interval_us: i64,
}

impl Default for DiskConfig {
    fn default() -> Self {
        DiskConfig {
            min_free_bytes: 2 * GB,
            stop_free_bytes: GB,
            check_interval_us: 5_000_000,
        }
    }
}

/// The answer of a disk check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskDecision {
    /// Not due yet; do not read free space.
    NotDue,
    /// Fine.
    Ok,
    /// Stop and finalize with this reason.
    Stop(Reason),
}

/// What the stop step should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalizePlan {
    /// Finalize to a temporary file, rename, delete the partial.
    Normal,
    /// Not enough room: publish the fragmented file as the `.mp4`.
    PublishFragmented,
}

/// Free-space decisions for one recording.
#[derive(Clone, Copy, Debug)]
pub struct DiskGuard {
    cfg: DiskConfig,
    last_check_us: Option<i64>,
}

/// Room finalizing needs beyond the file's size: the index and headers.
const FINALIZE_MARGIN: u64 = 8 * 1024 * 1024;

impl DiskGuard {
    /// A guard with these thresholds.
    pub fn new(cfg: DiskConfig) -> DiskGuard {
        DiskGuard {
            cfg,
            last_check_us: None,
        }
    }

    /// Whether a recording may start with `free_bytes` free.
    pub fn can_start(cfg: &DiskConfig, free_bytes: u64) -> Result<(), LimitKind> {
        if free_bytes < cfg.min_free_bytes {
            Err(LimitKind::DiskLow)
        } else {
            Ok(())
        }
    }

    /// Whether free space should be read now: at every fragment, and
    /// otherwise every `check_interval_us`.
    pub fn due(&self, now_us: i64, at_fragment: bool) -> bool {
        at_fragment
            || self
                .last_check_us
                .is_none_or(|t| now_us - t >= self.cfg.check_interval_us || now_us < t)
    }

    /// Decides with a free-space reading taken now (call only when
    /// [`due`](DiskGuard::due); a reading is recorded as the last check).
    pub fn check(&mut self, now_us: i64, free_bytes: u64, file_bytes: u64) -> DiskDecision {
        self.last_check_us = Some(now_us);
        if free_bytes < self.cfg.stop_free_bytes.saturating_add(file_bytes) {
            DiskDecision::Stop(Reason::DiskLow)
        } else {
            DiskDecision::Ok
        }
    }

    /// Reading and deciding in one step: [`DiskDecision::NotDue`] without
    /// calling `read_free` when no check is due.
    pub fn poll(
        &mut self,
        now_us: i64,
        at_fragment: bool,
        file_bytes: u64,
        read_free: impl FnOnce() -> u64,
    ) -> DiskDecision {
        if !self.due(now_us, at_fragment) {
            return DiskDecision::NotDue;
        }
        let free = read_free();
        self.check(now_us, free, file_bytes)
    }

    /// How to finish a file of `file_bytes` with `free_bytes` free.
    pub fn finalize_plan(free_bytes: u64, file_bytes: u64) -> FinalizePlan {
        let need = file_bytes
            .saturating_add(file_bytes / 50)
            .saturating_add(FINALIZE_MARGIN);
        if free_bytes >= need {
            FinalizePlan::Normal
        } else {
            FinalizePlan::PublishFragmented
        }
    }
}

/// Which kind of recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingKind {
    /// A live tile.
    Live,
    /// A doorbell call.
    Call,
}

/// The count limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CountLimits {
    /// Live recordings per core.
    pub max_live: u32,
    /// Call recordings per core.
    pub max_calls: u32,
    /// Main-stream recorders per device.
    pub max_main_per_device: u32,
}

impl Default for CountLimits {
    fn default() -> Self {
        CountLimits {
            max_live: 4,
            max_calls: 2,
            max_main_per_device: 2,
        }
    }
}

/// What is running now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Running {
    /// Live recordings.
    pub live: u32,
    /// Call recordings.
    pub calls: u32,
    /// Main-stream recorders on the device of the new recording.
    pub main_on_device: u32,
}

/// A start request, reduced to what admission needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartRequest {
    /// Live or call.
    pub kind: RecordingKind,
    /// True if the recorder uses the main stream.
    pub main_stream: bool,
    /// Free bytes where the file goes.
    pub free_bytes: u64,
}

/// Whether a recording may start: the count limits in the order live or
/// calls, main streams, then the disk.
pub fn admit(
    counts: &CountLimits,
    disk: &DiskConfig,
    running: &Running,
    req: &StartRequest,
) -> Result<(), LimitKind> {
    match req.kind {
        RecordingKind::Live if running.live >= counts.max_live => return Err(LimitKind::Live),
        RecordingKind::Call if running.calls >= counts.max_calls => return Err(LimitKind::Calls),
        _ => {}
    }
    if req.main_stream && running.main_on_device >= counts.max_main_per_device {
        return Err(LimitKind::MainStreams);
    }
    DiskGuard::can_start(disk, req.free_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_limits_are_clamped_and_defaults_are_the_contract() {
        let d = Limits::live_default();
        assert_eq!(d.max_duration_us, 3_600_000_000);
        assert_eq!(d.max_bytes, 4_000_000_000);
        let tiny = Limits::live(1, 1);
        assert_eq!(tiny.max_duration_us, 60_000_000);
        assert_eq!(tiny.max_bytes, 100_000_000);
        let huge = Limits::live(i64::MAX, u64::MAX);
        assert_eq!(huge.max_duration_us, 12 * 3600 * 1_000_000);
        assert_eq!(huge.max_bytes, 64_000_000_000);
        assert_eq!(Limits::call(), Limits::exact(7_200_000_000, 2_000_000_000));
        let neg = Limits::live(-5, 0);
        assert_eq!(neg.max_duration_us, 60_000_000);
    }

    #[test]
    fn start_needs_the_minimum_free() {
        let c = DiskConfig::default();
        for (free, ok) in [(0, false), (GB, false), (2 * GB - 1, false), (2 * GB, true)] {
            assert_eq!(DiskGuard::can_start(&c, free).is_ok(), ok, "{free}");
        }
        assert_eq!(DiskGuard::can_start(&c, 0), Err(LimitKind::DiskLow));
    }

    #[test]
    fn checks_are_due_at_fragments_and_every_five_seconds() {
        let mut g = DiskGuard::new(DiskConfig::default());
        assert!(g.due(0, false)); // never read
        assert_eq!(g.check(0, 10 * GB, 0), DiskDecision::Ok);
        assert!(!g.due(4_999_999, false));
        assert!(g.due(4_999_999, true));
        assert!(g.due(5_000_000, false));
        // A clock that went back is treated as due.
        assert!(g.due(-1, false));
    }

    #[test]
    fn poll_reads_free_space_only_when_due() {
        let mut g = DiskGuard::new(DiskConfig::default());
        let mut reads = 0;
        let mut go = |g: &mut DiskGuard, now, frag, file| {
            g.poll(now, frag, file, || {
                reads += 1;
                10 * GB
            })
        };
        assert_eq!(go(&mut g, 0, false, 0), DiskDecision::Ok);
        assert_eq!(go(&mut g, 1_000_000, false, 0), DiskDecision::NotDue);
        assert_eq!(go(&mut g, 1_000_001, true, 0), DiskDecision::Ok);
        assert_eq!(reads, 2);
    }

    #[test]
    fn stop_threshold_is_one_gb_plus_the_file() {
        let mut g = DiskGuard::new(DiskConfig::default());
        let table = [
            // (free, file, stops)
            (5 * GB, 0, false),
            (GB, 0, false),
            (GB - 1, 0, true),
            (3 * GB, 2 * GB, false),
            (3 * GB - 1, 2 * GB, true),
            (0, 0, true),
        ];
        for (free, file, stops) in table {
            let d = g.check(0, free, file);
            assert_eq!(
                d,
                if stops {
                    DiskDecision::Stop(Reason::DiskLow)
                } else {
                    DiskDecision::Ok
                },
                "{free} {file}"
            );
        }
        // No overflow on absurd sizes.
        assert_eq!(g.check(0, u64::MAX, u64::MAX), DiskDecision::Ok);
    }

    #[test]
    fn finalize_publishes_the_fragmented_file_when_there_is_no_room() {
        let mb = 1024 * 1024;
        let table = [
            (GB, 100 * mb, FinalizePlan::Normal),
            (100 * mb, 100 * mb, FinalizePlan::PublishFragmented),
            (110 * mb - 1, 100 * mb, FinalizePlan::PublishFragmented),
            (110 * mb, 100 * mb, FinalizePlan::Normal),
            (0, 0, FinalizePlan::PublishFragmented),
            (9 * mb, 0, FinalizePlan::Normal),
        ];
        for (free, file, want) in table {
            assert_eq!(DiskGuard::finalize_plan(free, file), want, "{free} {file}");
        }
    }

    fn req(kind: RecordingKind, main: bool, free: u64) -> StartRequest {
        StartRequest {
            kind,
            main_stream: main,
            free_bytes: free,
        }
    }

    #[test]
    fn admission_checks_counts_then_main_streams_then_disk() {
        let (c, d) = (CountLimits::default(), DiskConfig::default());
        let run = |live, calls, main| Running {
            live,
            calls,
            main_on_device: main,
        };
        let table = [
            (
                run(0, 0, 0),
                req(RecordingKind::Live, true, 10 * GB),
                Ok(()),
            ),
            (
                run(3, 0, 0),
                req(RecordingKind::Live, true, 10 * GB),
                Ok(()),
            ),
            (
                run(4, 0, 0),
                req(RecordingKind::Live, false, 10 * GB),
                Err(LimitKind::Live),
            ),
            // Calls are counted apart from live recordings.
            (
                run(4, 1, 0),
                req(RecordingKind::Call, false, 10 * GB),
                Ok(()),
            ),
            (
                run(0, 2, 0),
                req(RecordingKind::Call, false, 10 * GB),
                Err(LimitKind::Calls),
            ),
            (
                run(1, 0, 2),
                req(RecordingKind::Live, true, 10 * GB),
                Err(LimitKind::MainStreams),
            ),
            // A sub-stream recorder is not limited by main-stream count.
            (
                run(1, 0, 2),
                req(RecordingKind::Live, false, 10 * GB),
                Ok(()),
            ),
            (
                run(0, 0, 0),
                req(RecordingKind::Live, true, GB),
                Err(LimitKind::DiskLow),
            ),
            // The count limit wins over the disk.
            (
                run(4, 0, 0),
                req(RecordingKind::Live, true, 0),
                Err(LimitKind::Live),
            ),
        ];
        for (running, request, want) in table {
            assert_eq!(
                admit(&c, &d, &running, &request),
                want,
                "{running:?} {request:?}"
            );
        }
    }
}
