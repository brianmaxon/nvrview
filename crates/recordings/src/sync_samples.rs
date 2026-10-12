//! Keyframe times read from a finished MP4 (its sync samples).
//!
//! The edit planners ([`crate::edl`]) cut only where a source has a
//! keyframe, so they need the keyframes' times in each file. This reads
//! them from the sample tables of the file [`crate::finalize`] writes:
//! `mdhd` (timescale), `stts` (decode times), `stss` (sync samples), and,
//! when present, `ctts` (composition offsets) and the first `elst` entry
//! (the media time where the presentation starts). A table without `stss`
//! means every sample is a sync sample, as the format defines it.
//!
//! The reader is bounded: only `moov` is loaded (at most [`MAX_MOOV`]
//! bytes), the number of boxes it walks is capped, every table is checked
//! against the bytes that hold it before it is read, and the number of
//! keyframes is capped at [`MAX_KEYFRAMES`]. Malformed input returns an
//! [`Error`]; nothing here panics. A fragmented file (no sample tables in
//! `moov`) is not supported: run [`crate::finalize::recover_fragmented`]
//! first.

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// Largest `moov` box read, in bytes.
pub const MAX_MOOV: u64 = 64 * 1024 * 1024;
/// Most keyframes returned.
pub const MAX_KEYFRAMES: usize = 1 << 20;
/// Most boxes walked at the top level of the file or in one container.
const MAX_BOXES: usize = 4096;

/// Why a file's keyframes could not be read.
#[derive(Debug)]
pub enum Error {
    /// The file could not be read.
    Io(io::Error),
    /// The file is not a well-formed MP4 (what was wrong).
    Malformed(&'static str),
    /// A size or count is beyond the reader's bounds.
    TooLarge(&'static str),
    /// No video track, or no sample table with samples in it.
    NoVideo,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "cannot read the MP4: {e}"),
            Error::Malformed(m) => write!(f, "malformed MP4: {m}"),
            Error::TooLarge(m) => write!(f, "MP4 beyond the reader's limits: {m}"),
            Error::NoVideo => write!(f, "the MP4 has no video samples in its sample tables"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

/// The keyframes of a file's video track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncSamples {
    /// Media timescale (ticks per second) of the track.
    pub timescale: u32,
    /// Length of the video track in milliseconds (the sum of its sample
    /// durations).
    pub duration_ms: u64,
    /// Presentation time of each keyframe in milliseconds from the start
    /// of the file's picture (ascending, no duplicates).
    pub keyframes_ms: Vec<u64>,
    /// Number of video samples.
    pub samples: u64,
    /// The first sample entry's codec and picture size (`stsd`), when the
    /// file has a sample description this reader knows.
    pub video: Option<VideoInfo>,
}

/// The codec and picture size of a file's video track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoInfo {
    /// `h264` or `h265`.
    pub codec: &'static str,
    /// Picture width.
    pub width: u32,
    /// Picture height.
    pub height: u32,
}

/// Reads the first video sample entry of an `stsd` box: its type names the
/// codec, and its visual fields hold the size.
fn video_info(stsd: &[u8]) -> Option<VideoInfo> {
    // Full-box header (4), entry count (4), then the first entry: size (4),
    // type (4), 6 reserved + data reference index (8), 16 bytes of
    // predefined and reserved, width (2), height (2).
    let entry = stsd.get(8..)?;
    let codec = match entry.get(4..8)? {
        b"avc1" | b"avc3" => "h264",
        b"hvc1" | b"hev1" => "h265",
        _ => return None,
    };
    let at = |o: usize| entry.get(o..o + 2).map(|b| u32::from(b[0]) << 8 | u32::from(b[1]));
    Some(VideoInfo {
        codec,
        width: at(32)?,
        height: at(34)?,
    })
}

/// Reads the keyframes of the MP4 at `path`.
pub fn read_sync_samples(path: &Path) -> Result<SyncSamples, Error> {
    read_sync_samples_from(File::open(path)?)
}

/// [`read_sync_samples`] over any seekable source.
pub fn read_sync_samples_from<R: Read + Seek>(mut r: R) -> Result<SyncSamples, Error> {
    let len = r.seek(SeekFrom::End(0))?;
    let mut pos = 0u64;
    for _ in 0..MAX_BOXES {
        if pos + 8 > len {
            break;
        }
        r.seek(SeekFrom::Start(pos))?;
        let mut h = [0u8; 16];
        r.read_exact(&mut h[..8])?;
        let mut size = u64::from(be32(&h, 0));
        let mut header = 8u64;
        if size == 1 {
            r.read_exact(&mut h[8..16])?;
            size = be64(&h, 8);
            header = 16;
        } else if size == 0 {
            size = len - pos;
        }
        if size < header || size > len - pos {
            return Err(Error::Malformed("box size outside the file"));
        }
        if &h[4..8] == b"moov" {
            let body = size - header;
            if body > MAX_MOOV {
                return Err(Error::TooLarge("moov"));
            }
            let mut buf = vec![0u8; body as usize];
            r.read_exact(&mut buf)?;
            return parse_moov(&buf);
        }
        pos += size;
    }
    Err(Error::Malformed("no moov box"))
}

fn be32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u32::from_be_bytes)
}

