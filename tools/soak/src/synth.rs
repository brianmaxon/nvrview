//! A synthetic recording for runs and tests that need no real capture: the
//! bundled synthetic video and AAC fixtures, interleaved by mock-nvr's
//! device-like schedule (audio clock skew and arrival jitter), written as an
//! `NVRREC01` recording.

use bc_protocol::media::Record;
use bc_protocol::recording::Writer;
use mock_nvr::audio::{AudioConfig, AudioFixture, Item, Schedule};
use mock_nvr::fixture::Fixture;
use mock_nvr::replay::Replay;
use std::path::{Path, PathBuf};

/// Where the bundled fixtures live (relative to this crate).
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic")
}

/// `seconds` of stream: video at `fps`, audio at 16,013 Hz with 40 ms of
/// arrival jitter.
pub fn recording(fixtures: &Path, seconds: u64, fps: u64) -> Result<Replay, String> {
    let video = Fixture::load(&fixtures.join("h264-basic.h264")).map_err(|e| e.to_string())?;
    let audio = AudioFixture::load(&fixtures.join("aac-clicks.aac")).map_err(|e| e.to_string())?;
    let mut acfg = AudioConfig::new(audio);
    acfg.clock_hz = 16_013.0;
    acfg.jitter_us = 40_000;
    let schedule = Schedule::new(1_000_000 / fps, Some(&acfg));
    let mut jitter = mock_nvr::audio::Jitter::new(acfg.seed, acfg.jitter_us);
    let mut w = Writer::new(Vec::new()).map_err(|e| e.to_string())?;
    w.record(
        0,
        &Record::Info {
            width: 320,
            height: 240,
            fps: fps as u8,
        },
    )
    .map_err(|e| e.to_string())?;
    let mut index = video.first_keyframe();
    let mut last = 0u64;
    for (item, send_us) in schedule {
        if send_us > (seconds * 1_000_000) as f64 {
            break;
        }
        // Order is kept: a delayed message holds back those after it.
        let at = (send_us as u64 + jitter.next_us()).max(last);
        last = at;
        let rec = match item {
            Item::Video { pts_us, .. } => {
                let f = &video.frames()[index];
                index = video.next_index(index);
                Record::Video {
                    codec: video.codec(),
                    keyframe: f.keyframe,
                    pts_us,
                    data: f.data.clone(),
                    extra_header: Vec::new(),
                }
            }
            Item::Audio { k } => {
                let frames = acfg.fixture.frames();
                Record::Audio {
                    stream_index: acfg.stream_index,
                    data: frames[k as usize % frames.len()].clone(),
                }
            }
        };
        w.record(at, &rec).map_err(|e| e.to_string())?;
    }
    let bytes = w.finish().map_err(|e| e.to_string())?;
    Replay::from_reader(&bytes[..]).map_err(|e| e.to_string())
}
