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
        /// True for keyframe records (`x0dc`: second character `0`).
        keyframe: bool,
        /// Timestamp in microseconds.
        pts_us: u64,
        /// Annex-B NAL unit bytes.
        data: Vec<u8>,
        /// The record's extra header, exactly as received (often empty).
        /// When at least 8 bytes long, a keyframe's bytes 0-3 are a u32
        /// count of seconds of the device's wall clock ([`wall_clock_secs`]);
        /// a predicted frame's hold another value there. See "Records as
        /// the official client reads them" and "Sub and main of one camera"
        /// in docs/protocol.md.
        extra_header: Vec<u8>,
    },
    /// An audio frame.
    Audio {
        /// The stream index from the record magic: a digit magic reads as its
        /// two-digit number (`05wb` is 5). The first character can be any of
        /// `0` to `@`, each counted as its offset from `0` times ten (`:5wb`
        /// is 105), so `stream_index % 10` is always the second character.
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

/// Whether `c` may be the first character of a video or audio magic: any
/// character from `0` to `@` (0x30-0x40). Digits are what native cameras send;
/// a third-party camera behind an NVR sent `:` (`:0dc`, `:1dc`). The character
/// is given no meaning here (see "Records as the official client reads them"
/// in docs/protocol.md).
fn first_char_ok(c: u8) -> bool {
    (b'0'..=b'@').contains(&c)
}

/// Offset of an accepted first character from `0` (0-16).
fn first_char_value(c: u8) -> u8 {
    c - b'0'
}

fn classify(magic: [u8; 4]) -> Option<Kind> {
    match magic {
        [b'1', b'0', b'0', b'1' | b'2'] => Some(Kind::Info),
        // `<any of 0-@><frame type>dc`: type 0 is a keyframe, any other digit
        // a predicted frame.
        [a, b, b'd', b'c'] if first_char_ok(a) && b.is_ascii_digit() => Some(Kind::Video {
            keyframe: b == b'0',
        }),
        // `<any of 0-@><digit>wb`: the index is the first character's offset
        // from `0` times ten plus the second digit, so a digit magic reads as
        // its two-digit number (`05wb` is 5) and the second character is
        // always `index % 10`.
        [a, b, b'w', b'b'] if first_char_ok(a) && b.is_ascii_digit() => Some(Kind::Audio {
            index: first_char_value(a) * 10 + (b - b'0'),
        }),
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
                    extra_header: b[VIDEO_HEADER_LEN..VIDEO_HEADER_LEN + extra].to_vec(),
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

/// The device wall-clock seconds in a video extra header: the u32
/// little-endian at bytes 0-3, when the header is at least 8 bytes long
/// (docs/protocol.md, "Records as the official client reads them"). Only a
/// keyframe's header holds seconds there; a predicted frame's holds another
/// value, which this returns all the same.
pub fn wall_clock_secs(extra_header: &[u8]) -> Option<u32> {
    (extra_header.len() >= 8).then(|| u32_at(extra_header, 0))
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
/// The magic is `<first><frame type>dc`: `stream` gives the first character
/// as its offset from `0` (0-9 are digits; 10 to 16 are `:` to `@`, as a
/// third-party camera sends; larger values wrap to a digit), and the frame
/// type is `0` for a keyframe and `1` otherwise.
pub fn encode_video(
    stream: u8,
    keyframe: bool,
    codec: Codec,
    pts_us: u64,
    extra_header: &[u8],
    data: &[u8],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(VIDEO_HEADER_LEN + extra_header.len() + data.len() + 8);
    v.extend_from_slice(&[
        b'0' + if stream <= 16 { stream } else { stream % 10 },
        if keyframe { b'0' } else { b'1' },
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

/// Encodes an audio record (`NNwb`), padded to 8 bytes. The inverse of the
/// demuxer's index: indexes up to 169 round-trip (100 and up give a first
/// character from `:` to `@`); larger ones wrap the first character to a digit.
pub fn encode_audio(stream_index: u8, data: &[u8]) -> Vec<u8> {
    let tens = stream_index / 10;
    let mut v = Vec::with_capacity(AUDIO_HEADER_LEN + data.len() + 8);
    v.extend_from_slice(&[
        b'0' + if tens <= 16 { tens } else { tens % 10 },
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
            encode_video(0, true, Codec::H265, 1_000, &[], &k),
            Record::Video {
                codec: Codec::H265,
                keyframe: true,
                pts_us: 1_000,
                data: k,
                extra_header: Vec::new(),
            },
        );
        for (i, len) in [1usize, 7, 8, 9, 300].into_iter().enumerate() {
            let p = nal(5 + i as u8, len);
            let extra: &[u8] = if i == 2 { &[9u8; 8] } else { &[] };
            add(
                encode_video(0, false, Codec::H265, 2_000 + i as u64, extra, &p),
                Record::Video {
                    codec: Codec::H265,
                    keyframe: false,
                    pts_us: 2_000 + i as u64,
                    data: p,
                    extra_header: extra.to_vec(),
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
            encode_video(1, true, Codec::H264, 9, &[], &h),
            Record::Video {
                codec: Codec::H264,
                keyframe: true,
                pts_us: 9,
                data: h,
                extra_header: Vec::new(),
            },
        );
        (bytes, recs)
    }

    fn drain(d: &mut Demuxer, out: &mut Vec<Record>) {
        while let Some(r) = d.next_record().unwrap() {
            out.push(r);
        }
    }

    /// The frame type is the second character of the magic: `01dc` is a
    /// predicted frame and `10dc` a keyframe (stream index 1).
    #[test]
    fn frame_type_is_the_second_character() {
        for (magic, keyframe) in [
            (b"00dc", true),
            (b"01dc", false),
            (b"10dc", true),
            (b"11dc", false),
        ] {
            let mut rec = encode_video(0, true, Codec::H264, 7, &[], &[0, 0, 0, 1, 0x65]);
            rec[..4].copy_from_slice(magic);
            let mut d = Demuxer::new();
            d.push(&rec);
            match d.next_record().unwrap() {
                Some(Record::Video { keyframe: k, .. }) => assert_eq!(k, keyframe, "{magic:?}"),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(
            &encode_video(1, false, Codec::H265, 0, &[], &[])[..4],
            b"11dc"
        );
    }

    fn video_with_magic(magic: &[u8; 4], pts_us: u64, data: &[u8]) -> Vec<u8> {
        let mut rec = encode_video(0, true, Codec::H264, pts_us, &[], data);
        rec[..4].copy_from_slice(magic);
        rec
    }

    /// The first character of a video or audio magic may be anything from
    /// `0` to `@`; a third-party camera sends `:0dc` and `:1dc`.
    #[test]
    fn first_character_from_zero_to_at_is_accepted() {
        for first in b'0'..=b'@' {
            for (second, keyframe) in [(b'0', true), (b'1', false)] {
                let magic = [first, second, b'd', b'c'];
                let mut d = Demuxer::new();
                d.push(&video_with_magic(&magic, 3, &[0, 0, 0, 1, 0x65]));
                match d.next_record().unwrap() {
                    Some(Record::Video { keyframe: k, .. }) => assert_eq!(k, keyframe, "{magic:?}"),
                    other => panic!("{magic:?}: {other:?}"),
                }
            }
            let mut rec = encode_audio(5, &[1, 2, 3]);
            rec[0] = first;
            let mut d = Demuxer::new();
            d.push(&rec);
            match d.next_record().unwrap() {
                Some(Record::Audio { stream_index, data }) => {
                    assert_eq!(stream_index % 10, 5);
                    assert_eq!(stream_index / 10, first - b'0');
                    assert_eq!(data, vec![1, 2, 3]);
                    // The index round-trips through the encoder.
                    assert_eq!(encode_audio(stream_index, &data), rec);
                }
                other => panic!("{first}: {other:?}"),
            }
        }
        assert_eq!(
            &encode_video(10, true, Codec::H264, 0, &[], &[])[..4],
            b":0dc"
        );
        assert_eq!(
            &encode_video(10, false, Codec::H264, 0, &[], &[])[..4],
            b":1dc"
        );
        assert_eq!(
            &encode_video(16, true, Codec::H264, 0, &[], &[])[..4],
            b"@0dc"
        );
        assert_eq!(&encode_audio(105, &[])[..4], b":5wb");
    }

    /// Characters just outside `0`-`@` (and non-digit second characters)
    /// are still unknown magics.
    #[test]
    fn first_character_outside_the_range_is_rejected() {
        for magic in [
            b"A0dc", b"/0dc", b"a1dc", b" 0dc", b"A5wb", b"/5wb", b":xdc", b":ewb", b"0:dc",
        ] {
            let mut d = Demuxer::new();
            d.push(&video_with_magic(magic, 0, &[0, 0, 0, 1, 0x65]));
            assert_eq!(
                d.next_record(),
                Err(DemuxError::UnknownMagic(*magic)),
                "{magic:?}"
            );
        }
    }

    /// A third-party stream (`:0dc`/`:1dc`) mixed with digit-magic video and
    /// audio parses whole and in every chunking, and resynchronizes onto a
    /// `:` record after garbage.
    #[test]
    fn third_party_magics_mixed_and_resync() {
        let mut bytes = encode_info(1920, 1080, 15);
        let mut recs = vec![Record::Info {
            width: 1920,
            height: 1080,
            fps: 15,
        }];
        for i in 0..6u64 {
            let keyframe = i % 3 == 0;
            let data = nal(11 + i as u8, 50 + i as usize * 9);
            bytes.extend(encode_video(
                10,
                keyframe,
                Codec::H264,
                i * 66_000,
                &[],
                &data,
            ));
            recs.push(Record::Video {
                codec: Codec::H264,
                keyframe,
                pts_us: i * 66_000,
                data: data.clone(),
                extra_header: Vec::new(),
            });
            if i == 2 {
                bytes.extend(encode_video(0, false, Codec::H264, 1, &[], &data));
                recs.push(Record::Video {
                    codec: Codec::H264,
                    keyframe: false,
                    pts_us: 1,
                    data,
                    extra_header: Vec::new(),
                });
                bytes.extend(encode_audio(5, &[7; 13]));
                recs.push(Record::Audio {
                    stream_index: 5,
                    data: vec![7; 13],
                });
            }
        }
        for chunk in [bytes.len(), 1, 7, 8, 64] {
            let mut d = Demuxer::new();
            let mut out = Vec::new();
            for c in bytes.chunks(chunk) {
                d.push(c);
                drain(&mut d, &mut out);
            }
            assert_eq!(out, recs, "chunk {chunk}");
            assert_eq!(d.buffered(), 0);
        }
        // Garbage, including an out-of-range magic, then the `:` stream
        // without its info record: resync lands on the first `:0dc`.
        let info_len = encode_info(1, 1, 1).len();
        let mut stream = b"A0dcH264".to_vec();
        stream.extend([0xAAu8; 16]);
        stream.extend(&bytes[info_len..]);
        let mut d = Demuxer::new();
        d.push(&stream);
        assert!(matches!(d.next_record(), Err(DemuxError::UnknownMagic(_))));
        d.resync();
        let mut out = Vec::new();
        drain(&mut d, &mut out);
        assert_eq!(out, recs[1..]);
    }

    #[test]
    fn extra_header_is_kept_and_wall_clock_read() {
        let mut extra = 1_700_000_123u32.to_le_bytes().to_vec();
        extra.extend([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        let rec = encode_video(0, true, Codec::H264, 42, &extra, &[0, 0, 0, 1, 0x65]);
        let mut d = Demuxer::new();
        d.push(&rec);
        match d.next_record().unwrap() {
            Some(Record::Video { extra_header, .. }) => {
                assert_eq!(extra_header, extra);
                assert_eq!(wall_clock_secs(&extra_header), Some(1_700_000_123));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(wall_clock_secs(&[1, 2, 3, 4]), None);
        assert_eq!(wall_clock_secs(&[]), None);
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
        let mut v = encode_video(0, true, Codec::H264, 0, &[], &[1, 2, 3]);
        v[4..8].copy_from_slice(b"XXXX");
        let mut d = Demuxer::new();
        d.push(&v);
        assert!(matches!(d.next_record(), Err(DemuxError::UnknownCodec(_))));
        let mut v = encode_video(0, true, Codec::H264, 0, &[], &[1, 2, 3]);
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
