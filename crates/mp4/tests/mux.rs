//! End-to-end tests: pure-Rust structural checks (always on) and ffprobe/ffmpeg
//! checks (skipped when ffmpeg is not installed).

use crate::common::*;
use mp4_remux::{Error, Mp4Writer, Options, VideoCodec, VideoFrame};
use std::io::Cursor;

const STEP: i64 = 50_000; // 20 fps, as the fixtures

fn load(codec: VideoCodec, name: &str) -> Vec<Au> {
    split_aus(&fixture(name), codec)
}

fn tone() -> Vec<Vec<u8>> {
    adts_frames(&fixture("aac-tone.aac"))
}

fn check_samples(file: &[u8], aus: &[Au], codec: VideoCodec) {
    let t = &tracks(file)[0];
    assert_eq!(t.stsz.len(), aus.len());
    for (got, au) in sample_bytes(file, t).iter().zip(aus) {
        assert_eq!(got, &expected_sample(&au.data, codec));
    }
    let sync: Vec<u32> = aus
        .iter()
        .enumerate()
        .filter(|(_, a)| a.key)
        .map(|(i, _)| i as u32 + 1)
        .collect();
    assert_eq!(t.sync.as_deref(), Some(&sync[..]));
}

#[test]
fn fixtures_split_as_expected() {
    let a = load(VideoCodec::H264, "h264-basic.h264");
    assert_eq!(a.len(), 60);
    assert_eq!(a.iter().filter(|a| a.key).count(), 3);
    let a = load(VideoCodec::H265, "h265-basic.h265");
    assert_eq!(a.len(), 60);
    assert_eq!(a.iter().filter(|a| a.key).count(), 3);
}

fn basic_structure(codec: VideoCodec, name: &str, entry: &str) {
    let aus = load(codec, name);
    let audio = tone();
    let (file, summary) = mux(codec, &aus, STEP, &audio, Options::default());
    assert_eq!(summary.video_samples, 60);
    assert_eq!(summary.audio_samples, audio.len() as u64);
    assert_eq!(summary.file_size, file.len() as u64);
    assert!(!summary.uses_co64);
    let kinds: Vec<String> = top(&file).into_iter().map(|b| b.ty).collect();
    assert_eq!(kinds, ["ftyp", "mdat", "moov"]);
    check_samples(&file, &aus, codec);
    let ts = tracks(&file);
    assert_eq!(ts.len(), 2);
    let v = &ts[0];
    assert_eq!(v.handler, "vide");
    assert_eq!(v.stsd_entries, [entry]);
    assert_eq!(v.timescale, 90_000);
    assert_eq!(v.stts, [(60, 4500)]);
    assert_eq!(v.media_duration, 60 * 4500);
    assert!(v.ctts.is_empty());
    let a = &ts[1];
    assert_eq!(a.handler, "soun");
    assert_eq!(a.stsd_entries, ["mp4a"]);
    assert_eq!(a.timescale, 16_000);
    assert_eq!(a.stts, [(audio.len() as u32, 1024)]);
    assert_eq!(a.sync, None);
    // Audio payloads are raw AAC: the ADTS header is gone.
    for (got, frame) in sample_bytes(&file, a).iter().zip(&audio) {
        assert_eq!(got.as_slice(), &frame[7..]);
    }
    // Aligned start: no edit lists needed.
    assert!(v.edits.is_empty() && a.edits.is_empty());
}

#[test]
fn h264_structure() {
    basic_structure(VideoCodec::H264, "h264-basic.h264", "avc1");
}

#[test]
fn h265_structure() {
    basic_structure(VideoCodec::H265, "h265-basic.h265", "hvc1");
}

#[test]
fn faststart_puts_moov_first_and_offsets_are_valid() {
    for (codec, name) in [
        (VideoCodec::H264, "h264-basic.h264"),
        (VideoCodec::H265, "h265-basic.h265"),
    ] {
        let aus = load(codec, name);
        let opts = Options {
            faststart: true,
            ..Options::default()
        };
        let (file, s) = mux(codec, &aus, STEP, &tone(), opts);
        let kinds: Vec<String> = top(&file).into_iter().map(|b| b.ty).collect();
        assert_eq!(kinds, ["ftyp", "moov", "mdat"]);
        assert_eq!(s.file_size, file.len() as u64);
        check_samples(&file, &aus, codec);
        let a = &tracks(&file)[1];
        let frames = tone();
        for (got, frame) in sample_bytes(&file, a).iter().zip(&frames) {
            assert_eq!(got.as_slice(), &frame[7..]);
        }
    }
}

