//! Fragmented MP4: structural checks (always on) and ffprobe/ffmpeg checks
//! (skipped when ffmpeg is not installed).

mod common;

use common::*;
use mp4_remux::{
    AudioLayout, Error, FileOptions, FragmentOptions, FragmentedFile, Fragmenter, Options, Segment,
    SegmentKind, SegmentTrack, VideoCodec, VideoFrame, finalize_to_mp4, parse_adts,
};
use std::io::Cursor;

const STEP: i64 = 50_000; // 20 fps, as the fixtures
const T0: i64 = 1_000_000;
/// One AAC frame at 16 kHz.
const AUDIO_FRAME_US: i64 = 64_000;

fn load(codec: VideoCodec, name: &str) -> Vec<Au> {
    split_aus(&fixture(name), codec)
}

fn tone() -> Vec<Vec<u8>> {
    adts_frames(&fixture("aac-tone.aac"))
}

fn fixture_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264-basic.h264",
        VideoCodec::H265 => "h265-basic.h265",
    }
}

fn opts(audio: bool) -> FragmentOptions {
    FragmentOptions {
        audio: audio.then(|| parse_adts(&tone()[0]).unwrap().0),
        ..FragmentOptions::default()
    }
}

/// Feeds video and (timed) audio the way a device would, returning every
/// segment in order.
fn run(codec: VideoCodec, aus: &[Au], audio: bool, o: FragmentOptions) -> Vec<Segment> {
    let mut f = Fragmenter::new(codec, o);
    let frames = tone();
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
        f.push_audio_at(&frames[ai], T0 + ai as i64 * AUDIO_FRAME_US)
            .unwrap();
        ai += 1;
    }
    out.extend(f.finish().unwrap());
    out
}

fn concat(segs: &[Segment]) -> Vec<u8> {
    segs.iter().flat_map(|s| s.data.iter().copied()).collect()
}

// ---- a tiny fragment parser ---------------------------------------------------

#[derive(Debug)]
struct Traf {
    track: u32,
    tfdt: u64,
    /// (duration, size, flags, composition offset)
    samples: Vec<(u32, u32, u32, i32)>,
    /// Sample bytes from the mdat.
    data: Vec<Vec<u8>>,
}

#[derive(Debug)]
struct Frag {
    seq: u32,
    trafs: Vec<Traf>,
}

fn u32at(d: &[u8], i: usize) -> u32 {
    u32::from_be_bytes(d[i..i + 4].try_into().unwrap())
}

/// Parses a `moof` + `mdat` segment.
fn parse_frag(d: &[u8]) -> Frag {
    let b = top(d);
    assert_eq!(
        b.iter().map(|x| x.ty.as_str()).collect::<Vec<_>>(),
        ["moof", "mdat"]
    );
    let (moof, mdat) = (&b[0], &b[1]);
    let kids = boxes_in(d, moof.start + 8, moof.end);
    assert_eq!(kids[0].ty, "mfhd");
    let seq = u32at(d, kids[0].start + 12);
    let mut trafs = Vec::new();
    for k in kids.iter().filter(|k| k.ty == "traf") {
        let parts = boxes_in(d, k.start + 8, k.end);
        assert_eq!(
            parts.iter().map(|x| x.ty.as_str()).collect::<Vec<_>>(),
            ["tfhd", "tfdt", "trun"]
        );
        let tfhd = &parts[0];
        assert_eq!(u32at(d, tfhd.start + 8) & 0xff_ffff, 0x02_0000);
        let track = u32at(d, tfhd.start + 12);
        let tfdt = &parts[1];
        assert_eq!(d[tfdt.start + 8], 1, "64-bit tfdt");
        let t = u64::from_be_bytes(d[tfdt.start + 12..tfdt.start + 20].try_into().unwrap());
        let trun = &parts[2];
        let flags = u32at(d, trun.start + 8) & 0xff_ffff;
        assert_eq!(flags & 0x301, 0x301, "data offset, duration, size");
        let n = u32at(d, trun.start + 12) as usize;
        let off = u32at(d, trun.start + 16) as usize; // from moof start
        let mut i = trun.start + 20;
        let mut samples = Vec::new();
        for _ in 0..n {
            let dur = u32at(d, i);
            let size = u32at(d, i + 4);
            i += 8;
            let mut fl = 0;
            if flags & 0x400 != 0 {
                fl = u32at(d, i);
                i += 4;
            }
            let mut cto = 0;
            if flags & 0x800 != 0 {
                cto = u32at(d, i) as i32;
                i += 4;
            }
            samples.push((dur, size, fl, cto));
        }
        assert_eq!(i, trun.end);
        assert!(off >= moof.end + 8 - moof.start && off + moof.start <= mdat.end);
        let mut at = moof.start + off;
        let data = samples
            .iter()
            .map(|s| {
                let v = d[at..at + s.1 as usize].to_vec();
                at += s.1 as usize;
                v
            })
            .collect();
        trafs.push(Traf {
            track,
            tfdt: t,
            samples,
            data,
        });
    }
    Frag { seq, trafs }
}

fn media(segs: &[Segment]) -> Vec<&Segment> {
    segs.iter()
        .filter(|s| s.kind == SegmentKind::Media)
        .collect()
}

fn have_video_traf(f: &Frag) -> &Traf {
    f.trafs.iter().find(|t| t.track == 1).expect("video traf")
}

const SYNC: u32 = 0x0200_0000;
const NON_SYNC: u32 = 0x0101_0000;

// ---- structure ------------------------------------------------------------------

