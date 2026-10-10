//! The alarm-pattern detector: tone frames, pulses, and pattern matchers.

use crate::for_each_f32_chunk;
use crate::pattern::{MatchEvent, Matcher, Pattern, T3, T4};
use crate::tone::{Edge, FRAME, HighPass, ToneBank, ToneThresholds, ToneTracker};

/// Which alarm a pattern stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AlarmKind {
    /// Temporal-three: smoke or fire.
    Smoke,
    /// Temporal-four: carbon monoxide.
    Co,
}

impl AlarmKind {
    /// The event kind in the index (docs/FFI-RECORDINGS.md, section 2a).
    pub fn as_str(self) -> &'static str {
        match self {
            AlarmKind::Smoke => "smoke_alarm",
            AlarmKind::Co => "co_alarm",
        }
    }
}

/// The start or end of an alarm episode. Times are sample numbers, in the
/// numbering the caller used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmEvent {
    /// Enough cycles matched. Reported at the end of the last of them (plus
    /// the minimum pause), so `start` is in the past.
    Started {
        kind: AlarmKind,
        /// First sample of the first matched cycle.
        start: u64,
        /// Cycles matched so far.
        cycles: u32,
    },
    /// No further cycle arrived in time, or the stream ended.
    Ended {
        kind: AlarmKind,
        start: u64,
        /// End of the last pulse of the last matched cycle.
        end: u64,
        /// Cycles matched in the whole episode.
        cycles: u32,
    },
}

impl AlarmEvent {
    pub fn kind(&self) -> AlarmKind {
        match *self {
            AlarmEvent::Started { kind, .. } | AlarmEvent::Ended { kind, .. } => kind,
        }
    }
}

/// Settings of an [`AlarmDetector`]. The defaults suit residential smoke and
/// CO alarms near 3 kHz.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlarmConfig {
    /// Lowest tone frequency looked for, Hz.
    pub band_low_hz: f32,
    /// Highest tone frequency looked for, Hz.
    pub band_high_hz: f32,
    /// Share of the frame's power the tone needs to start a pulse.
    pub on_ratio: f32,
    /// Share below which a pulse stops.
    pub off_ratio: f32,
    /// Quietest tone that starts a pulse, dBFS (RMS of the tone alone). A
    /// pulse stops 3 dB below it.
    pub min_tone_dbfs: f32,
    /// A pulse stops when its tone falls this many dB below the pulse's
    /// peak (reverberation tails).
    pub release_db: f32,
    /// Duration tolerance, as a share of each nominal duration.
    pub rel_tolerance: f32,
    /// Duration tolerance added to the relative one, seconds.
    pub abs_tolerance_s: f32,
    /// Consecutive cycles needed to report an episode.
    pub min_cycles: u32,
    /// Cycles an episode may miss in a row before it ends.
    pub max_missed_cycles: u32,
    /// Look for T3 (smoke).
    pub smoke: bool,
    /// Look for T4 (CO).
    pub co: bool,
}

impl Default for AlarmConfig {
    fn default() -> Self {
        AlarmConfig {
            band_low_hz: 2900.0,
            band_high_hz: 3500.0,
            on_ratio: 0.15,
            off_ratio: 0.06,
            min_tone_dbfs: -70.0,
            release_db: 10.0,
            rel_tolerance: 0.3,
            abs_tolerance_s: 0.04,
            min_cycles: 2,
            max_missed_cycles: 1,
            smoke: true,
            co: true,
        }
    }
}

/// Finds smoke (T3) and CO (T4) alarm patterns in 16 kHz mono audio.
///
/// Feed blocks of any size with [`AlarmDetector::push`]; events come out
/// through the callback, in time order. Call [`AlarmDetector::flush`] at the
/// end of a stream to end an episode in progress. Allocates only in
/// [`AlarmDetector::new`].
#[derive(Clone, Debug)]
pub struct AlarmDetector {
    highpass: HighPass,
    bank: ToneBank,
    thresholds: ToneThresholds,
    tracker: ToneTracker,
    matchers: [(AlarmKind, Option<Matcher>); 2],
    frame: [f32; FRAME],
    fill: usize,
    /// Sample number of the first sample in `frame`.
    frame_start: u64,
    /// Sample number expected next, once anything was pushed.
    next_sample: Option<u64>,
}

