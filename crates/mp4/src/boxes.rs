//! Low-level ISO BMFF box construction (ISO/IEC 14496-12 and -14, -15).

use crate::adts::AacConfig;
use crate::nal::{H264Sps, H265Sps, ParamSets};

pub(crate) fn put_u16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_be_bytes());
}
pub(crate) fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_be_bytes());
}
pub(crate) fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_be_bytes());
}

/// Writes a box: 32-bit size, fourcc, then whatever `f` appends.
pub(crate) fn bx(buf: &mut Vec<u8>, fourcc: &[u8; 4], f: impl FnOnce(&mut Vec<u8>)) {
    let start = buf.len();
    put_u32(buf, 0);
    buf.extend_from_slice(fourcc);
    f(buf);
    let size = u32::try_from(buf.len() - start).expect("box under 4 GiB");
    buf[start..start + 4].copy_from_slice(&size.to_be_bytes());
}

/// A box with version and flags.
pub(crate) fn full(
    buf: &mut Vec<u8>,
    fourcc: &[u8; 4],
    version: u8,
    flags: u32,
    f: impl FnOnce(&mut Vec<u8>),
) {
    bx(buf, fourcc, |b| {
        put_u32(b, (u32::from(version) << 24) | (flags & 0xff_ffff));
        f(b);
    });
}

/// One chunk of consecutive samples of a single track.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChunkRec {
    /// Absolute file offset (before any faststart shift).
    pub offset: u64,
    pub samples: u32,
    /// 1-based sample description index.
    pub entry: u32,
}

/// Everything `stbl` needs apart from the sample description.
pub(crate) struct Tables<'a> {
    pub stsd: &'a [u8],
    pub stts: &'a [(u32, u32)],
    pub ctts: Option<&'a [(u32, u32)]>,
    pub sizes: &'a [u32],
    /// 1-based sync sample numbers; `None` means every sample is a sync sample.
    pub sync: Option<&'a [u32]>,
    pub chunks: &'a [ChunkRec],
    pub shift: u64,
    pub co64: bool,
}

pub(crate) fn stbl(buf: &mut Vec<u8>, t: &Tables<'_>) {
    bx(buf, b"stbl", |b| {
        b.extend_from_slice(t.stsd);
        full(b, b"stts", 0, 0, |b| {
            put_u32(b, t.stts.len() as u32);
            for &(c, d) in t.stts {
                put_u32(b, c);
                put_u32(b, d);
            }
        });
        if let Some(ctts) = t.ctts {
            full(b, b"ctts", 0, 0, |b| {
                put_u32(b, ctts.len() as u32);
                for &(c, d) in ctts {
                    put_u32(b, c);
                    put_u32(b, d);
                }
            });
        }
        if let Some(sync) = t.sync {
            full(b, b"stss", 0, 0, |b| {
                put_u32(b, sync.len() as u32);
                for &s in sync {
                    put_u32(b, s);
                }
            });
        }
        let runs = stsc_runs(t.chunks);
        full(b, b"stsc", 0, 0, |b| {
            put_u32(b, runs.len() as u32);
            for (first, n, entry) in runs {
                put_u32(b, first);
                put_u32(b, n);
                put_u32(b, entry);
            }
        });
        full(b, b"stsz", 0, 0, |b| {
            put_u32(b, 0);
            put_u32(b, t.sizes.len() as u32);
            for &s in t.sizes {
                put_u32(b, s);
            }
        });
        if t.co64 {
            full(b, b"co64", 0, 0, |b| {
                put_u32(b, t.chunks.len() as u32);
                for c in t.chunks {
                    put_u64(b, c.offset + t.shift);
                }
            });
        } else {
            full(b, b"stco", 0, 0, |b| {
                put_u32(b, t.chunks.len() as u32);
                for c in t.chunks {
                    put_u32(b, (c.offset + t.shift) as u32);
                }
            });
        }
    });
}

/// Run-length encodes chunks as (first_chunk, samples_per_chunk, entry).
pub(crate) fn stsc_runs(chunks: &[ChunkRec]) -> Vec<(u32, u32, u32)> {
    let mut runs: Vec<(u32, u32, u32)> = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        match runs.last() {
            Some(&(_, n, e)) if n == c.samples && e == c.entry => {}
            _ => runs.push((i as u32 + 1, c.samples, c.entry)),
        }
    }
    runs
}

pub(crate) fn stsd(entry_boxes: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    full(&mut out, b"stsd", 0, 0, |b| {
        put_u32(b, entry_boxes.len() as u32);
        for e in entry_boxes {
            b.extend_from_slice(e);
        }
    });
    out
}

fn visual_entry(fourcc: &[u8; 4], width: u32, height: u32, config: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    bx(&mut out, fourcc, |b| {
        b.extend_from_slice(&[0; 6]);
        put_u16(b, 1); // data_reference_index
        b.extend_from_slice(&[0; 16]); // pre_defined, reserved
        put_u16(b, width as u16);
        put_u16(b, height as u16);
        put_u32(b, 0x0048_0000); // 72 dpi
        put_u32(b, 0x0048_0000);
        put_u32(b, 0);
        put_u16(b, 1); // frame_count
        b.extend_from_slice(&[0; 32]); // compressorname
        put_u16(b, 0x18); // depth
        put_u16(b, 0xffff); // pre_defined = -1
        b.extend_from_slice(&config);
    });
    out
}

