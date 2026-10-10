//! Incremental BCMEDIA demuxer for one stream, plus record encoders.
//!
//! The payloads of one stream (one message ID), concatenated in order, form a
//! sequence of records, each padded so the next starts on an 8-byte boundary.
//!
//! Confirmed on hardware: the info record's header-size field is the total
//! record length from the record start; video extra-header sizes were always
//! multiples of 8 (so padding the payload alone or the whole record agree);
//! in audio records the u16 at +6 equals the u16 at +4.

use std::fmt;

/// Largest video payload accepted, to bound memory on corrupt input.
pub const MAX_VIDEO_PAYLOAD: u32 = 32 * 1024 * 1024;
/// Largest extra-header size accepted.
pub const MAX_EXTRA_HEADER: u32 = 64 * 1024;
/// Largest stream-info header size accepted.
pub const MAX_INFO_HEADER: u32 = 4096;
/// Smallest stream-info header size accepted (must cover the frame-rate byte).
pub const MIN_INFO_HEADER: u32 = 20;
const VIDEO_HEADER_LEN: usize = 24;
const AUDIO_HEADER_LEN: usize = 8;

/// Video codec of a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// H.264 (`H264`).
    H264,
    /// H.265 (`H265`).
    H265,
}

impl Codec {
    fn tag(self) -> [u8; 4] {
        match self {
            Codec::H264 => *b"H264",
            Codec::H265 => *b"H265",
        }
    }
}

/// A decoded media record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    /// Stream info (`1001`/`1002`). Width and height can be 0; the frame rate
    /// is advertised only and often higher than the real rate.
    Info {
        /// Width in pixels (may be 0).
        width: u32,
        /// Height in pixels (may be 0).
        height: u32,
        /// Advertised frame rate.
        fps: u8,
    },
    /// A video frame.
    Video {
        /// Codec named in the record header.
        codec: Codec,
        /// True for `00dc`-`09dc` records.
        keyframe: bool,
        /// Timestamp in microseconds.
        pts_us: u64,
        /// Annex-B NAL unit bytes.
        data: Vec<u8>,
    },
    /// An audio frame.
    Audio {
        /// The two-digit stream index from the record magic (`05wb` is 5).
        stream_index: u8,
        /// Raw audio payload.
        data: Vec<u8>,
    },
}

/// Demuxer errors. After an error the buffer is untouched; call
/// [`Demuxer::resync`] to continue with the next plausible record or drop the
/// stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemuxError {
    /// The next record starts with an unknown magic.
    UnknownMagic([u8; 4]),
    /// A video record names a codec other than `H264`/`H265`.
    UnknownCodec([u8; 4]),
    /// A size field is out of the accepted range.
    BadSize,
}

impl fmt::Display for DemuxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DemuxError::UnknownMagic(m) => write!(f, "unknown record magic {m:02x?}"),
            DemuxError::UnknownCodec(m) => write!(f, "unknown codec tag {m:02x?}"),
            DemuxError::BadSize => write!(f, "record size out of range"),
        }
    }
}

impl std::error::Error for DemuxError {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Info,
    Video { keyframe: bool },
    Audio { index: u8 },
}

fn digits(a: u8, b: u8) -> Option<u8> {
    (a.is_ascii_digit() && b.is_ascii_digit()).then(|| (a - b'0') * 10 + (b - b'0'))
}

fn classify(magic: [u8; 4]) -> Option<Kind> {
    match &magic {
        b"1001" | b"1002" => Some(Kind::Info),
        [a, b, b'd', b'c'] => digits(*a, *b).map(|_| Kind::Video {
            keyframe: *a == b'0',
        }),
        [a, b, b'w', b'b'] => digits(*a, *b).map(|index| Kind::Audio { index }),
        _ => None,
    }
}