impl AlarmDetector {
    pub fn new(cfg: AlarmConfig) -> AlarmDetector {
        let power = |db: f32| 10f32.powf(db / 10.0);
        let matcher = |p: &Pattern, on: bool| {
            on.then(|| {
                Matcher::new(
                    p,
                    cfg.rel_tolerance,
                    cfg.abs_tolerance_s,
                    cfg.min_cycles,
                    cfg.max_missed_cycles,
                )
            })
        };
        AlarmDetector {
            // Well below the band: about 1 kHz for the default band.
            highpass: HighPass::new((cfg.band_low_hz * 0.35).clamp(50.0, 1000.0)),
            bank: ToneBank::new(cfg.band_low_hz, cfg.band_high_hz),
            thresholds: ToneThresholds {
                on_ratio: cfg.on_ratio,
                off_ratio: cfg.off_ratio.min(cfg.on_ratio),
                min_power: power(cfg.min_tone_dbfs),
                stay_power: power(cfg.min_tone_dbfs - 3.0),
                release: power(-cfg.release_db.abs()),
            },
            tracker: ToneTracker::default(),
            matchers: [
                (AlarmKind::Smoke, matcher(&T3, cfg.smoke)),
                (AlarmKind::Co, matcher(&T4, cfg.co)),
            ],
            frame: [0.0; FRAME],
            fill: 0,
            frame_start: 0,
            next_sample: None,
        }
    }

    /// Analyzes `samples` (-1..1), the first of which is sample number
    /// `first_sample`. A block that does not start where the previous one
    /// ended is a discontinuity: the partial frame and any pulse in progress
    /// are dropped, and timing continues from `first_sample` (an episode in
    /// progress ends if the gap outlasts its hangover).
    pub fn push(&mut self, first_sample: u64, samples: &[f32], mut emit: impl FnMut(AlarmEvent)) {
        if self.next_sample != Some(first_sample) {
            if self.next_sample.is_some() {
                self.discontinuity(first_sample, &mut emit);
            }
            self.fill = 0;
            self.frame_start = first_sample;
        }
        let mut rest = samples;
        while !rest.is_empty() {
            let n = (FRAME - self.fill).min(rest.len());
            let hp = &mut self.highpass;
            for (o, &x) in self.frame[self.fill..self.fill + n]
                .iter_mut()
                .zip(&rest[..n])
            {
                *o = hp.run(if x.is_finite() { x } else { 0.0 });
            }
            self.fill += n;
            rest = &rest[n..];
            if self.fill == FRAME {
                self.process_frame(&mut emit);
                self.frame_start += FRAME as u64;
                self.fill = 0;
            }
        }
        self.next_sample = Some(first_sample + samples.len() as u64);
    }

    /// [`AlarmDetector::push`] for 16-bit samples.
    pub fn push_i16(
        &mut self,
        first_sample: u64,
        samples: &[i16],
        mut emit: impl FnMut(AlarmEvent),
    ) {
        for_each_f32_chunk(first_sample, samples, |pos, chunk| {
            self.push(pos, chunk, &mut emit)
        });
    }

    /// Ends the stream: reports the end of any episode in progress and
    /// forgets all state, so the detector can take a new stream.
    pub fn flush(&mut self, mut emit: impl FnMut(AlarmEvent)) {
        for (kind, m) in &mut self.matchers {
            if let Some(m) = m {
                let kind = *kind;
                m.finish(&mut |e| emit(to_event(kind, e)));
            }
        }
        self.tracker.reset();
        self.highpass.reset();
        self.fill = 0;
        self.next_sample = None;
    }

    fn discontinuity(&mut self, at: u64, emit: &mut impl FnMut(AlarmEvent)) {
        self.tracker.reset();
        self.highpass.reset();
        for (kind, m) in &mut self.matchers {
            if let Some(m) = m {
                let kind = *kind;
                m.reset_pulses(at);
                m.tick(at, &mut |e| emit(to_event(kind, e)));
            }
        }
    }

    fn process_frame(&mut self, emit: &mut impl FnMut(AlarmEvent)) {
        let ft = self.bank.analyze(&self.frame);
        let edge = self.tracker.update(&self.thresholds, self.frame_start, ft);
        let now = self.frame_start + FRAME as u64;
        for (kind, m) in &mut self.matchers {
            if let Some(m) = m {
                let kind = *kind;
                let mut out = |e| emit(to_event(kind, e));
                match edge {
                    Some(Edge::Start(t)) => m.pulse_start(t, &mut out),
                    Some(Edge::End(t)) => m.pulse_end(t),
                    None => {}
                }
                m.tick(now, &mut out);
            }
        }
    }
}

fn to_event(kind: AlarmKind, e: MatchEvent) -> AlarmEvent {
    match e {
        MatchEvent::Started { start, cycles } => AlarmEvent::Started {
            kind,
            start,
            cycles,
        },
        MatchEvent::Ended { start, end, cycles } => AlarmEvent::Ended {
            kind,
            start,
            end,
            cycles,
        },
    }
}
