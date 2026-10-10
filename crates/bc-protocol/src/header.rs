//! Message header: layout, classes, and encode/decode.

use std::fmt;

/// The four magic bytes that start every message.
pub const MAGIC: [u8; 4] = [0xf0, 0xde, 0xbc, 0x0a];
/// Header size for classes without a payload offset.
pub const SHORT_HEADER_LEN: usize = 20;
/// Header size for classes with a payload offset.
pub const LONG_HEADER_LEN: usize = 24;
/// Command number of a recording playback (docs/protocol.md, "Replay media
/// headers"): its messages have a 24-byte header whose bytes 16 to 19 are
/// not a status and class.
pub const REPLAY_COMMAND: u32 = 5;

/// Message class, which also decides the header size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    /// `0x1465`: the client's first (nonce request) message; 20-byte header.
    ClientHandshake,
    /// `0x1466`: the device's nonce reply; 20-byte header.
    DeviceHandshake,
    /// `0x1464`: client messages after the handshake; 24-byte header.
    Client,
    /// `0x0000`: device replies after the handshake; 24-byte header.
    Device,
}

impl Class {
    /// Wire value of the class.
    pub const fn value(self) -> u16 {
        match self {
            Class::ClientHandshake => 0x1465,
            Class::DeviceHandshake => 0x1466,
            Class::Client => 0x1464,
            Class::Device => 0x0000,
        }
    }

    /// Parses a wire value; `None` for classes this crate does not know.
    pub const fn from_value(value: u16) -> Option<Class> {
        match value {
            0x1465 => Some(Class::ClientHandshake),
            0x1466 => Some(Class::DeviceHandshake),
            0x1464 => Some(Class::Client),
            0x0000 => Some(Class::Device),
            _ => None,
        }
    }

    /// Header size in bytes for this class.
    pub const fn header_len(self) -> usize {
        if self.has_payload_offset() {
            LONG_HEADER_LEN
        } else {
            SHORT_HEADER_LEN
        }
    }

    /// Whether the header carries a payload offset field.
    pub const fn has_payload_offset(self) -> bool {
        matches!(self, Class::Client | Class::Device)
    }
}

/// Why a header could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderError {
    /// Fewer bytes than the header needs; at least `needed` bytes in total.
    Incomplete {
        /// Minimum total number of bytes required to make progress.
        needed: usize,
    },
    /// The data does not start with [`MAGIC`].
    BadMagic,
    /// The class field holds a value this crate does not know.
    UnknownClass(u16),
}

impl fmt::Display for HeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeaderError::Incomplete { needed } => {
                write!(f, "incomplete header, need {needed} bytes")
            }
            HeaderError::BadMagic => write!(f, "bad magic"),
            HeaderError::UnknownClass(c) => write!(f, "unknown message class {c:#06x}"),
        }
    }
}

impl std::error::Error for HeaderError {}

/// A decoded message header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Command number.
    pub command: u32,
    /// Length in bytes of everything after the header.
    pub body_len: u32,
    /// Message ID, chosen by the client; replies echo it.
    pub message_id: u32,
    /// Status (replies: 200, 401, ...; requests normally 0).
    pub status: u16,
    /// Message class.
    pub class: Class,
    /// Length of the XML extension at the start of the body. `Some` exactly
    /// for classes with a 24-byte header; `Some(0)` means the whole body is
    /// the payload.
    pub payload_offset: Option<u32>,
    /// For command 5 ([`REPLAY_COMMAND`]) only: the u32 in header bytes
    /// 16 to 19. On the device's media messages it is the `seq` the client
    /// chose in the preceding command 123; those bytes are then not a status
    /// and class, so `status` and `class` of such a header carry no meaning
    /// (`class` is always one of the two 24-byte classes). On other command-5
    /// messages (the client's request, the device's first reply) the value
    /// is just those bytes read as a number, and `status` and `class` are
    /// valid. The caller knows which kind of message it holds. When this is
    /// `Some`, [`Header::encode_into`] writes it in place of status and
    /// class.
    pub replay_seq: Option<u32>,
}

impl Header {
    /// Size of this header on the wire.
    pub fn len(&self) -> usize {
        self.class.header_len()
    }

