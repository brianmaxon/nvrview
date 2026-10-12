//! Several audio tracks: declaration, raw access units, track header flags,
//! alternate groups, handler names, encoder-delay edits, finalize, and
//! recovery of a cut file. Structural checks always run; ffprobe/ffmpeg
//! checks run only where they are installed.

mod common;

use common::*;
use mp4_remux::{
    AacConfig, AudioHandler, AudioTrackSpec, Error, FileOptions, FragmentOptions, FragmentedFile,
    Fragmenter, MAX_AUDIO_TRACKS, Mp4Writer, Options, VideoCodec, VideoFrame, finalize_to_mp4,
    parse_adts,
};
use std::io::Cursor;

const STEP: i64 = 50_000; // 20 fps, as the fixture
const T0: i64 = 1_000_000;
const PRIMING: u32 = 2112;
/// One raw AAC-LC mono frame of silence (any sample rate).
const SILENT: [u8; 4] = [0, 0, 0, 7];

fn h264() -> Vec<Au> {
    split_aus(&fixture("h264-basic.h264"), VideoCodec::H264)
}

fn tone() -> Vec<Vec<u8>> {
    adts_frames(&fixture("aac-tone.aac"))
}

/// AAC-LC, 48 kHz, mono, as an app's encoder reports it.
fn app_config() -> AacConfig {
    AacConfig::from_raw_audio_specific_config(&[0x11, 0x88]).unwrap()
}

/// A call recording's three audio tracks: the mix (enabled, first), the
/// camera's side, and this computer's side, all in alternate group 1.
fn call_tracks() -> Vec<AudioTrackSpec> {
    let visitor = parse_adts(&tone()[0]).unwrap().0;
    vec![
        AudioTrackSpec {
            handler: AudioHandler::Conversation,
            alternate_group: 1,
            priming_samples: PRIMING,
            ..AudioTrackSpec::new(app_config())
        },
        AudioTrackSpec {
            handler: AudioHandler::Visitor,
            enabled: false,
            alternate_group: 1,
            ..AudioTrackSpec::new(visitor)
        },
        AudioTrackSpec {
            handler: AudioHandler::You,
            enabled: false,
            alternate_group: 1,
            priming_samples: PRIMING,
            ..AudioTrackSpec::new(app_config())
        },
    ]
}

/// One audio event: (track, time µs, payload).
type Event = (usize, i64, Vec<u8>);

/// The audio of a 3-second call. `decodable` uses silent frames for the
/// 48 kHz tracks (so ffmpeg can decode them); otherwise each payload is
/// distinct, so the samples can be followed byte for byte.
fn call_audio(decodable: bool) -> Vec<Event> {
    let au48 = |k: usize| -> Vec<u8> {
        if decodable {
            SILENT.to_vec()
        } else {
            vec![(k % 251) as u8 + 1; 6 + k % 7]
        }
    };
    let mut ev = Vec::new();
    // Mix: from the first frame on, one unit per 1024 input samples.
    for k in 0..140usize {
        ev.push((0, T0 + k as i64 * 1024 * 1_000_000 / 48_000, au48(k)));
    }
    // Camera side: the tone, 30 ms late.
    for (k, f) in tone().iter().enumerate() {
        ev.push((1, T0 + 30_000 + k as i64 * 64_000, f[7..].to_vec()));
    }
    // This computer's side: starts after 0.5 s and skips units 20..25 (a
    // pause in talk-back).
    for k in (0..60usize).filter(|k| !(20..25).contains(k)) {
        ev.push((
            2,
            T0 + 500_000 + k as i64 * 1024 * 1_000_000 / 48_000,
            au48(1000 + k),
        ));
    }
    ev.sort_by_key(|e| e.1);
    ev
}

/// Writes the call as a fragmented file; returns it and the payloads each
/// track was given.
fn write_call(decodable: bool, max_fragment_us: i64) -> (Vec<u8>, Vec<Vec<Vec<u8>>>) {
    let o = FileOptions {
        fragment: FragmentOptions {
            max_fragment_us,
            ..FragmentOptions::for_download()
        },
        mfra: true,
    }
    .with_audio_tracks(call_tracks());
    let mut w = FragmentedFile::new(Vec::new(), VideoCodec::H264, o);
    let audio = call_audio(decodable);
    let mut given = vec![Vec::new(); 3];
    let mut ai = 0;
    for (i, au) in h264().iter().enumerate() {
        let t = T0 + i as i64 * STEP;
        w.push_video(VideoFrame::new(&au.data, au.key, t)).unwrap();
        while ai < audio.len() && audio[ai].1 <= t {
            let (track, ts, p) = &audio[ai];
            assert!(w.push_audio_raw(*track, p, Some(*ts)).unwrap());
            given[*track].push(p.clone());
            ai += 1;
        }
    }
    for (track, ts, p) in &audio[ai..] {
        assert!(w.push_audio_raw(*track, p, Some(*ts)).unwrap());
        given[*track].push(p.clone());
    }
    let (file, s) = w.finish().unwrap();
    assert_eq!(s.video_samples, 60);
    assert_eq!(
        s.audio_samples,
        given.iter().map(|g| g.len() as u64).sum::<u64>()
    );
    (file, given)
}

