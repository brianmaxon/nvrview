//! ONVIF WS-Discovery (the public WS-Discovery 2005/04 specification as
//! profiled by ONVIF Core): a SOAP `Probe` multicast over UDP, answered with
//! unicast `ProbeMatches` that list each device's service addresses
//! (`XAddrs`) and scopes. Sans-I/O.

use std::net::{Ipv4Addr, SocketAddrV4};

use crate::xml;

/// The WS-Discovery multicast group.
pub const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
/// The WS-Discovery port.
pub const PORT: u16 = 3702;
/// Where probes are sent.
pub const MULTICAST_TARGET: SocketAddrV4 = SocketAddrV4::new(MULTICAST_ADDR, PORT);

/// Largest datagram this module reads.
pub const MAX_DATAGRAM: usize = 65_507;

/// The ONVIF device types a probe can ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeType {
    /// `NetworkVideoTransmitter`: cameras and encoders.
    NetworkVideoTransmitter,
    /// `Device`: any ONVIF device, recorders included.
    Device,
}

/// Builds a `Probe` with `message_id` (a `uuid:` URN, see
/// [`new_message_id`]).
pub fn build_probe(message_id: &str, types: ProbeType) -> String {
    let (ns, ty) = match types {
        ProbeType::NetworkVideoTransmitter => (
            "http://www.onvif.org/ver10/network/wsdl",
            "NetworkVideoTransmitter",
        ),
        ProbeType::Device => ("http://www.onvif.org/ver10/device/wsdl", "Device"),
    };
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<e:Envelope xmlns:e="http://www.w3.org/2003/05/soap-envelope""#,
            r#" xmlns:w="http://schemas.xmlsoap.org/ws/2004/08/addressing""#,
            r#" xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery""#,
            r#" xmlns:t="{ns}">"#,
            r#"<e:Header>"#,
            r#"<w:MessageID>{id}</w:MessageID>"#,
            r#"<w:To e:mustUnderstand="true">urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To>"#,
            r#"<w:Action e:mustUnderstand="true">http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action>"#,
            r#"</e:Header>"#,
            r#"<e:Body><d:Probe><d:Types>t:{ty}</d:Types></d:Probe></e:Body>"#,
            r#"</e:Envelope>"#
        ),
        ns = ns,
        id = message_id,
        ty = ty,
    )
}

/// A fresh `uuid:` URN (random version 4 UUID) for a probe's MessageID.
pub fn new_message_id() -> String {
    let mut b = crate::runner::random_bytes::<16>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "uuid:{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// One `ProbeMatch`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeMatch {
    /// `EndpointReference/Address`: the device's stable ID (often a
    /// `urn:uuid:`). Identifying; keep internal.
    pub endpoint: Option<String>,
    /// `Types`, space-separated QNames, prefixes dropped.
    pub types: Vec<String>,
    /// `Scopes`, as URIs.
    pub scopes: Vec<String>,
    /// `XAddrs`: the device service URLs.
    pub xaddrs: Vec<String>,
}

impl ProbeMatch {
    /// The value of the first ONVIF scope `onvif://www.onvif.org/<key>/<value>`,
    /// percent-decoded.
    pub fn scope(&self, key: &str) -> Option<String> {
        let prefix = format!("onvif://www.onvif.org/{key}/");
        self.scopes
            .iter()
            .find_map(|s| s.strip_prefix(&prefix))
            .map(percent_decode)
            .filter(|v| !v.is_empty())
    }

    /// The device name (`name` scope).
    pub fn name(&self) -> Option<String> {
        self.scope("name")
    }

    /// The model (`hardware` scope).
    pub fn model(&self) -> Option<String> {
        self.scope("hardware")
    }
}

/// A parsed `ProbeMatches` message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeMatches {
    /// `RelatesTo`: the MessageID of the probe answered, if present.
    pub relates_to: Option<String>,
    /// The matches.
    pub matches: Vec<ProbeMatch>,
}

