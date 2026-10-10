//! Reading back a fragmented file written by this crate and remuxing it into
//! a regular MP4.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::adts::AacConfig;
use crate::error::{Error, Result};
use crate::nal::VideoCodec;
use crate::writer::{Mp4Writer, Options, Summary, TIMESCALE, VideoFrame};

fn bad(why: &'static str) -> Error {
    Error::InvalidFragmentedFile(why)
}

fn u32at(d: &[u8], i: usize) -> Result<u32> {
    d.get(i..i + 4)
        .map(|b| u32::from_be_bytes(b.try_into().expect("4 bytes")))
        .ok_or_else(|| bad("short box"))
}

fn u64at(d: &[u8], i: usize) -> Result<u64> {
    d.get(i..i + 8)
        .map(|b| u64::from_be_bytes(b.try_into().expect("8 bytes")))
        .ok_or_else(|| bad("short box"))
}

/// Child boxes of a payload: (fourcc, payload).
fn children(d: &[u8]) -> Result<Vec<([u8; 4], &[u8])>> {
    let mut v = Vec::new();
    let mut i = 0;
    while i < d.len() {
        let size = u32at(d, i)? as usize;
        if size < 8 || i + size > d.len() {
            return Err(bad("bad box size"));
        }
        let ty: [u8; 4] = d[i + 4..i + 8].try_into().expect("4 bytes");
        v.push((ty, &d[i + 8..i + size]));
        i += size;
    }
    Ok(v)
}

fn find<'a>(list: &[([u8; 4], &'a [u8])], ty: &[u8; 4]) -> Option<&'a [u8]> {
    list.iter().find(|(t, _)| t == ty).map(|(_, p)| *p)
}

struct VideoInfo {
    codec: VideoCodec,
    /// Annex-B parameter sets to put in front of every keyframe.
    sets: Vec<u8>,
}

struct Init {
    video_id: u32,
    video: VideoInfo,
    audio: Option<(u32, AacConfig)>,
}

fn annexb(out: &mut Vec<u8>, nal: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
}

fn parse_avcc(p: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 5;
    let n_sps = usize::from(*p.get(i).ok_or_else(|| bad("short avcC"))? & 0x1f);
    i += 1;
    for _ in 0..n_sps {
        let l = u16::from_be_bytes(
            p.get(i..i + 2)
                .ok_or_else(|| bad("short avcC"))?
                .try_into()
                .expect("2"),
        ) as usize;
        annexb(
            &mut out,
            p.get(i + 2..i + 2 + l).ok_or_else(|| bad("short avcC"))?,
        );
        i += 2 + l;
    }
    let n_pps = usize::from(*p.get(i).ok_or_else(|| bad("short avcC"))?);
    i += 1;
    for _ in 0..n_pps {
        let l = u16::from_be_bytes(
            p.get(i..i + 2)
                .ok_or_else(|| bad("short avcC"))?
                .try_into()
                .expect("2"),
        ) as usize;
        annexb(
            &mut out,
            p.get(i + 2..i + 2 + l).ok_or_else(|| bad("short avcC"))?,
        );
        i += 2 + l;
    }
    Ok(out)
}

fn parse_hvcc(p: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let arrays = usize::from(*p.get(22).ok_or_else(|| bad("short hvcC"))?);
    let mut i = 23;
    for _ in 0..arrays {
        let n = u16::from_be_bytes(
            p.get(i + 1..i + 3)
                .ok_or_else(|| bad("short hvcC"))?
                .try_into()
                .expect("2"),
        ) as usize;
        i += 3;
        for _ in 0..n {
            let l = u16::from_be_bytes(
                p.get(i..i + 2)
                    .ok_or_else(|| bad("short hvcC"))?
                    .try_into()
                    .expect("2"),
            ) as usize;
            annexb(
                &mut out,
                p.get(i + 2..i + 2 + l).ok_or_else(|| bad("short hvcC"))?,
            );
            i += 2 + l;
        }
    }
    Ok(out)
}

