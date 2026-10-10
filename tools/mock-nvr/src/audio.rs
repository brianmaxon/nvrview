//! Interleaved audio: an ADTS fixture sent as one frame per audio record,
//! placed among the video records the way a device places them.
//!
//! Devices stamp video but not audio (docs/protocol.md, "Audio payloads").
//! The mock assumes, as the viewer's timestamper does, that a record is sent
//! once its last sample has been captured: after every picture captured
//! before that moment and before every later one. The audio clock runs at
//! [`AudioConfig::clock_hz`] samples per second of *video* time, so a skew
//! between the camera's audio and video clocks can be set (real cameras were
//! measured about 20 ppm apart).

use std::fmt;
use std::path::Path;

/// A looping ADTS source: one frame per record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFixture {
    frames: Vec<Vec<u8>>,
}

/// Why an audio fixture could not be used.
#[derive(Debug)]
pub enum AudioFixtureError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The file holds no ADTS frame.
    Empty,
}

impl fmt::Display for AudioFixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioFixtureError::Io(e) => write!(f, "cannot read audio fixture: {e}"),
            AudioFixtureError::Empty => write!(f, "audio fixture holds no ADTS frame"),
        }
    }
}

impl std::error::Error for AudioFixtureError {}

impl AudioFixture {
    /// Loads a raw ADTS file (`.aac`).
    pub fn load(path: &Path) -> Result<AudioFixture, AudioFixtureError> {
        let bytes = std::fs::read(path).map_err(AudioFixtureError::Io)?;
        AudioFixture::from_adts(&bytes)
    }

    /// Splits a raw ADTS stream into frames (stops at the first byte that
    /// does not start one).
    pub fn from_adts(mut b: &[u8]) -> Result<AudioFixture, AudioFixtureError> {
        let mut frames = Vec::new();
        while b.len() >= 7 && b[0] == 0xFF && b[1] & 0xF0 == 0xF0 {
            let len = (usize::from(b[3] & 0x03) << 11)
                | (usize::from(b[4]) << 3)
                | usize::from(b[5] >> 5);
            if len < 7 || len > b.len() {
                break;
            }
            frames.push(b[..len].to_vec());
            b = &b[len..];
        }
        if frames.is_empty() {
            return Err(AudioFixtureError::Empty);
        }
        Ok(AudioFixture { frames })
    }

    /// The frames, in order.
    pub fn frames(&self) -> &[Vec<u8>] {
        &self.frames
    }
}

/// Audio settings of the mock.
#[derive(Clone, Debug)]
pub struct AudioConfig {
    /// The looping source.
    pub fixture: AudioFixture,
    /// Samples per second of video time (16,000 nominal).
    pub clock_hz: f64,
    /// Samples per record (1024 for AAC-LC).
    pub samples_per_record: u32,
    /// Extra delay before each message, uniformly random in `0..jitter_us`
    /// (order is kept: a delayed message holds back those after it).
    pub jitter_us: u32,
    /// Stream index written in the record magic (5 for `05wb`).
    pub stream_index: u8,
    /// Seed of the jitter generator.
    pub seed: u64,
}

impl AudioConfig {
    /// `fixture` at a nominal 16 kHz, no jitter, `05wb` records.
    pub fn new(fixture: AudioFixture) -> AudioConfig {
        AudioConfig {
            fixture,
            clock_hz: 16_000.0,
            samples_per_record: 1024,
            jitter_us: 0,
            stream_index: 5,
            seed: 1,
        }
    }
}

/// One item of the interleaved stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    /// Picture number `n`, stamped `pts_us`.
    Video { n: u64, pts_us: u64 },
    /// Audio record number `k`.
    Audio { k: u64 },
}

/// The order and send times of pictures and audio records, in video time.
#[derive(Clone, Debug)]
pub struct Schedule {
    frame_us: u64,
    record_s: Option<f64>,
    next_video: u64,
    next_audio: u64,
}

impl Schedule {
    /// Pictures every `frame_us`; audio records every `samples / clock_hz`
    /// seconds if `audio` is set.
    pub fn new(frame_us: u64, audio: Option<&AudioConfig>) -> Schedule {
        Schedule {
            frame_us: frame_us.max(1),
            record_s: audio.map(|a| f64::from(a.samples_per_record) / a.clock_hz.max(1.0)),
            next_video: 0,
            next_audio: 0,
        }
    }

    /// Video time (µs) at which audio record `k` is sent: when its last
    /// sample has been captured.
    pub fn audio_send_us(&self, k: u64) -> Option<f64> {
        self.record_s.map(|r| (k + 1) as f64 * r * 1e6)
    }
}

impl Iterator for Schedule {
    /// The item and its send time in video time (µs).
    type Item = (Item, f64);

    fn next(&mut self) -> Option<(Item, f64)> {
        let video_us = self.next_video * self.frame_us;
        match self.audio_send_us(self.next_audio) {
            // A picture captured at or before the record's send time goes
            // first.
            Some(a) if a < video_us as f64 => {
                let k = self.next_audio;
                self.next_audio += 1;
                Some((Item::Audio { k }, a))
            }
            _ => {
                let n = self.next_video;
                self.next_video += 1;
                Some((
                    Item::Video {
                        n,
                        pts_us: video_us,
                    },
                    video_us as f64,
                ))
            }
        }
    }
}

/// Uniform jitter source (xorshift), deterministic per seed.
#[derive(Clone, Debug)]
pub(crate) struct Jitter {
    state: u64,
    max_us: u32,
}

impl Jitter {
    pub fn new(seed: u64, max_us: u32) -> Jitter {
        Jitter {
            state: seed | 1,
            max_us,
        }
    }

    pub fn next_us(&mut self) -> u64 {
        if self.max_us == 0 {
            return 0;
        }
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state % u64::from(self.max_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_follow_every_earlier_picture() {
        let mut cfg = AudioConfig::new(AudioFixture {
            frames: vec![vec![0xFF, 0xF1, 0, 0, 0xE0, 0, 0]],
        });
        cfg.clock_hz = 16_013.0;
        let s = Schedule::new(100_000, Some(&cfg));
        let mut last_pts = None;
        let mut audio = 0;
        for (item, t) in s.take(2000) {
            match item {
                Item::Video { pts_us, .. } => {
                    assert!(last_pts.is_none_or(|p| p < pts_us));
                    last_pts = Some(pts_us);
                }
                Item::Audio { k } => {
                    let send = (k + 1) as f64 * 1024.0 / 16_013.0 * 1e6;
                    assert!((t - send).abs() < 1e-6);
                    // The picture before it was captured at or before its
                    // send time, the next one after.
                    let p = last_pts.unwrap() as f64;
                    assert!(p <= send && send < p + 100_000.0);
                    audio += 1;
                }
            }
        }
        // About 15.6 records per 10 pictures.
        assert!((1200..1240).contains(&audio), "{audio}");
    }

    #[test]
    fn loads_adts_frames() {
        let mut b = vec![0xFF, 0xF1, 0x60, 0x40, 0x01, 0x40, 0xFC];
        b.extend([0u8; 3]);
        b.extend(b.clone());
        let f = AudioFixture::from_adts(&b).unwrap();
        assert_eq!(f.frames().len(), 2);
        assert!(AudioFixture::from_adts(&[1, 2, 3]).is_err());
    }
}
