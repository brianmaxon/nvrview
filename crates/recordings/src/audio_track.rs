//! The AAC audio track of a finished MP4: where each frame is, so a caller
//! can read and decode them in order (sound analysis of downloaded days).
//!
//! Like [`crate::sync_samples`] this reads only `moov` (bounded by
//! [`crate::sync_samples::MAX_MOOV`]), checks every table against the bytes
//! that hold it, caps the number of frames at [`MAX_FRAMES`], and returns an
//! [`Error`] for malformed input instead of panicking. A fragmented file is
//! not supported (finalize it first). Only the first audio track is read,
//! and only AAC (the one codec the downloads carry).

use crate::sync_samples::{Error, be32, be64, children, find, read_moov, table};
use std::fs::File;
use std::path::Path;

/// Most audio frames returned (about 70 hours at 16 kHz).
pub const MAX_FRAMES: usize = 4_000_000;

/// Samples per channel in one AAC frame.
pub const SAMPLES_PER_FRAME: u32 = 1024;

const RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// The first audio track of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AacTrack {
    /// MPEG-4 audio object type (2 is AAC-LC).
    pub object_type: u8,
    /// Index into the standard sample-rate table.
    pub sample_rate_index: u8,
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Channel configuration (1 mono, 2 stereo).
    pub channels: u8,
    /// How long after the start of the file the track starts, in
    /// milliseconds (a leading empty edit).
    pub start_ms: u64,
    /// Each frame's byte offset in the file and its size, in order.
    pub frames: Vec<(u64, u32)>,
}

impl AacTrack {
    /// The audio's length in milliseconds, by sample count.
    pub fn duration_ms(&self) -> u64 {
        self.frames.len() as u64 * u64::from(SAMPLES_PER_FRAME) * 1000
            / u64::from(self.sample_rate.max(1))
    }

    /// Wraps a raw frame in a single-block ADTS header (no CRC), the form
    /// the audio decoder takes.
    pub fn adts_frame(&self, raw: &[u8]) -> Vec<u8> {
        let len = raw.len() + 7;
        let (ot, sf, ch) = (self.object_type, self.sample_rate_index, self.channels);
        let mut v = vec![
            0xff,
            0xf1,
            (ot.saturating_sub(1) << 6) | (sf << 2) | (ch >> 2),
            ((ch & 3) << 6) | ((len >> 11) as u8 & 3),
            (len >> 3) as u8,
            (((len & 7) as u8) << 5) | 0x1f,
            0xfc,
        ];
        v.extend_from_slice(raw);
        v
    }
}

/// Reads the audio track of the MP4 at `path`: `Ok(None)` when the file has
/// no AAC audio track.
pub fn read_aac_track(path: &Path) -> Result<Option<AacTrack>, Error> {
    let moov = read_moov(&mut File::open(path)?)?;
    parse_moov(&moov)
}

/// The ES descriptor's AudioSpecificConfig: the bytes after tag 5.
fn audio_specific_config(esds: &[u8]) -> Option<(u8, u8, u8)> {
    // Skip the full-box header, then walk descriptors: tag, length (1 to 4
    // bytes of 7 bits), body. Tags 3 (ES) and 4 (decoder config) contain
    // the others after a fixed prefix; scanning for tag 5 is enough.
    let mut i = 4;
    while i + 2 <= esds.len() {
        if esds[i] == 0x05 {
            let mut j = i + 1;
            let mut len = 0usize;
            for _ in 0..4 {
                let b = *esds.get(j)?;
                j += 1;
                len = len << 7 | usize::from(b & 0x7f);
                if b & 0x80 == 0 {
                    break;
                }
            }
            let asc = esds.get(j..j + len)?;
            if asc.len() < 2 {
                return None;
            }
            let v = u16::from_be_bytes([asc[0], asc[1]]);
            return Some((
                (v >> 11) as u8,
                ((v >> 7) & 0xf) as u8,
                ((v >> 3) & 0xf) as u8,
            ));
        }
        i += 1;
    }
    None
}

fn parse_moov(moov: &[u8]) -> Result<Option<AacTrack>, Error> {
    let top = children(moov)?;
    let movie_scale = match find(&top, b"mvhd") {
        Some(m) if m.first() == Some(&0) && m.len() >= 16 => be32(m, 12),
        Some(m) if m.first() == Some(&1) && m.len() >= 24 => be32(m, 20),
        _ => 1000,
    }
    .max(1);
    for (typ, trak) in &top {
        if typ != b"trak" {
            continue;
        }
        let t = children(trak)?;
        let Some(mdia) = find(&t, b"mdia") else {
            continue;
        };
        let m = children(mdia)?;
        if !find(&m, b"hdlr").is_some_and(|h| h.len() >= 12 && &h[8..12] == b"soun") {
            continue;
        }
        let minf = children(find(&m, b"minf").ok_or(Error::Malformed("no minf"))?)?;
        let stbl = children(find(&minf, b"stbl").ok_or(Error::Malformed("no stbl"))?)?;
        let stsd = find(&stbl, b"stsd").ok_or(Error::Malformed("no stsd"))?;
        let entries = children(stsd.get(8..).ok_or(Error::Malformed("short stsd"))?)?;
        let Some((ety, entry)) = entries.first() else {
            continue;
        };
        if ety != b"mp4a" {
            continue;
        }
        let kids = children(entry.get(28..).ok_or(Error::Malformed("short mp4a"))?)?;
        let esds = find(&kids, b"esds").ok_or(Error::Malformed("no esds"))?;
        let (object_type, sample_rate_index, channels) =
            audio_specific_config(esds).ok_or(Error::Malformed("no AudioSpecificConfig"))?;
        let Some(&sample_rate) = RATES.get(usize::from(sample_rate_index)) else {
            continue;
        };
        if object_type == 0 || channels == 0 || channels > 2 {
            continue;
        }
        let frames = frame_list(&stbl)?;
        let start_ms = match find(&t, b"edts") {
            Some(e) => match find(&children(e)?, b"elst") {
                Some(l) => leading_empty_edit(l, movie_scale)?,
                None => 0,
            },
            None => 0,
        };
        return Ok(Some(AacTrack {
            object_type,
            sample_rate_index,
            sample_rate,
            channels,
            start_ms,
            frames,
        }));
    }
    Ok(None)
}