fn parse_moov(moov: &[u8]) -> Result<Init> {
    let mut video = None;
    let mut audio = None;
    for (ty, trak) in children(moov)? {
        if &ty != b"trak" {
            continue;
        }
        let t = children(trak)?;
        let tkhd = find(&t, b"tkhd").ok_or_else(|| bad("no tkhd"))?;
        let id = u32at(tkhd, if tkhd.first() == Some(&1) { 20 } else { 12 })?;
        let mdia = children(find(&t, b"mdia").ok_or_else(|| bad("no mdia"))?)?;
        let hdlr = find(&mdia, b"hdlr").ok_or_else(|| bad("no hdlr"))?;
        let handler = hdlr.get(8..12).ok_or_else(|| bad("short hdlr"))?;
        let minf = children(find(&mdia, b"minf").ok_or_else(|| bad("no minf"))?)?;
        let stbl = children(find(&minf, b"stbl").ok_or_else(|| bad("no stbl"))?)?;
        let stsd = find(&stbl, b"stsd").ok_or_else(|| bad("no stsd"))?;
        let entries = children(stsd.get(8..).ok_or_else(|| bad("short stsd"))?)?;
        let (ety, entry) = *entries.first().ok_or_else(|| bad("empty stsd"))?;
        match handler {
            b"vide" => {
                let kids = children(entry.get(78..).ok_or_else(|| bad("short entry"))?)?;
                let (codec, sets) = match &ety {
                    b"avc1" | b"avc3" => (
                        VideoCodec::H264,
                        parse_avcc(find(&kids, b"avcC").ok_or_else(|| bad("no avcC"))?)?,
                    ),
                    b"hvc1" | b"hev1" => (
                        VideoCodec::H265,
                        parse_hvcc(find(&kids, b"hvcC").ok_or_else(|| bad("no hvcC"))?)?,
                    ),
                    _ => return Err(bad("unsupported video codec")),
                };
                video = Some((id, VideoInfo { codec, sets }));
            }
            b"soun" => {
                let kids = children(entry.get(28..).ok_or_else(|| bad("short entry"))?)?;
                let esds = find(&kids, b"esds").ok_or_else(|| bad("no esds"))?;
                // ES (3) -> DecoderConfig (4) -> DecoderSpecificInfo (5), as written here.
                let asc = esds
                    .windows(4)
                    .position(|w| w[0] == 0x05 && w[1] == 2)
                    .and_then(|i| esds.get(i + 2..i + 4))
                    .ok_or_else(|| bad("no AudioSpecificConfig"))?;
                let cfg = AacConfig::from_audio_specific_config([asc[0], asc[1]])
                    .ok_or_else(|| bad("unsupported audio config"))?;
                audio = Some((id, cfg));
            }
            _ => {}
        }
    }
    let (video_id, video) = video.ok_or_else(|| bad("no video track"))?;
    Ok(Init {
        video_id,
        video,
        audio,
    })
}

struct Run {
    track: u32,
    tfdt: u64,
    /// Absolute offset of the first sample, then (duration, size, key, cto).
    data_offset: i64,
    samples: Vec<(u32, u32, bool, i32)>,
}

fn parse_moof(moof: &[u8]) -> Result<Vec<Run>> {
    let mut runs = Vec::new();
    for (ty, traf) in children(moof)? {
        if &ty != b"traf" {
            continue;
        }
        let kids = children(traf)?;
        let tfhd = find(&kids, b"tfhd").ok_or_else(|| bad("no tfhd"))?;
        let track = u32at(tfhd, 4)?;
        if u32at(tfhd, 0)? & 0x02_0000 == 0 {
            return Err(bad("fragment not based at moof"));
        }
        let tfdt = find(&kids, b"tfdt").ok_or_else(|| bad("no tfdt"))?;
        let base = if tfdt[0] == 1 {
            u64at(tfdt, 4)?
        } else {
            u64::from(u32at(tfdt, 4)?)
        };
        let mut next_base = base;
        for (ty, trun) in &kids {
            if ty != b"trun" {
                continue;
            }
            let flags = u32at(trun, 0)? & 0xff_ffff;
            let count = u32at(trun, 4)? as usize;
            let mut i = 8;
            let mut data_offset = 0i64;
            if flags & 1 != 0 {
                data_offset = i64::from(u32at(trun, i)? as i32);
                i += 4;
            }
            if flags & 4 != 0 {
                i += 4;
            }
            if flags & 0x100 == 0 || flags & 0x200 == 0 {
                return Err(bad("trun without durations and sizes"));
            }
            let mut samples = Vec::with_capacity(count);
            for _ in 0..count {
                let dur = u32at(trun, i)?;
                let size = u32at(trun, i + 4)?;
                i += 8;
                let mut key = true;
                if flags & 0x400 != 0 {
                    key = u32at(trun, i)? & 0x0001_0000 == 0;
                    i += 4;
                }
                let mut cto = 0;
                if flags & 0x800 != 0 {
                    cto = u32at(trun, i)? as i32;
                    i += 4;
                }
                samples.push((dur, size, key, cto));
            }
            let span: u64 = samples.iter().map(|s| u64::from(s.0)).sum();
            runs.push(Run {
                track,
                tfdt: next_base,
                data_offset,
                samples,
            });
            next_base += span;
        }
    }
    Ok(runs)
}

fn ticks_to_us(t: u64, rate: u64) -> i64 {
    ((t as u128 * 2_000_000 + rate as u128) / (2 * rate as u128)) as i64
}