fn finalize(file: &[u8], faststart: bool) -> mp4_remux::Result<(Vec<u8>, mp4_remux::Summary)> {
    finalize_to_mp4(
        Cursor::new(file),
        Cursor::new(Vec::new()),
        Options {
            faststart,
            ..Options::default()
        },
    )
    .map(|(c, s)| (c.into_inner(), s))
}

fn be32(d: &[u8], i: usize) -> u32 {
    u32::from_be_bytes(d[i..i + 4].try_into().unwrap())
}

/// Track header and handler facts of every track in `moov`, in order.
#[derive(Debug, PartialEq, Eq)]
struct TrackHeader {
    id: u32,
    flags: u32,
    alternate_group: u16,
    handler: String,
}

fn headers(d: &[u8]) -> Vec<TrackHeader> {
    let moov = top_box(d, "moov");
    boxes_in(d, moov.start + moov.header, moov.end)
        .into_iter()
        .filter(|b| b.ty == "trak")
        .map(|trak| {
            let tkhd = child(d, &trak, "tkhd").unwrap();
            let p = tkhd.start + 8;
            let v1 = d[p] == 1;
            let flags = be32(d, p) & 0xff_ffff;
            let id = be32(d, p + if v1 { 20 } else { 12 });
            let g = p + if v1 { 46 } else { 34 };
            let mdia = child(d, &trak, "mdia").unwrap();
            let hdlr = child(d, &mdia, "hdlr").unwrap();
            let name = &d[hdlr.start + 8 + 24..hdlr.end];
            let name = &name[..name.iter().position(|&b| b == 0).unwrap()];
            TrackHeader {
                id,
                flags,
                alternate_group: u16::from_be_bytes([d[g], d[g + 1]]),
                handler: String::from_utf8(name.to_vec()).unwrap(),
            }
        })
        .collect()
}

fn expected_headers() -> Vec<TrackHeader> {
    [
        (1, 3, 0, "VideoHandler"),
        (2, 3, 1, "Conversation"),
        (3, 2, 1, "Visitor"),
        (4, 2, 1, "You"),
    ]
    .into_iter()
    .map(|(id, flags, alternate_group, h)| TrackHeader {
        id,
        flags,
        alternate_group,
        handler: h.to_string(),
    })
    .collect()
}

/// (track ID, sample count) of every `traf` in a `moof` payload range.
fn trafs(d: &[u8], moof: &BoxRef) -> Vec<(u32, u32, u64)> {
    boxes_in(d, moof.start + 8, moof.end)
        .into_iter()
        .filter(|b| b.ty == "traf")
        .map(|traf| {
            let tfhd = child(d, &traf, "tfhd").unwrap();
            let tfdt = child(d, &traf, "tfdt").unwrap();
            let trun = child(d, &traf, "trun").unwrap();
            let base = if d[tfdt.start + 8] == 1 {
                u64::from_be_bytes(d[tfdt.start + 12..tfdt.start + 20].try_into().unwrap())
            } else {
                u64::from(be32(d, tfdt.start + 12))
            };
            (be32(d, tfhd.start + 12), be32(d, trun.start + 12), base)
        })
        .collect()
}

