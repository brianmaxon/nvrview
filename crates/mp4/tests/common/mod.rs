#![allow(dead_code)]
//! Shared test helpers: fixture loading, a tiny MP4 box parser, and ffprobe.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use mp4_remux::{Mp4Writer, Options, VideoCodec, VideoFrame};

pub fn fixture(name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[derive(Clone)]
pub struct Au {
    pub data: Vec<u8>,
    pub key: bool,
}

pub fn nals(d: &[u8]) -> Vec<(usize, usize)> {
    // (start, end) of each NAL payload, start codes excluded.
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
    let mut v = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(d.len(), |n| n - 3);
        while e > s && d[e - 1] == 0 {
            e -= 1;
        }
        v.push((s, e));
    }
    v
}

/// Splits an elementary stream into access units. A new unit starts at a
/// non-VCL NAL after a VCL, or at a VCL whose first-slice flag is set.
pub fn split_aus(d: &[u8], codec: VideoCodec) -> Vec<Au> {
    let ns = nals(d);
    let mut aus: Vec<Au> = Vec::new();
    let mut cur_start: Option<usize> = None;
    let mut has_vcl = false;
    let mut key = false;
    let mut last_end = 0;
    for (s, e) in ns {
        let (is_vcl, first, is_key) = match codec {
            VideoCodec::H264 => {
                let t = d[s] & 0x1f;
                ((1..=5).contains(&t), d[s + 1] & 0x80 != 0, t == 5)
            }
            VideoCodec::H265 => {
                let t = (d[s] >> 1) & 0x3f;
                (t < 32, d[s + 2] & 0x80 != 0, (16..=21).contains(&t))
            }
        };
        let boundary = (!is_vcl && has_vcl) || (is_vcl && first && has_vcl);
        if boundary {
            let st = cur_start.take().unwrap();
            aus.push(Au {
                data: d[st..last_end].to_vec(),
                key,
            });
            has_vcl = false;
            key = false;
        }
        if cur_start.is_none() {
            cur_start = Some(s - 3);
        }
        if is_vcl {
            has_vcl = true;
            key |= is_key;
        }
        last_end = e;
    }
    if let Some(st) = cur_start {
        aus.push(Au {
            data: d[st..last_end].to_vec(),
            key,
        });
    }
    aus
}

/// ADTS frames of a raw .aac file.
pub fn adts_frames(d: &[u8]) -> Vec<Vec<u8>> {
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

/// The payload NAL units our writer should have stored for an access unit:
/// 4-byte length prefixed, without AUD or parameter sets.
pub fn expected_sample(au: &[u8], codec: VideoCodec) -> Vec<u8> {
    let mut out = Vec::new();
    for (s, e) in nals(au) {
        let drop = match codec {
            VideoCodec::H264 => matches!(au[s] & 0x1f, 7..=12),
            VideoCodec::H265 => matches!((au[s] >> 1) & 0x3f, 32..=38),
        };
        if !drop {
            out.extend_from_slice(&((e - s) as u32).to_be_bytes());
            out.extend_from_slice(&au[s..e]);
        }
    }
    out
}

pub fn mux(
    codec: VideoCodec,
    aus: &[Au],
    step_us: i64,
    audio: &[Vec<u8>],
    opts: Options,
) -> (Vec<u8>, mp4_remux::Summary) {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), codec, opts).unwrap();
    // Interleave audio by time, roughly as a device would.
    let mut ai = 0;
    for (i, au) in aus.iter().enumerate() {
        let t = i as i64 * step_us;
        w.write_video(VideoFrame::new(&au.data, au.key, 1_000_000 + t))
            .unwrap();
        while ai < audio.len() && (ai as i64 * 64_000) <= t {
            w.write_audio(&audio[ai]).unwrap();
            ai += 1;
        }
    }
    while ai < audio.len() {
        w.write_audio(&audio[ai]).unwrap();
        ai += 1;
    }
    let (c, s) = w.finish().unwrap();
    (c.into_inner(), s)
}

// ---- box parser -----------------------------------------------------------

#[derive(Clone, Debug)]
pub struct BoxRef {
    pub ty: String,
    pub start: usize,
    pub end: usize,
    pub header: usize,
}

