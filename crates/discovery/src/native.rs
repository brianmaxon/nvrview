//! The native discovery exchanges of NVRs and cameras that speak the
//! port-9000 protocol (docs/protocol.md, "LAN discovery"). Sans-I/O: these
//! functions build and parse datagrams; [`crate::runner`] moves them.
//!
//! Two exchanges:
//!
//! - **Search** ([`SEARCH_REQUEST`]): a 4-byte broadcast to UDP 2000 that
//!   devices answer with a fixed 388-byte [`SearchRecord`] sent to UDP 3000
//!   of the requester.
//! - **Probe** ([`build_probe`]): a 20-byte header plus an obfuscated XML
//!   body broadcast to UDP 2015, answered (so far only by a battery-powered
//!   camera) with a [`ProbeReply`] sent to the port named in the probe.

use std::net::Ipv4Addr;

use crate::xml;

/// The search request's UDP payload.
pub const SEARCH_REQUEST: [u8; 4] = [0xaa, 0xaa, 0x00, 0x00];
/// Devices listen for [`SEARCH_REQUEST`] on this UDP port.
pub const SEARCH_PORT: u16 = 2000;
/// Devices send their [`SearchRecord`] to this UDP port of the requester,
/// whatever port the request came from.
pub const SEARCH_REPLY_PORT: u16 = 3000;
/// Devices listen for the probe ([`build_probe`]) on this UDP port.
pub const PROBE_PORT: u16 = 2015;
/// Size of a search record as every observed device sends it.
pub const SEARCH_RECORD_LEN: usize = 388;

/// The probe header's magic, the bytes `3a cf 87 2a`.
pub const PROBE_MAGIC: [u8; 4] = [0x3a, 0xcf, 0x87, 0x2a];
/// Length of the probe header.
pub const PROBE_HEADER_LEN: usize = 20;
/// Largest probe body this module accepts in a reply.
const MAX_PROBE_BODY: usize = 8 * 1024;

/// Key words of the probe body obfuscation (docs/protocol.md).
const PROBE_KEY: [u32; 8] = [
    0x1F2D_3C4B,
    0x5A6C_7F8D,
    0x3817_2E4B,
    0x8271_635A,
    0x863F_1A2B,
    0xA5C6_F7D8,
    0x8371_E1B4,
    0x17F2_D3A5,
];

/// Device kind byte of a search record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    /// Byte 50 = 1.
    Camera,
    /// Byte 50 = 2.
    Nvr,
    /// Any other value.
    Other(u8),
}

/// One device's answer to [`SEARCH_REQUEST`] (docs/protocol.md, "The
/// 388-byte record"). Strings are as the device sent them, empty when the
/// field was empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRecord {
    /// Byte 50.
    pub kind: RecordKind,
    /// Byte 54: the channel count (1 on cameras).
    pub channels: u8,
    /// Bytes 51, 52 and 57, whose meaning is unknown; kept for diagnostics.
    pub unknown: [u8; 3],
    /// Bytes 80–85.
    pub mac: [u8; 6],
    /// Bytes 108–127, parsed; `None` if not a dotted IPv4 address.
    pub address: Option<Ipv4Addr>,
    /// Bytes 128–129: the TCP port of the native protocol.
    pub tcp_port: u16,
    /// Bytes 132–163: the name the user gave the device.
    pub name: String,
    /// Bytes 228–259: the device UID.
    pub uid: String,
    /// Bytes 260–291: a product-class tag, empty on many devices.
    pub product_tag: String,
}

impl SearchRecord {
    /// The MAC address as `xx:xx:xx:xx:xx:xx`, lower case.
    pub fn mac_string(&self) -> String {
        let m = self.mac;
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        )
    }
}

/// Field offsets of the search record.
mod off {
    pub const KIND: usize = 50;
    pub const UNKNOWN_A: usize = 51;
    pub const UNKNOWN_B: usize = 52;
    pub const CHANNELS: usize = 54;
    pub const UNKNOWN_C: usize = 57;
    pub const MAC: usize = 80;
    pub const MARKER: usize = 104;
    pub const ADDRESS: usize = 108;
    pub const PORT: usize = 128;
    pub const NAME: usize = 132;
    pub const MAC_TEXT: usize = 164;
    pub const UID: usize = 228;
    pub const TAG: usize = 260;
    pub const MAC_HEX: usize = 292;
}

/// Whether `datagram` is a search request (from any searcher).
pub fn is_search_request(datagram: &[u8]) -> bool {
    datagram == SEARCH_REQUEST
}