/// The length of an edit list's leading empty edit (media time -1), in
/// milliseconds.
fn leading_empty_edit(elst: &[u8], movie_scale: u32) -> Result<u64, Error> {
    let v1 = elst.first() == Some(&1);
    let entry = if v1 { 20 } else { 12 };
    let Some(e) = table(elst, entry, "elst")?.chunks_exact(entry).next() else {
        return Ok(0);
    };
    let (dur, media) = if v1 {
        (be64(e, 0), be64(e, 8) as i64)
    } else {
        (u64::from(be32(e, 0)), i64::from(be32(e, 4) as i32))
    };
    Ok(if media == -1 {
        dur.saturating_mul(1000) / u64::from(movie_scale)
    } else {
        0
    })
}

/// Offset and size of every sample, from `stsz`, `stsc`, and `stco` or
/// `co64`.
fn frame_list(stbl: &[([u8; 4], &[u8])]) -> Result<Vec<(u64, u32)>, Error> {
    let stsz = find(stbl, b"stsz").ok_or(Error::Malformed("no stsz"))?;
    if stsz.len() < 12 {
        return Err(Error::Malformed("stsz"));
    }
    let fixed = be32(stsz, 4);
    let count = be32(stsz, 8) as usize;
    if count > MAX_FRAMES {
        return Err(Error::TooLarge("audio frames"));
    }
    let sizes: Vec<u32> = if fixed != 0 {
        vec![fixed; count]
    } else {
        let need = count.checked_mul(4).ok_or(Error::Malformed("stsz"))?;
        let b = stsz
            .get(12..12 + need)
            .ok_or(Error::Malformed("stsz is short"))?;
        b.chunks_exact(4).map(|c| be32(c, 0)).collect()
    };
    let stsc: Vec<(u64, u64)> = table(
        find(stbl, b"stsc").ok_or(Error::Malformed("no stsc"))?,
        12,
        "stsc",
    )?
    .chunks_exact(12)
    .map(|e| (u64::from(be32(e, 0)), u64::from(be32(e, 4))))
    .collect();
    let offsets: Vec<u64> = if let Some(p) = find(stbl, b"stco") {
        table(p, 4, "stco")?
            .chunks_exact(4)
            .map(|e| u64::from(be32(e, 0)))
            .collect()
    } else if let Some(p) = find(stbl, b"co64") {
        table(p, 8, "co64")?
            .chunks_exact(8)
            .map(|e| be64(e, 0))
            .collect()
    } else {
        return Err(Error::Malformed("no chunk offsets"));
    };
    let mut out = Vec::with_capacity(sizes.len());
    let mut next = 0usize;
    for (ci, &base) in offsets.iter().enumerate() {
        let chunk = ci as u64 + 1;
        let per = stsc
            .iter()
            .rev()
            .find(|(first, _)| *first <= chunk)
            .map_or(0, |e| e.1);
        let mut at = base;
        for _ in 0..per {
            let Some(&size) = sizes.get(next) else {
                break;
            };
            out.push((at, size));
            at = at.saturating_add(u64::from(size));
            next += 1;
        }
    }
    if out.len() != sizes.len() {
        return Err(Error::Malformed("audio samples not in chunks"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_without_moov_is_an_error_and_without_audio_is_none() {
        // A moov with no tracks.
        assert_eq!(parse_moov(&[]).unwrap(), None);
        let dir = std::env::temp_dir();
        let missing = dir.join("nvrview-no-such-audio-track.mp4");
        assert!(read_aac_track(&missing).is_err());
    }

    #[test]
    fn adts_headers_round_trip_through_the_parser() {
        let t = AacTrack {
            object_type: 2,
            sample_rate_index: 8,
            sample_rate: 16_000,
            channels: 1,
            start_ms: 0,
            frames: vec![],
        };
        let f = t.adts_frame(&[1, 2, 3]);
        assert_eq!(f.len(), 10);
        assert_eq!(&f[..2], &[0xff, 0xf1]);
        assert_eq!(f[2], (1 << 6) | (8 << 2));
        assert_eq!(
            ((usize::from(f[3] & 3) << 11) | (usize::from(f[4]) << 3) | usize::from(f[5] >> 5)),
            10
        );
    }

    #[test]
    fn the_audio_specific_config_is_found_in_an_esds() {
        // Full-box header, ES (3), decoder config (4), decoder info (5).
        let esds = [
            0, 0, 0, 0, 3, 0x19, 0, 1, 0, 4, 0x11, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            5, 2, 0x14, 0x08, 6, 1, 2,
        ];
        assert_eq!(audio_specific_config(&esds), Some((2, 8, 1)));
    }
}