const CONTAINERS: [&str; 11] = [
    "moov", "trak", "mdia", "minf", "stbl", "edts", "dinf", "moof", "traf", "mvex", "mfra",
];

pub fn boxes_in(d: &[u8], from: usize, to: usize) -> Vec<BoxRef> {
    let mut v = Vec::new();
    let mut i = from;
    while i + 8 <= to {
        let size = u32::from_be_bytes(d[i..i + 4].try_into().unwrap()) as u64;
        let ty = String::from_utf8_lossy(&d[i + 4..i + 8]).to_string();
        let (header, size) = if size == 1 {
            (16, u64::from_be_bytes(d[i + 8..i + 16].try_into().unwrap()))
        } else {
            (8, size)
        };
        assert!(
            size >= header as u64 && i as u64 + size <= to as u64,
            "bad box {ty} at {i}"
        );
        v.push(BoxRef {
            ty,
            start: i,
            end: i + size as usize,
            header,
        });
        i += size as usize;
    }
    assert_eq!(i, to, "trailing bytes in box list");
    v
}

pub fn child(d: &[u8], parent: &BoxRef, ty: &str) -> Option<BoxRef> {
    assert!(CONTAINERS.contains(&parent.ty.as_str()));
    boxes_in(d, parent.start + parent.header, parent.end)
        .into_iter()
        .find(|b| b.ty == ty)
}

pub fn top(d: &[u8]) -> Vec<BoxRef> {
    boxes_in(d, 0, d.len())
}

pub fn top_box(d: &[u8], ty: &str) -> BoxRef {
    top(d)
        .into_iter()
        .find(|b| b.ty == ty)
        .unwrap_or_else(|| panic!("no {ty}"))
}

fn u32at(d: &[u8], i: usize) -> u32 {
    u32::from_be_bytes(d[i..i + 4].try_into().unwrap())
}
fn u64at(d: &[u8], i: usize) -> u64 {
    u64::from_be_bytes(d[i..i + 8].try_into().unwrap())
}

#[derive(Debug, Default)]
pub struct TrackTables {
    pub handler: String,
    pub timescale: u32,
    pub media_duration: u64,
    pub stts: Vec<(u32, u32)>,
    pub ctts: Vec<(u32, u32)>,
    pub stsz: Vec<u32>,
    pub stsc: Vec<(u32, u32, u32)>,
    pub chunk_offsets: Vec<u64>,
    pub co64: bool,
    pub sync: Option<Vec<u32>>,
    pub edits: Vec<(u64, i64)>,
    pub stsd_entries: Vec<String>,
    pub stsd: Option<BoxRef>,
}

impl TrackTables {
    /// Absolute offset and size of every sample, following stsc/stco.
    pub fn sample_locations(&self) -> Vec<(u64, u32)> {
        let mut out = Vec::new();
        let mut idx = 0;
        for (c, &off) in self.chunk_offsets.iter().enumerate() {
            let chunk_no = c as u32 + 1;
            let run = self.stsc.iter().rev().find(|r| r.0 <= chunk_no).unwrap();
            let mut o = off;
            for _ in 0..run.1 {
                let sz = self.stsz[idx];
                out.push((o, sz));
                o += u64::from(sz);
                idx += 1;
            }
        }
        assert_eq!(idx, self.stsz.len(), "stsc/stco do not cover all samples");
        out
    }

    /// Decode time of every sample in media ticks.
    pub fn dts(&self) -> Vec<u64> {
        let mut t = 0u64;
        let mut v = Vec::new();
        for &(c, d) in &self.stts {
            for _ in 0..c {
                v.push(t);
                t += u64::from(d);
            }
        }
        v
    }
}

