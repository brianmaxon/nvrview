//! The MP4 step: chunks from the mock NVR's archive store (and synthetic
//! chunks built by hand) become one standard MP4. Structure is checked with
//! ffprobe where it is installed.

use bc_protocol::archive::DateTime;
use bc_protocol::media::{Codec, Record};
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, FnConnector, SessionConfig};
use mock_nvr::archive::{ArchiveConfig, MockEvent};
use mock_nvr::audio::AudioFixture;
use mock_nvr::fixture::Fixture;
use recordings::chunk::{self, ChunkWriter};
use recordings::finalize::{JOINED_NAME, Mp4Finalizer, join_chunks, recover_fragmented};
use recordings::incident::{CameraEvent, CameraId, GroupingConfig, group};
use recordings::scheduler::{
    CameraSource, CameraState, IncidentDownload, PARTS_DIR, Scheduler, SchedulerConfig,
};
use recordings::secs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

fn have_ffprobe() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// (pts seconds, keyframe) of every packet of stream `kind` ("v" or "a").
fn packets(path: &Path, kind: &str) -> Vec<(f64, bool)> {
    let o = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", &format!("{kind}:0")])
        .args(["-show_entries", "packet=pts_time,flags", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "ffprobe: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let (t, f) = l.split_once(',')?;
            Some((t.parse().ok()?, f.starts_with('K')))
        })
        .collect()
}

/// Top-level box types of a file, in order.
fn top_level_boxes(path: &Path) -> Vec<String> {
    let b = std::fs::read(path).unwrap();
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 8 <= b.len() {
        let mut size = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        out.push(String::from_utf8_lossy(&b[at + 4..at + 8]).into_owned());
        if size == 1 {
            size = u64::from_be_bytes(b[at + 8..at + 16].try_into().unwrap()) as usize;
        }
        assert!(size >= 8 && at + size <= b.len(), "box {out:?} overruns");
        at += size;
    }
    assert_eq!(at, b.len());
    out
}

/// Decodes every frame to check the file is playable; returns the number of
/// decoded frames.
fn decode_frames(path: &Path) -> usize {
    let o = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        o.status.success() && o.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().parse().unwrap()
}

