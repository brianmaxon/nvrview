//! Generates fragmented MP4 samples for the Media Source test page in
//! `tests/mse/`. Everything is synthetic (the repository's test fixtures).
//!
//! ```text
//! cargo run -p mp4-remux --example mse_sample [OUT_DIR]
//! ```
//!
//! `OUT_DIR` defaults to `crates/mp4/tests/mse/out`. For each sample it
//! writes `NAME.bin` (the segments back to back, as a server would send them
//! over a WebSocket) and `NAME.json` (one record per segment: kind, track,
//! MIME type, byte range, timing), plus `index.json` listing the samples.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use mp4_remux::{
    AudioLayout, FragmentOptions, Fragmenter, Segment, SegmentKind, SegmentTrack, VideoCodec,
    VideoFrame, parse_adts,
};

const STEP_US: i64 = 50_000; // the fixtures are 20 fps
const AUDIO_US: i64 = 64_000; // 1024 samples at 16 kHz

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic")
}

/// Annex-B access units (start code included) and whether each is a keyframe.
fn access_units(d: &[u8], codec: VideoCodec) -> Vec<(Vec<u8>, bool)> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= d.len() {
        if d[i..i + 3] == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut aus = Vec::new();
    let (mut cur, mut has_vcl, mut key, mut last_end) = (None::<usize>, false, false, 0);
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(d.len(), |n| n - 3);
        while e > s && d[e - 1] == 0 {
            e -= 1;
        }
        let (vcl, first, is_key) = match codec {
            VideoCodec::H264 => {
                let t = d[s] & 0x1f;
                ((1..=5).contains(&t), d[s + 1] & 0x80 != 0, t == 5)
            }
            VideoCodec::H265 => {
                let t = (d[s] >> 1) & 0x3f;
                (t < 32, d[s + 2] & 0x80 != 0, (16..=21).contains(&t))
            }
        };
        if (!vcl && has_vcl) || (vcl && first && has_vcl) {
            aus.push((d[cur.take().expect("start")..last_end].to_vec(), key));
            has_vcl = false;
            key = false;
        }
        cur.get_or_insert(s - 3);
        if vcl {
            has_vcl = true;
            key |= is_key;
        }
        last_end = e;
    }
    if let Some(c) = cur {
        aus.push((d[c..last_end].to_vec(), key));
    }
    aus
}

fn adts_frames(d: &[u8]) -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    let mut i = 0;
    while i + 7 <= d.len() {
        let len = (usize::from(d[i + 3] & 3) << 11)
            | (usize::from(d[i + 4]) << 3)
            | usize::from(d[i + 5] >> 5);
        v.push(d[i..i + len].to_vec());
        i += len;
    }
    v
}

struct Sample {
    name: &'static str,
    codec: VideoCodec,
    file: &'static str,
    /// Times the fixture is repeated (timestamps continue).
    repeat: usize,
    audio: Option<AudioLayout>,
}

fn make(s: &Sample) -> Vec<Segment> {
    let dir = fixtures();
    let aus = access_units(&std::fs::read(dir.join(s.file)).expect("fixture"), s.codec);
    let tone = adts_frames(&std::fs::read(dir.join("aac-tone.aac")).expect("fixture"));
    let opts = FragmentOptions {
        audio: s.audio.map(|_| parse_adts(&tone[0]).expect("adts").0),
        audio_layout: s.audio.unwrap_or_default(),
        ..FragmentOptions::default()
    };
    let mut f = Fragmenter::new(s.codec, opts);
    let mut out = Vec::new();
    let t0 = 1_000_000i64;
    let (mut n, mut ai) = (0i64, 0i64);
    for _ in 0..s.repeat {
        for (data, key) in &aus {
            let t = n * STEP_US;
            out.extend(
                f.push_video(VideoFrame::new(data, *key, t0 + t))
                    .expect("video"),
            );
            n += 1;
            while s.audio.is_some() && ai * AUDIO_US <= t {
                let frame = &tone[ai as usize % tone.len()];
                f.push_audio_at(frame, t0 + ai * AUDIO_US).expect("audio");
                ai += 1;
            }
        }
    }
    out.extend(f.finish().expect("finish"));
    out
}

fn json_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn main() {
    let out_dir = std::env::args().nth(1).map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mse/out"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&out_dir).expect("create output directory");
    let samples = [
        Sample {
            name: "h264-video-audio-muxed",
            codec: VideoCodec::H264,
            file: "h264-basic.h264",
            repeat: 4,
            audio: Some(AudioLayout::Muxed),
        },
        Sample {
            name: "h264-video-audio-separate",
            codec: VideoCodec::H264,
            file: "h264-basic.h264",
            repeat: 4,
            audio: Some(AudioLayout::Separate),
        },
        Sample {
            name: "h264-video-only",
            codec: VideoCodec::H264,
            file: "h264-basic.h264",
            repeat: 4,
            audio: None,
        },
        Sample {
            name: "h264-resolution-change",
            codec: VideoCodec::H264,
            file: "h264-resize.h264",
            repeat: 2,
            audio: None,
        },
        Sample {
            name: "h265-resolution-change",
            codec: VideoCodec::H265,
            file: "h265-resize.h265",
            repeat: 2,
            audio: None,
        },
        Sample {
            name: "h265-video-audio-muxed",
            codec: VideoCodec::H265,
            file: "h265-basic.h265",
            repeat: 4,
            audio: Some(AudioLayout::Muxed),
        },
    ];
    let mut index = Vec::new();
    for s in &samples {
        let segs = make(s);
        let mut bin = Vec::new();
        let mut json = String::from("[\n");
        // MIME of the most recent init per track, so media records can name
        // the buffer they feed.
        for (i, seg) in segs.iter().enumerate() {
            let track = match seg.track {
                SegmentTrack::Muxed => "muxed",
                SegmentTrack::Video => "video",
                SegmentTrack::Audio => "audio",
            };
            let kind = if seg.kind == SegmentKind::Init {
                "init"
            } else {
                "media"
            };
            let mime = seg
                .init
                .as_ref()
                .map_or_else(|| "null".to_string(), |x| json_str(&x.mime));
            let reset = seg.init.as_ref().is_some_and(|x| x.reset);
            let sep = if i + 1 == segs.len() { "" } else { "," };
            writeln!(
                json,
                "  {{\"kind\":\"{kind}\",\"track\":\"{track}\",\"generation\":{},\"mime\":{mime},\"reset\":{reset},\"offset\":{},\"length\":{},\"start_us\":{},\"duration_us\":{},\"keyframe\":{}}}{sep}",
                seg.generation,
                bin.len(),
                seg.data.len(),
                seg.start_us,
                seg.duration_us,
                seg.starts_with_keyframe
            )
            .expect("string");
            bin.extend_from_slice(&seg.data);
        }
        json.push_str("]\n");
        std::fs::write(out_dir.join(format!("{}.bin", s.name)), &bin).expect("write bin");
        std::fs::write(out_dir.join(format!("{}.json", s.name)), json).expect("write json");
        println!("{}: {} segments, {} bytes", s.name, segs.len(), bin.len());
        index.push(json_str(s.name));
    }
    std::fs::write(
        out_dir.join("index.json"),
        format!("[{}]\n", index.join(",")),
    )
    .expect("write index");
    println!("wrote {}", out_dir.display());
}