fn check_structure(codec: VideoCodec, audio: bool) {
    let aus = load(codec, fixture_name(codec));
    let segs = run(codec, &aus, audio, opts(audio));
    assert_eq!(segs[0].kind, SegmentKind::Init);
    assert_eq!(
        segs.iter().filter(|s| s.kind == SegmentKind::Init).count(),
        1
    );
    // Init: ftyp, moov(mvhd, trak[, trak], mvex(trex...)), nothing else.
    let init = &segs[0].data;
    let t = top(init);
    assert_eq!(
        t.iter().map(|b| b.ty.as_str()).collect::<Vec<_>>(),
        ["ftyp", "moov"]
    );
    let moov = &t[1];
    let kids = boxes_in(init, moov.start + 8, moov.end);
    let names: Vec<&str> = kids.iter().map(|b| b.ty.as_str()).collect();
    let mut want = vec!["mvhd", "trak"];
    if audio {
        want.push("trak");
    }
    want.push("mvex");
    assert_eq!(names, want);
    let tr = tracks(init);
    assert_eq!(tr.len(), if audio { 2 } else { 1 });
    assert_eq!(tr[0].handler, "vide");
    assert_eq!(tr[0].timescale, 90_000);
    assert_eq!(
        tr[0].stsd_entries,
        [if codec == VideoCodec::H264 {
            "avc1"
        } else {
            "hvc1"
        }]
    );
    assert!(tr[0].stsz.is_empty() && tr[0].chunk_offsets.is_empty());
    if audio {
        assert_eq!(tr[1].handler, "soun");
        assert_eq!(tr[1].timescale, 16_000);
        assert_eq!(tr[1].stsd_entries, ["mp4a"]);
    }
    let mvex = kids.last().unwrap();
    let trex: Vec<_> = boxes_in(init, mvex.start + 8, mvex.end);
    assert_eq!(trex.len(), tr.len());
    assert!(trex.iter().all(|b| b.ty == "trex"));
    // Parameter sets are in the sample entry, never in the samples.
    let entry = &tr[0].stsd.as_ref().unwrap();
    let body = &init[entry.start..entry.end];
    let cfg = if codec == VideoCodec::H264 {
        b"avcC"
    } else {
        b"hvcC"
    };
    assert!(body.windows(4).any(|w| w == cfg));

    // Media segments.
    let ms = media(&segs);
    assert!(ms.len() >= 6, "{} fragments", ms.len());
    let frags: Vec<Frag> = ms.iter().map(|s| parse_frag(&s.data)).collect();
    let mut next_v = 0u64;
    let mut next_a: Option<u64> = None;
    let mut vi = 0;
    let mut ai = 0;
    let tonef = tone();
    for (k, (f, s)) in frags.iter().zip(&ms).enumerate() {
        assert_eq!(f.seq, k as u32 + 1);
        assert_eq!(s.track, SegmentTrack::Muxed);
        let v = have_video_traf(f);
        assert_eq!(f.trafs[0].track, 1, "video traf first");
        // tfdt continuity: each fragment starts where the last ended.
        assert_eq!(v.tfdt, next_v, "video tfdt at fragment {k}");
        assert_eq!(s.video_tfdt, Some(v.tfdt));
        next_v += v.samples.iter().map(|x| u64::from(x.0)).sum::<u64>();
        // Per-sample flags and bytes match the input keyframes and data.
        for (i, ((_, size, flags, _), bytes)) in v.samples.iter().zip(&v.data).enumerate() {
            let au = &aus[vi];
            assert_eq!(*flags, if au.key { SYNC } else { NON_SYNC }, "sample {vi}");
            assert_eq!(bytes, &expected_sample(&au.data, codec));
            assert_eq!(*size as usize, bytes.len());
            if i == 0 {
                assert_eq!(s.starts_with_keyframe, au.key);
            }
            // 20 fps in 90 kHz ticks.
            assert_eq!(v.samples[i].0, 4500);
            vi += 1;
        }
        if let Some(a) = f.trafs.get(1) {
            assert!(audio);
            assert_eq!(a.track, 2);
            if let Some(n) = next_a {
                assert_eq!(a.tfdt, n, "audio tfdt at fragment {k}");
            } else {
                // Audio starts with the video (both at T0).
                assert_eq!(a.tfdt, 0);
            }
            next_a = Some(a.tfdt + a.samples.iter().map(|x| u64::from(x.0)).sum::<u64>());
            for ((dur, _, flags, _), bytes) in a.samples.iter().zip(&a.data) {
                assert_eq!(*dur, 1024);
                assert_eq!(*flags, 0, "audio uses the trex default (sync)");
                assert_eq!(bytes.as_slice(), &tonef[ai][7..]);
                ai += 1;
            }
        }
    }
    assert_eq!(vi, aus.len());
    if audio {
        assert_eq!(ai, tonef.len(), "all audio delivered");
    } else {
        assert!(frags.iter().all(|f| f.trafs.len() == 1));
    }
    // Cut rule: keyframes always start a fragment; at most 10 frames (500 ms).
    let key_idx: Vec<usize> = aus
        .iter()
        .enumerate()
        .filter(|(_, a)| a.key)
        .map(|(i, _)| i)
        .collect();
    let mut starts = Vec::new();
    let mut at = 0;
    for f in &frags {
        starts.push(at);
        at += have_video_traf(f).samples.len();
    }
    for k in key_idx {
        assert!(starts.contains(&k), "keyframe {k} starts a fragment");
    }
    assert!(frags.iter().all(|f| have_video_traf(f).samples.len() <= 10));
}

