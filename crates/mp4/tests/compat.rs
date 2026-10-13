//! Byte compatibility of the single-track API.
//!
//! Each case below feeds the synthetic fixtures through an existing entry
//! point the way current callers do (recordings downloads use
//! `FragmentedFile` with `FileOptions::default().with_audio(..)` and then
//! `finalize_to_mp4`), and compares the length and a 64-bit FNV-1a digest of
//! the output with values recorded from the writer before multi-track audio
//! was added. Any change to the bytes these callers get fails here.

use crate::common::*;
use mp4_remux::{
    AudioLayout, ChangePolicy, FileOptions, FragmentOptions, FragmentedFile, Fragmenter, Mp4Writer,
    Options, Segment, VideoCodec, VideoFrame, finalize_to_mp4, parse_adts,
};
use std::io::Cursor;

const STEP: i64 = 50_000;
const T0: i64 = 1_000_000;
const AUDIO_FRAME_US: i64 = 64_000;

fn fnv(d: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in d {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn tone() -> Vec<Vec<u8>> {
    adts_frames(&fixture("aac-tone.aac"))
}

fn basic(codec: VideoCodec) -> Vec<Au> {
    let name = match codec {
        VideoCodec::H264 => "h264-basic.h264",
        VideoCodec::H265 => "h265-basic.h265",
    };
    split_aus(&fixture(name), codec)
}

/// Segment bytes plus every metadata field, so routing and timing are pinned
/// too.
fn segments_digest(segs: &[Segment]) -> (usize, u64) {
    let mut all = Vec::new();
    for s in segs {
        all.extend_from_slice(&s.data);
        all.extend_from_slice(
            format!(
                "{:?}|{:?}|{}|{}|{}|{}|{:?}|{:?}|{:?}",
                s.kind,
                s.track,
                s.generation,
                s.start_us,
                s.duration_us,
                s.starts_with_keyframe,
                s.video_tfdt,
                s.audio_tfdt,
                s.init
            )
            .as_bytes(),
        );
    }
    (all.len(), fnv(&all))
}

fn fragment(codec: VideoCodec, aus: &[Au], audio: bool, o: FragmentOptions) -> Vec<Segment> {
    let frames = tone();
    let mut f = Fragmenter::new(codec, o);
    let mut out = Vec::new();
    let mut ai = 0;
    for (i, au) in aus.iter().enumerate() {
        let t = i as i64 * STEP;
        out.extend(
            f.push_video(VideoFrame::new(&au.data, au.key, T0 + t))
                .unwrap(),
        );
        while audio && ai < frames.len() && ai as i64 * AUDIO_FRAME_US <= t {
            f.push_audio_at(&frames[ai], T0 + ai as i64 * AUDIO_FRAME_US)
                .unwrap();
            ai += 1;
        }
    }
    while audio && ai < frames.len() {
        // Untimed: follows the previous frame.
        f.push_audio(&frames[ai]).unwrap();
        ai += 1;
    }
    out.extend(f.finish().unwrap());
    out
}

fn file(codec: VideoCodec, audio: bool, mfra: bool) -> Vec<u8> {
    let frames = tone();
    let o = FileOptions {
        mfra,
        ..FileOptions::default()
    }
    .with_audio(audio.then(|| parse_adts(&frames[0]).unwrap().0));
    let mut w = FragmentedFile::new(Vec::new(), codec, o);
    let mut ai = 0;
    for (i, au) in basic(codec).iter().enumerate() {
        let t = i as i64 * STEP;
        w.push_video(VideoFrame::new(&au.data, au.key, T0 + t))
            .unwrap();
        // Audio slightly late, with one frame missing, as a camera might.
        while audio && ai < frames.len() && ai as i64 * AUDIO_FRAME_US <= t {
            if ai != 7 {
                w.push_audio_at(&frames[ai], T0 + 30_000 + ai as i64 * AUDIO_FRAME_US)
                    .unwrap();
            }
            ai += 1;
        }
    }
    w.finish().unwrap().0
}

fn finalize(src: &[u8], faststart: bool) -> Vec<u8> {
    finalize_to_mp4(
        Cursor::new(src),
        Cursor::new(Vec::new()),
        Options {
            faststart,
            ..Options::default()
        },
    )
    .unwrap()
    .0
    .into_inner()
}

fn digest(d: &[u8]) -> (usize, u64) {
    (d.len(), fnv(d))
}

fn cases() -> Vec<(String, (usize, u64))> {
    let mut v = Vec::new();
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        let aus = basic(codec);
        let cfg = parse_adts(&tone()[0]).unwrap().0;
        for (name, audio, layout) in [
            ("none", false, AudioLayout::Muxed),
            ("muxed", true, AudioLayout::Muxed),
            ("separate", true, AudioLayout::Separate),
        ] {
            let o = FragmentOptions {
                audio: audio.then_some(cfg),
                audio_layout: layout,
                creation_unix_secs: 1_700_000_000,
                ..FragmentOptions::default()
            };
            v.push((
                format!("fragment {codec:?} {name}"),
                segments_digest(&fragment(codec, &aus, audio, o)),
            ));
        }
        for (audio, mfra) in [(false, false), (true, true), (true, false)] {
            let f = file(codec, audio, mfra);
            v.push((
                format!("file {codec:?} audio={audio} mfra={mfra}"),
                digest(&f),
            ));
            for faststart in [false, true] {
                v.push((
                    format!("finalize {codec:?} audio={audio} mfra={mfra} faststart={faststart}"),
                    digest(&finalize(&f, faststart)),
                ));
            }
        }
        for faststart in [false, true] {
            let o = Options {
                faststart,
                creation_unix_secs: 1_700_000_000,
                ..Options::default()
            };
            v.push((
                format!("writer {codec:?} faststart={faststart}"),
                digest(&mux(codec, &aus, STEP, &tone(), o).0),
            ));
        }
    }
    // A parameter-set change with new inits (live delivery).
    let aus = split_aus(&fixture("h264-resize.h264"), VideoCodec::H264);
    let o = FragmentOptions {
        audio: Some(parse_adts(&tone()[0]).unwrap().0),
        on_change: ChangePolicy::NewInit,
        ..FragmentOptions::default()
    };
    v.push((
        "fragment resize".into(),
        segments_digest(&fragment(VideoCodec::H264, &aus, true, o)),
    ));
    // The writer with timed audio: late start, a gap filled with silence,
    // and a duplicate.
    let frames = tone();
    let mut w = Mp4Writer::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options::default(),
    )
    .unwrap();
    for (i, au) in basic(VideoCodec::H264).iter().enumerate() {
        w.write_video(VideoFrame::new(&au.data, au.key, T0 + i as i64 * STEP))
            .unwrap();
    }
    let mut t = T0 + 120_000;
    for (i, fr) in frames.iter().enumerate() {
        if i == 5 {
            t += 3 * AUDIO_FRAME_US;
        }
        w.write_audio_at(fr, t).unwrap();
        if i == 9 {
            assert!(!w.write_audio_at(fr, t).unwrap());
        }
        t += AUDIO_FRAME_US;
    }
    v.push((
        "writer timed audio".into(),
        digest(&w.finish().unwrap().0.into_inner()),
    ));
    v
}