fn be64(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u64::from_be_bytes)
}

type Boxes<'a> = Vec<([u8; 4], &'a [u8])>;

/// The child boxes of a container's payload, as (type, payload).
fn children(data: &[u8]) -> Result<Boxes<'_>, Error> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        if out.len() >= MAX_BOXES {
            return Err(Error::TooLarge("boxes in one container"));
        }
        let mut size = u64::from(be32(data, pos));
        let typ: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap_or([0; 4]);
        let mut header = 8usize;
        if size == 1 {
            if pos + 16 > data.len() {
                return Err(Error::Malformed("truncated box header"));
            }
            size = be64(data, pos + 8);
            header = 16;
        } else if size == 0 {
            size = (data.len() - pos) as u64;
        }
        if size < header as u64 || size > (data.len() - pos) as u64 {
            return Err(Error::Malformed("box size outside its container"));
        }
        out.push((typ, &data[pos + header..pos + size as usize]));
        pos += size as usize;
    }
    Ok(out)
}

fn find<'a>(list: &[([u8; 4], &'a [u8])], typ: &[u8; 4]) -> Option<&'a [u8]> {
    list.iter().find(|(t, _)| t == typ).map(|(_, p)| *p)
}

/// A table of `entry_size`-byte entries after a full-box header and a
/// count; the count is checked against the bytes present.
fn table<'a>(p: &'a [u8], entry_size: usize, what: &'static str) -> Result<&'a [u8], Error> {
    if p.len() < 8 {
        return Err(Error::Malformed(what));
    }
    let n = be32(p, 4) as usize;
    let need = n.checked_mul(entry_size).ok_or(Error::Malformed(what))?;
    p.get(8..8usize.saturating_add(need))
        .ok_or(Error::Malformed(what))
}

fn parse_moov(moov: &[u8]) -> Result<SyncSamples, Error> {
    for (typ, trak) in &children(moov)? {
        if typ != b"trak" {
            continue;
        }
        let t = children(trak)?;
        let Some(mdia) = find(&t, b"mdia") else {
            continue;
        };
        let m = children(mdia)?;
        let is_video = find(&m, b"hdlr").is_some_and(|h| h.len() >= 12 && &h[8..12] == b"vide");
        if !is_video {
            continue;
        }
        let mdhd = find(&m, b"mdhd").ok_or(Error::Malformed("no mdhd"))?;
        let timescale = match mdhd.first() {
            Some(0) if mdhd.len() >= 20 => be32(mdhd, 12),
            Some(1) if mdhd.len() >= 32 => be32(mdhd, 20),
            _ => return Err(Error::Malformed("mdhd")),
        };
        if timescale == 0 {
            return Err(Error::Malformed("zero timescale"));
        }
        let minf = children(find(&m, b"minf").ok_or(Error::Malformed("no minf"))?)?;
        let stbl = children(find(&minf, b"stbl").ok_or(Error::Malformed("no stbl"))?)?;
        let elst = match find(&t, b"edts") {
            Some(e) => find(&children(e)?, b"elst"),
            None => None,
        };
        return sample_tables(timescale, &stbl, elst);
    }
    Err(Error::NoVideo)
}

/// The media time at which the presentation starts: the first non-empty
/// edit's media time (an empty edit, `-1`, only delays the start and is
/// ignored).
fn first_media_time(elst: &[u8]) -> Result<u64, Error> {
    let v1 = elst.first() == Some(&1);
    let entry = if v1 { 20 } else { 12 };
    for e in table(elst, entry, "elst")?.chunks_exact(entry) {
        let mt = if v1 {
            be64(e, 8) as i64
        } else {
            i64::from(be32(e, 4) as i32)
        };
        if mt >= 0 {
            return Ok(mt as u64);
        }
    }
    Ok(0)
}

