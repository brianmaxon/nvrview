//! Sans-I/O sound analysis for nvrview: a per-second loudness envelope and a
//! deterministic detector for the standard smoke-alarm and CO-alarm rhythms.
//!
//! See docs/FFI-RECORDINGS.md, section 2a, and docs/PLAYBACK.md, "Sound". It
//! is an aid for finding and labeling events, never an alarm system: camera
//! microphones are not certified detectors, and a missed alarm must never be
//! presented as "no alarm".
//!
//! - [`LoudnessMeter`]: peak and RMS per second, in whole dBFS, stored as one
//!   byte per value ([`Level`]), with "no audio analyzed" distinct from
//!   silence. [`aggregate_peak`] and [`aggregate_rms`] combine seconds into
//!   larger buckets for the timeline.
//! - [`AlarmDetector`]: finds the temporal-three (T3, smoke) and
//!   temporal-four (T4, CO) patterns of a narrow-band tone near 3 kHz and
//!   reports the start and end of each alarm episode ([`AlarmEvent`]).
//!
//! Both take 16 kHz mono audio (the audio tap of docs/AUDIO.md) in blocks of
//! any size, as `f32` in -1..1 or `i16`, with the number of the block's first
//! sample. Sample numbers are the only clock: the caller maps them to camera
//! time, as it does for tap blocks. No I/O, no threads, no clocks, no
//! platform APIs, and no allocation after construction. The same input gives
//! the same output.
//!
//! # The patterns
//!
//! | Pattern | On | Off between pulses | Pulses | Off after the group | Cycle |
//! | --- | --- | --- | --- | --- | --- |
//! | T3, smoke and fire evacuation | 0.5 s | 0.5 s | 3 | 1.5 s | 4.0 s |
//! | T4, carbon monoxide | 0.1 s | 0.1 s | 4 | 5.0 s | 5.7 s |
//!
//! Sources: T3 is the "audible emergency evacuation signal" of ISO 8201 and
//! ANSI/ASA S3.41, required for fire evacuation (smoke alarms included) by
//! NFPA 72, "Distinctive evacuation signal"; each phase is specified as
//! nominal ±10%. T4 is the carbon monoxide alarm signal of NFPA 720 (now part
//! of NFPA 72) and the CO alarm listing standards (UL 2034): four 100 ms
//! pulses 100 ms apart, then about 5 s off. Reported (not measured here):
//! some CO alarms sound T4 continuously for about four minutes and then one
//! group about once a minute; single groups a minute apart are not reported,
//! since one group is not enough evidence.
//!
//! The tone of residential alarms is typically a piezo horn between about 2.9
//! and 3.5 kHz (the default band, [`AlarmConfig::band_low_hz`] and
//! [`AlarmConfig::band_high_hz`]). Low-frequency alarms (a 520 Hz square wave,
//! required in some sleeping areas) need a band set around 520 Hz.
//!
//! # How the detector works
//!
//! 1. A second-order high-pass filter (about 1 kHz for the default band)
//!    removes rumble and wind, which can carry most of a camera
//!    microphone's energy. Then frames of 256 samples (16 ms): a small bank
//!    of Goertzel filters spaced 62.5 Hz across the band, on Hann-windowed
//!    samples, finds the strongest narrow-band component. Its power is
//!    compared with the frame's whole power (above the high-pass): a pure
//!    tone gives a ratio near 1, white noise about 0.01 per filter, so loud
//!    broadband noise is not a tone. The strongest filter must also hold a
//!    large share of the band's filters together, so noise that fills the
//!    band is not a tone either.
//! 2. A pulse starts when the ratio reaches [`AlarmConfig::on_ratio`] and the
//!    tone is louder than [`AlarmConfig::min_tone_dbfs`], for two frames,
//!    rising at least 6 dB above the quietest tone since the last pulse. It
//!    stops after two frames below [`AlarmConfig::off_ratio`], below the
//!    level floor, or [`AlarmConfig::release_db`] below the pulse's own peak
//!    (so a room's reverberation tail neither fills the short T4 gaps nor
//!    starts a pulse of its own).
//! 3. A matcher per pattern checks the durations of pulses and gaps against
//!    the nominal ones within `nominal × rel_tolerance + abs_tolerance_s`
//!    (defaults 30% and 40 ms, wider than the standards' 10% to allow for
//!    reverberation, frame resolution, and real devices). A group is the
//!    pattern's pulses with short gaps between them; a cycle is a group
//!    followed by at least the minimum long gap. An episode starts when
//!    [`AlarmConfig::min_cycles`] consecutive cycles (default 2) are linked by
//!    valid long gaps, and ends when no further group arrives within one long
//!    gap plus [`AlarmConfig::max_missed_cycles`] cycle periods.
//!
//! # Measured
//!
//! On synthetic audio (`tests/detector.rs`, `examples/bench.rs`): every clean
//! alarm from -6 to -60 dBFS and 2.95 to 3.45 kHz, alarms with AAC-like
//! coding, resampling, reverberation up to 0.6 s, and ±20% timing jitter;
//! all alarms down to a tone 5 dB below broadband white or pink noise; no
//! report on any of the distractors (steady tones, single beeps and groups,
//! ring cadences, microwave and backup beepers, alarms outside the band,
//! noise bursts in the T3 rhythm, speech-like noise, music-like chords).
//! CPU: about 0.1 ms per second of audio for the detector and 0.01 ms for
//! the loudness meter (Apple M3 Max, release build), about 0.01% of one core
//! per camera.

mod alarm;
mod level;
mod pattern;
mod tone;

pub use alarm::{AlarmConfig, AlarmDetector, AlarmEvent, AlarmKind};
pub use level::{Level, LoudnessMeter, SecondLevels, aggregate_peak, aggregate_rms};
pub use pattern::{Pattern, T3, T4};

/// Sample rate of the audio both analyzers take (the audio tap's rate).
pub const SAMPLE_RATE: u32 = 16_000;

/// Converts `i16` samples to `f32` in -1..1 through a small stack buffer and
/// hands each piece to `f`, with the number of its first sample.
pub(crate) fn for_each_f32_chunk(
    first_sample: u64,
    samples: &[i16],
    mut f: impl FnMut(u64, &[f32]),
) {
    let mut buf = [0.0f32; 256];
    let mut pos = first_sample;
    for chunk in samples.chunks(buf.len()) {
        for (o, &s) in buf.iter_mut().zip(chunk) {
            *o = f32::from(s) / 32768.0;
        }
        f(pos, &buf[..chunk.len()]);
        pos += chunk.len() as u64;
    }
}
