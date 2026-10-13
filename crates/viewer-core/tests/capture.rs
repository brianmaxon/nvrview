//! Live and call recordings through the core against the mock NVR, on a
//! simulated clock (docs/CAPTURE.md): the finalized MP4 of a live tile, a
//! call recording with both sides, a long outage starting a part 2, the
//! disk guard, crash recovery of a partial file, and shutdown (or dropping
//! the core) finalizing a recording in progress.

#[path = "../../mp4/tests/common/mod.rs"]
mod mp4_common;

use crate::common::{PASSWORD, USER, fast_config, on_sim, sim_spawn, sim_until, start};
use mp4_common::{boxes_in, child, top, top_box, tracks};
use simtime::SimRuntime;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use viewer_core::{
    AudioEncoder, CaptureError, Core, CoreConfig, EncoderOpened, FreeSpace, LimitKind, StreamTier,
};

/// The doorbell's channel (it has a speaker); channel 0 is a plain camera.
const BELL: u8 = 1;
const GB: u64 = 1_000_000_000;
/// A silent AAC-LC mono access unit (what the fake encoder answers).
const SILENT_AU: [u8; 4] = [0, 0, 0, 7];

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

/// A mock with a 16 kHz AAC tone on every channel and talk-back on BELL.
fn mock(sim: &SimRuntime) -> mock_nvr::MockNvr {
    let video = mock_nvr::fixture::Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let mut cfg = mock_nvr::Config::new(video);
    cfg.channels = 2;
    cfg.talk_channels = vec![BELL];
    cfg.doorbell_channels = vec![BELL];
    cfg.audio = Some(Arc::new(mock_nvr::audio::AudioConfig::new(
        mock_nvr::audio::AudioFixture::load(&fixture_path("aac-tone.aac")).unwrap(),
    )));
    sim_spawn(sim, cfg)
}

/// Free space the test sets.
struct FakeFree(AtomicU64);

impl FreeSpace for FakeFree {
    fn free_bytes(&self, _dir: &Path) -> Option<u64> {
        Some(self.0.load(Ordering::SeqCst))
    }
}

/// The app's encoder, faked: answers every frame at once with a silent
/// unit (through the core, as the app would) and keeps the PCM it got.
/// (recording, track, rate, channels, bit rate) of an `open`.
type Opened = (u64, u32, u32, u32, u32);

#[derive(Default)]
struct FakeEncoder {
    core: OnceLock<Weak<Core>>,
    pcm: Mutex<HashMap<(u64, u32), Vec<f32>>>,
    opened: Mutex<Vec<Opened>>,
    closed: Mutex<Vec<(u64, u32, bool)>>,
}

impl AudioEncoder for FakeEncoder {
    fn open(
        &self,
        recording: u64,
        track: u32,
        sample_rate: u32,
        channels: u32,
        bit_rate: u32,
    ) -> Option<EncoderOpened> {
        self.opened
            .lock()
            .unwrap()
            .push((recording, track, sample_rate, channels, bit_rate));
        Some(EncoderOpened {
            // AAC-LC, 48 kHz, mono.
            audio_specific_config: vec![0x11, 0x88],
            priming_frames: 2112,
        })
    }

    fn encode(&self, recording: u64, track: u32, pcm: &[f32]) {
        assert_eq!(pcm.len(), 1024);
        self.pcm
            .lock()
            .unwrap()
            .entry((recording, track))
            .or_default()
            .extend_from_slice(pcm);
        let core = self.core.get().and_then(Weak::upgrade).unwrap();
        core.record_audio_encoded(recording, track, &SILENT_AU)
            .unwrap();
    }

    fn close(&self, recording: u64, track: u32, flush: bool) {
        if flush {
            // The encoder's delay comes out at the end.
            let core = self.core.get().and_then(Weak::upgrade).unwrap();
            for _ in 0..2 {
                core.record_audio_encoded(recording, track, &SILENT_AU)
                    .unwrap();
            }
        }
        self.closed.lock().unwrap().push((recording, track, flush));
    }
}

impl FakeEncoder {
    fn pcm(&self, recording: u64, track: u32) -> Vec<f32> {
        self.pcm
            .lock()
            .unwrap()
            .get(&(recording, track))
            .cloned()
            .unwrap_or_default()
    }
}

struct Rig {
    sim: SimRuntime,
    nvr: mock_nvr::MockNvr,
    core: Arc<Core>,
    rec: Arc<crate::common::Recorder>,
    data: tempfile::TempDir,
    live: tempfile::TempDir,
    free: Arc<FakeFree>,
    dev: u64,
}