#[test]
fn structure_h264() {
    check_structure(VideoCodec::H264, false);
    check_structure(VideoCodec::H264, true);
}

#[test]
fn structure_h265() {
    check_structure(VideoCodec::H265, false);
    check_structure(VideoCodec::H265, true);
}

#[test]
fn one_gop_per_fragment_with_long_limit() {
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let o = FragmentOptions {
        max_fragment_us: 60_000_000,
        ..opts(false)
    };
    let segs = run(VideoCodec::H264, &aus, false, o);
    let ms = media(&segs);
    assert_eq!(ms.len(), 3);
    for s in &ms {
        assert!(s.starts_with_keyframe);
        assert_eq!(parse_frag(&s.data).trafs[0].samples.len(), 20);
        assert_eq!(s.duration_us, 1_000_000);
    }
    assert_eq!(ms[1].start_us, 1_000_000);
}

#[test]
fn irregular_durations_and_late_join_info() {
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let mut f = Fragmenter::new(VideoCodec::H264, opts(false));
    assert!(f.current_init().is_empty());
    let mut t = 5_000_000i64;
    let mut deltas = Vec::new();
    let mut segs = Vec::new();
    for (i, au) in aus.iter().enumerate() {
        segs.extend(f.push_video(VideoFrame::new(&au.data, au.key, t)).unwrap());
        if i == 0 {
            assert_eq!(f.current_init().len(), 1);
            assert_eq!(f.current_init()[0].data, segs[0].data);
        }
        let d = 33_333 + (i as i64 * 7919) % 60_000;
        deltas.push(d);
        t += d;
    }
    segs.extend(f.finish().unwrap());
    let mut dts = Vec::new();
    for s in media(&segs) {
        let fr = parse_frag(&s.data);
        let v = have_video_traf(&fr);
        let mut at = v.tfdt;
        for x in &v.samples {
            dts.push(at);
            at += u64::from(x.0);
        }
    }
    assert_eq!(dts.len(), 60);
    for i in 1..dts.len() {
        let want = mp4_remux::us_to_ticks(5_000_000 + deltas[..i].iter().sum::<i64>())
            - mp4_remux::us_to_ticks(5_000_000);
        assert_eq!(dts[i] as i64, want, "sample {i}");
    }
}

#[test]
fn leading_non_keyframes_are_skipped() {
    let aus = load(VideoCodec::H264, "h264-leading-p.h264");
    let mut f = Fragmenter::new(VideoCodec::H264, opts(false));
    let mut segs = Vec::new();
    for (i, au) in aus.iter().enumerate() {
        segs.extend(
            f.push_video(VideoFrame::new(&au.data, au.key, T0 + i as i64 * STEP))
                .unwrap(),
        );
    }
    assert!(f.stats().leading_skipped > 0);
    assert_eq!(segs[0].kind, SegmentKind::Init);
    assert!(media(&segs).first().is_none_or(|s| s.starts_with_keyframe));
    let tail = f.finish().unwrap();
    assert!(tail.iter().all(|s| s.starts_with_keyframe) || !media(&segs).is_empty());
}

// ---- codec strings, parameter-set changes -----------------------------------------

#[test]
fn mime_codec_strings() {
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let segs = run(VideoCodec::H264, &aus, true, opts(true));
    let m = &segs[0].init.as_ref().unwrap().mime;
    assert!(m.starts_with("video/mp4; codecs=\"avc1."), "{m}");
    assert!(m.ends_with(",mp4a.40.2\""), "{m}");
    let hex = &m["video/mp4; codecs=\"avc1.".len()..][..6];
    assert!(
        hex.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_lowercase())
    );
    let (w, h) = (
        segs[0].init.as_ref().unwrap().width,
        segs[0].init.as_ref().unwrap().height,
    );
    assert_eq!((w, h), (320, 240));

    let aus = load(VideoCodec::H265, "h265-basic.h265");
    let segs = run(VideoCodec::H265, &aus, false, opts(false));
    let m = &segs[0].init.as_ref().unwrap().mime;
    assert!(m.starts_with("video/mp4; codecs=\"hvc1.1.6.L"), "{m}");
    if have_ffmpeg() {
        // The level in the string is the level ffprobe reports.
        let p = temp_file("hevc-mime.mp4", &concat(&segs));
        let o = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=level",
            ])
            .args(["-of", "default=nw=1:nk=1"])
            .arg(&p)
            .output()
            .unwrap();
        let level = String::from_utf8_lossy(&o.stdout).trim().to_string();
        assert!(m.contains(&format!(".L{level}")), "{m} vs level {level}");
    }
}

#[test]
fn h264_codec_string_matches_ffprobe() {
    if !have_ffmpeg() {
        return;
    }
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let segs = run(VideoCodec::H264, &aus, false, opts(false));
    let m = &segs[0].init.as_ref().unwrap().mime;
    let hex = &m["video/mp4; codecs=\"avc1.".len()..][..6];
    let level = u32::from_str_radix(&hex[4..6], 16).unwrap();
    let p = temp_file("avc-mime.mp4", &concat(&segs));
    let o = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=level",
        ])
        .args(["-of", "default=nw=1:nk=1"])
        .arg(&p)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), level.to_string());
}

fn resize_run(codec: VideoCodec, name: &str) -> Vec<Segment> {
    let aus = load(codec, name);
    run(codec, &aus, false, opts(false))
}

