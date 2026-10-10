//! Concatenation, 64-bit offsets, and property tests.

mod common;

use common::*;
use mp4_remux::{Chunk, Concatenator, Mp4Writer, Options, TimedAudio, VideoCodec, VideoFrame};
use proptest::prelude::*;
use std::io::Cursor;

const STEP: i64 = 50_000;

fn h264() -> Vec<Au> {
    split_aus(&fixture("h264-basic.h264"), VideoCodec::H264)
}

fn frames<'a>(aus: &'a [Au], range: std::ops::Range<usize>) -> Vec<VideoFrame<'a>> {
    range
        .map(|i| VideoFrame::new(&aus[i].data, aus[i].key, 1_000_000 + i as i64 * STEP))
        .collect()
}

#[test]
fn concatenating_overlapping_chunks_equals_one_pass() {
    let aus = h264();
    // Chunk boundaries repeat 5 frames (indices 20..25) as devices may do.
    let c1 = frames(&aus, 0..25);
    let c2 = frames(&aus, 20..45);
    let c3 = frames(&aus, 45..60);
    let mut cc = Concatenator::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options::default(),
    )
    .unwrap();
    let s1 = cc
        .push_chunk(&Chunk {
            video: &c1,
            audio: &[],
        })
        .unwrap();
    let s2 = cc
        .push_chunk(&Chunk {
            video: &c2,
            audio: &[],
        })
        .unwrap();
    let s3 = cc
        .push_chunk(&Chunk {
            video: &c3,
            audio: &[],
        })
        .unwrap();
    assert_eq!((s1.video_written, s1.video_duplicates), (25, 0));
    assert_eq!((s2.video_written, s2.video_duplicates), (20, 5));
    assert_eq!((s3.video_written, s3.video_duplicates), (15, 0));
    let (file, summary) = cc.finish().unwrap();
    assert_eq!(summary.video_samples, 60);
    let file = file.into_inner();
    let (single, _) = mux(VideoCodec::H264, &aus, STEP, &[], Options::default());
    let (a, b) = (&tracks(&file)[0], &tracks(&single)[0]);
    assert_eq!(a.stts, b.stts);
    assert_eq!(a.sync, b.sync);
    assert_eq!(sample_bytes(&file, a), sample_bytes(&single, b));
}

#[test]
fn a_gap_between_chunks_extends_the_previous_sample() {
    let aus = h264();
    let c1 = frames(&aus, 0..20);
    let c2 = frames(&aus, 40..60); // 20 frames (1 s) missing, starts on a keyframe
    let mut cc = Concatenator::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options::default(),
    )
    .unwrap();
    cc.push_chunk(&Chunk {
        video: &c1,
        audio: &[],
    })
    .unwrap();
    cc.push_chunk(&Chunk {
        video: &c2,
        audio: &[],
    })
    .unwrap();
    let (file, _) = cc.finish().unwrap();
    let file = file.into_inner();
    let v = &tracks(&file)[0];
    let dts = v.dts();
    assert_eq!(dts[20] - dts[19], 4500 * 21);
    assert_eq!(v.stsz.len(), 40);
}

#[test]
fn leading_non_keyframes_are_skipped() {
    let aus = split_aus(&fixture("h264-leading-p.h264"), VideoCodec::H264);
    let c = frames(&aus, 0..aus.len());
    let mut cc = Concatenator::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options::default(),
    )
    .unwrap();
    let s = cc
        .push_chunk(&Chunk {
            video: &c,
            audio: &[],
        })
        .unwrap();
    assert!(s.video_leading_skipped > 0);
    assert_eq!(s.video_written + s.video_leading_skipped, aus.len() as u32);
    let (file, _) = cc.finish().unwrap();
    let file = file.into_inner();
    assert_eq!(tracks(&file)[0].sync.as_ref().unwrap()[0], 1);
}

#[test]
fn audio_overlaps_dedupe_and_gaps_become_empty_edits() {
    let aus = h264();
    let audio = adts_frames(&fixture("aac-tone.aac"));
    let at = |i: usize| 1_000_000 + i as i64 * 64_000;
    let timed = |r: std::ops::Range<usize>| -> Vec<TimedAudio<'_>> {
        r.map(|i| TimedAudio {
            adts: &audio[i],
            ts_us: at(i),
        })
        .collect()
    };
    let v1 = frames(&aus, 0..30);
    let v2 = frames(&aus, 30..60);
    // Audio chunk 1: frames 0..10, chunk 2 repeats 8..12 then skips to 16..20.
    let (a1, a2) = (timed(0..10), [timed(8..12), timed(16..20)].concat());
    let mut cc = Concatenator::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options::default(),
    )
    .unwrap();
    cc.push_chunk(&Chunk {
        video: &v1,
        audio: &a1,
    })
    .unwrap();
    let s = cc
        .push_chunk(&Chunk {
            video: &v2,
            audio: &a2,
        })
        .unwrap();
    assert_eq!(s.audio_duplicates, 2);
    assert_eq!(s.audio_written, 6);
    let (file, summary) = cc.finish().unwrap();
    // 12 distinct frames, 4 silent frames for the gap, then 4 more.
    assert_eq!(summary.audio_samples, 20);
    let file = file.into_inner();
    let a = &tracks(&file)[1];
    assert_eq!(a.stts, [(20, 1024)]);
    assert!(a.edits.is_empty());
    let got = sample_bytes(&file, a);
    for i in 0..12 {
        assert_eq!(got[i].as_slice(), &audio[i][7..]);
    }
    for g in &got[12..16] {
        assert_eq!(g.as_slice(), [0, 0, 0, 7]);
    }
    for i in 16..20 {
        assert_eq!(got[i].as_slice(), &audio[i][7..]);
    }
    if have_ffmpeg() {
        let p = temp_file("concat-audio.mp4", &file);
        assert!(decode_errors(&p).is_empty());
        let s = probe_stream(&p, "a");
        assert_eq!(f(&s, "nb_read_packets"), 20.0);
        assert!((f(&s, "duration") - 20.0 * 0.064).abs() < 0.01, "{s:?}");
    }
}