/// `avc1` sample entry with `avcC` built from the parameter sets.
pub(crate) fn avc1_entry(sets: &ParamSets, sps: &H264Sps) -> Vec<u8> {
    let mut cfg = Vec::new();
    bx(&mut cfg, b"avcC", |b| {
        b.push(1);
        b.push(sps.profile_idc);
        b.push(sps.constraint_flags);
        b.push(sps.level_idc);
        b.push(0xfc | 3); // 4-byte NAL lengths
        b.push(0xe0 | sets.sps.len() as u8);
        for s in &sets.sps {
            put_u16(b, s.len() as u16);
            b.extend_from_slice(s);
        }
        b.push(sets.pps.len() as u8);
        for p in &sets.pps {
            put_u16(b, p.len() as u16);
            b.extend_from_slice(p);
        }
        if matches!(sps.profile_idc, 100 | 110 | 122 | 144) {
            b.push(0xfc | sps.chroma_format_idc);
            b.push(0xf8 | (sps.bit_depth_luma - 8));
            b.push(0xf8 | (sps.bit_depth_chroma - 8));
            b.push(0); // no SPS extensions
        }
    });
    visual_entry(b"avc1", sps.width, sps.height, cfg)
}

/// `hvc1` sample entry with `hvcC`; parameter sets live only here.
pub(crate) fn hvc1_entry(sets: &ParamSets, sps: &H265Sps) -> Vec<u8> {
    let mut cfg = Vec::new();
    bx(&mut cfg, b"hvcC", |b| {
        b.push(1);
        b.push((sps.profile_space << 6) | (sps.tier_flag << 5) | sps.profile_idc);
        put_u32(b, sps.compat_flags);
        b.extend_from_slice(&sps.constraint_flags);
        b.push(sps.level_idc);
        put_u16(b, 0xf000); // min_spatial_segmentation_idc = 0 (unknown)
        b.push(0xfc); // parallelismType unknown
        b.push(0xfc | sps.chroma_format_idc);
        b.push(0xf8 | (sps.bit_depth_luma - 8));
        b.push(0xf8 | (sps.bit_depth_chroma - 8));
        put_u16(b, 0); // avgFrameRate unspecified
        // constantFrameRate 0, numTemporalLayers, temporalIdNested, lengthSizeMinusOne 3
        b.push((sps.num_temporal_layers << 3) | (sps.temporal_id_nested << 2) | 3);
        let arrays: Vec<(u8, &Vec<Vec<u8>>)> =
            [(32u8, &sets.vps), (33, &sets.sps), (34, &sets.pps)]
                .into_iter()
                .filter(|(_, l)| !l.is_empty())
                .collect();
        b.push(arrays.len() as u8);
        for (ty, list) in arrays {
            b.push(0x80 | ty); // array_completeness = 1
            put_u16(b, list.len() as u16);
            for n in list {
                put_u16(b, n.len() as u16);
                b.extend_from_slice(n);
            }
        }
    });
    visual_entry(b"hvc1", sps.width, sps.height, cfg)
}

/// `mp4a` sample entry with `esds`.
pub(crate) fn mp4a_entry(cfg: &AacConfig, avg_bitrate: u32, max_bitrate: u32) -> Vec<u8> {
    let asc = cfg.audio_specific_config();
    let mut out = Vec::new();
    bx(&mut out, b"mp4a", |b| {
        b.extend_from_slice(&[0; 6]);
        put_u16(b, 1);
        b.extend_from_slice(&[0; 8]);
        put_u16(b, u16::from(cfg.channels.min(2)));
        put_u16(b, 16);
        put_u32(b, 0);
        put_u32(
            b,
            if cfg.sample_rate <= 0xffff {
                cfg.sample_rate << 16
            } else {
                0
            },
        );
        full(b, b"esds", 0, 0, |b| {
            // ES_Descriptor
            b.push(0x03);
            b.push(23 + asc.len() as u8);
            put_u16(b, 0); // ES_ID
            b.push(0);
            // DecoderConfigDescriptor
            b.push(0x04);
            b.push(13 + 2 + asc.len() as u8);
            b.push(0x40); // MPEG-4 audio
            b.push(0x15); // audio stream, reserved bit set
            b.extend_from_slice(&[0, 0, 0]); // bufferSizeDB
            put_u32(b, max_bitrate);
            put_u32(b, avg_bitrate);
            // DecoderSpecificInfo
            b.push(0x05);
            b.push(asc.len() as u8);
            b.extend_from_slice(&asc);
            // SLConfigDescriptor
            b.push(0x06);
            b.push(1);
            b.push(2);
        });
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stsc_compresses_equal_runs() {
        let c = |s, e| ChunkRec {
            offset: 0,
            samples: s,
            entry: e,
        };
        let runs = stsc_runs(&[c(5, 1), c(5, 1), c(3, 1), c(3, 2), c(3, 2)]);
        assert_eq!(runs, [(1, 5, 1), (3, 3, 1), (4, 3, 2)]);
    }

    #[test]
    fn box_sizes_patch() {
        let mut b = Vec::new();
        full(&mut b, b"abcd", 1, 2, |b| put_u32(b, 7));
        assert_eq!(
            b,
            [0, 0, 0, 16, b'a', b'b', b'c', b'd', 1, 0, 0, 2, 0, 0, 0, 7]
        );
    }
}