fn spawn_mock(audio: bool) -> mock_nvr::MockNvr {
    let mut cfg = mock_nvr::Config::new(Fixture::load(&fixture_path("h264-basic.h264")).unwrap());
    cfg.channels = 1;
    let mut a = ArchiveConfig::new(vec![d(10, 0, 0, 0)]);
    a.events_per_day = 0;
    a.events = vec![MockEvent {
        channel: 0,
        kind: "vehicle".into(),
        start: d(10, 7, 42, 5),
        end: d(10, 7, 42, 25),
    }];
    if audio {
        a.audio = Some(Arc::new(
            AudioFixture::load(&fixture_path("aac-tone.aac")).unwrap(),
        ));
    }
    cfg.archive = Some(a);
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

fn session(nvr: &mock_nvr::MockNvr) -> Arc<DeviceSession> {
    let mut cfg = SessionConfig::new("example-user", "example-password");
    cfg.query_timeout = Duration::from_secs(5);
    cfg.backoff.initial = Duration::from_millis(50);
    cfg.backoff.jitter = 0.0;
    Arc::new(DeviceSession::spawn(in_memory(nvr), cfg))
}

fn download() -> IncidentDownload {
    let events = vec![CameraEvent {
        start: secs(d(10, 7, 42, 5)),
        end: secs(d(10, 7, 42, 25)),
        camera: CameraId::new("nvr-a", 0),
        kind: "vehicle".into(),
    }];
    IncidentDownload {
        incident: group(&events, &GroupingConfig::default())
            .into_iter()
            .next()
            .unwrap(),
        cameras: vec![CameraSource {
            camera: CameraId::new("nvr-a", 0),
            name: "Front".into(),
            uid: None,
            stream: StreamTier::Main,
        }],
    }
}

fn config(root: &Path) -> SchedulerConfig {
    let mut c = SchedulerConfig::new(root);
    c.chunk_secs = 10;
    c.retry_delay = Duration::from_millis(50);
    c
}

/// Waits until every camera is `Done` or has given up, or (without a
/// finalizer) has all its chunks.
async fn wait_done(s: &Scheduler, finalizing: bool) {
    let end = |c: &CameraState| match c {
        CameraState::Done | CameraState::Failed(_) => true,
        CameraState::ChunksComplete => !finalizing,
        _ => false,
    };
    tokio::time::timeout(
        Duration::from_secs(30),
        s.wait_for(|p| !p.is_empty() && p.iter().all(|i| i.cameras.iter().all(|c| end(&c.state)))),
    )
    .await
    .expect("download did not settle");
}

/// Video and audio frame counts and times of every complete chunk, with the
/// overlap removed the way the join should.
fn expected(parts: &Path, n: u32) -> (Vec<u64>, usize) {
    let mut pts: Vec<u64> = Vec::new();
    let mut audio = 0usize;
    for i in 0..n {
        let idx = chunk::read_index(parts, i).unwrap().unwrap();
        let last = pts.last().copied();
        let new: Vec<_> = idx
            .video
            .iter()
            .enumerate()
            .filter(|(_, v)| last.is_none_or(|l| v.pts_us > l))
            .collect();
        let first_new = new.first().map_or(usize::MAX, |(i, _)| *i);
        audio += idx
            .audio
            .iter()
            .filter(|a| last.is_none() || a.after_video as usize >= first_new)
            .count();
        pts.extend(new.iter().map(|(_, v)| v.pts_us));
    }
    (pts, audio)
}

#[tokio::test(start_paused = true)]
async fn downloaded_incident_becomes_one_seamless_mp4() {
    let nvr = spawn_mock(true);
    let sess = session(&nvr);
    let root = tempfile::tempdir().unwrap();
    let fin = Arc::new(Mp4Finalizer::default());
    let sched = Scheduler::new(config(root.path()), Some(fin));
    sched.add_device("nvr-a", sess.clone());
    let folder = sched.enqueue(&download()).unwrap();
    wait_done(&sched, true).await;

    let p = sched.progress().borrow().clone();
    let cam = &p[0].cameras[0];
    assert_eq!(cam.state, CameraState::Done);
    let out = cam.output.clone();
    assert!(out.is_file());
    let parts = folder.join(PARTS_DIR).join("nvr-a-ch00");
    assert!(!parts.join(JOINED_NAME).exists(), "fragmented file removed");
    assert!(!out.with_extension("mp4.tmp").exists());

    // Chunks overlap (the device cuts at the keyframe before each window),
    // so the join has to have dropped frames.
    let n = cam.chunks_total as u32;
    assert!(n >= 3);
    let raw: usize = (0..n)
        .map(|i| chunk::read_index(&parts, i).unwrap().unwrap().video.len())
        .sum();
    let (pts, audio_frames) = expected(&parts, n);
    assert!(pts.len() < raw, "overlap existed: {raw} raw, {}", pts.len());
    // Mock frames are every 100 ms and the cut windows touch: a joined
    // stream has no repeated frame and no hole.
    assert!(pts.windows(2).all(|w| w[1] - w[0] == 100_000));

    // Standard layout: ftyp, moov before mdat (faststart), no fragments.
    let boxes = top_level_boxes(&out);
    assert_eq!(boxes[0], "ftyp");
    let moov = boxes.iter().position(|b| b == "moov").unwrap();
    let mdat = boxes.iter().position(|b| b == "mdat").unwrap();
    assert!(moov < mdat, "{boxes:?}");
    assert!(!boxes.iter().any(|b| b == "moof"), "{boxes:?}");

    if have_ffprobe() {
        let v = packets(&out, "v");
        assert_eq!(v.len(), pts.len(), "every unique frame, none twice");
        assert!(v[0].1, "starts at a keyframe");
        assert!(v.windows(2).all(|w| w[1].0 > w[0].0), "times increase");
        for (got, want) in v.iter().zip(&pts) {
            let rel = (*want - pts[0]) as f64 / 1e6;
            assert!((got.0 - rel).abs() < 1e-3, "{} vs {rel}", got.0);
        }
        // The audio frames of the chunks, minus the overlap's, in one track
        // (64 ms each, played back to back, no gaps over a second).
        let a = packets(&out, "a");
        assert_eq!(a.len(), audio_frames);
        assert!(a.windows(2).all(|w| (w[1].0 - w[0].0 - 0.064).abs() < 0.01));
        assert!(a[0].0.abs() < 0.2, "audio starts with the video");
        assert_eq!(decode_frames(&out), pts.len());
    }

    sched.shutdown().await;
    drop(sched);
    if let Ok(s) = Arc::try_unwrap(sess) {
        s.close().await;
    }
}

#[tokio::test(start_paused = true)]
async fn chunks_left_by_a_run_without_a_muxer_are_finalized_on_restart() {
    let nvr = spawn_mock(false);
    let sess = session(&nvr);
    let root = tempfile::tempdir().unwrap();
    let first = Scheduler::new(config(root.path()), None);
    first.add_device("nvr-a", sess.clone());
    first.enqueue(&download()).unwrap();
    wait_done(&first, false).await;
    let out = first.progress().borrow()[0].cameras[0].output.clone();
    assert_eq!(
        first.progress().borrow()[0].cameras[0].state,
        CameraState::ChunksComplete
    );
    assert!(!out.exists());
    first.shutdown().await;
    drop(first);

    let second = Scheduler::new(config(root.path()), Some(Arc::new(Mp4Finalizer::default())));
    second.add_device("nvr-a", sess.clone());
    assert_eq!(second.resume_all().unwrap(), 1);
    wait_done(&second, true).await;
    assert_eq!(
        second.progress().borrow()[0].cameras[0].state,
        CameraState::Done
    );
    assert!(out.is_file());
    if have_ffprobe() {
        // No audio in this archive: a video-only file.
        assert!(packets(&out, "a").is_empty());
        assert!(!packets(&out, "v").is_empty());
    }
    second.shutdown().await;
    drop(second);
    if let Ok(s) = Arc::try_unwrap(sess) {
        s.close().await;
    }
}

// ---- synthetic chunks ------------------------------------------------------

/// The fixture has 60 frames with keyframes at 0, 20, and 40. Frame `f` is
/// stamped `f` * 100 ms (so overlapping chunks agree), and carries one audio
/// frame after it when `f` is even.
const FRAMES: usize = 60;

/// Writes chunk `n` holding the fixture's frames `frames`.
fn write_chunk(dir: &Path, n: u32, fx: &Fixture, frames: std::ops::Range<usize>) {
    assert_eq!(fx.first_keyframe(), 0);
    let mut w = ChunkWriter::create(dir, n, 0, 10).unwrap();
    let aac = AudioFixture::load(&fixture_path("aac-tone.aac")).unwrap();
    for f in frames {
        let fr = &fx.frames()[f];
        w.write(&Record::Video {
            codec: Codec::H264,
            keyframe: fr.keyframe,
            pts_us: f as u64 * 100_000,
            data: fr.data.clone(),
            extra_header: Vec::new(),
        })
        .unwrap();
        if f % 2 == 0 {
            w.write(&Record::Audio {
                stream_index: 5,
                data: aac.frames()[f / 2 % aac.frames().len()].clone(),
            })
            .unwrap();
        }
    }
    w.finish().unwrap();
}

#[test]
fn overlap_is_dropped_by_timestamp_and_audio_follows_its_position() {
    let fx = Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    assert_eq!(fx.frames().len(), FRAMES);
    let dir = tempfile::tempdir().unwrap();
    // Chunks start at keyframes, as the device cuts them, and each repeats
    // the end of the one before.
    write_chunk(dir.path(), 0, &fx, 0..30);
    write_chunk(dir.path(), 1, &fx, 20..50);
    write_chunk(dir.path(), 2, &fx, 40..60);
    let out = dir.path().join("joined.fmp4");
    let (summary, stats) = join_chunks(dir.path(), 3, &out, false).unwrap();
    assert_eq!(stats.video_written, 60);
    assert_eq!(stats.video_duplicates, 20);
    // 30 even frames carry audio; those of the 20 repeated frames (10) came
    // again with the overlap and are dropped.
    assert_eq!(stats.audio_written, 30);
    assert_eq!(stats.audio_duplicates, 10);
    assert_eq!(summary.video_samples, 60);
    assert_eq!(summary.audio_samples, 30);

    let mp4 = dir.path().join("out.mp4");
    recover_fragmented(&out, &mp4, true).unwrap();
    assert!(!out.exists());
    if have_ffprobe() {
        let v = packets(&mp4, "v");
        assert_eq!(v.len(), 60);
        assert!(v.windows(2).all(|w| (w[1].0 - w[0].0 - 0.1).abs() < 1e-3));
        assert_eq!(packets(&mp4, "a").len(), 30);
        assert_eq!(decode_frames(&mp4), 60);
    }
}

#[test]
fn an_incomplete_chunk_stops_the_join_without_a_partial_output() {
    let fx = Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_chunk(dir.path(), 0, &fx, 0..12);
    let fin = Mp4Finalizer::default();
    let out = dir.path().join("out.mp4");
    let e = fin.finalize_with_stats(dir.path(), 2, &out).unwrap_err();
    assert!(e.to_string().contains("chunk 1"), "{e}");
    assert!(!out.exists());
    assert!(!dir.path().join("out.mp4.tmp").exists());
    // A chunk with no video contributes nothing.
    let mut w = ChunkWriter::create(dir.path(), 1, 0, 10).unwrap();
    w.write(&Record::Info {
        width: 0,
        height: 0,
        fps: 10,
    })
    .unwrap();
    w.finish().unwrap();
    let stats = fin.finalize_with_stats(dir.path(), 2, &out).unwrap();
    assert_eq!((stats.video_written, stats.empty_chunks), (12, 1));
    assert!(out.is_file());
}

#[test]
fn a_killed_join_leaves_a_fragmented_file_that_recovers() {
    let fx = Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_chunk(dir.path(), 0, &fx, 0..60);
    let joined = dir.path().join(JOINED_NAME);
    join_chunks(dir.path(), 1, &joined, true).unwrap();
    let whole = std::fs::read(&joined).unwrap();
    let boxes = top_level_boxes(&joined);
    assert_eq!(&boxes[..2], ["ftyp", "moov"], "{boxes:?}");
    assert!(boxes.contains(&"moof".to_string()) && boxes.last().unwrap() == "mfra");

    // Cut the file inside its last fragment (and, for good measure, after
    // the mfra index was cut off with it): as if killed mid-write.
    for cut in [whole.len() - 1, whole.len() * 2 / 3, whole.len() / 2] {
        std::fs::write(&joined, &whole[..cut]).unwrap();
        let mp4 = dir.path().join(format!("recovered-{cut}.mp4"));
        let s = recover_fragmented(&joined, &mp4, true).unwrap();
        assert!(s.video_samples > 0 && s.video_samples <= 60);
        if have_ffprobe() {
            let v = packets(&mp4, "v");
            assert_eq!(v.len() as u64, s.video_samples);
            assert!(v[0].1);
            assert_eq!(decode_frames(&mp4), v.len());
        }
        assert!(!dir.path().join(format!("recovered-{cut}.mp4.tmp")).exists());
    }
    // The partial file itself (not yet finalized) plays up to its last
    // complete fragment.
    std::fs::write(&joined, &whole[..whole.len() * 2 / 3]).unwrap();
    if have_ffprobe() {
        assert!(!packets(&joined, "v").is_empty());
    }
}

/// Connects to `nvr` (on the test's runtime) through in-memory pipes.
fn in_memory(nvr: &mock_nvr::MockNvr) -> impl bc_session::Connector + use<> {
    let addr = nvr.addr();
    FnConnector(move || mock_nvr::connect(addr))
}

fn fnv(d: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in d {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// A download's join and finalize give the same bytes as the single-track
/// writer did before multi-track audio was added to `mp4-remux` (length and
/// FNV-1a digest of the fragmented and the final file, recorded then).
#[test]
fn joined_download_bytes_are_unchanged() {
    let fx = Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_chunk(dir.path(), 0, &fx, 0..30);
    write_chunk(dir.path(), 1, &fx, 20..50);
    write_chunk(dir.path(), 2, &fx, 40..60);
    let joined = dir.path().join(JOINED_NAME);
    join_chunks(dir.path(), 3, &joined, false).unwrap();
    let frag = std::fs::read(&joined).unwrap();
    let mp4 = dir.path().join("out.mp4");
    recover_fragmented(&joined, &mp4, true).unwrap();
    let fin = std::fs::read(&mp4).unwrap();
    let got = [(frag.len(), fnv(&frag)), (fin.len(), fnv(&fin))];
    assert_eq!(
        got,
        [
            (0xdc80, 0x3ffc_b61e_619f_462d),
            (0xd7ce, 0x160a_9324_3249_18b7)
        ]
    );
}