#[test]
fn irregular_timestamps_become_per_sample_durations() {
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let mut w = Mp4Writer::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options::default(),
    )
    .unwrap();
    let mut t = 5_000_000i64;
    let mut deltas = Vec::new();
    for (i, au) in aus.iter().enumerate() {
        w.write_video(VideoFrame::new(&au.data, au.key, t)).unwrap();
        let d = 33_333 + (i as i64 * 7919) % 60_000;
        deltas.push(d);
        t += d;
    }
    let (file, _) = w.finish().unwrap();
    let file = file.into_inner();
    let v = &tracks(&file)[0];
    let dts = v.dts();
    for i in 1..dts.len() {
        let want = mp4_remux::us_to_ticks(5_000_000 + deltas[..i].iter().sum::<i64>())
            - mp4_remux::us_to_ticks(5_000_000);
        assert_eq!(dts[i] as i64, want, "sample {i}");
    }
    // Last sample repeats the previous delta.
    let last = dts[dts.len() - 1] - dts[dts.len() - 2];
    assert_eq!(v.media_duration, dts[dts.len() - 1] + last);
}

#[test]
fn audio_start_offsets_become_edit_lists() {
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    // Video starts at 1.000 s; audio starts 0.25 s later.
    let opts = Options {
        audio_start_us: Some(1_250_000),
        ..Options::default()
    };
    let (file, _) = mux(VideoCodec::H264, &aus, STEP, &tone(), opts);
    let ts = tracks(&file);
    assert!(ts[0].edits.is_empty());
    assert_eq!(ts[1].edits[0], (22_500, -1));
    assert_eq!(ts[1].edits[1].1, 0);
    // Audio 0.25 s before video: video gets the empty edit.
    let opts = Options {
        audio_start_us: Some(750_000),
        ..Options::default()
    };
    let (file, _) = mux(VideoCodec::H264, &aus, STEP, &tone(), opts);
    let ts = tracks(&file);
    assert_eq!(ts[0].edits[0], (22_500, -1));
    assert!(ts[1].edits.is_empty());
}

#[test]
fn resolution_change_adds_a_sample_description() {
    for (codec, name, entry) in [
        (VideoCodec::H264, "h264-resize.h264", "avc1"),
        (VideoCodec::H265, "h265-resize.h265", "hvc1"),
    ] {
        let aus = load(codec, name);
        assert_eq!(aus.len(), 80);
        let (file, s) = mux(codec, &aus, STEP, &[], Options::default());
        assert_eq!(s.video_entries, 2);
        let v = &tracks(&file)[0];
        assert_eq!(v.stsd_entries, [entry, entry]);
        let e: Vec<u32> = v.stsc.iter().map(|r| r.2).collect();
        assert!(e.contains(&1) && e.contains(&2));
        check_samples(&file, &aus, codec);
        // Chunks never mix descriptions: sample 41 starts a chunk of entry 2.
        let mut first_of_chunk = vec![];
        let mut n = 0;
        for (i, &off) in v.chunk_offsets.iter().enumerate() {
            let run = v.stsc.iter().rev().find(|r| r.0 <= i as u32 + 1).unwrap();
            let _ = off;
            first_of_chunk.push((n + 1, run.2));
            n += run.1;
        }
        assert!(first_of_chunk.contains(&(41, 2)), "{first_of_chunk:?}");
    }
}

#[test]
fn repeated_parameter_sets_do_not_add_descriptions() {
    // Every keyframe in the fixture repeats the same sets.
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let (_, s) = mux(VideoCodec::H264, &aus, STEP, &[], Options::default());
    assert_eq!(s.video_entries, 1);
}

#[test]
fn errors() {
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let new = || {
        Mp4Writer::new(
            Cursor::new(Vec::new()),
            VideoCodec::H264,
            Options::default(),
        )
        .unwrap()
    };
    // First sample without parameter sets (the file starts with a P-frame).
    let mut w = new();
    let p = &aus[1];
    assert!(matches!(
        w.write_video(VideoFrame::new(&p.data, false, 0)),
        Err(Error::MissingParameterSets)
    ));
    // Timestamps must increase.
    let mut w = new();
    w.write_video(VideoFrame::new(&aus[0].data, true, 100))
        .unwrap();
    assert!(matches!(
        w.write_video(VideoFrame::new(&aus[1].data, false, 100)),
        Err(Error::NonMonotonicTimestamp)
    ));
    assert!(matches!(
        w.write_video(VideoFrame::new(&aus[1].data, false, 50)),
        Err(Error::NonMonotonicTimestamp)
    ));
    // A failed frame leaves the writer usable.
    w.write_video(VideoFrame::new(&aus[1].data, false, 50_100))
        .unwrap();
    // Bad ADTS.
    assert!(matches!(
        w.write_audio(&[1, 2, 3]),
        Err(Error::InvalidAdts(_))
    ));
    // No Annex-B.
    assert!(matches!(
        w.write_video(VideoFrame::new(&[1, 2, 3, 4], false, 90_000)),
        Err(Error::InvalidAnnexB)
    ));
    // Nothing written.
    assert!(matches!(new().finish(), Err(Error::NoVideo)));
}