#[test]
fn fragmented_init_declares_every_track() {
    let (file, given) = write_call(false, 500_000);
    assert_eq!(headers(&file), expected_headers());
    let t = tracks(&file);
    assert_eq!(t.len(), 4);
    assert_eq!(
        t.iter().map(|x| x.handler.as_str()).collect::<Vec<_>>(),
        ["vide", "soun", "soun", "soun"]
    );
    assert_eq!(
        t.iter().map(|x| x.timescale).collect::<Vec<_>>(),
        [90_000, 48_000, 16_000, 48_000]
    );
    // Encoder delay: one edit over all fragments starting at the priming.
    assert_eq!(t[0].edits, []);
    assert_eq!(t[1].edits, [(0, i64::from(PRIMING))]);
    assert_eq!(t[2].edits, []);
    assert_eq!(t[3].edits, [(0, i64::from(PRIMING))]);
    // mvhd next_track_ID and one trex per track.
    let moov = top_box(&file, "moov");
    let mvhd = child(&file, &moov, "mvhd").unwrap();
    assert_eq!(be32(&file, mvhd.end - 4), 5);
    let mvex = child(&file, &moov, "mvex").unwrap();
    let trex: Vec<u32> = boxes_in(&file, mvex.start + 8, mvex.end)
        .iter()
        .map(|b| be32(&file, b.start + 12))
        .collect();
    assert_eq!(trex, [1, 2, 3, 4]);
    // Every sample is in some fragment, track by track, in time order.
    let mut counts = [0u32; 5];
    let mut last_tfdt = [None::<u64>; 5];
    for m in top(&file).iter().filter(|b| b.ty == "moof") {
        let tr = trafs(&file, m);
        assert_eq!(tr[0].0, 1, "video traf first");
        for (id, n, base) in tr {
            counts[id as usize] += n;
            assert!(last_tfdt[id as usize].is_none_or(|l| l < base));
            last_tfdt[id as usize] = Some(base);
        }
    }
    assert_eq!(counts[1], 60);
    for k in 0..3 {
        assert_eq!(counts[k + 2] as usize, given[k].len(), "track {}", k + 2);
    }
    // The random-access index has an entry list for every track, and each
    // entry names the traf that holds that track.
    let mfra = top_box(&file, "mfra");
    let kids = boxes_in(&file, mfra.start + 8, mfra.end);
    let tfra: Vec<&BoxRef> = kids.iter().filter(|b| b.ty == "tfra").collect();
    assert_eq!(tfra.len(), 4);
    for (k, b) in tfra.iter().enumerate() {
        let id = be32(&file, b.start + 12);
        assert_eq!(id, k as u32 + 1);
        let n = be32(&file, b.start + 20) as usize;
        assert!(n > 0);
        for e in 0..n {
            let at = b.start + 24 + e * 19;
            let time = u64::from_be_bytes(file[at..at + 8].try_into().unwrap());
            let off = u64::from_be_bytes(file[at + 8..at + 16].try_into().unwrap()) as usize;
            let traf_no = file[at + 16] as usize;
            let moof = top(&file).into_iter().find(|m| m.start == off).unwrap();
            let tr = trafs(&file, &moof);
            assert_eq!(tr[traf_no - 1].0, id);
            assert_eq!(tr[traf_no - 1].2, time);
        }
    }
}

#[test]
fn finalize_keeps_every_track_with_its_flags_and_edits() {
    let (file, given) = write_call(false, 500_000);
    for faststart in [false, true] {
        let (mp4, s) = finalize(&file, faststart).unwrap();
        assert_eq!(headers(&mp4), expected_headers());
        assert_eq!(s.video_samples, 60);
        let t = tracks(&mp4);
        // Track 4 had a 5-unit pause, filled with silence.
        let want = [140u64, tone().len() as u64, 55 + 5];
        assert_eq!(s.audio_track_samples, want);
        assert_eq!(s.audio_samples, want.iter().sum::<u64>());
        for k in 0..3 {
            let tt = &t[k + 1];
            assert_eq!(tt.stsz.len() as u64, want[k]);
            assert_eq!(tt.media_duration, want[k] * 1024, "track {}", k + 2);
            assert_eq!(tt.stts, [(want[k] as u32, 1024)]);
        }
        // Sample bytes: the mix and the camera side unchanged; this
        // computer's side with five silent frames in the pause.
        assert_eq!(sample_bytes(&mp4, &t[1]), given[0]);
        assert_eq!(sample_bytes(&mp4, &t[2]), given[1]);
        let mut you = given[2].clone();
        you.splice(20..20, std::iter::repeat_n(SILENT.to_vec(), 5));
        assert_eq!(sample_bytes(&mp4, &t[3]), you);
        // Edits (movie ticks, 90 kHz): the mix starts with the video and
        // skips its priming; the camera side starts 30 ms in; this
        // computer's side 500 ms in, skipping its priming.
        let presented = |n: u64, rate: u64, skip: u64| (n * 1024 - skip) * 90_000 / rate;
        assert_eq!(
            t[1].edits,
            [(presented(140, 48_000, 2112), i64::from(PRIMING))]
        );
        assert_eq!(t[2].edits, [(2700, -1), (presented(want[1], 16_000, 0), 0)]);
        assert_eq!(
            t[3].edits,
            [
                (45_000, -1),
                (presented(60, 48_000, 2112), i64::from(PRIMING))
            ]
        );
        if have_ffmpeg() {
            let p = temp_file(&format!("call-fin-{faststart}.mp4"), &mp4);
            assert_eq!(audio_streams(&p), 3);
        }
    }
}