fn pad8(n: usize) -> usize {
    (8 - n % 8) % 8
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Incremental demuxer: push payload bytes, pull records.
#[derive(Debug, Default)]
pub struct Demuxer {
    buf: Vec<u8>,
    pos: usize,
    /// Bytes still to discard from upcoming input (padding or resync).
    skip: usize,
}

impl Demuxer {
    /// Creates an empty demuxer.
    pub fn new() -> Demuxer {
        Demuxer::default()
    }

    /// Appends payload bytes of this stream.
    pub fn push(&mut self, data: &[u8]) {
        let mut data = data;
        if self.skip > 0 {
            let n = self.skip.min(data.len());
            self.skip -= n;
            data = &data[n..];
        }
        if self.pos > 0 && self.pos >= self.buf.len() / 2 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(data);
    }

    /// Number of buffered bytes not yet returned as records.
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Consumes `total` bytes (record plus padding); padding that has not
    /// arrived yet is skipped when it does.
    fn finish(&mut self, total: usize) {
        let have = self.buffered();
        if total > have {
            self.pos = self.buf.len();
            self.skip = total - have;
        } else {
            self.pos += total;
        }
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
    }

    /// Returns the next complete record, `Ok(None)` if more bytes are needed.
    pub fn next_record(&mut self) -> Result<Option<Record>, DemuxError> {
        let b = &self.buf[self.pos..];
        if b.len() < 8 {
            return Ok(None);
        }
        let magic = [b[0], b[1], b[2], b[3]];
        let kind = classify(magic).ok_or(DemuxError::UnknownMagic(magic))?;
        match kind {
            Kind::Info => {
                let hsize = u32_at(b, 4);
                if !(MIN_INFO_HEADER..=MAX_INFO_HEADER).contains(&hsize) {
                    return Err(DemuxError::BadSize);
                }
                let hsize = hsize as usize;
                if b.len() < hsize {
                    return Ok(None);
                }
                let rec = Record::Info {
                    width: u32_at(b, 8),
                    height: u32_at(b, 12),
                    fps: b[17],
                };
                self.finish(hsize + pad8(hsize));
                Ok(Some(rec))
            }
            Kind::Video { keyframe } => {
                if b.len() < VIDEO_HEADER_LEN {
                    return Ok(None);
                }
                let codec = match &b[4..8] {
                    b"H264" => Codec::H264,
                    b"H265" => Codec::H265,
                    _ => return Err(DemuxError::UnknownCodec([b[4], b[5], b[6], b[7]])),
                };
                let size = u32_at(b, 8);
                let extra = u32_at(b, 12);
                if size > MAX_VIDEO_PAYLOAD || extra > MAX_EXTRA_HEADER {
                    return Err(DemuxError::BadSize);
                }
                let (size, extra) = (size as usize, extra as usize);
                let end = VIDEO_HEADER_LEN + extra + size;
                if b.len() < end {
                    return Ok(None);
                }
                let mut ts = [0u8; 8];
                ts.copy_from_slice(&b[16..24]);
                let rec = Record::Video {
                    codec,
                    keyframe,
                    pts_us: u64::from_le_bytes(ts),
                    data: b[VIDEO_HEADER_LEN + extra..end].to_vec(),
                };
                self.finish(end + pad8(end));
                Ok(Some(rec))
            }
            Kind::Audio { index } => {
                let size = u16::from_le_bytes([b[4], b[5]]) as usize;
                let end = AUDIO_HEADER_LEN + size;
                if b.len() < end {
                    return Ok(None);
                }
                let rec = Record::Audio {
                    stream_index: index,
                    data: b[AUDIO_HEADER_LEN..end].to_vec(),
                };
                self.finish(end + pad8(end));
                Ok(Some(rec))
            }
        }
    }

    /// Recovers after an error: discards 8 bytes, then 8-byte steps until the
    /// buffer starts with a plausible record magic. If the buffer runs out,
    /// the remaining alignment is discarded from later pushes.
    pub fn resync(&mut self) {
        let avail = self.buffered();
        let mut off = 8;
        while off + 4 <= avail {
            let b = &self.buf[self.pos + off..];
            if classify([b[0], b[1], b[2], b[3]]).is_some() {
                self.pos += off;
                return;
            }
            off += 8;
        }
        if off >= avail {
            self.buf.clear();
            self.pos = 0;
            self.skip = off - avail;
        } else {
            // Fewer than 4 bytes left to judge: keep them for the next call.
            self.pos += off;
        }
    }
}

/// Encodes a stream-info record (32-byte header).
pub fn encode_info(width: u32, height: u32, fps: u8) -> Vec<u8> {
    let mut v = vec![0u8; 32];
    v[..4].copy_from_slice(b"1001");
    v[4..8].copy_from_slice(&32u32.to_le_bytes());
    v[8..12].copy_from_slice(&width.to_le_bytes());
    v[12..16].copy_from_slice(&height.to_le_bytes());
    v[17] = fps;
    v
}

/// Encodes a video record with an optional extra header, padded to 8 bytes.
/// `stream_index` is the two-digit index (`0..=9` keyframe, `10..=19`
/// predicted by convention; only the first digit's value matters here).
pub fn encode_video(
    stream_index: u8,
    codec: Codec,
    pts_us: u64,
    extra_header: &[u8],
    data: &[u8],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(VIDEO_HEADER_LEN + extra_header.len() + data.len() + 8);
    v.extend_from_slice(&[
        b'0' + stream_index / 10 % 10,
        b'0' + stream_index % 10,
        b'd',
        b'c',
    ]);
    v.extend_from_slice(&codec.tag());
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(&(extra_header.len() as u32).to_le_bytes());
    v.extend_from_slice(&pts_us.to_le_bytes());
    v.extend_from_slice(extra_header);
    v.extend_from_slice(data);
    v.resize(v.len() + pad8(v.len()), 0);
    v
}

/// Encodes an audio record (`NNwb`), padded to 8 bytes.
pub fn encode_audio(stream_index: u8, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(AUDIO_HEADER_LEN + data.len() + 8);
    v.extend_from_slice(&[
        b'0' + stream_index / 10 % 10,
        b'0' + stream_index % 10,
        b'w',
        b'b',
    ]);
    v.extend_from_slice(&(data.len() as u16).to_le_bytes());
    v.extend_from_slice(&(data.len() as u16).to_le_bytes());
    v.extend_from_slice(data);
    v.resize(v.len() + pad8(v.len()), 0);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(seed: u8, len: usize) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1, 0x65];
        v.extend((0..len).map(|i| (i as u8).wrapping_mul(seed).wrapping_add(seed)));
        v
    }

    fn sample() -> (Vec<u8>, Vec<Record>) {
        let mut bytes = encode_info(2560, 1440, 25);
        let mut recs = vec![Record::Info {
            width: 2560,
            height: 1440,
            fps: 25,
        }];
        let mut add = |bytes_part: Vec<u8>, rec: Record| {
            bytes.extend(bytes_part);
            recs.push(rec);
        };
        let k = nal(3, 1001);
        add(
            encode_video(0, Codec::H265, 1_000, &[], &k),
            Record::Video {
                codec: Codec::H265,
                keyframe: true,
                pts_us: 1_000,
                data: k,
            },
        );
        for (i, len) in [1usize, 7, 8, 9, 300].into_iter().enumerate() {
            let p = nal(5 + i as u8, len);
            let extra: &[u8] = if i == 2 { &[9u8; 8] } else { &[] };
            add(
                encode_video(10, Codec::H265, 2_000 + i as u64, extra, &p),
                Record::Video {
                    codec: Codec::H265,
                    keyframe: false,
                    pts_us: 2_000 + i as u64,
                    data: p,
                },
            );
            let a: Vec<u8> = (0..(i * 5 + 1)).map(|x| x as u8).collect();
            add(
                encode_audio(5, &a),
                Record::Audio {
                    stream_index: 5,
                    data: a,
                },
            );
        }
        let h = nal(9, 40);
        add(
            encode_video(1, Codec::H264, 9, &[], &h),
            Record::Video {
                codec: Codec::H264,
                keyframe: true,
                pts_us: 9,
                data: h,
            },
        );
        (bytes, recs)
    }

    fn drain(d: &mut Demuxer, out: &mut Vec<Record>) {
        while let Some(r) = d.next_record().unwrap() {
            out.push(r);
        }
    }

    #[test]
    fn whole_and_chunked() {
        let (bytes, recs) = sample();
        assert_eq!(bytes.len() % 8, 0);
        for chunk in [bytes.len(), 1, 3, 5, 8, 13, 100, 1000] {
            let mut d = Demuxer::new();
            let mut out = Vec::new();
            for c in bytes.chunks(chunk) {
                d.push(c);
                drain(&mut d, &mut out);
            }
            assert_eq!(out, recs, "chunk {chunk}");
            assert_eq!(d.buffered(), 0);
        }
    }

    #[test]
    fn trailing_padding_arriving_late_is_skipped() {
        let a = encode_audio(1, &[1, 2, 3]);
        let b = encode_audio(2, &[4]);
        let mut d = Demuxer::new();
        d.push(&a[..11]); // header + payload, padding missing
        assert!(matches!(
            d.next_record(),
            Ok(Some(Record::Audio {
                stream_index: 1,
                ..
            }))
        ));
        d.push(&a[11..]);
        d.push(&b);
        assert_eq!(
            d.next_record().unwrap(),
            Some(Record::Audio {
                stream_index: 2,
                data: vec![4]
            })
        );
    }

    #[test]
    fn unknown_magic_and_resync() {
        let (bytes, recs) = sample();
        let mut stream = vec![0xAAu8; 24];
        stream.extend(&bytes);
        let mut d = Demuxer::new();
        d.push(&stream);
        assert!(matches!(d.next_record(), Err(DemuxError::UnknownMagic(_))));
        d.resync();
        let mut out = Vec::new();
        drain(&mut d, &mut out);
        assert_eq!(out, recs);
    }

    #[test]
    fn bad_codec_and_sizes() {
        let mut v = encode_video(0, Codec::H264, 0, &[], &[1, 2, 3]);
        v[4..8].copy_from_slice(b"XXXX");
        let mut d = Demuxer::new();
        d.push(&v);
        assert!(matches!(d.next_record(), Err(DemuxError::UnknownCodec(_))));
        let mut v = encode_video(0, Codec::H264, 0, &[], &[1, 2, 3]);
        v[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut d = Demuxer::new();
        d.push(&v);
        assert_eq!(d.next_record(), Err(DemuxError::BadSize));
        let mut v = encode_info(1, 1, 1);
        v[4..8].copy_from_slice(&0u32.to_le_bytes());
        let mut d = Demuxer::new();
        d.push(&v);
        assert_eq!(d.next_record(), Err(DemuxError::BadSize));
    }

    #[test]
    fn random_bytes_do_not_panic_or_loop() {
        let mut state = 0x0f1e_2d3c_4b5a_6978u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let (valid, _) = sample();
        for round in 0..300 {
            let mut d = Demuxer::new();
            for _ in 0..20 {
                let len = (next() % 80) as usize;
                let mut chunk: Vec<u8> = (0..len).map(|_| next() as u8).collect();
                if round % 3 == 0 {
                    let n = (next() as usize % valid.len()).min(chunk.len());
                    chunk[..n].copy_from_slice(&valid[..n]);
                }
                d.push(&chunk);
                for _ in 0..50 {
                    match d.next_record() {
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(_) => d.resync(),
                    }
                }
            }
        }
    }
}