pub fn tracks(d: &[u8]) -> Vec<TrackTables> {
    let moov = top_box(d, "moov");
    boxes_in(d, moov.start + moov.header, moov.end)
        .into_iter()
        .filter(|b| b.ty == "trak")
        .map(|trak| {
            let mut t = TrackTables::default();
            if let Some(edts) = child(d, &trak, "edts") {
                let elst = child(d, &edts, "elst").unwrap();
                let p = elst.start + 8;
                let v = d[p];
                let n = u32at(d, p + 4) as usize;
                let mut q = p + 8;
                for _ in 0..n {
                    if v == 1 {
                        t.edits.push((u64at(d, q), u64at(d, q + 8) as i64));
                        q += 20;
                    } else {
                        t.edits
                            .push((u64::from(u32at(d, q)), i64::from(u32at(d, q + 4) as i32)));
                        q += 12;
                    }
                }
            }
            let mdia = child(d, &trak, "mdia").unwrap();
            let mdhd = child(d, &mdia, "mdhd").unwrap();
            let p = mdhd.start + 8;
            if d[p] == 1 {
                t.timescale = u32at(d, p + 20);
                t.media_duration = u64at(d, p + 24);
            } else {
                t.timescale = u32at(d, p + 12);
                t.media_duration = u64::from(u32at(d, p + 16));
            }
            let hdlr = child(d, &mdia, "hdlr").unwrap();
            t.handler = String::from_utf8_lossy(&d[hdlr.start + 16..hdlr.start + 20]).to_string();
            let minf = child(d, &mdia, "minf").unwrap();
            let stbl = child(d, &minf, "stbl").unwrap();
            for b in boxes_in(d, stbl.start + stbl.header, stbl.end) {
                let p = b.start + 8; // version/flags
                let n = u32at(d, p + 4) as usize;
                let body = p + 8;
                match b.ty.as_str() {
                    "stsd" => {
                        for e in boxes_in(d, body, b.end) {
                            t.stsd_entries.push(e.ty);
                        }
                        t.stsd = Some(b.clone());
                    }
                    "stts" => {
                        t.stts = (0..n)
                            .map(|i| (u32at(d, body + 8 * i), u32at(d, body + 8 * i + 4)))
                            .collect()
                    }
                    "ctts" => {
                        t.ctts = (0..n)
                            .map(|i| (u32at(d, body + 8 * i), u32at(d, body + 8 * i + 4)))
                            .collect()
                    }
                    "stsc" => {
                        t.stsc = (0..n)
                            .map(|i| {
                                (
                                    u32at(d, body + 12 * i),
                                    u32at(d, body + 12 * i + 4),
                                    u32at(d, body + 12 * i + 8),
                                )
                            })
                            .collect()
                    }
                    "stsz" => {
                        assert_eq!(u32at(d, p + 4), 0, "constant sample size not expected");
                        let count = u32at(d, p + 8) as usize;
                        t.stsz = (0..count).map(|i| u32at(d, p + 12 + 4 * i)).collect()
                    }
                    "stss" => t.sync = Some((0..n).map(|i| u32at(d, body + 4 * i)).collect()),
                    "stco" => {
                        t.chunk_offsets =
                            (0..n).map(|i| u64::from(u32at(d, body + 4 * i))).collect()
                    }
                    "co64" => {
                        t.co64 = true;
                        t.chunk_offsets = (0..n).map(|i| u64at(d, body + 8 * i)).collect()
                    }
                    _ => {}
                }
            }
            t
        })
        .collect()
}

/// Concatenated sample bytes of one track.
pub fn sample_bytes(d: &[u8], t: &TrackTables) -> Vec<Vec<u8>> {
    t.sample_locations()
        .into_iter()
        .map(|(o, s)| d[o as usize..(o + u64::from(s)) as usize].to_vec())
        .collect()
}

// ---- ffmpeg / ffprobe -----------------------------------------------------

