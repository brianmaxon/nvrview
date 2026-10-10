//! Sound analysis of live tiles (docs/PLAYBACK.md "Sound",
//! docs/FFI-RECORDINGS.md section 2a): `crates/sound-events` on every live
//! tile with audio, audible or not.
//!
//! - **Where it runs.** On the tile's own thread, right after each record
//!   is decoded, on the record's 16 kHz mono form
//!   ([`audio_pipeline::MonoBlock`], the samples the tap carries). It costs
//!   a few microseconds per 64 ms record, a small fraction of decoding the
//!   record (measured by `examples/sound_cost.rs`), so a queue and a thread
//!   of its own would cost more than they save. No allocation per record:
//!   the analyzers allocate once, when the tile opens.
//! - **Alarms.** [`sound_events::AlarmDetector`] finds smoke (T3) and CO
//!   (T4) alarm patterns. Each episode becomes a `detection` event with
//!   `source` `mac_audio` and `kind` `smoke_alarm` or `co_alarm`, `start`
//!   when the detector reports it and `stop` when it ends, when audio stops
//!   arriving for [`STALL_NS`], or when the tile ends. Several tiles of one
//!   channel (main and sub, say) report one episode once: the core counts
//!   the tiles reporting each (device, channel, kind) and sends `start` for
//!   the first and `stop` after the last.
//! - **Loudness.** [`sound_events::LoudnessMeter`] measures peak and RMS
//!   per second; a bounded ring per tile keeps the last
//!   [`SoundSettings::loudness_history_s`] seconds, one byte per value
//!   ([`Level`]), read with [`crate::Core::tile_loudness`].
//! - **Time.** Analyzer positions count 16 kHz samples since the Unix
//!   epoch: each stream (and each discontinuity) is anchored at the wall
//!   clock when its first record arrived, and samples count on from there,
//!   so a loudness second is a Unix second (to within the delivery delay)
//!   and positions never go back. `at_ns` of an alarm event is the core's
//!   clock when the alarm's first matched beep (`start`) or last beep
//!   (`stop`) arrived, so a `start` is sent some seconds after its `at_ns`
//!   (the detector needs two cycles: about 7.5 s for T3, 10 s for T4).

