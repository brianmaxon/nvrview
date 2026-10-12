//! The MQTT 3.1.1 packets this client and the test broker need, as a
//! sans-I/O codec: [`encode`] appends a packet to a buffer, and a
//! [`Decoder`] is fed bytes as they arrive and yields whole packets.
//!
//! Written from the public OASIS MQTT 3.1.1 specification. Only the packets
//! of a subscribe-only client and of a broker serving one are present.

use std::fmt;

/// Largest packet the decoder accepts (fixed header included). A bigger one
/// is a protocol violation, not something to buffer.
pub const MAX_PACKET: usize = 64 * 1024;

/// A string that never prints.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Packet {
    Connect {
        client_id: String,
        keepalive_secs: u16,
        clean_session: bool,
        username: Option<String>,
        password: Option<Secret>,
    },
    ConnAck {
        session_present: bool,
        code: u8,
    },
    Publish {
        topic: String,
        payload: Vec<u8>,
        qos: u8,
        retain: bool,
        dup: bool,
        /// Present for QoS 1 and 2 only.
        pid: Option<u16>,
    },
    PubAck {
        pid: u16,
    },
    Subscribe {
        pid: u16,
        /// Topic filter and requested QoS.
        filters: Vec<(String, u8)>,
    },
    SubAck {
        pid: u16,
        codes: Vec<u8>,
    },
    PingReq,
    PingResp,
    Disconnect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Remaining length uses more than four bytes.
    BadLength,
    TooLarge,
    /// A packet type or flags this codec does not handle.
    Unsupported,
    /// Truncated or inconsistent contents.
    Malformed,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DecodeError::BadLength => "bad remaining length",
            DecodeError::TooLarge => "packet too large",
            DecodeError::Unsupported => "unsupported packet",
            DecodeError::Malformed => "malformed packet",
        })
    }
}

impl std::error::Error for DecodeError {}