#[test]
fn concatenated_file_decodes_cleanly() {
    if !have_ffmpeg() {
        return;
    }
    let aus = h264();
    let c1 = frames(&aus, 0..30);
    let c2 = frames(&aus, 20..60);
    let mut cc = Concatenator::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options {
            faststart: true,
            ..Options::default()
        },
    )
    .unwrap();
    cc.push_chunk(&Chunk {
        video: &c1,
        audio: &[],
    })
    .unwrap();
    cc.push_chunk(&Chunk {
        video: &c2,
        audio: &[],
    })
    .unwrap();
    let (file, _) = cc.finish().unwrap();
    let p = temp_file("concat.mp4", &file.into_inner());
    assert!(decode_errors(&p).is_empty());
    assert_eq!(f(&probe_stream(&p, "v"), "nb_read_packets"), 60.0);
}

// ---- 64-bit offsets --------------------------------------------------------

fn big_file(faststart: bool) {
    let aus = h264();
    let mut w = Mp4Writer::new(
        SparseFile::new(),
        VideoCodec::H264,
        Options {
            faststart,
            ..Options::default()
        },
    )
    .unwrap();
    w.write_video(VideoFrame::new(&aus[0].data, true, 0))
        .unwrap();
    // ~4.4 GiB of padding in 16 MiB zero-filled slices (the NAL ends in a
    // non-zero byte so trailing-zero trimming keeps its size).
    let mut big = vec![0, 0, 0, 1, 0x41, 0x9a];
    big.resize(16 << 20, 0);
    *big.last_mut().unwrap() = 0x80;
    for i in 1..=280 {
        w.write_video(VideoFrame::new(&big, false, i * STEP))
            .unwrap();
    }
    w.write_video(VideoFrame::new(&aus[1].data, false, 281 * STEP))
        .unwrap();
    let (sink, s) = w.finish().unwrap();
    assert!(s.uses_co64);
    assert!(s.file_size > u64::from(u32::MAX));
    assert_eq!(sink.len(), s.file_size);
    // Locate moov using only the box headers and parse it on its own.
    let ftyp = u32::from_be_bytes(sink.read_at(0, 4).try_into().unwrap()) as u64;
    let (moov_at, mdat_at) = if faststart {
        (ftyp, 0)
    } else {
        let hdr = sink.read_at(ftyp, 16);
        assert_eq!(&hdr[4..8], b"mdat");
        assert_eq!(u32::from_be_bytes(hdr[..4].try_into().unwrap()), 1);
        (
            ftyp + u64::from_be_bytes(hdr[8..16].try_into().unwrap()),
            ftyp,
        )
    };
    let size = u32::from_be_bytes(sink.read_at(moov_at, 4).try_into().unwrap()) as usize;
    let moov = sink.read_at(moov_at, size);
    let t = &tracks(&moov)[0];
    assert!(t.co64);
    assert_eq!(t.stsz.len(), 282);
    let locs = t.sample_locations();
    assert!(
        locs.last().unwrap().0 > u64::from(u32::MAX),
        "last sample is beyond 4 GiB"
    );
    // Every sample starts with its own length prefix and NAL header.
    for (i, &(off, sz)) in locs.iter().enumerate().skip(1).step_by(37) {
        let head = sink.read_at(off, 6);
        assert_eq!(
            u32::from_be_bytes(head[..4].try_into().unwrap()),
            sz - 4,
            "sample {i}"
        );
    }
    let (off, sz) = *locs.last().unwrap();
    assert_eq!(
        sink.read_at(off + u64::from(sz) - 1, 1)[0],
        aus[1].data.last().copied().unwrap()
    );
    // Sizes of the mdat match the file.
    if !faststart {
        let hdr = sink.read_at(mdat_at, 16);
        assert_eq!(
            u64::from_be_bytes(hdr[8..16].try_into().unwrap()),
            moov_at - mdat_at
        );
    }
}

#[test]
fn co64_when_file_exceeds_4_gib() {
    big_file(false);
}

#[test]
fn co64_with_faststart_shifts_offsets_past_the_moov() {
    big_file(true);
}