/// Number of audio streams ffprobe sees.
fn audio_streams(p: &std::path::Path) -> usize {
    let o = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a"])
        .args(["-show_entries", "stream=index", "-of", "csv=p=0"])
        .arg(p)
        .output()
        .unwrap();
    assert!(o.status.success());
    String::from_utf8_lossy(&o.stdout).lines().count()
}

#[test]
fn call_files_decode_in_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    let (file, _) = write_call(true, 500_000);
    let (mp4, _) = finalize(&file, true).unwrap();
    for (name, d) in [("frag", &file), ("final", &mp4)] {
        let p = temp_file(&format!("call-{name}.mp4"), d);
        let errs = decode_errors(&p);
        assert!(errs.is_empty(), "{name}: {errs}");
        assert_eq!(audio_streams(&p), 3, "{name}");
        assert_eq!(probe_stream(&p, "v")["nb_read_packets"], "60", "{name}");
        let a = probe_stream(&p, "a");
        assert_eq!(a["sample_rate"], "48000", "{name}");
        assert_eq!(a["nb_read_packets"], "140", "{name}");
        // The priming edit: the mix's first units sit before time 0 (ffmpeg
        // decodes and drops them), and the stream starts at 0.
        let first = probe_packets(&p, "a")[0].0;
        assert!((first + 2112.0 / 48_000.0).abs() < 1e-3, "{name}: {first}");
        let streams = stream_facts(&p);
        let want = [
            ("VideoHandler", 1, 0.0),
            ("Conversation", 1, 0.0),
            ("Visitor", 0, 0.03),
            ("You", 0, 0.5),
        ];
        assert_eq!(streams.len(), 4, "{name}");
        for ((h, default, start), (wh, wd, ws)) in streams.iter().zip(want) {
            assert_eq!((h.as_str(), *default), (wh, wd), "{name}");
            assert!((start - ws).abs() < 1e-3, "{name} {h}: {start}");
        }
    }
}

/// (handler name, default disposition, start time) per stream, from
/// ffprobe.
fn stream_facts(p: &std::path::Path) -> Vec<(String, u8, f64)> {
    let o = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries"])
        .arg("stream=start_time:stream_disposition=default:stream_tags=handler_name")
        .args(["-of", "compact=nk=0"])
        .arg(p)
        .output()
        .unwrap();
    assert!(o.status.success());
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| {
            let get = |k: &str| {
                l.split('|')
                    .find_map(|f| f.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
                    .unwrap_or_else(|| panic!("no {k} in {l}"))
                    .to_string()
            };
            (
                get("tag:handler_name"),
                get("disposition:default").parse().unwrap(),
                get("start_time").parse().unwrap(),
            )
        })
        .collect()
}

/// A small deterministic generator (64-bit LCG) for cut points.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % below as u64) as usize
    }
}

#[test]
fn a_cut_file_recovers_every_complete_fragment() {
    let (file, given) = write_call(false, 300_000);
    let (full, _) = finalize(&file, false).unwrap();
    let full_tracks = tracks(&full);
    let moov_end = top_box(&file, "moov").end;
    // (end of the fragment's mdat, samples per track 1..=4) for each fragment.
    let top_boxes = top(&file);
    let frags: Vec<(usize, [u32; 5])> = top_boxes
        .iter()
        .filter(|b| b.ty == "moof")
        .map(|m| {
            let mdat = top_boxes.iter().find(|x| x.start == m.end).unwrap();
            assert_eq!(mdat.ty, "mdat");
            let mut n = [0u32; 5];
            for (id, c, _) in trafs(&file, m) {
                n[id as usize] += c;
            }
            (mdat.end, n)
        })
        .collect();
    assert!(frags.len() >= 8, "{} fragments", frags.len());
    for seed in [1u64, 7, 42, 2026, 0xdead_beef] {
        let mut rng = Lcg(seed);
        for _ in 0..12 {
            let cut = moov_end + rng.next(file.len() - moov_end) + 1;
            let mut want = [0u32; 5];
            for (end, n) in &frags {
                if *end <= cut {
                    for k in 1..5 {
                        want[k] += n[k];
                    }
                }
            }
            let r = finalize(&file[..cut], false);
            if want[1] == 0 {
                assert!(matches!(r, Err(Error::NoVideo)), "cut {cut}");
                continue;
            }
            let (mp4, s) = r.unwrap_or_else(|e| panic!("cut {cut}: {e}"));
            assert_eq!(s.video_samples, u64::from(want[1]), "cut {cut}");
            assert_eq!(headers(&mp4), expected_headers(), "cut {cut}");
            let t = tracks(&mp4);
            // Each audio track holds a prefix of the whole file's samples
            // (plus, for the paused track, the same silence fill).
            for k in 2..5 {
                let got = sample_bytes(&mp4, &t[k - 1]);
                let all = sample_bytes(&full, &full_tracks[k - 1]);
                assert!(got.len() <= all.len());
                assert_eq!(got[..], all[..got.len()], "cut {cut} track {k}");
                if k < 4 {
                    assert_eq!(got.len(), want[k] as usize, "cut {cut} track {k}");
                }
            }
            assert_eq!(given[0].len(), 140);
        }
    }
}