/// Parses a search record. Returns `None` for anything that is not one:
/// shorter than 388 bytes, or without the request's bytes at offset 104.
/// Longer datagrams are accepted (the extra bytes are ignored) in case
/// other firmware appends fields.
pub fn parse_search_record(datagram: &[u8]) -> Option<SearchRecord> {
    if datagram.len() < SEARCH_RECORD_LEN
        || datagram[off::MARKER..off::MARKER + 4] != SEARCH_REQUEST
    {
        return None;
    }
    let text = |start: usize, end: usize| c_string(&datagram[start..end]);
    let kind = match datagram[off::KIND] {
        1 => RecordKind::Camera,
        2 => RecordKind::Nvr,
        other => RecordKind::Other(other),
    };
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&datagram[off::MAC..off::MAC + 6]);
    Some(SearchRecord {
        kind,
        channels: datagram[off::CHANNELS],
        unknown: [
            datagram[off::UNKNOWN_A],
            datagram[off::UNKNOWN_B],
            datagram[off::UNKNOWN_C],
        ],
        mac,
        address: text(off::ADDRESS, off::PORT).parse().ok(),
        tcp_port: u16::from_le_bytes([datagram[off::PORT], datagram[off::PORT + 1]]),
        name: text(off::NAME, off::MAC_TEXT),
        uid: text(off::UID, off::TAG),
        product_tag: text(off::TAG, off::MAC_HEX),
    })
}

/// Builds a search record from its fields, laid out as devices send it.
/// For tests and mock devices.
pub fn encode_search_record(record: &SearchRecord) -> Vec<u8> {
    let mut out = vec![0u8; SEARCH_RECORD_LEN];
    out[off::KIND] = match record.kind {
        RecordKind::Camera => 1,
        RecordKind::Nvr => 2,
        RecordKind::Other(v) => v,
    };
    out[off::UNKNOWN_A] = record.unknown[0];
    out[off::UNKNOWN_B] = record.unknown[1];
    out[off::CHANNELS] = record.channels;
    out[off::UNKNOWN_C] = record.unknown[2];
    out[off::MAC..off::MAC + 6].copy_from_slice(&record.mac);
    out[off::MARKER..off::MARKER + 4].copy_from_slice(&SEARCH_REQUEST);
    let mut put = |start: usize, end: usize, s: &str| {
        let bytes = s.as_bytes();
        // Leave room for the terminating NUL.
        let n = bytes.len().min(end - start - 1);
        out[start..start + n].copy_from_slice(&bytes[..n]);
    };
    if let Some(a) = record.address {
        put(off::ADDRESS, off::PORT, &a.to_string());
    }
    put(off::NAME, off::MAC_TEXT, &record.name);
    put(off::MAC_TEXT, off::UID, &record.mac_string());
    put(off::UID, off::TAG, &record.uid);
    put(off::TAG, off::MAC_HEX, &record.product_tag);
    out[off::PORT..off::PORT + 2].copy_from_slice(&record.tcp_port.to_le_bytes());
    out
}

/// The text before the first NUL, with invalid UTF-8 replaced.
fn c_string(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).trim().to_owned()
}

