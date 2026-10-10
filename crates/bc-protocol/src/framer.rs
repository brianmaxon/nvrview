//! Incremental message decoder and message encoder.

use crate::header::{Class, Header, HeaderError, MAGIC};
use std::fmt;

/// Largest body the decoder accepts, to bound memory on corrupt input.
pub const MAX_BODY_LEN: u32 = 32 * 1024 * 1024;

/// A complete message: header plus body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawMessage {
    /// The decoded header.
    pub header: Header,
    /// The body: extension followed by payload.
    pub body: Vec<u8>,
}

impl RawMessage {
    fn split_at(&self) -> usize {
        (self.header.payload_offset.unwrap_or(0) as usize).min(self.body.len())
    }

    /// The XML extension (empty when the payload offset is 0).
    pub fn extension(&self) -> &[u8] {
        &self.body[..self.split_at()]
    }

    /// The payload (the whole body when the payload offset is 0).
    pub fn payload(&self) -> &[u8] {
        &self.body[self.split_at()..]
    }

    /// Whether the header's payload offset exceeds the body length.
    pub fn offset_out_of_range(&self) -> bool {
        self.header.payload_offset.unwrap_or(0) as usize > self.body.len()
    }
}

/// Decoder errors. After an error the decoder keeps its buffer; call
/// [`Decoder::resync`] to skip to the next magic or [`Decoder::clear`] to drop
/// everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The buffered data does not start with the magic.
    BadMagic,
    /// The header's class is unknown.
    UnknownClass(u16),
    /// The body length exceeds [`MAX_BODY_LEN`].
    BodyTooLarge(u32),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::BadMagic => write!(f, "bad magic"),
            FrameError::UnknownClass(c) => write!(f, "unknown message class {c:#06x}"),
            FrameError::BodyTooLarge(n) => write!(f, "body length {n} exceeds limit"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Incremental decoder: push bytes as they arrive, pull complete messages.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    pos: usize,
}

impl Decoder {
    /// Creates an empty decoder.
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// Appends received bytes.
    pub fn push(&mut self, data: &[u8]) {
        if self.pos > 0 && self.pos >= self.buf.len() / 2 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        self.buf.extend_from_slice(data);
    }

    /// Number of buffered bytes not yet returned as messages.
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Returns the next complete message, `Ok(None)` if more bytes are needed.
    pub fn next_message(&mut self) -> Result<Option<RawMessage>, FrameError> {
        let avail = &self.buf[self.pos..];
        let (header, hlen) = match Header::decode(avail) {
            Ok(v) => v,
            Err(HeaderError::Incomplete { .. }) => return Ok(None),
            Err(HeaderError::BadMagic) => return Err(FrameError::BadMagic),
            Err(HeaderError::UnknownClass(c)) => return Err(FrameError::UnknownClass(c)),
        };
        if header.body_len > MAX_BODY_LEN {
            return Err(FrameError::BodyTooLarge(header.body_len));
        }
        let total = hlen + header.body_len as usize;
        if avail.len() < total {
            return Ok(None);
        }
        let body = avail[hlen..total].to_vec();
        self.pos += total;
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        Ok(Some(RawMessage { header, body }))
    }

    /// Discards at least one byte, then everything up to the next magic (or
    /// all but a possible partial magic at the end). Call after an error.
    pub fn resync(&mut self) {
        let avail = &self.buf[self.pos..];
        let mut skip = avail.len();
        for i in 1..avail.len() {
            let rest = &avail[i..];
            let n = rest.len().min(4);
            if rest[..n] == MAGIC[..n] {
                skip = i;
                break;
            }
        }
        self.pos += skip;
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
    }

    /// Drops all buffered bytes.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.pos = 0;
    }
}