fn put_len(out: &mut Vec<u8>, mut n: usize) {
    loop {
        let mut b = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Appends the wire form of `p` to `out`.
pub fn encode(p: &Packet, out: &mut Vec<u8>) {
    let mut body = Vec::new();
    let first = match p {
        Packet::Connect {
            client_id,
            keepalive_secs,
            clean_session,
            username,
            password,
        } => {
            put_str(&mut body, "MQTT");
            body.push(4);
            let mut flags = 0u8;
            if *clean_session {
                flags |= 0x02;
            }
            if username.is_some() {
                flags |= 0x80;
            }
            if password.is_some() {
                flags |= 0x40;
            }
            body.push(flags);
            body.extend_from_slice(&keepalive_secs.to_be_bytes());
            put_str(&mut body, client_id);
            if let Some(u) = username {
                put_str(&mut body, u);
            }
            if let Some(pw) = password {
                put_str(&mut body, &pw.0);
            }
            0x10
        }
        Packet::ConnAck {
            session_present,
            code,
        } => {
            body.push(u8::from(*session_present));
            body.push(*code);
            0x20
        }
        Packet::Publish {
            topic,
            payload,
            qos,
            retain,
            dup,
            pid,
        } => {
            put_str(&mut body, topic);
            if *qos > 0 {
                body.extend_from_slice(&pid.unwrap_or(1).to_be_bytes());
            }
            body.extend_from_slice(payload);
            0x30 | (u8::from(*dup) << 3) | (qos << 1) | u8::from(*retain)
        }
        Packet::PubAck { pid } => {
            body.extend_from_slice(&pid.to_be_bytes());
            0x40
        }
        Packet::Subscribe { pid, filters } => {
            body.extend_from_slice(&pid.to_be_bytes());
            for (f, q) in filters {
                put_str(&mut body, f);
                body.push(*q);
            }
            0x82
        }
        Packet::SubAck { pid, codes } => {
            body.extend_from_slice(&pid.to_be_bytes());
            body.extend_from_slice(codes);
            0x90
        }
        Packet::PingReq => 0xc0,
        Packet::PingResp => 0xd0,
        Packet::Disconnect => 0xe0,
    };
    out.push(first);
    put_len(out, body.len());
    out.extend_from_slice(&body);
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn u8(&mut self) -> Result<u8, DecodeError> {
        let (&b, rest) = self.0.split_first().ok_or(DecodeError::Malformed)?;
        self.0 = rest;
        Ok(b)
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.0.len() < n {
            return Err(DecodeError::Malformed);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn string(&mut self) -> Result<String, DecodeError> {
        let n = self.u16()? as usize;
        String::from_utf8(self.bytes(n)?.to_vec()).map_err(|_| DecodeError::Malformed)
    }
    fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.0)
    }
}

fn decode_body(first: u8, body: &[u8]) -> Result<Packet, DecodeError> {
    let mut c = Cursor(body);
    let flags = first & 0x0f;
    match first >> 4 {
        1 => {
            if c.string()? != "MQTT" || c.u8()? != 4 {
                return Err(DecodeError::Unsupported);
            }
            let f = c.u8()?;
            let keepalive_secs = c.u16()?;
            let client_id = c.string()?;
            let username = if f & 0x80 != 0 {
                Some(c.string()?)
            } else {
                None
            };
            let password = if f & 0x40 != 0 {
                Some(Secret(c.string()?))
            } else {
                None
            };
            Ok(Packet::Connect {
                client_id,
                keepalive_secs,
                clean_session: f & 0x02 != 0,
                username,
                password,
            })
        }
        2 => {
            let s = c.u8()?;
            Ok(Packet::ConnAck {
                session_present: s & 1 != 0,
                code: c.u8()?,
            })
        }
        3 => {
            let qos = (flags >> 1) & 3;
            if qos == 3 {
                return Err(DecodeError::Malformed);
            }
            let topic = c.string()?;
            let pid = if qos > 0 { Some(c.u16()?) } else { None };
            Ok(Packet::Publish {
                topic,
                payload: c.rest().to_vec(),
                qos,
                retain: flags & 1 != 0,
                dup: flags & 8 != 0,
                pid,
            })
        }
        4 => Ok(Packet::PubAck { pid: c.u16()? }),
        8 => {
            if flags != 2 {
                return Err(DecodeError::Malformed);
            }
            let pid = c.u16()?;
            let mut filters = Vec::new();
            while !c.0.is_empty() {
                let f = c.string()?;
                filters.push((f, c.u8()?));
            }
            Ok(Packet::Subscribe { pid, filters })
        }
        9 => {
            let pid = c.u16()?;
            Ok(Packet::SubAck {
                pid,
                codes: c.rest().to_vec(),
            })
        }
        12 => Ok(Packet::PingReq),
        13 => Ok(Packet::PingResp),
        14 => Ok(Packet::Disconnect),
        _ => Err(DecodeError::Unsupported),
    }
}

/// Collects bytes and yields whole packets.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole packet, `Ok(None)` when more bytes are needed. After
    /// an error the stream cannot be trusted: drop the connection.
    pub fn next_packet(&mut self) -> Result<Option<Packet>, DecodeError> {
        if self.buf.is_empty() {
            return Ok(None);
        }
        let mut len = 0usize;
        let mut mult = 1usize;
        let mut i = 1;
        loop {
            let Some(&b) = self.buf.get(i) else {
                return Ok(None);
            };
            len += (b & 0x7f) as usize * mult;
            i += 1;
            if b & 0x80 == 0 {
                break;
            }
            mult *= 128;
            if i > 4 {
                return Err(DecodeError::BadLength);
            }
        }
        if i + len > MAX_PACKET {
            return Err(DecodeError::TooLarge);
        }
        if self.buf.len() < i + len {
            return Ok(None);
        }
        let first = self.buf[0];
        let p = decode_body(first, &self.buf[i..i + len]);
        self.buf.drain(..i + len);
        p.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(p: Packet) {
        let mut out = Vec::new();
        encode(&p, &mut out);
        let mut d = Decoder::new();
        // Feed one byte at a time: partial packets must wait.
        for (i, b) in out.iter().enumerate() {
            assert_eq!(d.next_packet().unwrap(), None, "early at {i}");
            d.push(&[*b]);
        }
        assert_eq!(d.next_packet().unwrap(), Some(p));
        assert_eq!(d.next_packet().unwrap(), None);
    }

    #[test]
    fn round_trips() {
        round(Packet::Connect {
            client_id: "c".into(),
            keepalive_secs: 30,
            clean_session: true,
            username: Some("example-user".into()),
            password: Some(Secret("example-password".into())),
        });
        round(Packet::ConnAck {
            session_present: false,
            code: 5,
        });
        round(Packet::Publish {
            topic: "example/topic".into(),
            payload: vec![7; 300],
            qos: 1,
            retain: true,
            dup: false,
            pid: Some(9),
        });
        round(Packet::Publish {
            topic: "t".into(),
            payload: vec![],
            qos: 0,
            retain: false,
            dup: false,
            pid: None,
        });
        round(Packet::PubAck { pid: 3 });
        round(Packet::Subscribe {
            pid: 1,
            filters: vec![("a/#".into(), 1)],
        });
        round(Packet::SubAck {
            pid: 1,
            codes: vec![1, 0x80],
        });
        round(Packet::PingReq);
        round(Packet::PingResp);
        round(Packet::Disconnect);
    }

    #[test]
    fn oversized_and_bad_lengths_are_errors() {
        let mut d = Decoder::new();
        d.push(&[0x30, 0xff, 0xff, 0xff, 0xff, 0x01]);
        assert_eq!(d.next_packet(), Err(DecodeError::BadLength));
        let mut d = Decoder::new();
        d.push(&[0x30, 0x80, 0x80, 0x10]);
        assert_eq!(d.next_packet(), Err(DecodeError::TooLarge));
    }

    #[test]
    fn password_never_prints() {
        let p = Packet::Connect {
            client_id: "c".into(),
            keepalive_secs: 1,
            clean_session: true,
            username: None,
            password: Some(Secret("example-password".into())),
        };
        assert!(!format!("{p:?}").contains("example-password"));
    }
}