#[test]
fn parameter_set_change_still_ends_the_file() {
    let aus = split_aus(&fixture("h264-resize.h264"), VideoCodec::H264);
    let mut w = FragmentedFile::new(
        Vec::new(),
        VideoCodec::H264,
        FileOptions::default().with_audio_tracks(call_tracks()),
    );
    let mut at = None;
    for (i, au) in aus.iter().enumerate() {
        let t = T0 + i as i64 * STEP;
        match w.push_video(VideoFrame::new(&au.data, au.key, t)) {
            Ok(_) => {}
            Err(Error::ParameterSetsChanged) => {
                at = Some(i);
                break;
            }
            Err(e) => panic!("{e}"),
        }
        w.push_audio_raw(0, &SILENT, Some(t)).unwrap();
    }
    assert_eq!(at, Some(40));
    let (file, s) = w.finish().unwrap();
    assert_eq!(s.video_samples, 40);
    let (mp4, s) = finalize(&file, true).unwrap();
    assert_eq!(s.video_samples, 40);
    assert_eq!(headers(&mp4), expected_headers());
    assert_eq!(s.audio_track_samples[1..], [0, 0]);
}

#[test]
fn one_declared_plain_track_equals_the_single_track_api() {
    let aus = h264();
    let frames = tone();
    let cfg = parse_adts(&frames[0]).unwrap().0;
    let run = |declared: bool| {
        let o = FragmentOptions {
            audio: (!declared).then_some(cfg),
            audio_tracks: if declared {
                vec![AudioTrackSpec::new(cfg)]
            } else {
                Vec::new()
            },
            ..FragmentOptions::for_download()
        };
        let mut f = Fragmenter::new(VideoCodec::H264, o);
        let mut out = Vec::new();
        let mut ai = 0;
        for (i, au) in aus.iter().enumerate() {
            let t = T0 + i as i64 * STEP;
            out.extend(f.push_video(VideoFrame::new(&au.data, au.key, t)).unwrap());
            while ai < frames.len() && T0 + ai as i64 * 64_000 <= t {
                let ts = T0 + ai as i64 * 64_000;
                if declared {
                    f.push_audio_raw(0, &frames[ai][7..], Some(ts)).unwrap();
                } else {
                    f.push_audio_at(&frames[ai], ts).unwrap();
                }
                ai += 1;
            }
        }
        out.extend(f.finish().unwrap());
        out.into_iter().flat_map(|s| s.data).collect::<Vec<u8>>()
    };
    assert_eq!(run(true), run(false));
}