#[test]
fn parameter_set_change_makes_a_new_init() {
    for (codec, name) in [
        (VideoCodec::H264, "h264-resize.h264"),
        (VideoCodec::H265, "h265-resize.h265"),
    ] {
        let segs = resize_run(codec, name);
        let inits: Vec<usize> = segs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == SegmentKind::Init)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(inits.len(), 2, "{name}");
        let (a, b) = (&segs[inits[0]], &segs[inits[1]]);
        let (ia, ib) = (a.init.as_ref().unwrap(), b.init.as_ref().unwrap());
        assert!(!ia.reset && ib.reset);
        assert_eq!((ia.width, ia.height), (320, 240));
        assert_eq!((ib.width, ib.height), (480, 270));
        assert_eq!((a.generation, b.generation), (0, 1));
        assert_ne!(ia.mime, ib.mime);
        // Media before the second init belongs to generation 0, after to 1,
        // and the new generation starts with a keyframe.
        for s in &segs[..inits[1]] {
            assert_eq!(s.generation, 0);
        }
        let after = media(&segs[inits[1]..]);
        assert!(after.iter().all(|s| s.generation == 1));
        assert!(after[0].starts_with_keyframe);
        // Timeline continues across the change (40 frames at 20 fps).
        assert_eq!(after[0].start_us, 2_000_000);
        // Every sample of generation 0 is 320x240 data: the old fragments are
        // complete (their total is the first 40 frames).
        let n0: usize = media(&segs[..inits[1]])
            .iter()
            .map(|s| have_video_traf(&parse_frag(&s.data)).samples.len())
            .sum();
        assert_eq!(n0, 40);
        assert_eq!(tracks(&b.data)[0].stsd_entries.len(), 1);
    }
}

/// Box-level check of a generation switch against what Media Source
/// Extensions needs from a stream (an init segment may arrive between media
/// segments; the next media segment must start with a sync sample and decode
/// times must continue): same track ID, timescale and handler in both inits,
/// one sample entry whose configuration record carries the new parameter
/// sets, sequence numbers that keep increasing, and a `tfdt` that starts
/// exactly where the previous fragment ended.
#[test]
fn generation_switch_is_valid_for_media_source() {
    for (codec, name) in [
        (VideoCodec::H264, "h264-resize.h264"),
        (VideoCodec::H265, "h265-resize.h265"),
    ] {
        let aus = load(codec, name);
        let segs = run(codec, &aus, false, opts(false));
        let at = segs
            .iter()
            .position(|s| s.init.as_ref().is_some_and(|i| i.reset))
            .expect("second init");
        let (old_init, new_init) = (&segs[0].data, &segs[at].data);
        let (ta, tb) = (&tracks(old_init)[0], &tracks(new_init)[0]);
        assert_eq!((ta.timescale, &ta.handler), (tb.timescale, &tb.handler));
        assert_eq!(
            ta.stsd_entries, tb.stsd_entries,
            "{name}: same sample entry type"
        );
        assert_eq!(tb.stsd_entries.len(), 1);
        for init in [old_init, new_init] {
            let moov = top_box(init, "moov");
            let trak = child(init, &moov, "trak").unwrap();
            let tkhd = boxes_in(init, trak.start + 8, trak.end)
                .into_iter()
                .find(|b| b.ty == "tkhd")
                .unwrap();
            assert_eq!(u32at(init, tkhd.start + 20), 1, "track ID");
            let mvex = child(init, &moov, "mvex").unwrap();
            let trex = child(init, &mvex, "trex").unwrap();
            assert_eq!(u32at(init, trex.start + 12), 1, "trex track ID");
            assert_eq!(u32at(init, trex.start + 16), 1, "sample description index");
        }
        // The new init's configuration record holds the parameter sets of
        // the first frame of the second encode (frame 40), not the old ones.
        let key = &aus[40].data;
        assert!(aus[40].key);
        let is_set = |t: u8| match codec {
            VideoCodec::H264 => matches!(t & 0x1f, 7 | 8),
            VideoCodec::H265 => matches!((t >> 1) & 0x3f, 32..=34),
        };
        let sets: Vec<&[u8]> = nals(key)
            .into_iter()
            .map(|(s, e)| &key[s..e])
            .filter(|n| is_set(n[0]))
            .collect();
        assert!(!sets.is_empty(), "{name}: keyframe carries parameter sets");
        let stsd = tb.stsd.as_ref().unwrap();
        let entry = &new_init[stsd.start..stsd.end];
        for n in &sets {
            assert!(
                entry.windows(n.len()).any(|w| w == *n),
                "{name}: new parameter set in the sample entry"
            );
        }
        assert!(
            sets.iter()
                .any(|n| !old_init.windows(n.len()).any(|w| w == *n)),
            "{name}: the old init has different parameter sets"
        );
        // Media segments on both sides of the switch.
        let before = parse_frag(&segs[at - 1].data);
        let after = parse_frag(&segs[at + 1].data);
        assert!(after.seq > before.seq, "{name}: sequence numbers increase");
        let (vb, va) = (have_video_traf(&before), have_video_traf(&after));
        let end: u64 = vb.tfdt + vb.samples.iter().map(|s| u64::from(s.0)).sum::<u64>();
        assert_eq!(va.tfdt, end, "{name}: decode times continue");
        assert_eq!(va.tfdt, 2 * 90_000, "{name}: 40 frames at 20 fps");
        assert_eq!(va.samples[0].2, SYNC, "{name}: starts with a sync sample");
        assert!(va.samples[1..].iter().all(|s| s.2 == NON_SYNC));
        // The first slice of the new generation is an IDR / IRAP picture
        // (SEI may come before it, as in the first generation).
        let first = &va.data[0];
        let mut i = 0;
        let slice = loop {
            let len = u32at(first, i) as usize;
            assert!(i + 4 + len <= first.len(), "{name}: NAL lengths");
            let t = first[i + 4];
            let vcl = match codec {
                VideoCodec::H264 => (1..=5).contains(&(t & 0x1f)),
                VideoCodec::H265 => (t >> 1) & 0x3f < 32,
            };
            if vcl {
                break t;
            }
            i += 4 + len;
        };
        let irap = match codec {
            VideoCodec::H264 => slice & 0x1f == 5,
            VideoCodec::H265 => (16..=21).contains(&((slice >> 1) & 0x3f)),
        };
        assert!(
            irap,
            "{name}: the new generation starts with a keyframe slice"
        );
    }
}