/// Remuxes a fragmented file written by [`FragmentedFile`](crate::FragmentedFile)
/// (or the concatenated segments of a [`Fragmenter`](crate::Fragmenter) with
/// one init) into a regular MP4 with `moov` first when `opts.faststart` is
/// set.
///
/// A file cut short (crash, or any truncation) is accepted: everything up to
/// the last complete `moof`+`mdat` pair is used, and a trailing partial
/// fragment is ignored. The `mfra` index, if present, is not needed.
/// Only files from this crate are supported: one video sample entry,
/// `default-base-is-moof` fragments, and per-sample durations and sizes.
/// Audio gaps between fragments become silence only for mono AAC-LC (as in
/// [`Mp4Writer::write_audio_at`]).
pub fn finalize_to_mp4<R: Read + Seek, W: Read + Write + Seek>(
    mut src: R,
    out: W,
    opts: Options,
) -> Result<(W, Summary)> {
    let len = src.seek(SeekFrom::End(0))?;
    let mut pos = 0u64;
    let mut init: Option<Init> = None;
    let mut writer: Option<Mp4Writer<W>> = None;
    let mut out = Some(out);
    let mut opts = opts;
    while pos + 8 <= len {
        let mut head = [0u8; 8];
        src.seek(SeekFrom::Start(pos))?;
        src.read_exact(&mut head)?;
        let size = u64::from(u32::from_be_bytes(head[..4].try_into().expect("4")));
        let ty: [u8; 4] = head[4..].try_into().expect("4");
        if size < 8 {
            return Err(bad("unsupported box size"));
        }
        if pos + size > len {
            break; // partial box at the end
        }
        match &ty {
            b"moov" => {
                let mut buf = vec![0u8; size as usize - 8];
                src.read_exact(&mut buf)?;
                init = Some(parse_moov(&buf)?);
            }
            b"moof" => {
                let ini = init.as_ref().ok_or_else(|| bad("moof before moov"))?;
                let mut buf = vec![0u8; size as usize - 8];
                src.read_exact(&mut buf)?;
                let runs = parse_moof(&buf)?;
                // The media data box follows; it must be complete.
                let mpos = pos + size;
                if mpos + 8 > len {
                    break;
                }
                src.seek(SeekFrom::Start(mpos))?;
                src.read_exact(&mut head)?;
                let msize = u64::from(u32::from_be_bytes(head[..4].try_into().expect("4")));
                if &head[4..] != b"mdat" || msize < 8 {
                    return Err(bad("moof not followed by mdat"));
                }
                if mpos + msize > len {
                    break;
                }
                let mut mdat = vec![0u8; msize as usize - 8];
                src.read_exact(&mut mdat)?;
                let mdat_start = mpos + 8;
                if writer.is_none() {
                    // The audio start is the first audio fragment's time.
                    if let Some((aid, cfg)) = ini.audio
                        && let Some(r) = runs.iter().find(|r| r.track == aid)
                    {
                        opts.audio_start_us = Some(ticks_to_us(r.tfdt, u64::from(cfg.sample_rate)));
                    }
                    writer = Some(Mp4Writer::new(
                        out.take().expect("sink"),
                        ini.video.codec,
                        opts.clone(),
                    )?);
                }
                let w = writer.as_mut().expect("writer");
                for r in &runs {
                    let mut at = pos as i64 + r.data_offset - mdat_start as i64;
                    let mut t = r.tfdt;
                    for &(dur, sz, key, cto) in &r.samples {
                        let a = usize::try_from(at).map_err(|_| bad("bad data offset"))?;
                        let data = mdat
                            .get(a..a + sz as usize)
                            .ok_or_else(|| bad("sample outside mdat"))?;
                        if r.track == ini.video_id {
                            let mut au = Vec::new();
                            if key {
                                au.extend_from_slice(&ini.video.sets);
                            }
                            let mut i = 0;
                            while i < data.len() {
                                let l = u32at(data, i)? as usize;
                                annexb(
                                    &mut au,
                                    data.get(i + 4..i + 4 + l)
                                        .ok_or_else(|| bad("bad NAL length"))?,
                                );
                                i += 4 + l;
                            }
                            let dts = ticks_to_us(t, u64::from(TIMESCALE));
                            let pts = ticks_to_us(
                                (t as i64 + i64::from(cto)) as u64,
                                u64::from(TIMESCALE),
                            );
                            w.write_video(VideoFrame {
                                data: &au,
                                keyframe: key,
                                dts_us: dts,
                                pts_us: pts,
                            })?;
                        } else if let Some((aid, cfg)) = ini.audio
                            && r.track == aid
                        {
                            let frame = cfg.adts_frame(data);
                            let ts = ticks_to_us(t, u64::from(cfg.sample_rate));
                            w.write_audio_at(&frame, ts)?;
                        }
                        t += u64::from(dur);
                        at += i64::from(sz);
                    }
                }
                pos = mpos + msize;
                continue;
            }
            _ => {}
        }
        pos += size;
    }
    let w = writer.ok_or(Error::NoVideo)?;
    w.finish()
}