/// Why a message could not be encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// An extension was given for a class whose header has no payload offset.
    ExtensionNotAllowed,
    /// The body is larger than a header can describe.
    BodyTooLarge,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::ExtensionNotAllowed => {
                write!(f, "class has no payload offset; extension not allowed")
            }
            EncodeError::BodyTooLarge => write!(f, "body too large"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Encodes a complete message with status 0. The body is `extension` followed
/// by `payload`; body length and payload offset are computed.
///
/// This is the low-level encoder and does not apply the command allowlist
/// (see [`crate::session`]); it is also what a device simulator uses.
pub fn encode_message(
    command: u32,
    message_id: u32,
    class: Class,
    extension: &[u8],
    payload: &[u8],
) -> Result<Vec<u8>, EncodeError> {
    encode_message_with_status(command, message_id, 0, class, extension, payload)
}

/// Like [`encode_message`] with an explicit status (for device replies).
pub fn encode_message_with_status(
    command: u32,
    message_id: u32,
    status: u16,
    class: Class,
    extension: &[u8],
    payload: &[u8],
) -> Result<Vec<u8>, EncodeError> {
    if !extension.is_empty() && !class.has_payload_offset() {
        return Err(EncodeError::ExtensionNotAllowed);
    }
    let body_len = extension
        .len()
        .checked_add(payload.len())
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(EncodeError::BodyTooLarge)?;
    let header = Header {
        command,
        body_len,
        message_id,
        status,
        class,
        payload_offset: class.has_payload_offset().then_some(extension.len() as u32),
    };
    let mut out = Vec::with_capacity(header.len() + body_len as usize);
    header.encode_into(&mut out);
    out.extend_from_slice(extension);
    out.extend_from_slice(payload);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stream() -> (Vec<u8>, Vec<RawMessage>) {
        type Spec<'a> = (u32, u32, Class, &'a [u8], &'a [u8]);
        let specs: [Spec; 4] = [
            (1, 10, Class::ClientHandshake, b"", b""),
            (3, 11, Class::Client, b"", b"<body/>"),
            (3, 11, Class::Device, b"<Extension/>", b"payload-bytes"),
            (93, 12, Class::Device, b"", b""),
        ];
        let mut bytes = Vec::new();
        let mut msgs = Vec::new();
        for (cmd, id, class, ext, payload) in specs {
            let enc = encode_message(cmd, id, class, ext, payload).unwrap();
            let (header, hl) = Header::decode(&enc).unwrap();
            assert_eq!(header.body_len as usize, ext.len() + payload.len());
            msgs.push(RawMessage {
                header,
                body: enc[hl..].to_vec(),
            });
            bytes.extend(enc);
        }
        (bytes, msgs)
    }

    fn drain(d: &mut Decoder) -> Vec<RawMessage> {
        let mut v = Vec::new();
        while let Some(m) = d.next_message().unwrap() {
            v.push(m);
        }
        v
    }

    #[test]
    fn one_chunk() {
        let (bytes, msgs) = sample_stream();
        let mut d = Decoder::new();
        d.push(&bytes);
        assert_eq!(drain(&mut d), msgs);
        assert_eq!(d.buffered(), 0);
        assert_eq!(msgs[2].extension(), b"<Extension/>");
        assert_eq!(msgs[2].payload(), b"payload-bytes");
        assert_eq!(msgs[1].payload(), b"<body/>");
    }

    #[test]
    fn byte_at_a_time_and_odd_chunks() {
        let (bytes, msgs) = sample_stream();
        for chunk in [1usize, 2, 3, 7, 19, 24, 25] {
            let mut d = Decoder::new();
            let mut got = Vec::new();
            for c in bytes.chunks(chunk) {
                d.push(c);
                got.extend(drain(&mut d));
            }
            assert_eq!(got, msgs, "chunk size {chunk}");
        }
    }

    #[test]
    fn bad_magic_and_resync() {
        let (bytes, msgs) = sample_stream();
        let mut d = Decoder::new();
        d.push(&[0xde, 0xad, 0xbe, 0xef, 0x00]);
        d.push(&bytes);
        assert_eq!(d.next_message(), Err(FrameError::BadMagic));
        d.resync();
        assert_eq!(drain(&mut d), msgs);
    }

    #[test]
    fn oversized_body_is_rejected() {
        let mut enc = encode_message(3, 1, Class::Device, b"", b"").unwrap();
        enc[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut d = Decoder::new();
        d.push(&enc);
        assert_eq!(d.next_message(), Err(FrameError::BodyTooLarge(u32::MAX)));
    }

    #[test]
    fn extension_rejected_for_short_header() {
        assert_eq!(
            encode_message(1, 1, Class::ClientHandshake, b"x", b""),
            Err(EncodeError::ExtensionNotAllowed)
        );
    }

    #[test]
    fn random_bytes_do_not_panic() {
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..300 {
            let mut d = Decoder::new();
            for _ in 0..20 {
                let len = (next() % 64) as usize;
                let mut chunk: Vec<u8> = (0..len).map(|_| next() as u8).collect();
                if next() % 3 == 0 && chunk.len() >= 4 {
                    chunk[..4].copy_from_slice(&MAGIC);
                }
                d.push(&chunk);
                for _ in 0..50 {
                    match d.next_message() {
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(_) => d.resync(),
                    }
                }
            }
        }
    }
}