fn config(data: &Path) -> CoreConfig {
    CoreConfig {
        data_dir: Some(data.to_path_buf()),
        ..fast_config()
    }
}

fn configure(core: &Core, live: &Path, extra: &str) {
    let dir = serde_json::to_string(&live.to_string_lossy()).unwrap();
    core.capture_configure(&format!(r#"{{"live_dir":{dir}{extra}}}"#))
        .unwrap();
}

fn rig() -> Rig {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let data = tempfile::tempdir().unwrap();
    let live = tempfile::tempdir().unwrap();
    let (core, rec) = start(on_sim(&sim, config(data.path())));
    let free = Arc::new(FakeFree(AtomicU64::new(100 * GB)));
    core.capture_set_free_space(free.clone()).unwrap();
    configure(&core, live.path(), "");
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    Rig {
        sim,
        nvr,
        core,
        rec,
        data,
        live,
        free,
        dev,
    }
}

impl Rig {
    fn events(&self, recording: u64) -> Vec<serde_json::Value> {
        self.rec
            .events_of("recording")
            .into_iter()
            .filter(|e| e["recording"] == recording)
            .collect()
    }

    fn states(&self, recording: u64) -> Vec<String> {
        self.events(recording)
            .iter()
            .map(|e| e["state"].as_str().unwrap().to_owned())
            .collect()
    }

    fn last(&self, recording: u64) -> serde_json::Value {
        self.events(recording).pop().unwrap()
    }

    fn until_state(&self, recording: u64, state: &str, secs: u64) {
        sim_until(&self.sim, &self.core, state, secs, || {
            self.states(recording).iter().any(|s| s == state)
        });
    }

    fn until_ended(&self, recording: u64, secs: u64) -> serde_json::Value {
        sim_until(
            &self.sim,
            &self.core,
            "the end of the recording",
            secs,
            || {
                self.states(recording)
                    .iter()
                    .any(|s| s == "finished" || s == "failed")
            },
        );
        self.last(recording)
    }

    fn start(&self, request: &str) -> (u64, PathBuf) {
        let answer: serde_json::Value =
            serde_json::from_str(&self.core.record_start(request).unwrap()).unwrap();
        assert_eq!(answer["state"], "waiting_keyframe");
        (
            answer["recording"].as_u64().unwrap(),
            PathBuf::from(answer["file"].as_str().unwrap()),
        )
    }

    fn registry(&self) -> serde_json::Value {
        let p = self.data.path().join("capture").join("partials.json");
        std::fs::read(&p).map_or(serde_json::Value::Null, |b| {
            serde_json::from_slice(&b).unwrap()
        })
    }

    fn files(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.live.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }
}

/// The handler name of each track (`VideoHandler`, `SoundHandler`, ...),
/// its `tkhd` flags, and its alternate group.
fn track_headers(d: &[u8]) -> Vec<(String, u32, u16)> {
    let be32 = |i: usize| u32::from_be_bytes(d[i..i + 4].try_into().unwrap());
    let moov = top_box(d, "moov");
    boxes_in(d, moov.start + moov.header, moov.end)
        .into_iter()
        .filter(|b| b.ty == "trak")
        .map(|trak| {
            let tkhd = child(d, &trak, "tkhd").unwrap();
            let p = tkhd.start + 8;
            let v1 = d[p] == 1;
            let flags = be32(p) & 0xff_ffff;
            let g = p + if v1 { 46 } else { 34 };
            let mdia = child(d, &trak, "mdia").unwrap();
            let hdlr = child(d, &mdia, "hdlr").unwrap();
            let name = &d[hdlr.start + 8 + 24..hdlr.end];
            let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
            (
                String::from_utf8(name.to_vec()).unwrap(),
                flags,
                u16::from_be_bytes([d[g], d[g + 1]]),
            )
        })
        .collect()
}

/// A finished, standard (not fragmented) MP4: its tracks' tables.
fn read_mp4(path: &Path) -> (Vec<u8>, Vec<mp4_common::TrackTables>) {
    let d = std::fs::read(path).unwrap();
    let kinds: Vec<String> = top(&d).into_iter().map(|b| b.ty).collect();
    assert!(kinds.contains(&"moov".into()), "{kinds:?}");
    assert!(
        !kinds.contains(&"moof".into()),
        "still fragmented: {kinds:?}"
    );
    let t = tracks(&d);
    (d, t)
}

fn seconds(t: &mp4_common::TrackTables) -> f64 {
    t.media_duration as f64 / f64::from(t.timescale)
}

#[test]
fn a_live_tile_records_to_a_finalized_mp4() {
    let r = rig();
    let tile = r.core.open_tile(r.dev, 0, StreamTier::Sub).unwrap();
    sim_until(&r.sim, &r.core, "the tile's picture", 10, || {
        r.rec.decodes(tile) > 0
    });
    let (id, file) = r.start(&format!(
        r#"{{"kind":"live","tile":{tile},"name":"Front Door","utc_offset_s":0}}"#
    ));
    assert_eq!(file.parent(), Some(r.live.path()));
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.ends_with(" Front Door.mp4"), "{name}");
    r.until_state(id, "recording", 10);
    // The partial file is listed for recovery while it is written.
    let reg = r.registry();
    assert_eq!(reg["entries"][0]["id"], id);
    assert!(
        r.files()
            .iter()
            .any(|f| f.ends_with(".partial.mp4") && f.starts_with(&name[..name.len() - 4]))
    );
    // Record about 4 s of the stream.
    sim_until(&r.sim, &r.core, "4 s recorded", 20, || {
        r.last(id)["duration_ms"].as_u64().unwrap_or(0) >= 4000
    });
    // The status is current to the recorder's tick (250 ms); the event
    // stays once a second.
    let events = r.events(id).len();
    let at_event = r.last(id)["duration_ms"].as_u64().unwrap();
    let status_ms = || {
        let s: serde_json::Value = serde_json::from_str(&r.core.record_status()).unwrap();
        s["recordings"][0]["duration_ms"].as_u64().unwrap_or(0)
    };
    sim_until(&r.sim, &r.core, "the status moves on", 5, || {
        status_ms() >= at_event + 250
    });
    assert_eq!(r.events(id).len(), events, "a recording event came sooner");
    let decodes_before = r.rec.decodes(tile);
    let status: serde_json::Value = serde_json::from_str(&r.core.record_status()).unwrap();
    assert_eq!(status["recordings"][0]["recording"], id);
    assert_eq!(status["recordings"][0]["state"], "recording");
    r.core.record_stop(id).unwrap();
    let end = r.until_ended(id, 10);
    assert_eq!(end["state"], "finished");
    assert_eq!(end["reason"], "stopped");
    assert_eq!(end["fragmented"], false);
    assert_eq!(end["audio"], "passthrough");
    assert_eq!(end["tracks"], serde_json::json!(["video", "audio"]));
    assert_eq!(end["tier"], "main");
    assert_eq!(end["segment"], 1);
    assert_eq!(end["gaps"], 0);
    let states = r.states(id);
    assert_eq!(states.first().map(String::as_str), Some("waiting_keyframe"));
    assert!(states.iter().any(|s| s == "finalizing"));
    // Only the final file is left, and the registry is empty.
    assert_eq!(r.files(), std::slice::from_ref(&name));
    assert_eq!(r.registry()["entries"], serde_json::json!([]));
    assert_eq!(r.core.record_status(), r#"{"recordings":[]}"#);
    // The tile kept going on its own stream.
    sim_until(&r.sim, &r.core, "the tile goes on", 5, || {
        r.rec.decodes(tile) > decodes_before + 10
    });

    let (d, t) = read_mp4(&file);
    let headers = track_headers(&d);
    assert_eq!(
        headers,
        [
            ("VideoHandler".to_owned(), 3, 0),
            ("SoundHandler".to_owned(), 3, 0)
        ]
    );
    let (video, audio) = (&t[0], &t[1]);
    // It starts at a keyframe.
    assert_eq!(video.sync.as_ref().unwrap()[0], 1);
    // 25 fps for about the duration reported.
    let dur = end["duration_ms"].as_u64().unwrap() as f64 / 1000.0;
    let frames = video.stsz.len() as f64;
    assert!(
        (frames / 25.0 - dur).abs() < 0.2,
        "{frames} frames, {dur} s"
    );
    assert!((seconds(video) - dur).abs() < 0.2, "{} s", seconds(video));
    assert_eq!(audio.timescale, 16_000);
    assert!(
        (seconds(audio) - seconds(video)).abs() < 0.5,
        "audio {} s, video {} s",
        seconds(audio),
        seconds(video)
    );
    assert!(audio.stsz.len() > 40);
    r.sim.run_free(|| r.core.shutdown());
}

#[test]
fn a_call_recording_holds_both_sides() {
    let r = rig();
    // Without an encoder a call cannot be recorded.
    // On the main stream, so the tile sees the frames the recording gets.
    let tile = r.core.open_tile(r.dev, BELL, StreamTier::Main).unwrap();
    sim_until(&r.sim, &r.core, "the tile's picture", 10, || {
        r.rec.decodes(tile) > 0
    });
    let call = r.live.path().join("2026-10-11 17.02.03 Doorbell.mp4");
    let call_json = serde_json::to_string(&call.to_string_lossy()).unwrap();
    let req = format!(r#"{{"kind":"call","tile":{tile},"file":{call_json}}}"#);
    assert_eq!(
        r.core.record_start(&req).err(),
        Some(CaptureError::NotConfigured)
    );
    let enc = Arc::new(FakeEncoder::default());
    enc.core.set(Arc::downgrade(&r.core)).unwrap();
    r.core.capture_set_encoder(Some(enc.clone())).unwrap();

    r.core.set_talk_permission(r.dev, BELL, true).unwrap();
    r.core.set_talk_conversation(tile, true).unwrap();
    let started = r.sim.now();
    let (id, file) = r.start(&req);
    assert_eq!(file, call);
    // One call recording per camera at a time.
    assert_eq!(
        r.core.record_start(&req).err(),
        Some(CaptureError::Limit(LimitKind::Calls))
    );
    r.until_state(id, "recording", 10);
    // Talk for a second: a 500 Hz tone.
    r.core.talk_start(tile).unwrap();
    sim_until(&r.sim, &r.core, "talk active", 10, || {
        r.rec
            .events_of("talk")
            .last()
            .is_some_and(|e| e["state"] == "active")
    });
    let tone: Vec<f32> = (0..16_000)
        .map(|i| (0.5 * (2.0 * std::f64::consts::PI * 500.0 * i as f64 / 16_000.0).sin()) as f32)
        .collect();
    r.core.talk_push(tile, &tone, 16_000).unwrap();
    r.core.talk_stop(tile, true).unwrap();
    sim_until(&r.sim, &r.core, "talk stopped", 10, || {
        r.rec
            .events_of("talk")
            .last()
            .is_some_and(|e| e["state"] == "stopped")
    });
    sim_until(&r.sim, &r.core, "a second more", 10, || {
        r.last(id)["duration_ms"].as_u64().unwrap_or(0) >= 5000
    });
    // The call ends: so does its recording.
    r.core.set_talk_conversation(tile, false).unwrap();
    let end = r.until_ended(id, 10);
    assert_eq!(end["state"], "finished");
    assert_eq!(end["reason"], "stopped");
    assert_eq!(end["kind"], "call");
    assert_eq!(end["audio"], "passthrough");
    assert_eq!(
        end["tracks"],
        serde_json::json!(["video", "Conversation", "Visitor", "You"])
    );
    assert_eq!(end["encode_dropped_ms"], 0);

    // The encoder was asked for Conversation (0) and You (2); the Visitor
    // track is the camera's AAC passed through.
    let opened = enc.opened.lock().unwrap().clone();
    assert_eq!(
        opened,
        [(id, 0, 48_000, 1, 96_000), (id, 2, 48_000, 1, 96_000)]
    );
    assert_eq!(*enc.closed.lock().unwrap(), [(id, 0, true), (id, 2, true)]);

    // You: the tone, about one second of it, nowhere else.
    let you = enc.pcm(id, 2);
    let conversation = enc.pcm(id, 0);
    assert_eq!(you.len(), conversation.len());
    let loud: Vec<usize> = you
        .chunks(480)
        .enumerate()
        .filter(|(_, c)| c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32 > 0.05)
        .map(|(i, _)| i)
        .collect();
    let span = (loud.last().unwrap() - loud.first().unwrap() + 1) as f64 * 0.01;
    assert!((0.9..=1.1).contains(&span), "{span} s of tone");
    assert_eq!(loud.len(), loud.last().unwrap() - loud.first().unwrap() + 1);
    // Conversation holds both: the tone, and the camera's audio elsewhere.
    let energy = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>();
    // You sits where the doorbell received it, within 40 ms: file time 0
    // is the arrival of the first keyframe after the start, and the tone
    // begins in the first talk record that carries it.
    let seen: Vec<bool> = r
        .rec
        .seen(tile)
        .into_iter()
        .filter_map(|s| match s {
            crate::common::Seen::Decode { keyframe, .. } => Some(keyframe),
            _ => None,
        })
        .collect();
    let origin = r
        .rec
        .decode_times(tile)
        .into_iter()
        .zip(seen)
        .find(|((_, _, at), key)| *key && *at > started)
        .map(|((_, _, at), _)| at)
        .unwrap();
    let log = &r.nvr.talk_log()[0];
    let (sent_at, first) = log
        .records
        .iter()
        .find_map(|rec| {
            let pcm = rec.decode().unwrap().samples;
            pcm.iter().position(|x| x.abs() > 1000).map(|i| (rec.at, i))
        })
        .unwrap();
    let expected = sent_at.duration_since(origin).as_secs_f64() + first as f64 / 16_000.0;
    let placed = you.iter().position(|x| x.abs() > 1000.0 / 32768.0).unwrap() as f64 / 48_000.0;
    assert!(
        (placed - expected).abs() <= 0.040,
        "You at {placed:.3} s, sent at {expected:.3} s"
    );
    // After the tone: the visitor alone, in the mix and not in You.
    let after = (loud.last().unwrap() + 20) * 480;
    assert!(
        energy(&conversation[after..]) > 100.0,
        "the visitor is in the mix"
    );
    assert!(energy(&you[after..]) < 1e-6);
    let mid = loud[loud.len() / 2] * 480;
    assert!(energy(&conversation[mid..mid + 480]) > energy(&you[mid..mid + 480]) * 0.5);
    assert!(conversation.iter().all(|x| x.abs() <= 0.98));

    let (d, t) = read_mp4(&file);
    assert_eq!(
        track_headers(&d),
        [
            ("VideoHandler".to_owned(), 3, 0),
            ("Conversation".to_owned(), 3, 1),
            ("Visitor".to_owned(), 2, 1),
            ("You".to_owned(), 2, 1)
        ]
    );
    // Every frame given to the encoder came back as a unit (plus the two
    // the flush added), and the priming is an edit.
    assert_eq!(t[1].stsz.len(), conversation.len() / 1024 + 2);
    assert_eq!(t[3].stsz.len(), you.len() / 1024 + 2);
    assert_eq!(t[1].edits.last().unwrap().1, 2112);
    assert_eq!(t[2].timescale, 16_000);
    assert!(t[2].stsz.len() > 40);
    assert!((seconds(&t[1]) - seconds(&t[0])).abs() < 0.5);
    r.sim.run_free(|| r.core.shutdown());
}

#[test]
fn a_long_outage_starts_part_2_and_a_short_one_is_a_gap() {
    let r = rig();
    let tile = r.core.open_tile(r.dev, 0, StreamTier::Sub).unwrap();
    let (id, file) = r.start(&format!(r#"{{"kind":"live","tile":{tile},"name":"Yard"}}"#));
    r.until_state(id, "recording", 10);
    sim_until(&r.sim, &r.core, "2 s recorded", 10, || {
        r.last(id)["duration_ms"].as_u64().unwrap_or(0) >= 2000
    });
    // A short outage (3 s): the same file goes on, the gap counted.
    r.nvr.set_refuse_connections(true);
    r.nvr.drop_connections();
    r.sim.advance(Duration::from_secs(3));
    r.nvr.set_refuse_connections(false);
    sim_until(&r.sim, &r.core, "a counted gap", 20, || {
        r.last(id)["gaps"].as_u64().unwrap_or(0) == 1
    });
    assert_eq!(r.last(id)["segment"], 1);
    // A long one: the stream is up but silent for over a minute.
    r.nvr.set_silent_previews(true);
    r.nvr.drop_connections();
    sim_until(&r.sim, &r.core, "part 1 finalized", 90, || {
        r.files().iter().all(|f| !f.contains(".partial"))
            && r.states(id)
                .iter()
                .filter(|s| *s == "waiting_keyframe")
                .count()
                >= 2
    });
    assert!(file.exists());
    r.nvr.set_silent_previews(false);
    r.nvr.drop_connections();
    r.until_state(id, "segment", 20);
    let seg = r
        .events(id)
        .into_iter()
        .find(|e| e["state"] == "segment")
        .unwrap();
    assert_eq!(seg["segment"], 2);
    let part2 = PathBuf::from(seg["file"].as_str().unwrap());
    let stem = file.file_stem().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        part2.file_name().unwrap().to_string_lossy(),
        format!("{stem} (part 2).mp4")
    );
    sim_until(&r.sim, &r.core, "part 2 under way", 10, || {
        r.last(id)["state"] == "recording"
    });
    r.core.record_stop(id).unwrap();
    let end = r.until_ended(id, 10);
    assert_eq!(end["reason"], "stopped");
    assert_eq!(end["segment"], 2);
    assert_eq!(end["file"], seg["file"]);
    // Both parts are finished files that start at a keyframe.
    for p in [&file, &part2] {
        let (_, t) = read_mp4(p);
        assert_eq!(t[0].sync.as_ref().unwrap()[0], 1);
        assert!(t[0].stsz.len() > 10);
    }
    // Part 1 holds the short gap: its video is longer than its frames.
    let (_, t) = read_mp4(&file);
    assert!(seconds(&t[0]) > t[0].stsz.len() as f64 / 25.0 + 0.05);
    r.sim.run_free(|| r.core.shutdown());
}

#[test]
fn low_disk_refuses_a_start_and_stops_a_recording() {
    let r = rig();
    let tile = r.core.open_tile(r.dev, 0, StreamTier::Sub).unwrap();
    let req = format!(r#"{{"kind":"live","tile":{tile}}}"#);
    r.free.0.store(GB, Ordering::SeqCst);
    let e = r.core.record_start(&req).unwrap_err();
    assert_eq!(e, CaptureError::Limit(LimitKind::DiskLow));
    assert_eq!(e.limit_answer().as_deref(), Some(r#"{"limit":"disk_low"}"#));
    assert!(r.files().is_empty());

    r.free.0.store(3 * GB, Ordering::SeqCst);
    let (id, file) = r.start(&req);
    r.until_state(id, "recording", 10);
    sim_until(&r.sim, &r.core, "1 s recorded", 10, || {
        r.last(id)["duration_ms"].as_u64().unwrap_or(0) >= 1000
    });
    // Under 1 GB plus the file: it stops within the 5 s check interval.
    r.free.0.store(GB / 2, Ordering::SeqCst);
    let end = r.until_ended(id, 6);
    assert_eq!(end["state"], "finished");
    assert_eq!(end["reason"], "disk_low");
    assert_eq!(end["fragmented"], false);
    read_mp4(&file);
    r.sim.run_free(|| r.core.shutdown());
}

#[test]
fn the_count_limits_refuse_with_their_code() {
    let r = rig();
    configure(&r.core, r.live.path(), r#","max_live":1"#);
    let tile = r.core.open_tile(r.dev, 0, StreamTier::Sub).unwrap();
    let req = format!(r#"{{"kind":"live","tile":{tile}}}"#);
    let (id, _) = r.start(&req);
    assert_eq!(
        r.core.record_start(&req).err(),
        Some(CaptureError::Limit(LimitKind::Live))
    );
    assert_eq!(
        r.core.record_start(r#"{"kind":"live","tile":999}"#).err(),
        Some(CaptureError::UnknownId)
    );
    assert_eq!(
        r.core.record_start(r#"{"kind":"live"}"#).err(),
        Some(CaptureError::InvalidArgument)
    );
    assert_eq!(r.core.record_stop(999), Err(CaptureError::UnknownId));
    r.core.record_stop(id).unwrap();
    r.until_ended(id, 10);
    // At most two main-stream recorders per device.
    configure(&r.core, r.live.path(), r#","max_live":4"#);
    let a = r.start(&req).0;
    let b = r.start(&req).0;
    assert_eq!(
        r.core.record_start(&req).err(),
        Some(CaptureError::Limit(LimitKind::MainStreams))
    );
    let c = r
        .start(&format!(
            r#"{{"kind":"live","tile":{tile},"quality":"sub"}}"#
        ))
        .0;
    for id in [a, b, c] {
        r.core.record_stop(id).unwrap();
        r.until_ended(id, 10);
    }
    r.sim.run_free(|| r.core.shutdown());
}

/// Records a while, then copies the partial file and the registry as a
/// crash would have left them.
fn crashed_state(r: &Rig) -> (PathBuf, Vec<u8>, Vec<u8>) {
    let tile = r.core.open_tile(r.dev, 0, StreamTier::Sub).unwrap();
    let (id, file) = r.start(&format!(r#"{{"kind":"live","tile":{tile}}}"#));
    r.until_state(id, "recording", 10);
    sim_until(&r.sim, &r.core, "3 s recorded", 10, || {
        r.last(id)["duration_ms"].as_u64().unwrap_or(0) >= 3000
    });
    let partial = file.with_file_name(format!(
        "{}.partial.mp4",
        file.file_stem().unwrap().to_string_lossy()
    ));
    let bytes = std::fs::read(&partial).unwrap();
    let registry = std::fs::read(r.data.path().join("capture").join("partials.json")).unwrap();
    r.core.record_stop(id).unwrap();
    r.until_ended(id, 10);
    (file, bytes, registry)
}

#[test]
fn a_partial_file_left_by_a_crash_is_recovered() {
    let r = rig();
    let (file, bytes, registry) = crashed_state(&r);
    r.sim.run_free(|| r.core.shutdown());
    // The crash: the partial file is all there is.
    std::fs::remove_file(&file).unwrap();
    let partial = file.with_file_name(format!(
        "{}.partial.mp4",
        file.file_stem().unwrap().to_string_lossy()
    ));
    // Cut inside the last fragment, as a crash mid-write would.
    std::fs::write(&partial, &bytes[..bytes.len() - 100]).unwrap();
    // A second entry whose partial file never got a fragment.
    let tiny = r.live.path().join("tiny.partial.mp4");
    std::fs::write(&tiny, [0u8; 100]).unwrap();
    let mut reg: serde_json::Value = serde_json::from_slice(&registry).unwrap();
    let entry = serde_json::json!({
        "id": 999,
        "kind": "live",
        "partial": tiny.to_string_lossy(),
        "target": r.live.path().join("tiny.mp4").to_string_lossy(),
    });
    reg["entries"].as_array_mut().unwrap().push(entry);
    std::fs::write(
        r.data.path().join("capture").join("partials.json"),
        serde_json::to_vec(&reg).unwrap(),
    )
    .unwrap();
    // An unlisted partial file is never touched.
    let stranger = r.live.path().join("other.partial.mp4");
    std::fs::write(&stranger, [1u8; 5000]).unwrap();

    // The next launch.
    let sim = SimRuntime::new();
    let (core, rec) = start(on_sim(&sim, config(r.data.path())));
    configure(&core, r.live.path(), "");
    core.flush_events();
    let recovered: Vec<_> = rec
        .events_of("recording")
        .into_iter()
        .filter(|e| e["state"] == "recovered")
        .collect();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0]["file"], file.to_string_lossy().as_ref());
    let (_, t) = read_mp4(&file);
    assert_eq!(t[0].sync.as_ref().unwrap()[0], 1);
    assert!(t[0].stsz.len() > 25);
    assert!(!partial.exists());
    assert!(!tiny.exists());
    assert!(!r.live.path().join("tiny.mp4").exists());
    assert!(stranger.exists());
    let reg: serde_json::Value = serde_json::from_slice(
        &std::fs::read(r.data.path().join("capture").join("partials.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(reg["entries"], serde_json::json!([]));
    // A second configure does not recover again.
    configure(&core, r.live.path(), "");
    core.flush_events();
    assert_eq!(
        rec.events_of("recording")
            .iter()
            .filter(|e| e["state"] == "recovered")
            .count(),
        1
    );
    sim.run_free(|| core.shutdown());
}

fn shutdown_finalizes(drop_core: bool) {
    let r = rig();
    let tile = r.core.open_tile(r.dev, 0, StreamTier::Sub).unwrap();
    let (id, file) = r.start(&format!(r#"{{"kind":"live","tile":{tile}}}"#));
    r.until_state(id, "recording", 10);
    sim_until(&r.sim, &r.core, "2 s recorded", 10, || {
        r.last(id)["duration_ms"].as_u64().unwrap_or(0) >= 2000
    });
    let Rig { sim, core, rec, .. } = r;
    if drop_core {
        sim.run_free(move || drop(core));
    } else {
        sim.run_free(|| core.shutdown());
        assert_eq!(
            core.record_start(&format!(r#"{{"kind":"live","tile":{tile}}}"#))
                .err(),
            Some(CaptureError::Closed)
        );
    }
    let end = rec
        .events_of("recording")
        .into_iter()
        .rfind(|e| e["recording"] == id)
        .unwrap();
    assert_eq!(end["state"], "finished");
    assert_eq!(end["reason"], "core_shutdown");
    let (_, t) = read_mp4(&file);
    assert!(t[0].stsz.len() >= 50);
    let names: Vec<String> = std::fs::read_dir(file.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    // The device's login was let go.
    assert_eq!(r.nvr.active_logins(), 0);
    let _ = (r.data, r.live, r.free, r.dev);
}

#[test]
fn shutdown_mid_recording_finalizes() {
    shutdown_finalizes(false);
}

#[test]
fn dropping_the_core_mid_recording_finalizes() {
    shutdown_finalizes(true);
}

#[test]
fn a_recording_outlives_its_tile_and_ends_with_its_device() {
    let r = rig();
    let tile = r.core.open_tile(r.dev, 0, StreamTier::Sub).unwrap();
    let (id, file) = r.start(&format!(r#"{{"kind":"live","tile":{tile}}}"#));
    r.until_state(id, "recording", 10);
    r.core.close_tile(tile).unwrap();
    let at = r.last(id)["duration_ms"].as_u64().unwrap();
    sim_until(&r.sim, &r.core, "recording without its tile", 10, || {
        r.last(id)["duration_ms"].as_u64().unwrap() >= at + 2000
    });
    r.core.remove_device(r.dev).unwrap();
    let end = r.until_ended(id, 5);
    assert_eq!(end["reason"], "device_removed");
    read_mp4(&file);
    r.sim.run_free(|| r.core.shutdown());
}

/// The apps' configuration: each recorder on a thread of its own, the
/// device on a real socket (conditions are polled, nothing waits a guessed
/// time).
#[test]
fn recorders_run_on_their_own_threads() {
    let nvr = crate::common::mock(1);
    let data = tempfile::tempdir().unwrap();
    let live = tempfile::tempdir().unwrap();
    let (core, rec) = start(config(data.path()));
    core.capture_set_free_space(Arc::new(FakeFree(AtomicU64::new(100 * GB))))
        .unwrap();
    configure(&core, live.path(), "");
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    let answer: serde_json::Value = serde_json::from_str(
        &core
            .record_start(&format!(r#"{{"kind":"live","tile":{tile}}}"#))
            .unwrap(),
    )
    .unwrap();
    let id = answer["recording"].as_u64().unwrap();
    let file = PathBuf::from(answer["file"].as_str().unwrap());
    let last = || {
        core.flush_events();
        rec.events_of("recording")
            .into_iter()
            .rfind(|e| e["recording"] == id)
    };
    crate::common::wait_until("a second recorded", 20, || {
        last().is_some_and(|e| e["duration_ms"].as_u64().unwrap_or(0) >= 1000)
    });
    core.record_stop(id).unwrap();
    crate::common::wait_until("finished", 20, || {
        last().is_some_and(|e| e["state"] == "finished")
    });
    let (_, t) = read_mp4(&file);
    assert_eq!(t[0].sync.as_ref().unwrap()[0], 1);
    assert!(t[0].stsz.len() >= 25);
    core.shutdown();
}

#[test]
fn a_resolution_change_starts_the_next_part() {
    let sim = SimRuntime::new();
    let video = mock_nvr::fixture::Fixture::load(&fixture_path("h264-resize.h264")).unwrap();
    let mut cfg = mock_nvr::Config::new(video);
    cfg.channels = 1;
    let nvr = sim_spawn(&sim, cfg);
    let data = tempfile::tempdir().unwrap();
    let live = tempfile::tempdir().unwrap();
    let (core, rec) = start(on_sim(&sim, config(data.path())));
    core.capture_set_free_space(Arc::new(FakeFree(AtomicU64::new(100 * GB))))
        .unwrap();
    configure(&core, live.path(), "");
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    let answer: serde_json::Value = serde_json::from_str(
        &core
            .record_start(&format!(r#"{{"kind":"live","tile":{tile}}}"#))
            .unwrap(),
    )
    .unwrap();
    let id = answer["recording"].as_u64().unwrap();
    let events = || {
        rec.events_of("recording")
            .into_iter()
            .filter(|e| e["recording"] == id)
            .collect::<Vec<_>>()
    };
    sim_until(&sim, &core, "the next part", 30, || {
        events().iter().any(|e| e["state"] == "segment")
    });
    let at = events().last().unwrap()["duration_ms"].as_u64().unwrap();
    sim_until(&sim, &core, "a second of the next part", 10, || {
        events().last().unwrap()["duration_ms"].as_u64().unwrap() >= at + 1000
    });
    core.record_stop(id).unwrap();
    sim_until(&sim, &core, "finished", 10, || {
        events().iter().any(|e| e["state"] == "finished")
    });
    let parts: Vec<PathBuf> = std::fs::read_dir(live.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(parts.len(), 2, "{parts:?}");
    for p in &parts {
        let (_, t) = read_mp4(p);
        // Starts at a keyframe (no `stss` when every frame is one).
        assert!(t[0].sync.as_ref().is_none_or(|s| s[0] == 1));
        assert!(t[0].stsz.len() > 1, "{} frames", t[0].stsz.len());
        // One picture size per file.
        assert_eq!(t[0].stsd_entries.len(), 1);
    }
    sim.run_free(|| core.shutdown());
}