#[test]
fn audio_config_change_is_rejected() {
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let mut w = Mp4Writer::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options::default(),
    )
    .unwrap();
    w.write_video(VideoFrame::new(&aus[0].data, true, 0))
        .unwrap();
    w.write_audio(&tone()[0]).unwrap();
    let stereo = adts_frames(&fixture("aac-stereo-48k.aac"));
    assert!(matches!(
        w.write_audio(&stereo[0]),
        Err(Error::AudioConfigChanged)
    ));
}

// ---- ffprobe / ffmpeg ------------------------------------------------------

fn ffmpeg_check(
    codec: VideoCodec,
    name: &str,
    expect_codec: &str,
    w: f64,
    h: f64,
    faststart: bool,
) {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not installed; skipping");
        return;
    }
    let aus = load(codec, name);
    let audio = tone();
    let opts = Options {
        faststart,
        ..Options::default()
    };
    let (file, _) = mux(codec, &aus, STEP, &audio, opts);
    let p = temp_file(&format!("{expect_codec}-{faststart}.mp4"), &file);
    let v = probe_stream(&p, "v");
    assert_eq!(v["codec_name"], expect_codec);
    assert_eq!((f(&v, "width"), f(&v, "height")), (w, h));
    assert_eq!(f(&v, "nb_read_packets"), aus.len() as f64);
    assert!((f(&v, "duration") - aus.len() as f64 * 0.05).abs() < 0.001);
    assert!((f(&v, "start_time") - 1.0).abs() < 0.001 || f(&v, "start_time").abs() < 0.001);
    let a = probe_stream(&p, "a");
    assert_eq!(a["codec_name"], "aac");
    assert_eq!((f(&a, "sample_rate"), f(&a, "channels")), (16000.0, 1.0));
    assert_eq!(f(&a, "nb_read_packets"), audio.len() as f64);
    // Audio and video start within one video frame of each other.
    assert!(
        (f(&v, "start_time") - f(&a, "start_time")).abs() <= 0.05,
        "{v:?} {a:?}"
    );
    // Keyframes where the source had them.
    let pk = probe_packets(&p, "v");
    let keys: Vec<usize> = pk
        .iter()
        .enumerate()
        .filter(|(_, k)| k.1)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(keys, [0, 20, 40]);
    for (i, (t, _)) in pk.iter().enumerate() {
        assert!((t - (pk[0].0 + i as f64 * 0.05)).abs() < 0.001);
    }
    let errs = decode_errors(&p);
    assert!(errs.is_empty(), "decode errors: {errs}");
}

#[test]
fn ffmpeg_accepts_h264() {
    ffmpeg_check(
        VideoCodec::H264,
        "h264-basic.h264",
        "h264",
        320.0,
        240.0,
        false,
    );
    ffmpeg_check(
        VideoCodec::H264,
        "h264-basic.h264",
        "h264",
        320.0,
        240.0,
        true,
    );
}

#[test]
fn ffmpeg_accepts_h265() {
    ffmpeg_check(
        VideoCodec::H265,
        "h265-basic.h265",
        "hevc",
        320.0,
        240.0,
        false,
    );
    ffmpeg_check(
        VideoCodec::H265,
        "h265-basic.h265",
        "hevc",
        320.0,
        240.0,
        true,
    );
}

#[test]
fn ffmpeg_decodes_resolution_change_and_reports_both_sizes() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not installed; skipping");
        return;
    }
    for (codec, name) in [
        (VideoCodec::H264, "h264-resize.h264"),
        (VideoCodec::H265, "h265-resize.h265"),
    ] {
        let aus = load(codec, name);
        let (file, _) = mux(codec, &aus, STEP, &[], Options::default());
        let p = temp_file(&format!("resize-{codec:?}.mp4"), &file);
        let v = probe_stream(&p, "v");
        assert_eq!(f(&v, "nb_read_packets"), 80.0);
        let errs = decode_errors(&p);
        assert!(errs.is_empty(), "decode errors: {errs}");
        // Frame sizes as decoded: 40 at 320x240 then 40 at 480x270.
        let o = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "frame=width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&p)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&o.stdout);
        let sizes: Vec<&str> = text.lines().map(|l| l.trim_end_matches(',')).collect();
        assert_eq!(sizes.len(), 80);
        assert_eq!(sizes[0], "320,240");
        assert_eq!(sizes[79], "480,270");
    }
}

#[test]
fn ffmpeg_audio_start_offsets() {
    if !have_ffmpeg() {
        return;
    }
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    for (audio_start, want_diff) in [(1_250_000i64, 0.25f64), (750_000, -0.25)] {
        let opts = Options {
            audio_start_us: Some(audio_start),
            ..Options::default()
        };
        let (file, _) = mux(VideoCodec::H264, &aus, STEP, &tone(), opts);
        let p = temp_file(&format!("off{audio_start}.mp4"), &file);
        let v = probe_stream(&p, "v");
        let a = probe_stream(&p, "a");
        let d = f(&a, "start_time") - f(&v, "start_time");
        assert!(
            (d - want_diff).abs() < 0.001,
            "audio-video start difference {d}"
        );
        assert!(decode_errors(&p).is_empty());
    }
}