/// CRC-32 with zlib's reflected polynomial but an initial value of 0 and no
/// final XOR: the probe header's checksum of the (obfuscated) body.
pub fn probe_checksum(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// Applies (or removes: the operation is its own inverse) the probe body
/// obfuscation for transaction ID `tid`.
pub fn obfuscate(body: &[u8], tid: u32) -> Vec<u8> {
    body.chunks(4)
        .enumerate()
        .flat_map(|(i, chunk)| {
            let key = PROBE_KEY[i % PROBE_KEY.len()]
                .wrapping_add(tid)
                .to_le_bytes();
            chunk
                .iter()
                .zip(key)
                .map(|(b, k)| b ^ k)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Frames an XML body as a probe datagram (header plus obfuscated body).
pub fn frame_probe_body(xml: &str, tid: u32) -> Vec<u8> {
    let body = obfuscate(xml.as_bytes(), tid);
    let mut out = Vec::with_capacity(PROBE_HEADER_LEN + body.len());
    out.extend_from_slice(&PROBE_MAGIC);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&tid.to_le_bytes());
    out.extend_from_slice(&probe_checksum(&body).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// A framed probe datagram: header fields and the de-obfuscated XML body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeFrame {
    /// Header bytes 12–15.
    pub tid: u32,
    /// The body, de-obfuscated.
    pub xml: String,
}

/// Unframes a probe datagram (either direction). `None` unless the magic,
/// length and checksum match and the body is UTF-8.
pub fn unframe_probe(datagram: &[u8]) -> Option<ProbeFrame> {
    if datagram.len() < PROBE_HEADER_LEN || datagram[..4] != PROBE_MAGIC {
        return None;
    }
    let u32_at = |i: usize| {
        u32::from_le_bytes([
            datagram[i],
            datagram[i + 1],
            datagram[i + 2],
            datagram[i + 3],
        ])
    };
    let len = u32_at(4) as usize;
    let tid = u32_at(12);
    let body = datagram.get(PROBE_HEADER_LEN..)?;
    if len > MAX_PROBE_BODY || body.len() != len || probe_checksum(body) != u32_at(16) {
        return None;
    }
    let xml = String::from_utf8(obfuscate(body, tid)).ok()?;
    Some(ProbeFrame { tid, xml })
}

/// Builds the probe the official client broadcasts to [`PROBE_PORT`]:
/// devices that answer send their [`ProbeReply`] to `reply_port`.
pub fn build_probe(tid: u32, reply_port: u16) -> Vec<u8> {
    let xml = format!("<P2P><C2D_S><to><port>{reply_port}</port></to></C2D_S></P2P>");
    frame_probe_body(&xml, tid)
}

/// A device's answer to the probe (`D2C_S_R`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeReply {
    /// The probe's transaction ID, echoed.
    pub tid: u32,
    /// `uid`, if present and non-empty.
    pub uid: Option<String>,
    /// `bat/pct`: battery charge in percent, if present.
    pub battery_percent: Option<u8>,
}

/// Parses a probe reply. `None` for anything else, including probes
/// (`C2D_S`) from other searchers.
pub fn parse_probe_reply(datagram: &[u8]) -> Option<ProbeReply> {
    let frame = unframe_probe(datagram)?;
    let fields = xml::leaf_texts(&frame.xml).ok()?;
    if !fields
        .iter()
        .any(|(path, _)| path.starts_with("P2P/D2C_S_R"))
    {
        return None;
    }
    let get = |path: &str| {
        fields
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, v)| v.as_str())
    };
    Some(ProbeReply {
        tid: frame.tid,
        uid: get("P2P/D2C_S_R/uid")
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        battery_percent: get("P2P/D2C_S_R/bat/pct")
            .and_then(|s| s.parse::<u8>().ok())
            .filter(|&p| p <= 100),
    })
}

/// Builds a probe reply as the observed battery camera sends it. For tests
/// and mock devices; `uid` must not need XML escaping.
pub fn encode_probe_reply(tid: u32, uid: &str, battery_percent: u8) -> Vec<u8> {
    let xml = format!(
        "<P2P><D2C_S_R><bat><low>0</low><pct>{battery_percent}</pct><c>0</c><p>2</p></bat>\
         <uid>{uid}</uid></D2C_S_R></P2P>"
    );
    frame_probe_body(&xml, tid)
}

/// The `port` a probe (`C2D_S`) asks devices to answer to; for mock devices.
pub fn probe_reply_port(datagram: &[u8]) -> Option<(u32, u16)> {
    let frame = unframe_probe(datagram)?;
    let fields = xml::leaf_texts(&frame.xml).ok()?;
    let port = fields
        .iter()
        .find(|(p, _)| p == "P2P/C2D_S/to/port")?
        .1
        .parse()
        .ok()?;
    Some((frame.tid, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SearchRecord {
        SearchRecord {
            kind: RecordKind::Nvr,
            channels: 16,
            unknown: [3, 7, 1],
            mac: [0x00, 0x00, 0x5e, 0x00, 0x53, 0x01],
            address: Some(Ipv4Addr::new(192, 0, 2, 10)),
            tcp_port: 9000,
            name: "NVR 1".into(),
            uid: "UID-EXAMPLE".into(),
            product_tag: String::new(),
        }
    }

    #[test]
    fn search_record_round_trip() {
        let bytes = encode_search_record(&sample());
        assert_eq!(bytes.len(), SEARCH_RECORD_LEN);
        assert_eq!(parse_search_record(&bytes), Some(sample()));
        assert_eq!(sample().mac_string(), "00:00:5e:00:53:01");
    }

    #[test]
    fn search_record_layout() {
        let bytes = encode_search_record(&sample());
        assert_eq!(bytes[50], 2);
        assert_eq!(bytes[54], 16);
        assert_eq!(&bytes[104..108], &SEARCH_REQUEST);
        assert_eq!(&bytes[108..118], b"192.0.2.10");
        assert_eq!(&bytes[128..132], &[0x28, 0x23, 0, 0]);
        assert_eq!(&bytes[132..137], b"NVR 1");
        assert_eq!(&bytes[164..181], b"00:00:5e:00:53:01");
        assert_eq!(&bytes[228..239], b"UID-EXAMPLE");
    }

    #[test]
    fn long_strings_are_cut_with_a_terminator() {
        let mut r = sample();
        r.name = "N".repeat(100);
        let parsed = parse_search_record(&encode_search_record(&r)).unwrap();
        assert_eq!(parsed.name.len(), 31);
        assert_eq!(parsed.uid, "UID-EXAMPLE");
    }

    #[test]
    fn malformed_search_records_are_rejected() {
        let good = encode_search_record(&sample());
        assert!(parse_search_record(&good[..387]).is_none());
        let mut bad = good.clone();
        bad[104] = 0;
        assert!(parse_search_record(&bad).is_none());
        assert!(parse_search_record(&SEARCH_REQUEST).is_none());
        assert!(parse_search_record(&[]).is_none());
        // A bad address field is reported as no address, not an error.
        let mut no_addr = good.clone();
        no_addr[108..128].copy_from_slice(b"not an address\0\0\0\0\0\0");
        assert_eq!(parse_search_record(&no_addr).unwrap().address, None);
        // Longer datagrams are accepted.
        let mut longer = good;
        longer.extend_from_slice(&[0u8; 16]);
        assert_eq!(parse_search_record(&longer), Some(sample()));
    }

    #[test]
    fn search_request() {
        assert!(is_search_request(&[0xaa, 0xaa, 0, 0]));
        assert!(!is_search_request(&[0xaa, 0xaa, 0]));
    }

    #[test]
    fn checksum_matches_a_reference_computation() {
        // The zero-init, no-final-XOR variant of reflected CRC-32 on the
        // standard check string (zlib's CRC-32 of it is 0xCBF43926).
        assert_eq!(probe_checksum(b""), 0);
        assert_eq!(probe_checksum(b"123456789"), 0x2DFD_2D88);
    }

    #[test]
    fn obfuscation_is_an_involution_keyed_by_tid() {
        let body = b"<P2P><C2D_S><to><port>1</port></to></C2D_S></P2P>";
        let a = obfuscate(body, 7);
        assert_ne!(&a[..], &body[..]);
        assert_eq!(obfuscate(&a, 7), body);
        assert_ne!(obfuscate(body, 8), a);
        // Word 0 is XORed with K[0] + tid, little-endian.
        let k = 0x1F2D_3C4Bu32.wrapping_add(7).to_le_bytes();
        assert_eq!(a[0], body[0] ^ k[0]);
        assert_eq!(a[3], body[3] ^ k[3]);
        // The key wraps around after eight words, with a carry across bytes.
        let long = vec![0u8; 40];
        let o = obfuscate(&long, u32::MAX);
        assert_eq!(&o[32..36], &0x1F2D_3C4Au32.to_le_bytes());
    }

    #[test]
    fn probe_layout() {
        let p = build_probe(0x8b47, 40000);
        assert_eq!(&p[..4], &PROBE_MAGIC);
        let len = u32::from_le_bytes(p[4..8].try_into().unwrap()) as usize;
        assert_eq!(len, p.len() - PROBE_HEADER_LEN);
        assert_eq!(u32::from_le_bytes(p[8..12].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(p[12..16].try_into().unwrap()), 0x8b47);
        assert_eq!(
            u32::from_le_bytes(p[16..20].try_into().unwrap()),
            probe_checksum(&p[20..])
        );
        assert_eq!(probe_reply_port(&p), Some((0x8b47, 40000)));
        // A probe is not a reply.
        assert_eq!(parse_probe_reply(&p), None);
    }

    #[test]
    fn probe_reply_round_trip() {
        let r = encode_probe_reply(1234, "UID-EXAMPLE", 87);
        assert_eq!(
            parse_probe_reply(&r),
            Some(ProbeReply {
                tid: 1234,
                uid: Some("UID-EXAMPLE".into()),
                battery_percent: Some(87),
            })
        );
    }

    #[test]
    fn malformed_probe_replies_are_rejected() {
        let good = encode_probe_reply(5, "UID-EXAMPLE", 50);
        for i in [0, 4, 12, 16, 20, good.len() - 1] {
            let mut bad = good.clone();
            bad[i] ^= 0x01;
            assert_eq!(parse_probe_reply(&bad), None, "byte {i}");
        }
        assert_eq!(parse_probe_reply(&good[..good.len() - 1]), None);
        assert_eq!(parse_probe_reply(&good[..10]), None);
        let not_xml = frame_probe_body("<P2P><D2C_S_R><uid>", 5);
        assert_eq!(parse_probe_reply(&not_xml), None);
        let other = frame_probe_body("<P2P><D2C_X/></P2P>", 5);
        assert_eq!(parse_probe_reply(&other), None);
        // Out-of-range battery values are dropped, not wrapped.
        let odd = frame_probe_body(
            "<P2P><D2C_S_R><bat><pct>300</pct></bat><uid></uid></D2C_S_R></P2P>",
            5,
        );
        assert_eq!(
            parse_probe_reply(&odd),
            Some(ProbeReply {
                tid: 5,
                uid: None,
                battery_percent: None
            })
        );
    }
}