/// Parses a `ProbeMatches` datagram. `None` for anything else (a `Probe`
/// from another searcher, a `Hello`, malformed XML).
pub fn parse_probe_matches(datagram: &[u8]) -> Option<ProbeMatches> {
    if datagram.len() > MAX_DATAGRAM {
        return None;
    }
    let text = std::str::from_utf8(datagram).ok()?;
    let fields = xml::leaf_texts(text).ok()?;
    let mut out = ProbeMatches::default();
    let mut current = ProbeMatch::default();
    let mut any = false;
    for (path, value) in fields {
        let mut parts = path.rsplit('/');
        let leaf = parts.next().unwrap_or_default();
        let parent = parts.next().unwrap_or_default();
        match (parent, leaf) {
            ("Header", "RelatesTo") => out.relates_to = Some(value),
            ("EndpointReference", "Address") if path.contains("ProbeMatch/") => {
                current.endpoint = Some(value).filter(|v| !v.is_empty());
            }
            ("ProbeMatch", "Types") => {
                current.types = value
                    .split_whitespace()
                    .map(|q| q.rsplit(':').next().unwrap_or(q).to_owned())
                    .collect();
            }
            ("ProbeMatch", "Scopes") => {
                current.scopes = value.split_whitespace().map(str::to_owned).collect();
            }
            ("ProbeMatch", "XAddrs") => {
                current.xaddrs = value.split_whitespace().map(str::to_owned).collect();
            }
            ("ProbeMatches", "ProbeMatch") => {
                out.matches.push(std::mem::take(&mut current));
            }
            ("Body", "ProbeMatches") => any = true,
            _ => {}
        }
    }
    any.then_some(out)
}

/// Host and port of an `http://host:port/...` URL with an IPv4 host. The
/// port defaults to 80 (443 for https).
pub fn url_host_port(url: &str) -> Option<(Ipv4Addr, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    match authority.split_once(':') {
        Some((host, port)) => Some((host.parse().ok()?, port.parse().ok()?)),
        None => Some((authority.parse().ok()?, default)),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(v) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Builds a `ProbeMatches` answer with one match. For tests and mock
/// devices; values must not need XML escaping.
pub fn encode_probe_matches(relates_to: &str, m: &ProbeMatch) -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<SOAP-ENV:Envelope xmlns:SOAP-ENV="http://www.w3.org/2003/05/soap-envelope""#,
            r#" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing""#,
            r#" xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery""#,
            r#" xmlns:dn="http://www.onvif.org/ver10/network/wsdl">"#,
            r#"<SOAP-ENV:Header>"#,
            r#"<wsa:MessageID>uuid:00000000-0000-4000-8000-000000000001</wsa:MessageID>"#,
            r#"<wsa:RelatesTo>{rel}</wsa:RelatesTo>"#,
            r#"<wsa:To>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</wsa:To>"#,
            r#"<wsa:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/ProbeMatches</wsa:Action>"#,
            r#"</SOAP-ENV:Header>"#,
            r#"<SOAP-ENV:Body><d:ProbeMatches><d:ProbeMatch>"#,
            r#"<wsa:EndpointReference><wsa:Address>{ep}</wsa:Address></wsa:EndpointReference>"#,
            r#"<d:Types>{types}</d:Types>"#,
            r#"<d:Scopes>{scopes}</d:Scopes>"#,
            r#"<d:XAddrs>{xaddrs}</d:XAddrs>"#,
            r#"<d:MetadataVersion>1</d:MetadataVersion>"#,
            r#"</d:ProbeMatch></d:ProbeMatches></SOAP-ENV:Body></SOAP-ENV:Envelope>"#
        ),
        rel = relates_to,
        ep = m.endpoint.as_deref().unwrap_or_default(),
        types = m
            .types
            .iter()
            .map(|t| format!("dn:{t}"))
            .collect::<Vec<_>>()
            .join(" "),
        scopes = m.scopes.join(" "),
        xaddrs = m.xaddrs.join(" "),
    )
}