#[test]
fn reject_policy_returns_an_error_and_consumes_nothing() {
    let aus = load(VideoCodec::H264, "h264-resize.h264");
    let mut f = Fragmenter::new(
        VideoCodec::H264,
        FragmentOptions {
            on_change: mp4_remux::ChangePolicy::Reject,
            ..opts(false)
        },
    );
    let mut failed = None;
    for (i, au) in aus.iter().enumerate() {
        match f.push_video(VideoFrame::new(&au.data, au.key, T0 + i as i64 * STEP)) {
            Ok(_) => {}
            Err(Error::ParameterSetsChanged) => {
                failed = Some(i);
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(failed, Some(40));
    assert_eq!(f.stats().video_samples, 40);
}

// ---- audio ------------------------------------------------------------------------

#[test]
fn separate_layout_routes_tracks() {
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let o = FragmentOptions {
        audio_layout: AudioLayout::Separate,
        ..opts(true)
    };
    let segs = run(VideoCodec::H264, &aus, true, o);
    let inits: Vec<_> = segs
        .iter()
        .filter(|s| s.kind == SegmentKind::Init)
        .collect();
    assert_eq!(inits.len(), 2);
    assert_eq!(inits[0].track, SegmentTrack::Video);
    assert_eq!(inits[1].track, SegmentTrack::Audio);
    assert!(
        inits[0]
            .init
            .as_ref()
            .unwrap()
            .mime
            .starts_with("video/mp4")
    );
    assert!(!inits[0].init.as_ref().unwrap().mime.contains("mp4a"));
    assert_eq!(
        inits[1].init.as_ref().unwrap().mime,
        "audio/mp4; codecs=\"mp4a.40.2\""
    );
    let mut atotal = 0;
    let mut next = 0u64;
    for s in media(&segs) {
        let f = parse_frag(&s.data);
        assert_eq!(f.trafs.len(), 1);
        match s.track {
            SegmentTrack::Video => assert_eq!(f.trafs[0].track, 1),
            SegmentTrack::Audio => {
                assert_eq!(f.trafs[0].track, 2);
                assert_eq!(f.trafs[0].tfdt, next);
                next += f.trafs[0]
                    .samples
                    .iter()
                    .map(|x| u64::from(x.0))
                    .sum::<u64>();
                atotal += f.trafs[0].samples.len();
            }
            SegmentTrack::Muxed => panic!("no muxed segments"),
        }
    }
    assert_eq!(atotal, tone().len());
    if have_ffmpeg() {
        for (kind, n) in [
            (SegmentTrack::Video, 60usize),
            (SegmentTrack::Audio, tone().len()),
        ] {
            let parts: Vec<Segment> = segs.iter().filter(|s| s.track == kind).cloned().collect();
            let p = temp_file(&format!("sep-{kind:?}.mp4"), &concat(&parts));
            assert!(
                decode_errors(&p).is_empty(),
                "{kind:?}: {}",
                decode_errors(&p)
            );
            let k = if kind == SegmentTrack::Video {
                "v"
            } else {
                "a"
            };
            assert_eq!(probe_stream(&p, k)["nb_read_packets"], n.to_string());
        }
    }
}

#[test]
fn audio_gap_extends_and_duplicate_is_dropped() {
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let frames = tone();
    let cfg = parse_adts(&frames[0]).unwrap().0;
    let mut f = Fragmenter::new(
        VideoCodec::H264,
        FragmentOptions {
            max_fragment_us: 60_000_000,
            audio: Some(cfg),
            ..FragmentOptions::default()
        },
    );
    let mut segs = Vec::new();
    for (i, au) in aus.iter().take(20).enumerate() {
        segs.extend(
            f.push_video(VideoFrame::new(&au.data, au.key, T0 + i as i64 * STEP))
                .unwrap(),
        );
    }
    assert!(f.push_audio_at(&frames[0], T0).unwrap());
    assert!(f.push_audio_at(&frames[1], T0 + AUDIO_FRAME_US).unwrap());
    // Duplicate of frame 1's slot.
    assert!(!f.push_audio_at(&frames[2], T0 + AUDIO_FRAME_US).unwrap());
    // Two frames missing: the next frame lands three slots after frame 1.
    assert!(
        f.push_audio_at(&frames[3], T0 + 4 * AUDIO_FRAME_US)
            .unwrap()
    );
    segs.extend(
        f.push_video(VideoFrame::new(&aus[20].data, true, T0 + 20 * STEP))
            .unwrap(),
    );
    let fr = parse_frag(&media(&segs)[0].data);
    let a = &fr.trafs[1];
    let durs: Vec<u32> = a.samples.iter().map(|s| s.0).collect();
    assert_eq!(durs, [1024, 4 * 1024 - 1024, 1024]);
    assert_eq!(f.stats().audio_dropped, 1);
}

#[test]
fn audio_config_change_is_an_error() {
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let mut f = Fragmenter::new(VideoCodec::H264, opts(true));
    f.push_video(VideoFrame::new(&aus[0].data, true, T0))
        .unwrap();
    let st = adts_frames(&fixture("aac-stereo-48k.aac"));
    assert!(matches!(
        f.push_audio(&st[0]),
        Err(Error::AudioConfigChanged)
    ));
}

#[test]
fn bounded_fragments_without_keyframes() {
    // One keyframe, then ten seconds of P-frames: fragments are cut by time.
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let p = fake_p_frame(&[7; 300]);
    let mut f = Fragmenter::new(VideoCodec::H264, opts(false));
    let mut segs = f
        .push_video(VideoFrame::new(&aus[0].data, true, T0))
        .unwrap();
    for i in 1..200 {
        segs.extend(
            f.push_video(VideoFrame::new(&p, false, T0 + i * STEP))
                .unwrap(),
        );
    }
    segs.extend(f.finish().unwrap());
    let ms = media(&segs);
    assert!(ms.len() >= 19);
    for s in &ms {
        assert!(parse_frag(&s.data).trafs[0].samples.len() <= 10);
    }
    assert!(ms[0].starts_with_keyframe && ms[1..].iter().all(|s| !s.starts_with_keyframe));
    // Non-sync fragments say so in their sample flags.
    assert_eq!(parse_frag(&ms[1].data).trafs[0].samples[0].2, NON_SYNC);
    // A byte limit cuts earlier.
    let mut f = Fragmenter::new(
        VideoCodec::H264,
        FragmentOptions {
            max_fragment_bytes: 1000,
            max_fragment_us: 60_000_000,
            ..opts(false)
        },
    );
    let mut segs = f
        .push_video(VideoFrame::new(&aus[0].data, true, T0))
        .unwrap();
    for i in 1..50 {
        segs.extend(
            f.push_video(VideoFrame::new(&p, false, T0 + i * STEP))
                .unwrap(),
        );
    }
    assert!(media(&segs).len() >= 10);
}

// ---- ffprobe / ffmpeg -----------------------------------------------------------------

#[test]
fn concatenated_segments_decode_cleanly() {
    if !have_ffmpeg() {
        return;
    }
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        for audio in [false, true] {
            let aus = load(codec, fixture_name(codec));
            let segs = run(codec, &aus, audio, opts(audio));
            let p = temp_file(&format!("live-{codec:?}-{audio}.mp4"), &concat(&segs));
            let errs = decode_errors(&p);
            assert!(errs.is_empty(), "{codec:?} audio={audio}: {errs}");
            let v = probe_stream(&p, "v");
            assert_eq!(v["nb_read_packets"], "60");
            assert_eq!(f(&v, "width"), 320.0);
            let keys: Vec<bool> = probe_packets(&p, "v").iter().map(|x| x.1).collect();
            let want: Vec<bool> = aus.iter().map(|a| a.key).collect();
            assert_eq!(keys, want, "sync flags as ffprobe sees them");
            if audio {
                let a = probe_stream(&p, "a");
                assert_eq!(a["nb_read_packets"], tone().len().to_string());
                assert_eq!(a["codec_name"], "aac");
            }
        }
    }
}

#[test]
fn first_segment_of_each_gop_can_start_a_late_joiner() {
    if !have_ffmpeg() {
        return;
    }
    let aus = load(VideoCodec::H264, "h264-basic.h264");
    let segs = run(VideoCodec::H264, &aus, true, opts(true));
    // Join at the last keyframe fragment: init + that fragment onward.
    let ms = media(&segs);
    let k = ms.iter().rposition(|s| s.starts_with_keyframe).unwrap();
    let mut data = segs[0].data.clone();
    for s in &ms[k..] {
        data.extend_from_slice(&s.data);
    }
    let p = temp_file("late.mp4", &data);
    assert!(decode_errors(&p).is_empty(), "{}", decode_errors(&p));
    assert_eq!(probe_stream(&p, "v")["nb_read_packets"], "20");
}

#[test]
fn parameter_set_change_decodes_as_two_streams() {
    if !have_ffmpeg() {
        return;
    }
    for (codec, name, ext) in [
        (VideoCodec::H264, "h264-resize.h264", "264"),
        (VideoCodec::H265, "h265-resize.h265", "265"),
    ] {
        let segs = resize_run(codec, name);
        let i = segs
            .iter()
            .rposition(|s| s.kind == SegmentKind::Init)
            .unwrap();
        // Each generation (init + its segments) is a valid file by itself.
        let (first, second) = (&segs[..i], &segs[i..]);
        for (label, part, w) in [("a", first, 320.0), ("b", second, 480.0)] {
            let p = temp_file(&format!("chg-{ext}-{label}.mp4"), &concat(part));
            assert!(decode_errors(&p).is_empty(), "{name} {label}");
            let v = probe_stream(&p, "v");
            assert_eq!(v["nb_read_packets"], "40");
            assert_eq!(f(&v, "width"), w);
        }
    }
}

// ---- crash-safe files -------------------------------------------------------------------

/// Writes the fixture as a fragmented file; returns the bytes, the offsets
/// after each completed fragment, and the number of video frames in each.
fn write_file(codec: VideoCodec, audio: bool, mfra: bool) -> (Vec<u8>, Vec<u64>, Vec<usize>) {
    let aus = load(codec, fixture_name(codec));
    let frames = tone();
    let o = FileOptions {
        fragment: FragmentOptions {
            max_fragment_us: 500_000,
            ..FragmentOptions::for_download()
        },
        mfra,
    }
    .with_audio(audio.then(|| parse_adts(&frames[0]).unwrap().0));
    let mut w = FragmentedFile::new(Vec::new(), codec, o);
    let mut bounds = Vec::new();
    let mut ai = 0;
    for (i, au) in aus.iter().enumerate() {
        let t = i as i64 * STEP;
        if w.push_video(VideoFrame::new(&au.data, au.key, T0 + t))
            .unwrap()
        {
            bounds.push(w.bytes_written());
        }
        while audio && ai < frames.len() && ai as i64 * AUDIO_FRAME_US <= t {
            w.push_audio_at(&frames[ai], T0 + ai as i64 * AUDIO_FRAME_US)
                .unwrap();
            ai += 1;
        }
    }
    while audio && ai < frames.len() {
        w.push_audio_at(&frames[ai], T0 + ai as i64 * AUDIO_FRAME_US)
            .unwrap();
        ai += 1;
    }
    let (file, s) = w.finish().unwrap();
    assert_eq!(s.file_size, file.len() as u64);
    assert_eq!(s.video_samples, 60);
    assert_eq!(s.has_mfra, mfra);
    // Frames per fragment, from the file itself.
    let mut counts = Vec::new();
    for b in top(&file).iter().filter(|b| b.ty == "moof") {
        let mdat_end = top(&file)
            .iter()
            .find(|m| m.ty == "mdat" && m.start == b.end)
            .unwrap()
            .end;
        counts.push(
            have_video_traf(&parse_frag(&file[b.start..mdat_end]))
                .samples
                .len(),
        );
    }
    assert_eq!(counts.iter().sum::<usize>(), 60);
    (file, bounds, counts)
}

#[test]
fn file_layout_and_mfra() {
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        let (file, bounds, counts) = write_file(codec, true, true);
        let t = top(&file);
        let kinds: Vec<&str> = t.iter().map(|b| b.ty.as_str()).collect();
        assert_eq!(&kinds[..2], ["ftyp", "moov"]);
        assert_eq!(kinds.last(), Some(&"mfra"));
        assert!(
            kinds[2..kinds.len() - 1]
                .chunks(2)
                .all(|c| c == ["moof", "mdat"])
        );
        // The boundaries the writer reported are the ends of mdat boxes.
        let ends: Vec<u64> = t
            .iter()
            .filter(|b| b.ty == "mdat")
            .map(|b| b.end as u64)
            .collect();
        assert_eq!(&ends[..bounds.len()], &bounds[..]);
        // mfra: mfro is last and gives the mfra size; every entry points at a moof
        // whose tfdt matches.
        let mfra = t.last().unwrap();
        let mfro = &file[mfra.end - 16..mfra.end];
        assert_eq!(&mfro[4..8], b"mfro");
        assert_eq!(u32at(mfro, 12) as usize, mfra.end - mfra.start);
        let kids = boxes_in(&file, mfra.start + 8, mfra.end);
        assert_eq!(
            kids.iter().map(|b| b.ty.as_str()).collect::<Vec<_>>(),
            ["tfra", "tfra", "mfro"]
        );
        let moofs: Vec<_> = t.iter().filter(|b| b.ty == "moof").collect();
        for (k, id, want_entries) in [(&kids[0], 1u32, 3usize), (&kids[1], 2, 5)] {
            assert_eq!(u32at(&file, k.start + 12), id);
            let n = u32at(&file, k.start + 20) as usize;
            // Video: keyframes at 0, 20, 40 give three entries. Audio: the tone
            // lasts 2.1 s, so the first five fragments carry it.
            assert_eq!(n, want_entries);
            for e in 0..n {
                let at = k.start + 24 + e * 19;
                let time = u64::from_be_bytes(file[at..at + 8].try_into().unwrap());
                let off = u64::from_be_bytes(file[at + 8..at + 16].try_into().unwrap()) as usize;
                let m = moofs
                    .iter()
                    .find(|m| m.start == off)
                    .expect("entry points at a moof");
                let end = t
                    .iter()
                    .find(|x| x.ty == "mdat" && x.start == m.end)
                    .unwrap()
                    .end;
                let fr = parse_frag(&file[m.start..end]);
                let tr = fr.trafs.iter().find(|x| x.track == id).unwrap();
                assert_eq!(tr.tfdt, time);
                if id == 1 {
                    assert_eq!(tr.samples[0].2, SYNC);
                }
            }
        }
        // The last fragment is written by finish, so it has no boundary entry.
        assert_eq!(counts.len(), 6);
        assert_eq!(bounds.len(), 5);
    }
}

#[test]
fn no_mfra_when_disabled() {
    let (file, _, _) = write_file(VideoCodec::H264, false, false);
    assert!(top(&file).iter().all(|b| b.ty != "mfra"));
}

#[test]
fn truncated_files_play_up_to_the_last_complete_fragment() {
    if !have_ffmpeg() {
        return;
    }
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        for audio in [false, true] {
            let (file, bounds, counts) = write_file(codec, audio, true);
            let mut frames = 0;
            for (k, &b) in bounds.iter().enumerate() {
                frames += counts[k];
                let p = temp_file(
                    &format!("trunc-{codec:?}-{audio}-{k}.mp4"),
                    &file[..b as usize],
                );
                let errs = decode_errors(&p);
                assert!(
                    errs.is_empty(),
                    "{codec:?} audio={audio} cut after fragment {k}: {errs}"
                );
                let v = probe_stream(&p, "v");
                assert_eq!(
                    v["nb_read_packets"],
                    frames.to_string(),
                    "cut after fragment {k}"
                );
            }
            // The whole file, with its mfra index, decodes cleanly too.
            let p = temp_file(&format!("full-{codec:?}-{audio}.mp4"), &file);
            assert!(decode_errors(&p).is_empty());
            assert_eq!(probe_stream(&p, "v")["nb_read_packets"], "60");
            // A cut in the middle of a fragment still plays what came before it
            // (players may or may not show part of the broken fragment).
            let k = 2;
            let mid = (bounds[k] + bounds[k + 1]) / 2;
            let p = temp_file(&format!("mid-{codec:?}-{audio}.mp4"), &file[..mid as usize]);
            let n: usize = probe_stream(&p, "v")["nb_read_packets"].parse().unwrap();
            let before: usize = counts[..=k].iter().sum();
            assert!(
                n >= before && n <= before + counts[k + 1],
                "mid-fragment cut gave {n}"
            );
        }
    }
}