fn sample_tables(
    timescale: u32,
    stbl: &[([u8; 4], &[u8])],
    elst: Option<&[u8]>,
) -> Result<SyncSamples, Error> {
    let stts = table(
        find(stbl, b"stts").ok_or(Error::Malformed("no stts"))?,
        8,
        "stts",
    )?;
    let runs: Vec<(u64, u64)> = stts
        .as_chunks::<8>()
        .0
        .iter()
        .map(|e| (u64::from(be32(e, 0)), u64::from(be32(e, 4))))
        .collect();
    let samples: u64 = runs.iter().map(|r| r.0).sum();
    if samples == 0 {
        return Err(Error::NoVideo);
    }
    let total_ticks: u64 = runs
        .iter()
        .try_fold(0u64, |a, r| a.checked_add(r.0.checked_mul(r.1)?))
        .ok_or(Error::Malformed("stts duration overflows"))?;
    let skip = elst.map(first_media_time).transpose()?.unwrap_or(0);
    let ms = |ticks: i128| -> u64 {
        (ticks.max(0) * 1000 / i128::from(timescale)).min(i128::from(u64::MAX)) as u64
    };

    // Which samples are sync samples (1-based, ascending).
    let sync: Vec<u64> = match find(stbl, b"stss") {
        Some(p) => {
            let t = table(p, 4, "stss")?;
            if t.len() / 4 > MAX_KEYFRAMES {
                return Err(Error::TooLarge("keyframes"));
            }
            let v: Vec<u64> = t
                .as_chunks::<4>()
                .0
                .iter()
                .map(|e| u64::from(be32(e, 0)))
                .collect();
            if v.windows(2).any(|w| w[0] >= w[1]) || v.first() == Some(&0) {
                return Err(Error::Malformed("stss is not ascending"));
            }
            if v.last().is_some_and(|&l| l > samples) {
                return Err(Error::Malformed("stss beyond the samples"));
            }
            v
        }
        None => {
            if samples > MAX_KEYFRAMES as u64 {
                return Err(Error::TooLarge("samples without stss"));
            }
            (1..=samples).collect()
        }
    };

    // Composition offsets per run, when present.
    let ctts_runs: Vec<(u64, i64)> = match find(stbl, b"ctts") {
        Some(p) => {
            let signed = p.first().is_some_and(|&v| v >= 1);
            table(p, 8, "ctts")?
                .as_chunks::<8>()
                .0
                .iter()
                .map(|e| {
                    let off = if signed {
                        i64::from(be32(e, 4) as i32)
                    } else {
                        i64::from(be32(e, 4))
                    };
                    (u64::from(be32(e, 0)), off)
                })
                .collect()
        }
        None => Vec::new(),
    };

    // Walk both run lists once, in step with the ascending sync samples.
    let mut keyframes_ms = Vec::with_capacity(sync.len());
    let (mut run, mut run_first, mut dts) = (0usize, 1u64, 0u128);
    let (mut crun, mut crun_first) = (0usize, 1u64);
    for s in sync {
        while run < runs.len() && s >= run_first + runs[run].0 {
            dts += u128::from(runs[run].0) * u128::from(runs[run].1);
            run_first += runs[run].0;
            run += 1;
        }
        let Some(&(_, delta)) = runs.get(run) else {
            return Err(Error::Malformed("sync sample beyond stts"));
        };
        let at = dts + u128::from(s - run_first) * u128::from(delta);
        while crun < ctts_runs.len() && s >= crun_first + ctts_runs[crun].0 {
            crun_first += ctts_runs[crun].0;
            crun += 1;
        }
        let comp = ctts_runs.get(crun).map_or(0, |r| r.1);
        keyframes_ms.push(ms(at as i128 + i128::from(comp) - i128::from(skip)));
    }
    keyframes_ms.sort_unstable();
    keyframes_ms.dedup();
    Ok(SyncSamples {
        timescale,
        duration_ms: ms(i128::from(total_ticks)),
        keyframes_ms,
        samples,
        video: find(stbl, b"stsd").and_then(video_info),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(payload);
        v
    }

    fn full(typ: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
        let mut p = vec![version, 0, 0, 0];
        p.extend_from_slice(body);
        bx(typ, &p)
    }

    fn u32s(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    struct Spec {
        timescale: u32,
        stts: Vec<(u32, u32)>,
        stss: Option<Vec<u32>>,
        ctts: Option<Vec<(u32, u32)>>,
        elst_media_time: Option<i32>,
    }

    fn mp4(s: &Spec) -> Vec<u8> {
        let mut mdhd = u32s(&[0, 0, s.timescale, 0]);
        mdhd.extend_from_slice(&[0x55, 0xc4, 0, 0]);
        let mut hdlr = u32s(&[0]);
        hdlr.extend_from_slice(b"vide");
        hdlr.extend_from_slice(&[0; 13]);
        let pairs = |v: &[(u32, u32)]| {
            let mut b = u32s(&[v.len() as u32]);
            for &(a, c) in v {
                b.extend(u32s(&[a, c]));
            }
            b
        };
        let mut stbl = full(b"stts", 0, &pairs(&s.stts));
        if let Some(ss) = &s.stss {
            let mut b = u32s(&[ss.len() as u32]);
            b.extend(u32s(ss));
            stbl.extend(full(b"stss", 0, &b));
        }
        if let Some(c) = &s.ctts {
            stbl.extend(full(b"ctts", 0, &pairs(c)));
        }
        let minf = bx(b"minf", &bx(b"stbl", &stbl));
        let mut mdia = full(b"mdhd", 0, &mdhd);
        mdia.extend(full(b"hdlr", 0, &hdlr));
        mdia.extend(minf);
        let mut trak = Vec::new();
        if let Some(mt) = s.elst_media_time {
            let mut b = u32s(&[1, 0, mt as u32]);
            b.extend_from_slice(&[0, 1, 0, 0]);
            trak.extend(bx(b"edts", &full(b"elst", 0, &b)));
        }
        trak.extend(bx(b"mdia", &mdia));
        let mut f = bx(b"ftyp", b"isom\0\0\0\0isom");
        f.extend(bx(b"mdat", &[0; 16]));
        f.extend(bx(b"moov", &bx(b"trak", &trak)));
        f
    }

    fn read(bytes: Vec<u8>) -> Result<SyncSamples, Error> {
        read_sync_samples_from(Cursor::new(bytes))
    }

    fn base() -> Spec {
        Spec {
            timescale: 90_000,
            // 25 fps for 100 samples.
            stts: vec![(100, 3600)],
            stss: Some(vec![1, 51]),
            ctts: None,
            elst_media_time: None,
        }
    }

    #[test]
    fn reads_sync_sample_times() {
        let s = read(mp4(&base())).unwrap();
        assert_eq!(s.keyframes_ms, vec![0, 2000]);
        assert_eq!(s.duration_ms, 4000);
        assert_eq!(s.samples, 100);
        assert_eq!(s.timescale, 90_000);
    }

    #[test]
    fn runs_with_different_durations() {
        let mut spec = base();
        spec.stts = vec![(10, 9000), (10, 4500)];
        spec.stss = Some(vec![1, 11, 15]);
        let s = read(mp4(&spec)).unwrap();
        // Sample 11 starts after ten 100 ms samples; 15 after four 50 ms more.
        assert_eq!(s.keyframes_ms, vec![0, 1000, 1200]);
        assert_eq!(s.duration_ms, 1500);
    }

    #[test]
    fn reads_codec_and_size_from_the_sample_description() {
        let mut entry = vec![0u8; 78];
        entry[4..8].copy_from_slice(b"hvc1");
        entry[32..34].copy_from_slice(&2560u16.to_be_bytes());
        entry[34..36].copy_from_slice(&1440u16.to_be_bytes());
        let mut stsd = u32s(&[0, 1]);
        stsd.extend(entry);
        let v = video_info(&stsd).unwrap();
        assert_eq!((v.codec, v.width, v.height), ("h265", 2560, 1440));
        stsd[12..16].copy_from_slice(b"mp4v");
        assert_eq!(video_info(&stsd), None);
        assert_eq!(video_info(&[0; 10]), None);
        assert_eq!(read(mp4(&base())).unwrap().video, None);
    }

    #[test]
    fn no_stss_means_every_sample() {
        let mut spec = base();
        spec.stts = vec![(3, 90_000)];
        spec.stss = None;
        assert_eq!(read(mp4(&spec)).unwrap().keyframes_ms, vec![0, 1000, 2000]);
    }

    #[test]
    fn composition_offsets_and_edit_list_shift_the_times() {
        let mut spec = base();
        spec.ctts = Some(vec![(100, 7200)]);
        spec.elst_media_time = Some(7200);
        // Presentation = decode + 80 ms offset - 80 ms edit start.
        assert_eq!(read(mp4(&spec)).unwrap().keyframes_ms, vec![0, 2000]);
        spec.elst_media_time = None;
        assert_eq!(read(mp4(&spec)).unwrap().keyframes_ms, vec![80, 2080]);
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        assert!(read(Vec::new()).is_err());
        assert!(read(b"not an mp4 at all".to_vec()).is_err());
        // Box larger than the file.
        let mut f = mp4(&base());
        f[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(read(f), Err(Error::Malformed(_))));
        // Box smaller than its header.
        let mut f = mp4(&base());
        f[0..4].copy_from_slice(&4u32.to_be_bytes());
        assert!(read(f).is_err());
        // Zero timescale.
        let mut spec = base();
        spec.timescale = 0;
        assert!(matches!(read(mp4(&spec)), Err(Error::Malformed(_))));
        // stss beyond the samples, not ascending, or with sample zero.
        for bad in [vec![1, 101], vec![5, 3], vec![0, 4]] {
            let mut spec = base();
            spec.stss = Some(bad);
            assert!(matches!(read(mp4(&spec)), Err(Error::Malformed(_))));
        }
        // No samples at all.
        let mut spec = base();
        spec.stts = vec![];
        spec.stss = Some(vec![]);
        assert!(matches!(read(mp4(&spec)), Err(Error::NoVideo)));
        // Every truncation of a good file fails or succeeds, never panics.
        let good = mp4(&base());
        for n in 0..good.len() {
            let _ = read(good[..n].to_vec());
        }
    }

    #[test]
    fn counts_larger_than_their_bytes_are_rejected() {
        let mut f = mp4(&base());
        let at = f.windows(4).position(|w| w == b"stss").unwrap() + 8;
        f[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(read(f), Err(Error::Malformed(_))));
        let mut f = mp4(&base());
        let at = f.windows(4).position(|w| w == b"stts").unwrap() + 8;
        f[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(read(f), Err(Error::Malformed(_))));
    }

    #[test]
    fn an_oversized_moov_is_refused_without_reading_it() {
        let mut f = bx(b"ftyp", b"isom\0\0\0\0isom");
        f.extend_from_slice(&1u32.to_be_bytes());
        f.extend_from_slice(b"moov");
        f.extend_from_slice(&(MAX_MOOV + 100).to_be_bytes());
        assert!(read(f).is_err());
    }

    #[test]
    fn no_video_track_and_no_moov() {
        let f = bx(b"ftyp", b"isom\0\0\0\0isom");
        assert!(matches!(read(f), Err(Error::Malformed(_))));
        let mut f = bx(b"ftyp", b"isom\0\0\0\0isom");
        f.extend(bx(b"moov", &[]));
        assert!(matches!(read(f), Err(Error::NoVideo)));
    }

    #[test]
    fn reads_back_a_file_written_by_the_finalize_step() {
        use mp4_remux::{FileOptions, FragmentedFile, VideoCodec, VideoFrame};
        use std::io::BufWriter;
        let fx = mock_nvr::fixture::Fixture::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264"),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let joined = dir.path().join("joined.fmp4");
        let out = dir.path().join("out.mp4");
        let mut w = FragmentedFile::new(
            BufWriter::new(File::create(&joined).unwrap()),
            VideoCodec::H264,
            FileOptions::default(),
        );
        let frames = &fx.frames()[fx.first_keyframe()..];
        let mut expected = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            let ts = i as i64 * 40_000;
            if f.keyframe {
                expected.push(ts as u64 / 1000);
            }
            w.push_video(VideoFrame::new(&f.data, f.keyframe, ts))
                .unwrap();
        }
        w.finish().unwrap();
        crate::finalize::recover_fragmented(&joined, &out, true).unwrap();
        let s = read_sync_samples(&out).unwrap();
        assert_eq!(s.samples, frames.len() as u64);
        assert!(!expected.is_empty());
        assert_eq!(s.keyframes_ms, expected);
        assert_eq!(s.duration_ms, frames.len() as u64 * 40);
        let v = s.video.expect("the finalized file names its codec");
        assert_eq!(v.codec, "h264");
        assert!(v.width > 0 && v.height > 0);
    }
}