#[test]
fn track_errors() {
    let aus = h264();
    let mut f = Fragmenter::new(
        VideoCodec::H264,
        FragmentOptions {
            audio_tracks: call_tracks(),
            ..FragmentOptions::default()
        },
    );
    f.push_video(VideoFrame::new(&aus[0].data, true, T0))
        .unwrap();
    assert!(matches!(
        f.push_audio_raw(3, &SILENT, None),
        Err(Error::InvalidTrack(_))
    ));
    assert!(matches!(
        f.push_audio_raw(0, &[], None),
        Err(Error::InvalidTrack(_))
    ));
    assert!(matches!(
        f.push_audio_raw(0, &vec![1; 70_000], None),
        Err(Error::InvalidTrack(_))
    ));
    assert!(f.push_audio_raw(0, &SILENT, None).unwrap());
    // ADTS frames go to the first track and must match its format.
    assert!(matches!(
        f.push_audio(&tone()[0]),
        Err(Error::AudioConfigChanged)
    ));

    let too_many = vec![AudioTrackSpec::new(app_config()); MAX_AUDIO_TRACKS + 1];
    let mut f = Fragmenter::new(
        VideoCodec::H264,
        FragmentOptions {
            audio_tracks: too_many.clone(),
            ..FragmentOptions::default()
        },
    );
    assert!(matches!(
        f.push_video(VideoFrame::new(&aus[0].data, true, T0)),
        Err(Error::InvalidTrack(_))
    ));
    let r = Mp4Writer::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options {
            audio_tracks: too_many,
            ..Options::default()
        },
    );
    assert!(matches!(r, Err(Error::InvalidTrack(_))));

    // The writer: raw units only to declared tracks.
    let mut w = Mp4Writer::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options::default(),
    )
    .unwrap();
    assert!(matches!(
        w.write_audio_raw(0, &SILENT),
        Err(Error::InvalidTrack(_))
    ));
}

#[test]
fn writer_keeps_declared_tracks_that_got_no_samples() {
    let aus = h264();
    let mut w = Mp4Writer::new(
        Cursor::new(Vec::new()),
        VideoCodec::H264,
        Options {
            audio_tracks: call_tracks(),
            ..Options::default()
        },
    )
    .unwrap();
    for (i, au) in aus.iter().enumerate() {
        w.write_video(VideoFrame::new(&au.data, au.key, T0 + i as i64 * STEP))
            .unwrap();
    }
    for k in 0..10 {
        w.write_audio_raw(0, &SILENT).unwrap();
        assert!(
            w.write_audio_raw_at(2, &SILENT, T0 + k * 1024 * 1_000_000 / 48_000)
                .unwrap()
        );
    }
    let (c, s) = w.finish().unwrap();
    let mp4 = c.into_inner();
    assert_eq!(s.audio_track_samples, [10, 0, 10]);
    assert_eq!(headers(&mp4), expected_headers());
    let t = tracks(&mp4);
    assert!(t[2].stsz.is_empty() && t[2].edits.is_empty());
    assert_eq!(t[2].media_duration, 0);
    if have_ffmpeg() {
        let p = temp_file("declared-empty.mp4", &mp4);
        assert!(decode_errors(&p).is_empty(), "{}", decode_errors(&p));
    }
}

#[test]
fn separate_layout_puts_every_audio_track_in_the_audio_stream() {
    let aus = h264();
    let mut f = Fragmenter::new(
        VideoCodec::H264,
        FragmentOptions {
            audio_tracks: call_tracks(),
            audio_layout: mp4_remux::AudioLayout::Separate,
            ..FragmentOptions::default()
        },
    );
    let audio = call_audio(false);
    let mut segs = Vec::new();
    let mut ai = 0;
    for (i, au) in aus.iter().enumerate() {
        let t = T0 + i as i64 * STEP;
        segs.extend(f.push_video(VideoFrame::new(&au.data, au.key, t)).unwrap());
        while ai < audio.len() && audio[ai].1 <= t {
            f.push_audio_raw(audio[ai].0, &audio[ai].2, Some(audio[ai].1))
                .unwrap();
            ai += 1;
        }
    }
    segs.extend(f.finish().unwrap());
    let inits: Vec<_> = segs
        .iter()
        .filter(|s| s.kind == mp4_remux::SegmentKind::Init)
        .collect();
    assert_eq!(inits.len(), 2);
    assert_eq!(headers(&inits[0].data).len(), 1);
    assert_eq!(headers(&inits[1].data), expected_headers()[1..]);
    // One codec string for the three tracks of the same codec.
    assert_eq!(
        inits[1].init.as_ref().unwrap().mime,
        "audio/mp4; codecs=\"mp4a.40.2\""
    );
    let mut seen = [0usize; 5];
    for s in segs
        .iter()
        .filter(|s| s.track == mp4_remux::SegmentTrack::Audio && s.init.is_none())
    {
        assert!(s.video_tfdt.is_none());
        assert_eq!(
            s.audio_tfdt,
            s.audio_tfdts.iter().find(|t| t.0 == 2).map(|t| t.1)
        );
        for &(id, _) in &s.audio_tfdts {
            seen[id as usize] += 1;
        }
    }
    assert!(seen[2..].iter().all(|&n| n > 0), "{seen:?}");
}