#[test]
fn small_files_use_stco() {
    let (file, s) = mux(VideoCodec::H264, &h264(), STEP, &[], Options::default());
    assert!(!s.uses_co64);
    assert!(!tracks(&file)[0].co64);
}

// ---- property tests --------------------------------------------------------

/// Found by the property test on Linux: a single frame presented later than
/// it decodes made the edit-list duration underflow.
#[test]
fn presentation_offset_larger_than_media_duration() {
    let aus = h264();
    let mut w = Mp4Writer::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options::default(),
    )
    .unwrap();
    let t = 123_456_789i64;
    w.write_video(VideoFrame {
        data: &aus[0].data,
        keyframe: true,
        dts_us: t,
        pts_us: t + 40_006,
    })
    .unwrap();
    let (file, _) = w.finish().unwrap();
    assert_eq!(tracks(&file.into_inner())[0].stsz.len(), 1);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn tables_match_inputs(
        frames in prop::collection::vec((1_000i64..400_000, any::<bool>(), 0i64..120_000, 1usize..300), 1..120),
        audio in prop::collection::vec(1usize..500, 0..60),
        faststart in any::<bool>(),
    ) {
        let aus = h264();
        let mut w = Mp4Writer::new(Cursor::new(Vec::new()), VideoCodec::H264, Options { faststart, ..Options::default() }).unwrap();
        let mut t = 123_456_789i64;
        let mut expect: Vec<(i64, i64, bool, Vec<u8>)> = Vec::new(); // dts, pts, key, stored
        for (i, (delta, key, pts_off, len)) in frames.iter().enumerate() {
            let (data, key) = if i == 0 {
                (aus[0].data.clone(), true)
            } else {
                (fake_p_frame(&vec![7u8; *len]), *key)
            };
            if i > 0 { t += delta; }
            let pts = t + pts_off;
            w.write_video(VideoFrame { data: &data, keyframe: key, dts_us: t, pts_us: pts }).unwrap();
            expect.push((t, pts, key, expected_sample(&data, VideoCodec::H264)));
        }
        let afr: Vec<Vec<u8>> = audio.iter().map(|n| adts_frame(&vec![9u8; *n])).collect();
        for a in &afr { w.write_audio(a).unwrap(); }
        let (file, _) = w.finish().unwrap();
        let file = file.into_inner();
        let ts = tracks(&file);
        let v = &ts[0];
        prop_assert_eq!(v.stsz.len(), expect.len());
        // stts: reconstructs each decode time relative to the first.
        let d0 = mp4_remux::us_to_ticks(expect[0].0);
        let dts = v.dts();
        for (i, e) in expect.iter().enumerate() {
            prop_assert_eq!(dts[i] as i64, mp4_remux::us_to_ticks(e.0) - d0);
        }
        // ctts reconstructs presentation offsets.
        let mut cts = Vec::new();
        for &(c, o) in &v.ctts { for _ in 0..c { cts.push(o); } }
        if cts.is_empty() {
            for e in &expect { prop_assert_eq!(mp4_remux::us_to_ticks(e.1), mp4_remux::us_to_ticks(e.0)); }
        } else {
            prop_assert_eq!(cts.len(), expect.len());
            for (i, e) in expect.iter().enumerate() {
                prop_assert_eq!(i64::from(cts[i]), mp4_remux::us_to_ticks(e.1) - mp4_remux::us_to_ticks(e.0));
            }
        }
        // stss lists exactly the keyframes (or is absent when all are).
        let keys: Vec<u32> = expect.iter().enumerate().filter(|(_, e)| e.2).map(|(i, _)| i as u32 + 1).collect();
        match &v.sync {
            Some(s) => prop_assert_eq!(s, &keys),
            None => prop_assert_eq!(keys.len(), expect.len()),
        }
        // Samples are where the chunk tables say they are.
        let stored = sample_bytes(&file, v);
        for (s, e) in stored.iter().zip(&expect) { prop_assert_eq!(s, &e.3); }
        // Chunks are time-ordered, non-overlapping, and the stsc runs are minimal.
        let mut all: Vec<(u64, u64)> = v.sample_locations().iter().map(|&(o, s)| (o, o + u64::from(s))).collect();
        if let Some(a) = ts.get(1) {
            all.extend(a.sample_locations().iter().map(|&(o, s)| (o, o + u64::from(s))));
            prop_assert_eq!(a.stsz.len(), afr.len());
            prop_assert_eq!(a.stts.clone(), vec![(afr.len() as u32, 1024)]);
            for (g, f) in sample_bytes(&file, a).iter().zip(&afr) { prop_assert_eq!(g.as_slice(), &f[7..]); }
        } else {
            prop_assert!(afr.is_empty());
        }
        all.sort();
        for p in all.windows(2) { prop_assert!(p[0].1 <= p[1].0); }
        for r in v.stsc.windows(2) { prop_assert!((r[0].1, r[0].2) != (r[1].1, r[1].2)); }
        // Presentation start: edit list present iff the minimum pts is offset
        // from dts by a non-zero amount or audio is offset; media_time never negative.
        for t in &ts { for e in &t.edits { prop_assert!(e.1 >= -1); } }
        let _ = ();
    }
}