pub fn have_ffmpeg() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|t| {
        Command::new(t)
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

pub fn temp_file(name: &str, data: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mp4-remux-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

/// `ffprobe` key=value lines for the first stream of `kind` ("v" or "a").
pub fn probe_stream(path: &Path, kind: &str) -> HashMap<String, String> {
    let o = Command::new("ffprobe")
        .args(["-v", "error", "-count_packets", "-select_streams", &format!("{kind}:0"), "-show_entries"])
        .arg("stream=codec_name,width,height,nb_read_packets,start_time,duration,sample_rate,channels")
        .args(["-of", "default=nw=1"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            l.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// (pts_time seconds, is_key) per video packet in file order.
pub fn probe_packets(path: &Path, kind: &str) -> Vec<(f64, bool)> {
    let o = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            &format!("{kind}:0"),
            "-show_entries",
            "packet=pts_time,flags",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(o.status.success());
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| {
            let (t, f) = l.split_once(',').unwrap();
            (t.parse().unwrap(), f.contains('K'))
        })
        .collect()
}

/// Decodes every stream; returns ffmpeg's stderr (empty means clean).
pub fn decode_errors(path: &Path) -> String {
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-xerror", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    let mut s = String::from_utf8_lossy(&o.stderr).to_string();
    if !o.status.success() {
        s.push_str("(nonzero exit)");
    }
    s
}

pub fn f(m: &HashMap<String, String>, k: &str) -> f64 {
    m.get(k)
        .unwrap_or_else(|| panic!("missing {k} in {m:?}"))
        .parse()
        .unwrap()
}

/// Builds an ADTS frame (AAC-LC, 16 kHz, mono) around an arbitrary payload.
pub fn adts_frame(payload: &[u8]) -> Vec<u8> {
    let len = payload.len() + 7;
    let mut v = vec![
        0xff,
        0xf1,
        (1 << 6) | (8 << 2),
        (1 << 6) | ((len >> 11) as u8 & 3),
        (len >> 3) as u8,
        (((len & 7) as u8) << 5) | 0x1f,
        0xfc,
    ];
    v.extend_from_slice(payload);
    v
}

/// A synthetic non-IDR slice NAL unit (Annex-B, H.264) with the given payload.
pub fn fake_p_frame(payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0, 0, 0, 1, 0x41, 0x9a];
    v.extend(payload.iter().map(|b| b | 1)); // never 0, so no start codes appear
    v
}

/// A write/read/seek sink that stores only pages containing non-zero bytes,
/// so multi-GiB files of zero padding cost almost no memory.
pub struct SparseFile {
    pages: std::collections::BTreeMap<u64, Box<[u8; 4096]>>,
    pos: u64,
    len: u64,
}

impl SparseFile {
    pub fn new() -> Self {
        Self {
            pages: Default::default(),
            pos: 0,
            len: 0,
        }
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn read_at(&self, at: u64, n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        self.fill(at, &mut out);
        out
    }

    /// Copies the stored pages over `out` (zeroed), which starts at `at`.
    /// Visits only the pages that exist, so reading gigabytes of zeros
    /// costs a memset.
    fn fill(&self, at: u64, out: &mut [u8]) {
        if out.is_empty() {
            return;
        }
        let end = at + out.len() as u64;
        for (&idx, pg) in self.pages.range(at / 4096..end.div_ceil(4096)) {
            let page_at = idx * 4096;
            let from = at.max(page_at);
            let to = end.min(page_at + 4096);
            out[(from - at) as usize..(to - at) as usize]
                .copy_from_slice(&pg[(from - page_at) as usize..(to - page_at) as usize]);
        }
    }
}

impl std::io::Write for SparseFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut off = 0;
        while off < buf.len() {
            let p = self.pos + off as u64;
            let in_page = (p % 4096) as usize;
            let n = (4096 - in_page).min(buf.len() - off);
            let src = &buf[off..off + n];
            let idx = p / 4096;
            if let Some(pg) = self.pages.get_mut(&idx) {
                pg[in_page..in_page + n].copy_from_slice(src);
            } else if src.iter().fold(0, |acc, &b| acc | b) != 0 {
                // (A fold without an early exit runs vectorized.)
                let mut pg = Box::new([0u8; 4096]);
                pg[in_page..in_page + n].copy_from_slice(src);
                self.pages.insert(idx, pg);
            }
            off += n;
        }
        self.pos += buf.len() as u64;
        self.len = self.len.max(self.pos);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Read for SparseFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (self.len.saturating_sub(self.pos)).min(buf.len() as u64) as usize;
        buf[..n].fill(0);
        self.fill(self.pos, &mut buf[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl std::io::Seek for SparseFile {
    fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
        self.pos = match from {
            std::io::SeekFrom::Start(p) => p,
            std::io::SeekFrom::End(d) => (self.len as i64 + d) as u64,
            std::io::SeekFrom::Current(d) => (self.pos as i64 + d) as u64,
        };
        Ok(self.pos)
    }
}