/// Recorded from the single-track writer (commit before multi-track audio).
const GOLDEN: &[(&str, usize, u64)] = &[
    ("fragment H264 none", 48310, 0x157444ca88641675),
    ("fragment H264 muxed", 57754, 0xcc88770bab6efb16),
    ("fragment H264 separate", 58418, 0x865bf0588de189b6),
    (
        "file H264 audio=false mfra=false",
        47565,
        0xaa977f50768644d3,
    ),
    (
        "finalize H264 audio=false mfra=false faststart=false",
        46841,
        0x3866d852f7d80ce2,
    ),
    (
        "finalize H264 audio=false mfra=false faststart=true",
        46841,
        0x59559939b4855aa2,
    ),
    ("file H264 audio=true mfra=true", 56772, 0x0661592c96984a1f),
    (
        "finalize H264 audio=true mfra=true faststart=false",
        55610,
        0xd1c7d911ac378b48,
    ),
    (
        "finalize H264 audio=true mfra=true faststart=true",
        55610,
        0x8c60964778810d76,
    ),
    ("file H264 audio=true mfra=false", 56586, 0x5ecf496f6d32a859),
    (
        "finalize H264 audio=true mfra=false faststart=false",
        55610,
        0xd1c7d911ac378b48,
    ),
    (
        "finalize H264 audio=true mfra=false faststart=true",
        55610,
        0x8c60964778810d76,
    ),
    ("writer H264 faststart=false", 55805, 0xe8e4a4ca0819aff4),
    ("writer H264 faststart=true", 55805, 0x9596c59c240544f1),
    ("fragment H265 none", 67516, 0xdfd26b9daa34f06c),
    ("fragment H265 muxed", 76960, 0x6eb636fe81cb765d),
    ("fragment H265 separate", 77624, 0x2239f168d9439f0d),
    (
        "file H265 audio=false mfra=false",
        66767,
        0xa1f8a30ecba3b92a,
    ),
    (
        "finalize H265 audio=false mfra=false faststart=false",
        66043,
        0xcd0b9df315aedfbc,
    ),
    (
        "finalize H265 audio=false mfra=false faststart=true",
        66043,
        0x45510faa927373d4,
    ),
    ("file H265 audio=true mfra=true", 75974, 0x7e72e89ce3c57082),
    (
        "finalize H265 audio=true mfra=true faststart=false",
        74812,
        0xc741a6de504d2868,
    ),
    (
        "finalize H265 audio=true mfra=true faststart=true",
        74812,
        0x56bf52698629c3a9,
    ),
    ("file H265 audio=true mfra=false", 75788, 0x8b570809adb842ba),
    (
        "finalize H265 audio=true mfra=false faststart=false",
        74812,
        0xc741a6de504d2868,
    ),
    (
        "finalize H265 audio=true mfra=false faststart=true",
        74812,
        0x56bf52698629c3a9,
    ),
    ("writer H265 faststart=false", 75007, 0xd1dbb7d4c8cde208),
    ("writer H265 faststart=true", 75007, 0x55c2aafe2ef67858),
    ("fragment resize", 83604, 0xd0d461d08a735884),
    ("writer timed audio", 55877, 0xfc2f36fd10d2e8db),
];

#[test]
fn single_track_output_is_unchanged() {
    let got = cases();
    if GOLDEN.is_empty() {
        for (n, (len, h)) in &got {
            println!("    ({n:?}, {len}, 0x{h:016x}),");
        }
        panic!("no golden values recorded");
    }
    assert_eq!(got.len(), GOLDEN.len());
    for ((name, (len, h)), (gname, glen, gh)) in got.iter().zip(GOLDEN) {
        assert_eq!(name, gname);
        assert_eq!((*len, *h), (*glen, *gh), "{name}: output changed");
    }
}