/// The MessageID of a `Probe` datagram, for mock devices.
pub fn probe_message_id(datagram: &[u8]) -> Option<String> {
    let fields = xml::leaf_texts(std::str::from_utf8(datagram).ok()?).ok()?;
    if !fields.iter().any(|(p, _)| p.ends_with("Body/Probe")) {
        return None;
    }
    fields
        .into_iter()
        .find(|(p, _)| p.ends_with("Header/MessageID"))
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ProbeMatch {
        ProbeMatch {
            endpoint: Some("urn:uuid:00000000-0000-4000-8000-00000000abcd".into()),
            types: vec!["NetworkVideoTransmitter".into()],
            scopes: vec![
                "onvif://www.onvif.org/type/video_encoder".into(),
                "onvif://www.onvif.org/name/Camera%201".into(),
                "onvif://www.onvif.org/hardware/Model-X".into(),
            ],
            xaddrs: vec!["http://192.0.2.20:8000/onvif/device_service".into()],
        }
    }

    #[test]
    fn probe_is_well_formed_and_carries_the_id() {
        let id = new_message_id();
        assert!(id.starts_with("uuid:") && id.len() == 41, "{id}");
        assert_eq!(id.as_bytes()[19], b'4');
        let p = build_probe(&id, ProbeType::NetworkVideoTransmitter);
        assert_eq!(probe_message_id(p.as_bytes()), Some(id.clone()));
        let fields = xml::leaf_texts(&p).unwrap();
        assert!(fields.contains(&(
            "Envelope/Body/Probe/Types".into(),
            "t:NetworkVideoTransmitter".into()
        )));
        let d = build_probe(&id, ProbeType::Device);
        assert!(d.contains("t:Device<"));
        // A probe is not a ProbeMatches.
        assert_eq!(parse_probe_matches(p.as_bytes()), None);
    }

    #[test]
    fn probe_matches_round_trip() {
        let s = encode_probe_matches("uuid:rel", &sample());
        let pm = parse_probe_matches(s.as_bytes()).unwrap();
        assert_eq!(pm.relates_to.as_deref(), Some("uuid:rel"));
        assert_eq!(pm.matches, vec![sample()]);
        assert_eq!(pm.matches[0].name().as_deref(), Some("Camera 1"));
        assert_eq!(pm.matches[0].model().as_deref(), Some("Model-X"));
    }

    #[test]
    fn several_matches_and_other_prefixes() {
        let x = concat!(
            r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:a="x" xmlns:d="y">"#,
            r#"<s:Header><a:RelatesTo>uuid:r</a:RelatesTo></s:Header><s:Body><d:ProbeMatches>"#,
            r#"<d:ProbeMatch><a:EndpointReference><a:Address>urn:a</a:Address></a:EndpointReference>"#,
            r#"<d:XAddrs>http://192.0.2.1/onvif/device_service http://[2001:db8::1]/x</d:XAddrs></d:ProbeMatch>"#,
            r#"<d:ProbeMatch><a:EndpointReference><a:Address>urn:b</a:Address></a:EndpointReference>"#,
            r#"<d:XAddrs>http://192.0.2.2:8080/onvif/device_service</d:XAddrs></d:ProbeMatch>"#,
            r#"</d:ProbeMatches></s:Body></s:Envelope>"#
        );
        let pm = parse_probe_matches(x.as_bytes()).unwrap();
        assert_eq!(pm.matches.len(), 2);
        assert_eq!(pm.matches[0].endpoint.as_deref(), Some("urn:a"));
        assert_eq!(pm.matches[0].xaddrs.len(), 2);
        assert_eq!(pm.matches[1].endpoint.as_deref(), Some("urn:b"));
    }

    #[test]
    fn malformed_and_foreign_datagrams_are_ignored() {
        assert_eq!(parse_probe_matches(b""), None);
        assert_eq!(parse_probe_matches(b"\xff\xfe"), None);
        assert_eq!(parse_probe_matches(b"<a><b></a>"), None);
        assert_eq!(
            parse_probe_matches(b"<Envelope><Body><Hello/></Body></Envelope>"),
            None
        );
        let s = encode_probe_matches("uuid:rel", &sample());
        assert_eq!(parse_probe_matches(&s.as_bytes()[..s.len() - 3]), None);
    }

    #[test]
    fn urls() {
        let ip = Ipv4Addr::new(192, 0, 2, 5);
        assert_eq!(url_host_port("http://192.0.2.5/onvif"), Some((ip, 80)));
        assert_eq!(url_host_port("https://192.0.2.5"), Some((ip, 443)));
        assert_eq!(url_host_port("http://192.0.2.5:8000/x?y"), Some((ip, 8000)));
        assert_eq!(url_host_port("http://u:p@192.0.2.5:81/"), Some((ip, 81)));
        assert_eq!(url_host_port("http://[2001:db8::1]/x"), None);
        assert_eq!(url_host_port("http://camera.example/x"), None);
        assert_eq!(url_host_port("rtsp://192.0.2.5/"), None);
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("a%zz"), "a%zz");
        assert_eq!(percent_decode("%C3%A9"), "\u{e9}");
    }
}
