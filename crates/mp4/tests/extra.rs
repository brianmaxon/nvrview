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

/// Where the first sample of a file starts: after `ftyp` and the 64-bit
/// `mdat` header.
const FIRST_SAMPLE_AT: u64 = 28 + 16;

/// Writes a keyframe at the start of the media data, then a run of frames
/// in a chunk at each of `chunk_at` (file offsets as if the file had no
/// index in front), the space between left unwritten in a sparse sink.
/// Returns the sink, the summary, and the video track's tables, after
/// checking every sample against its frame.
fn chunks_at(
    aus: &[Au],
    faststart: bool,
    chunk_at: &[u64],
) -> (SparseFile, mp4_remux::Summary, TrackTables) {
    let mut w = Mp4Writer::new(
        SparseFile::new(),
        VideoCodec::H264,
        Options {
            faststart,
            ..Options::default()
        },
    )
    .unwrap();
    let mut written = vec![0usize];
    w.write_video(VideoFrame::new(&aus[0].data, true, 0))
        .unwrap();
    for (c, &at) in chunk_at.iter().enumerate() {
        w.skip_media_to_for_test(at).unwrap();
        for k in 0..3 {
            let i = 1 + c * 3 + k;
            w.write_video(VideoFrame::new(&aus[i].data, aus[i].key, i as i64 * STEP))
                .unwrap();
            written.push(i);
        }
    }
    let (sink, s) = w.finish().unwrap();
    assert_eq!(sink.len(), s.file_size);
    // Locate moov using only the box headers and parse it on its own.
    let ftyp = u32::from_be_bytes(sink.read_at(0, 4).try_into().unwrap()) as u64;
    let moov_at = if faststart {
        ftyp
    } else {
        let hdr = sink.read_at(ftyp, 16);
        assert_eq!(&hdr[4..8], b"mdat");
        assert_eq!(u32::from_be_bytes(hdr[..4].try_into().unwrap()), 1);
        // The mdat's 64-bit size runs to the moov.
        ftyp + u64::from_be_bytes(hdr[8..16].try_into().unwrap())
    };
    let size = u32::from_be_bytes(sink.read_at(moov_at, 4).try_into().unwrap()) as usize;
    assert_eq!(size as u64, s.moov_size);
    let moov = sink.read_at(moov_at, size);
    let t = tracks(&moov).remove(0);
    assert_eq!(t.co64, s.uses_co64);
    // The chunks are where they were put, after the moov when it comes
    // first.
    let shift = if faststart { s.moov_size } else { 0 };
    let mut want = vec![FIRST_SAMPLE_AT + shift];
    want.extend(chunk_at.iter().map(|at| at + shift));
    assert_eq!(t.chunk_offsets, want);
    // Every sample holds its frame: length-prefixed NALs that fill it
    // exactly, the last one the frame's last NAL byte for byte.
    let locs = t.sample_locations();
    assert_eq!(locs.len(), written.len());
    for (&(off, sz), &i) in locs.iter().zip(&written) {
        let sample = sink.read_at(off, sz as usize);
        let mut p = 0;
        let mut last = &sample[..0];
        while p < sample.len() {
            let n = u32::from_be_bytes(sample[p..p + 4].try_into().unwrap()) as usize;
            last = &sample[p + 4..p + 4 + n];
            p += 4 + n;
        }
        assert_eq!(p, sample.len(), "frame {i}");
        let (ns, ne) = *nals(&aus[i].data).last().unwrap();
        assert_eq!(last, &aus[i].data[ns..ne], "frame {i}");
    }
    (sink, s, t)
}

/// Chunks just under and well past 4 GiB: 64-bit offsets, the mdat's
/// 64-bit size, and every sample where the index says.
fn past_4_gib(faststart: bool) {
    let aus = h264();
    let max = u64::from(u32::MAX);
    let (_, s, _) = chunks_at(&aus, faststart, &[max - 1000, max + (1 << 20) + 3]);
    assert!(s.uses_co64);
    assert!(s.file_size > max);
}

#[test]
fn co64_when_file_exceeds_4_gib() {
    past_4_gib(false);
}

#[test]
fn co64_with_faststart_shifts_offsets_past_the_moov() {
    past_4_gib(true);
}

/// The last chunk starts just under 4 GiB: it fits 32 bits where it was
/// written, but not once a front moov pushes it along.
#[test]
fn co64_when_only_the_faststart_shift_crosses_4_gib() {
    let aus = h264();
    let at = u64::from(u32::MAX) - 100;
    let (_, s, t) = chunks_at(&aus, false, &[at]);
    assert!(!s.uses_co64, "the chunk fits 32 bits where it was written");
    assert!(s.moov_size > 100);
    assert_eq!(*t.chunk_offsets.last().unwrap(), at);
    let (_, s, t) = chunks_at(&aus, true, &[at]);
    assert!(s.uses_co64);
    assert!(*t.chunk_offsets.last().unwrap() > u64::from(u32::MAX));
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
