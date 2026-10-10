//! Message header: layout, classes, and encode/decode.

use std::fmt;

/// The four magic bytes that start every message.
pub const MAGIC: [u8; 4] = [0xf0, 0xde, 0xbc, 0x0a];
/// Header size for classes without a payload offset.
pub const SHORT_HEADER_LEN: usize = 20;
/// Header size for classes with a payload offset.
pub const LONG_HEADER_LEN: usize = 24;

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
        out.extend_from_slice(&self.status.to_le_bytes());
        out.extend_from_slice(&self.class.value().to_le_bytes());
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
        let raw_class = u16_at(18);
        let class = Class::from_value(raw_class).ok_or(HeaderError::UnknownClass(raw_class))?;
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
        }
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
            0xf0, 0xde, 0xbc, 0x0a, 3, 0, 0, 0, 77, 0, 0, 0, 4, 3, 2, 1, 200, 0, 0x64, 0x14, 5, 0,
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
}