// ---- finalize ----------------------------------------------------------------------------

fn finalize(file: &[u8], faststart: bool) -> (Vec<u8>, mp4_remux::Summary) {
    let (out, s) = finalize_to_mp4(
        Cursor::new(file),
        Cursor::new(Vec::new()),
        Options {
            faststart,
            ..Options::default()
        },
    )
    .unwrap();
    (out.into_inner(), s)
}

#[test]
fn finalize_matches_a_direct_mp4() {
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        for audio in [false, true] {
            let aus = load(codec, fixture_name(codec));
            let (file, _, _) = write_file(codec, audio, true);
            let (mp4, s) = finalize(&file, true);
            assert_eq!(s.video_samples, 60);
            let kinds: Vec<String> = top(&mp4).into_iter().map(|b| b.ty).collect();
            assert_eq!(kinds, ["ftyp", "moov", "mdat"]);
            let t = tracks(&mp4);
            assert_eq!(t[0].stsz.len(), 60);
            for (got, au) in sample_bytes(&mp4, &t[0]).iter().zip(&aus) {
                assert_eq!(got, &expected_sample(&au.data, codec));
            }
            let sync: Vec<u32> = aus
                .iter()
                .enumerate()
                .filter(|(_, a)| a.key)
                .map(|(i, _)| i as u32 + 1)
                .collect();
            assert_eq!(t[0].sync.as_deref(), Some(&sync[..]));
            // Same decode times as a direct, 20 fps mux.
            let dts = t[0].dts();
            assert!(dts.iter().enumerate().all(|(i, &d)| d == i as u64 * 4500));
            if audio {
                let frames = tone();
                assert_eq!(t[1].stsz.len(), frames.len());
                for (got, fr) in sample_bytes(&mp4, &t[1]).iter().zip(&frames) {
                    assert_eq!(got.as_slice(), &fr[7..]);
                }
            }
            if have_ffmpeg() {
                let p = temp_file(&format!("fin-{codec:?}-{audio}.mp4"), &mp4);
                assert!(decode_errors(&p).is_empty());
                assert_eq!(probe_stream(&p, "v")["nb_read_packets"], "60");
            }
        }
    }
}