    /// Always false; a header is never empty. Present for API symmetry.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Appends the encoded header to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&self.command.to_le_bytes());
        out.extend_from_slice(&self.body_len.to_le_bytes());
        out.extend_from_slice(&self.message_id.to_le_bytes());
        match self.replay_seq {
            Some(seq) if self.command == REPLAY_COMMAND && self.class.has_payload_offset() => {
                out.extend_from_slice(&seq.to_le_bytes());
            }
            _ => {
                out.extend_from_slice(&self.status.to_le_bytes());
                // Unlike every other field, the class is big-endian: `0x1465`
                // is the byte sequence `14 65` on the wire (docs/protocol.md).
                out.extend_from_slice(&self.class.value().to_be_bytes());
            }
        }
        if self.class.has_payload_offset() {
            out.extend_from_slice(&self.payload_offset.unwrap_or(0).to_le_bytes());
        }
    }

    /// Decodes a header from the start of `buf`, returning it and its size.
    ///
    /// Returns [`HeaderError::Incomplete`] when more bytes are needed. A bad
    /// magic is reported as soon as the bytes seen so far disagree with it.
    pub fn decode(buf: &[u8]) -> Result<(Header, usize), HeaderError> {
        let n = buf.len().min(4);
        if buf[..n] != MAGIC[..n] {
            return Err(HeaderError::BadMagic);
        }
        if buf.len() < SHORT_HEADER_LEN {
            return Err(HeaderError::Incomplete {
                needed: SHORT_HEADER_LEN,
            });
        }
        let u32_at = |o: usize| u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
        let u16_at = |o: usize| u16::from_le_bytes([buf[o], buf[o + 1]]);
        let raw_class = u16::from_be_bytes([buf[18], buf[19]]);
        // Command 5 always has a 24-byte header, and bytes 16 to 19 of its
        // media messages are a sequence number, not a status and class: a
        // number whose high half reads as an unknown class, or as a
        // 20-byte-header class, must not fail or shorten the header.
        let replay = u32_at(4) == REPLAY_COMMAND;
        let class = match Class::from_value(raw_class) {
            Some(c) if !replay || c.has_payload_offset() => c,
            _ if replay => Class::Device,
            _ => return Err(HeaderError::UnknownClass(raw_class)),
        };
        let len = class.header_len();
        if buf.len() < len {
            return Err(HeaderError::Incomplete { needed: len });
        }
        let header = Header {
            command: u32_at(4),
            body_len: u32_at(8),
            message_id: u32_at(12),
            status: u16_at(16),
            class,
            payload_offset: class.has_payload_offset().then(|| u32_at(20)),
            replay_seq: replay.then(|| u32_at(16)),
        };
        Ok((header, len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(class: Class) -> Header {
        Header {
            command: 3,
            body_len: 77,
            message_id: 0x0102_0304,
            status: 200,
            class,
            payload_offset: class.has_payload_offset().then_some(5),
            replay_seq: None,
        }
    }

    /// The 20 bytes the official client sent to start a session (fixed
    /// header only; nothing session-specific).
    #[test]
    fn handshake_header_matches_the_wire() {
        let h = Header {
            command: 1,
            body_len: 0,
            message_id: 0,
            status: 0xdc12,
            class: Class::ClientHandshake,
            payload_offset: None,
            replay_seq: None,
        };
        let mut out = Vec::new();
        h.encode_into(&mut out);
        let wire = [
            0xf0, 0xde, 0xbc, 0x0a, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12, 0xdc, 0x14, 0x65,
        ];
        assert_eq!(out, wire);
        assert_eq!(Header::decode(&wire).unwrap().0, h);
    }

    #[test]
    fn round_trip_all_classes() {
        for class in [
            Class::ClientHandshake,
            Class::DeviceHandshake,
            Class::Client,
            Class::Device,
        ] {
            let h = sample(class);
            let mut buf = Vec::new();
            h.encode_into(&mut buf);
            assert_eq!(buf.len(), class.header_len());
            assert_eq!(Header::decode(&buf), Ok((h, class.header_len())));
        }
    }

    #[test]
    fn exact_layout() {
        let mut buf = Vec::new();
        sample(Class::Client).encode_into(&mut buf);
        let expected: [u8; 24] = [
            0xf0, 0xde, 0xbc, 0x0a, 3, 0, 0, 0, 77, 0, 0, 0, 4, 3, 2, 1, 200, 0, 0x14, 0x64, 5, 0,
            0, 0,
        ];
        assert_eq!(buf, expected);
    }

    #[test]
    fn errors() {
        assert_eq!(
            Header::decode(&[0xf0, 0xde]),
            Err(HeaderError::Incomplete { needed: 20 })
        );
        assert_eq!(Header::decode(&[0xf0, 0x00]), Err(HeaderError::BadMagic));
        let mut buf = Vec::new();
        sample(Class::Client).encode_into(&mut buf);
        assert_eq!(
            Header::decode(&buf[..21]),
            Err(HeaderError::Incomplete { needed: 24 })
        );
        buf[18] = 0x99;
        assert!(matches!(
            Header::decode(&buf),
            Err(HeaderError::UnknownClass(_))
        ));
    }

    fn replay_wire(seq: u32, offset: u32) -> Vec<u8> {
        let mut w = vec![0xf0, 0xde, 0xbc, 0x0a, 5, 0, 0, 0, 9, 0, 0, 0, 7, 0, 0, 0];
        w.extend_from_slice(&seq.to_le_bytes());
        w.extend_from_slice(&offset.to_le_bytes());
        w
    }

    /// Media messages of a playback carry a sequence number where other
    /// messages have a status and class; whatever its value, the header is
    /// 24 bytes long and the sequence number is returned.
    #[test]
    fn replay_media_header_holds_a_sequence_number() {
        // A number close to a Unix time, as the client chose them; one whose
        // high half is the bytes of a 24-byte class (0x1464), one that is a
        // 20-byte class (0x1465), and one that is no class at all.
        for seq in [
            0x6a00_1234u32,
            0x6414_0001,
            0x6514_ffff,
            0x6614_0000,
            0x1234_5678,
        ] {
            let wire = replay_wire(seq, 12);
            let (h, len) = Header::decode(&wire).unwrap();
            assert_eq!(len, 24, "seq {seq:#x}");
            assert_eq!(h.command, 5);
            assert_eq!((h.body_len, h.message_id), (9, 7));
            assert_eq!(h.payload_offset, Some(12));
            assert_eq!(h.replay_seq, Some(seq));
            assert!(h.class.has_payload_offset());
            let mut out = Vec::new();
            h.encode_into(&mut out);
            assert_eq!(out, wire, "seq {seq:#x} survives a round trip");
        }
    }

    /// The same bytes in another command are still a status and class, and an
    /// unknown class is still an error there.
    #[test]
    fn only_command_five_reads_a_sequence_number() {
        let mut wire = replay_wire(0x6a00_1234, 0);
        wire[4] = 3;
        assert_eq!(Header::decode(&wire), Err(HeaderError::UnknownClass(0x6a)));
        let (h, _) = Header::decode(&replay_wire(0x6a00_1234, 0)).unwrap();
        assert_eq!(h.replay_seq, Some(0x6a00_1234));
        let (h, _) = Header::decode(&{
            let mut w = replay_wire(0, 0);
            w[4] = 3;
            w[16..20].copy_from_slice(&[200, 0, 0x14, 0x64]);
            w
        })
        .unwrap();
        assert_eq!(
            (h.status, h.class, h.replay_seq),
            (200, Class::Client, None)
        );
    }

    /// The device's first reply to a play request (command 5) and the
    /// client's request keep a status and class; the sequence field is just
    /// those bytes.
    #[test]
    fn replay_control_messages_keep_status_and_class() {
        let wire = [
            0xf0, 0xde, 0xbc, 0x0a, 5, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 200, 0, 0, 0, 0, 0, 0, 0,
        ];
        let (h, len) = Header::decode(&wire).unwrap();
        assert_eq!((len, h.status, h.class), (24, 200, Class::Device));
        assert_eq!(h.replay_seq, Some(200));
    }
}