use crate::events::{EventSink, Priority};
use crate::json::Object;
use audio_pipeline::MonoBlock;
use sound_events::{
    AlarmConfig, AlarmDetector, AlarmEvent, AlarmKind, LoudnessMeter, SecondLevels,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

pub use sound_events::Level;

/// Sample rate of the analysis.
const RATE: u64 = sound_events::SAMPLE_RATE as u64;

/// An alarm episode in progress stops when no audio has arrived for this
/// long (a stalled stream must not leave a `start` without its `stop`).
pub const STALL_NS: u64 = 3_000_000_000;

/// Within one stream, a block whose arrival is this far past where the
/// samples counted so far put it starts a new anchor (audio went missing
/// without a discontinuity being reported).
const REANCHOR_SAMPLES: u64 = 5 * RATE;

/// Most seconds of loudness a tile keeps.
pub const MAX_LOUDNESS_HISTORY_S: u32 = 3600;

/// The `source` of detections found in sound (docs/FFI-RECORDINGS.md,
/// section 2a).
pub const SOURCE: &str = "mac_audio";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sound analysis settings ([`crate::CoreConfig::sound`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoundSettings {
    /// Detect smoke (T3) and CO (T4) alarm patterns in every live tile's
    /// audio and report them as `detection` events. On by default.
    pub alarms: bool,
    /// Seconds of per-second loudness kept per tile (at most
    /// [`MAX_LOUDNESS_HISTORY_S`]); 0 keeps none. Default 600.
    pub loudness_history_s: u32,
}

impl Default for SoundSettings {
    fn default() -> Self {
        SoundSettings {
            alarms: true,
            loudness_history_s: 600,
        }
    }
}

/// What a [`TileSound`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundOutput {
    /// An alarm episode started or ended; `at_ns` on the core's clock.
    Alarm {
        kind: AlarmKind,
        start: bool,
        at_ns: u64,
    },
    /// One second of loudness; `second` is a Unix second.
    Second(SecondLevels),
}

/// One tile's analyzers, fed on the tile's thread. Sans-I/O: outputs go to
/// the caller's closure.
pub struct TileSound {
    meter: Option<LoudnessMeter>,
    detector: Option<Box<AlarmDetector>>,
    /// Position of the next sample if the stream continues; `None` before
    /// the first block and after a stall.
    next: Option<u64>,
    /// Position after the last block (positions never go back).
    end: u64,
    epoch: u32,
    /// The tile's own sample number expected next in `epoch`.
    expect_first: u64,
    /// Host clock (ns) at which the last block arrived.
    arrived_ns: u64,
    /// Smoke, CO.
    active: [bool; 2],
}

fn slot(kind: AlarmKind) -> usize {
    match kind {
        AlarmKind::Smoke => 0,
        AlarmKind::Co => 1,
    }
}

const KINDS: [AlarmKind; 2] = [AlarmKind::Smoke, AlarmKind::Co];

/// Host time (ns) of analyzer position `sample`, given that position
/// `end` arrived at `arrived_ns`.
fn host_ns(sample: u64, end: u64, arrived_ns: u64) -> u64 {
    let back = end.saturating_sub(sample);
    arrived_ns.saturating_sub(back.saturating_mul(1_000_000_000) / RATE)
}

impl TileSound {
    /// Analyzers per `settings`; `None` when both are off.
    pub fn new(settings: &SoundSettings) -> Option<TileSound> {
        if !settings.alarms && settings.loudness_history_s == 0 {
            return None;
        }
        Some(TileSound {
            meter: (settings.loudness_history_s > 0).then(LoudnessMeter::new),
            detector: settings
                .alarms
                .then(|| Box::new(AlarmDetector::new(AlarmConfig::default()))),
            next: None,
            end: 0,
            epoch: 0,
            expect_first: 0,
            arrived_ns: 0,
            active: [false; 2],
        })
    }

    /// True while an alarm episode is in progress.
    pub fn alarm_active(&self) -> bool {
        self.active.iter().any(|&a| a)
    }

    /// Analyzes one record's 16 kHz mono block. `now_wall_us` is used only
    /// when the block carries no wall-clock time.
    pub fn push(&mut self, b: &MonoBlock<'_>, now_wall_us: i64, mut emit: impl FnMut(SoundOutput)) {
        let n = b.samples.len() as u64;
        if n == 0 {
            return;
        }
        let wall_us = b.wall_clock_us.unwrap_or(now_wall_us).max(0) as u64;
        // The block's last sample arrived at `wall_us`.
        let wall_pos =
            ((u128::from(wall_us) * u128::from(RATE) / 1_000_000) as u64).saturating_sub(n);
        let contiguous = !b.discontinuity
            && b.epoch == self.epoch
            && b.first_sample.abs_diff(self.expect_first) <= 2;
        let pos = match self.next {
            Some(next) if contiguous && wall_pos <= next + REANCHOR_SAMPLES => next,
            // A new anchor, always ahead of what was analyzed: the
            // analyzers see a gap (a discontinuity), never a step back.
            _ => wall_pos.max(self.end + 1),
        };
        let end = pos + n;
        self.epoch = b.epoch;
        self.expect_first = b.first_sample + n;
        self.next = Some(end);
        self.end = end;
        self.arrived_ns = b.arrived_us.saturating_mul(1000);
        if let Some(m) = self.meter.as_mut() {
            m.push(pos, b.samples, |s| emit(SoundOutput::Second(s)));
        }
        let arrived_ns = self.arrived_ns;
        let active = &mut self.active;
        if let Some(d) = self.detector.as_mut() {
            d.push(pos, b.samples, |e| {
                alarm(e, end, arrived_ns, active, &mut emit);
            });
        }
    }

    /// Call regularly (the tile's tick): ends everything in progress once
    /// no audio has arrived for [`STALL_NS`].
    pub fn tick(&mut self, now_ns: u64, emit: impl FnMut(SoundOutput)) {
        if self.next.is_some() && now_ns > self.arrived_ns.saturating_add(STALL_NS) {
            self.flush(now_ns, emit);
        }
    }

    /// The stream ended (or stalled): reports the second in progress and
    /// the end of any alarm episode. The next block starts a new anchor.
    pub fn flush(&mut self, now_ns: u64, mut emit: impl FnMut(SoundOutput)) {
        if let Some(m) = self.meter.as_mut() {
            m.flush(|s| emit(SoundOutput::Second(s)));
        }
        let (end, arrived_ns) = (self.end, self.arrived_ns);
        let active = &mut self.active;
        if let Some(d) = self.detector.as_mut() {
            d.flush(|e| alarm(e, end, arrived_ns, active, &mut emit));
        }
        // The detector ends every episode on flush; this is a backstop.
        for kind in KINDS {
            if std::mem::take(&mut self.active[slot(kind)]) {
                emit(SoundOutput::Alarm {
                    kind,
                    start: false,
                    at_ns: now_ns,
                });
            }
        }
        self.next = None;
    }
}

fn alarm(
    e: AlarmEvent,
    end: u64,
    arrived_ns: u64,
    active: &mut [bool; 2],
    emit: &mut impl FnMut(SoundOutput),
) {
    let (kind, start, sample) = match e {
        AlarmEvent::Started { kind, start, .. } => (kind, true, start),
        AlarmEvent::Ended { kind, end, .. } => (kind, false, end),
    };
    if active[slot(kind)] != start {
        active[slot(kind)] = start;
        emit(SoundOutput::Alarm {
            kind,
            start,
            at_ns: host_ns(sample, end, arrived_ns),
        });
    }
}

/// A tile's loudness history: the last seconds of peak and RMS, one byte
/// per value.
struct LoudnessRing {
    peak: Box<[u8]>,
    rms: Box<[u8]>,
    /// The newest second stored, and the oldest stored since the tile
    /// opened.
    newest: Option<u64>,
    first: u64,
}

impl LoudnessRing {
    fn new(seconds: u32) -> LoudnessRing {
        let cap = seconds.clamp(1, MAX_LOUDNESS_HISTORY_S) as usize;
        LoudnessRing {
            peak: vec![0; cap].into_boxed_slice(),
            rms: vec![0; cap].into_boxed_slice(),
            newest: None,
            first: 0,
        }
    }

    fn cap(&self) -> u64 {
        self.peak.len() as u64
    }

    fn put(&mut self, s: &SecondLevels) {
        let cap = self.cap();
        let sec = s.second;
        let i = (sec % cap) as usize;
        match self.newest {
            Some(n) if sec <= n => {
                if n - sec >= cap {
                    return;
                }
                self.peak[i] = self.peak[i].max(s.peak.to_byte());
                self.rms[i] = self.rms[i].max(s.rms.to_byte());
                self.first = self.first.min(sec);
                return;
            }
            Some(n) => {
                // Seconds skipped had no audio.
                for t in (n + 1).max(sec.saturating_sub(cap - 1))..sec {
                    let j = (t % cap) as usize;
                    self.peak[j] = 0;
                    self.rms[j] = 0;
                }
            }
            None => self.first = sec,
        }
        self.peak[i] = s.peak.to_byte();
        self.rms[i] = s.rms.to_byte();
        self.newest = Some(sec);
    }

    fn snapshot(&self) -> TileLoudness {
        let Some(n) = self.newest else {
            return TileLoudness::default();
        };
        let start = self.first.max((n + 1).saturating_sub(self.cap()));
        let idx = |t: u64| (t % self.cap()) as usize;
        TileLoudness {
            start_s: start,
            peak: (start..=n)
                .map(|t| Level::from_byte(self.peak[idx(t)]))
                .collect(),
            rms: (start..=n)
                .map(|t| Level::from_byte(self.rms[idx(t)]))
                .collect(),
        }
    }
}

/// A live tile's recent loudness ([`crate::Core::tile_loudness`]): one
/// value per second from `start_s` (a Unix second), oldest first, up to
/// the newest complete second. [`Level::NONE`] marks seconds without
/// audio. Empty before any audio was measured.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TileLoudness {
    pub start_s: u64,
    pub peak: Vec<Level>,
    pub rms: Vec<Level>,
}

/// The core's sound state: which alarm episodes are active per channel,
/// and each tile's loudness ring.
pub(crate) struct SoundHub {
    settings: SoundSettings,
    events: EventSink,
    /// Tiles reporting each active (device, channel, kind).
    active: Mutex<HashMap<(u64, u8, AlarmKind), u32>>,
    lanes: Mutex<HashMap<u64, Arc<Mutex<LoudnessRing>>>>,
}

impl SoundHub {
    pub(crate) fn new(settings: SoundSettings, events: EventSink) -> SoundHub {
        SoundHub {
            settings,
            events,
            active: Mutex::new(HashMap::new()),
            lanes: Mutex::new(HashMap::new()),
        }
    }

    /// The analysis of a new tile, or `None` when sound analysis is off.
    /// Dropping it ends the tile's episodes and forgets its loudness.
    pub(crate) fn add_tile(
        self: &Arc<SoundHub>,
        tile: u64,
        device: u64,
        channel: u8,
    ) -> Option<TileSoundLink> {
        let analyzer = TileSound::new(&self.settings)?;
        let ring = (self.settings.loudness_history_s > 0).then(|| {
            let r = Arc::new(Mutex::new(LoudnessRing::new(
                self.settings.loudness_history_s,
            )));
            lock(&self.lanes).insert(tile, r.clone());
            r
        });
        Some(TileSoundLink {
            hub: self.clone(),
            tile,
            device,
            channel,
            analyzer,
            ring,
        })
    }

    /// A tile's loudness; `None` for a tile without a ring.
    pub(crate) fn loudness(&self, tile: u64) -> Option<TileLoudness> {
        let ring = lock(&self.lanes).get(&tile)?.clone();
        let snapshot = lock(&ring).snapshot();
        Some(snapshot)
    }

    fn alarm(&self, device: u64, channel: u8, kind: AlarmKind, start: bool, at_ns: u64) {
        let mut active = lock(&self.active);
        let key = (device, channel, kind);
        let count = active.entry(key).or_insert(0);
        let send = if start {
            *count += 1;
            *count == 1
        } else if *count > 0 {
            *count -= 1;
            *count == 0
        } else {
            false
        };
        if *count == 0 {
            active.remove(&key);
        }
        if send {
            // Sent under the lock, so starts and stops of one channel stay
            // in order across tiles.
            self.events.send(
                detection_event(device, channel, kind, start, at_ns),
                Priority::State,
            );
        }
    }
}

/// The `detection` event of an alarm found in sound (docs/FFI.md,
/// "Events"): the fields of a device detection, with `source` `mac_audio`.
/// No `lens`: a camera's microphone is not tied to one of its lenses, and a
/// tile's stream tier does not tell lenses apart.
pub(crate) fn detection_event(
    device: u64,
    channel: u8,
    kind: AlarmKind,
    start: bool,
    at_ns: u64,
) -> String {
    Object::event("detection")
        .u64("device", device)
        .u64("channel", u64::from(channel))
        .str("kind", kind.as_str())
        .str("state", if start { "start" } else { "stop" })
        .str("source", SOURCE)
        .u64("at_ns", at_ns)
        .finish()
}

/// A tile thread's link to its analysis.
pub(crate) struct TileSoundLink {
    hub: Arc<SoundHub>,
    tile: u64,
    device: u64,
    channel: u8,
    analyzer: TileSound,
    ring: Option<Arc<Mutex<LoudnessRing>>>,
}

impl TileSoundLink {
    fn sink(
        hub: &SoundHub,
        ring: &Option<Arc<Mutex<LoudnessRing>>>,
        device: u64,
        channel: u8,
    ) -> impl FnMut(SoundOutput) {
        move |o| match o {
            SoundOutput::Alarm { kind, start, at_ns } => {
                hub.alarm(device, channel, kind, start, at_ns)
            }
            SoundOutput::Second(s) => {
                if let Some(r) = ring {
                    lock(r).put(&s);
                }
            }
        }
    }

    /// One record's 16 kHz mono block.
    pub(crate) fn push(&mut self, b: &MonoBlock<'_>) {
        let sink = Self::sink(&self.hub, &self.ring, self.device, self.channel);
        self.analyzer.push(b, wall_now_us(), sink);
    }

    /// The tile's tick: ends episodes of a stalled stream.
    pub(crate) fn tick(&mut self, now_ns: u64) {
        let sink = Self::sink(&self.hub, &self.ring, self.device, self.channel);
        self.analyzer.tick(now_ns, sink);
    }

    /// The tile's stream restarted or ended: ends its episodes now.
    pub(crate) fn end(&mut self) {
        let sink = Self::sink(&self.hub, &self.ring, self.device, self.channel);
        self.analyzer.flush(crate::clock::now_ns(), sink);
    }
}

impl Drop for TileSoundLink {
    fn drop(&mut self) {
        self.end();
        if self.ring.is_some() {
            lock(&self.hub.lanes).remove(&self.tile);
        }
    }
}

fn wall_now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_micros()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(second: u64, peak: i8) -> SecondLevels {
        SecondLevels {
            second,
            peak: Level::from_dbfs(f32::from(peak)),
            rms: Level::from_dbfs(f32::from(peak) - 3.0),
            samples: 16_000,
        }
    }

    /// `secs` seconds of a 3.1 kHz tone at 0.3 in the T3 rhythm.
    fn t3(secs: u64) -> Vec<f32> {
        (0..secs * RATE)
            .map(|n| {
                let t = n as f64 / RATE as f64;
                let c = t % 4.0;
                if c < 3.0 && c % 1.0 < 0.5 {
                    0.3 * (2.0 * std::f64::consts::PI * 3100.0 * t).sin() as f32
                } else {
                    0.0
                }
            })
            .collect()
    }

    const WALL0_US: i64 = 1_700_000_000_000_000;
    const HOST0_US: u64 = 5_000_000;

    /// Feeds `pcm` in 1024-sample records from tile sample `first`,
    /// arriving in real time from `t0_us` (host and wall offsets alike).
    fn feed(
        s: &mut TileSound,
        pcm: &[f32],
        epoch: u32,
        first: u64,
        t0_us: u64,
        out: &mut Vec<SoundOutput>,
    ) -> u64 {
        let mut t = t0_us;
        for (k, chunk) in pcm.chunks(1024).enumerate() {
            t = t0_us + ((k as u64 + 1) * 1024 * 1_000_000) / RATE;
            let b = MonoBlock {
                samples: chunk,
                epoch,
                first_sample: first + k as u64 * 1024,
                camera_time_us: None,
                wall_clock_us: Some(WALL0_US + t as i64),
                arrived_us: HOST0_US + t,
                discontinuity: k == 0,
            };
            s.push(&b, 0, |o| out.push(o));
        }
        t
    }

    fn alarms(out: &[SoundOutput]) -> Vec<(AlarmKind, bool, u64)> {
        out.iter()
            .filter_map(|o| match *o {
                SoundOutput::Alarm { kind, start, at_ns } => Some((kind, start, at_ns)),
                SoundOutput::Second(_) => None,
            })
            .collect()
    }

    #[test]
    fn t3_starts_and_a_stall_stops_it() {
        let mut s = TileSound::new(&SoundSettings::default()).unwrap();
        let mut out = Vec::new();
        let end = feed(&mut s, &t3(12), 0, 0, 0, &mut out);
        let a = alarms(&out);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!((a[0].0, a[0].1), (AlarmKind::Smoke, true));
        // `at_ns` is when the first beep arrived: near the start.
        let first_beep_ns = (HOST0_US + 64_000) * 1000;
        assert!(a[0].2.abs_diff(first_beep_ns) < 200_000_000, "{a:?}");
        assert!(s.alarm_active());
        // Seconds of loudness, as Unix seconds.
        let secs: Vec<_> = out
            .iter()
            .filter_map(|o| match o {
                SoundOutput::Second(l) => Some(*l),
                _ => None,
            })
            .collect();
        assert!(secs.len() >= 10);
        let unix0 = (WALL0_US / 1_000_000) as u64;
        assert!(secs.iter().all(|l| (unix0..unix0 + 14).contains(&l.second)));
        assert!(secs.iter().any(|l| l.peak.db() == Some(-10)));

        // Audio stops: nothing before the stall limit, then the stop.
        out.clear();
        let host_end_ns = (HOST0_US + end) * 1000;
        s.tick(host_end_ns + STALL_NS - 1_000_000, |o| out.push(o));
        assert!(alarms(&out).is_empty());
        s.tick(host_end_ns + STALL_NS + 1_000_000, |o| out.push(o));
        let a = alarms(&out);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!((a[0].0, a[0].1), (AlarmKind::Smoke, false));
        assert!(a[0].2 <= host_end_ns);
        assert!(!s.alarm_active());
        // A later tick changes nothing.
        out.clear();
        s.tick(host_end_ns + 3 * STALL_NS, |o| out.push(o));
        assert!(out.is_empty());
    }

    #[test]
    fn a_new_epoch_keeps_the_episode_and_flush_ends_it() {
        let mut s = TileSound::new(&SoundSettings::default()).unwrap();
        let mut out = Vec::new();
        let pcm = t3(16);
        let (a, b) = pcm.split_at(10 * RATE as usize);
        let t = feed(&mut s, a, 0, 0, 0, &mut out);
        assert_eq!(alarms(&out).len(), 1);
        // The stream restarts (a tier switch): numbering restarts at 0.
        feed(&mut s, b, 1, 0, t, &mut out);
        assert_eq!(alarms(&out).len(), 1, "still one episode");
        s.flush(0, |o| out.push(o));
        let a = alarms(&out);
        assert_eq!(a.len(), 2);
        assert!(!a[1].1);
    }

    #[test]
    fn steady_tone_and_silence_report_nothing() {
        let mut s = TileSound::new(&SoundSettings::default()).unwrap();
        let mut out = Vec::new();
        let tone: Vec<f32> = (0..20 * RATE)
            .map(|n| 0.3 * (2.0 * std::f32::consts::PI * 3100.0 * n as f32 / 16_000.0).sin())
            .collect();
        feed(&mut s, &tone, 0, 0, 0, &mut out);
        feed(
            &mut s,
            &vec![0.0; 10 * RATE as usize],
            0,
            20 * RATE,
            20_000_000,
            &mut out,
        );
        s.flush(0, |o| out.push(o));
        assert!(alarms(&out).is_empty());
    }

    #[test]
    fn settings_switch_parts_off() {
        assert!(
            TileSound::new(&SoundSettings {
                alarms: false,
                loudness_history_s: 0
            })
            .is_none()
        );
        let mut s = TileSound::new(&SoundSettings {
            alarms: false,
            loudness_history_s: 60,
        })
        .unwrap();
        let mut out = Vec::new();
        feed(&mut s, &t3(12), 0, 0, 0, &mut out);
        assert!(alarms(&out).is_empty());
        assert!(!out.is_empty(), "loudness still measured");
    }

    #[test]
    fn ring_keeps_the_last_seconds_and_marks_gaps() {
        let mut r = LoudnessRing::new(5);
        assert_eq!(r.snapshot(), TileLoudness::default());
        r.put(&levels(100, -20));
        r.put(&levels(101, -21));
        r.put(&levels(104, -24));
        let s = r.snapshot();
        assert_eq!(s.start_s, 100);
        let db: Vec<_> = s.peak.iter().map(|l| l.db()).collect();
        assert_eq!(db, [Some(-20), Some(-21), None, None, Some(-24)]);
        r.put(&levels(106, -26));
        let s = r.snapshot();
        assert_eq!(s.start_s, 102);
        assert_eq!(s.peak.len(), 5);
        assert_eq!(s.peak[4].db(), Some(-26));
        assert_eq!(s.peak[3], Level::NONE);
        // Too old: ignored. A long gap clears everything.
        r.put(&levels(90, -1));
        r.put(&levels(1000, -10));
        let s = r.snapshot();
        assert_eq!(s.start_s, 996);
        assert_eq!(s.peak.iter().filter(|l| l.is_some()).count(), 1);
    }
}