#[test]
fn finalize_recovers_a_truncated_file() {
    let (file, bounds, counts) = write_file(VideoCodec::H264, true, true);
    for k in [0usize, 2, 3] {
        let frames: usize = counts[..=k].iter().sum();
        // At a boundary, and with a partial fragment after it.
        let tail = bounds[k] as usize + 1000.min((bounds[k + 1] - bounds[k]) as usize - 1);
        for end in [bounds[k] as usize, tail] {
            let (mp4, s) = finalize(&file[..end], false);
            assert_eq!(s.video_samples as usize, frames, "cut {end}");
            assert_eq!(tracks(&mp4)[0].stsz.len(), frames);
            if have_ffmpeg() {
                let p = temp_file(&format!("rec-{k}-{end}.mp4"), &mp4);
                assert!(decode_errors(&p).is_empty());
            }
        }
    }
    // Too short to hold a fragment: nothing to recover.
    let moov_end = top(&file)[1].end;
    let r = finalize_to_mp4(
        Cursor::new(&file[..moov_end]),
        Cursor::new(Vec::new()),
        Options::default(),
    );
    assert!(matches!(r, Err(Error::NoVideo)));
}

#[test]
fn file_rejects_parameter_set_change_and_can_start_another() {
    let aus = load(VideoCodec::H264, "h264-resize.h264");
    let mut w = FragmentedFile::new(Vec::new(), VideoCodec::H264, FileOptions::default());
    let mut at = None;
    for (i, au) in aus.iter().enumerate() {
        match w.push_video(VideoFrame::new(&au.data, au.key, T0 + i as i64 * STEP)) {
            Ok(_) => {}
            Err(Error::ParameterSetsChanged) => {
                at = Some(i);
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(at, Some(40));
    let (first, s) = w.finish().unwrap();
    assert_eq!(s.video_samples, 40);
    // Second file starts with the frame that was refused.
    let mut w = FragmentedFile::new(Vec::new(), VideoCodec::H264, FileOptions::default());
    for (i, au) in aus.iter().enumerate().skip(40) {
        w.push_video(VideoFrame::new(&au.data, au.key, T0 + i as i64 * STEP))
            .unwrap();
    }
    let (second, s) = w.finish().unwrap();
    assert_eq!(s.video_samples, 40);
    if have_ffmpeg() {
        for (n, d) in [(1, first), (2, second)] {
            let p = temp_file(&format!("two-{n}.mp4"), &d);
            assert!(decode_errors(&p).is_empty());
            assert_eq!(probe_stream(&p, "v")["nb_read_packets"], "40");
        }
    }
}
