//! XML bodies the client sends, and the few values parsed from replies.

use crate::stream::StreamTier;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::fmt;

/// XML declaration that starts every body.
pub const XML_DECLARATION: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>"#;

/// Preview policy: which frame the device starts the stream with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewPolicy {
    /// `newestIFrame`: start at the most recent keyframe.
    NewestIFrame,
}

impl PreviewPolicy {
    /// Name used in the `policy` XML element.
    pub const fn name(self) -> &'static str {
        match self {
            PreviewPolicy::NewestIFrame => "newestIFrame",
        }
    }
}

fn wrap(inner: &str) -> String {
    format!("{XML_DECLARATION}\n<body>\n{inner}</body>\n")
}

/// Login body from the already salted hashes (see
/// [`crate::crypto::credential_hash`]). The caller obfuscates it.
pub fn login_body(user_hash: &str, password_hash: &str) -> String {
    wrap(&format!(
        "<LoginUser version=\"1.1\">\n<userName>{user_hash}</userName>\n<password>{password_hash}</password>\n\
         <userVer>1</userVer>\n</LoginUser>\n<LoginNet version=\"1.1\">\n<type>LAN</type>\n<udpPort>0</udpPort>\n</LoginNet>\n"
    ))
}

/// Body of a logout (command 2): no credentials.
pub fn logout_body() -> String {
    wrap("<LoginUser version=\"1.1\">\n<userVer>1</userVer>\n</LoginUser>\n")
}

/// Body of a preview start (command 3).
pub fn preview_start_body(channel: u8, tier: StreamTier, policy: PreviewPolicy) -> String {
    wrap(&format!(
        "<Preview version=\"1.1\">\n<channelId>{channel}</channelId>\n<handle>{}</handle>\n\
         <streamType>{}</streamType>\n<policy>{}</policy>\n</Preview>\n",
        tier.handle(channel),
        tier.name(),
        policy.name()
    ))
}

/// Body of a preview stop (command 4).
pub fn preview_stop_body(channel: u8, tier: StreamTier) -> String {
    wrap(&format!(
        "<Preview version=\"1.1\">\n<channelId>{channel}</channelId>\n<handle>{}</handle>\n</Preview>\n",
        tier.handle(channel)
    ))
}

/// Extension for per-channel queries (command 318).
pub fn channel_extension(channel: u8) -> String {
    format!(
        "{XML_DECLARATION}\n<Extension version=\"1.1\">\n<channelId>{channel}</channelId>\n</Extension>\n"
    )
}

/// XML parse failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XmlError {
    /// The text is not well-formed enough to read.
    Malformed,
    /// A required element is missing.
    Missing(&'static str),
    /// An element holds a value that is not a valid number.
    BadNumber(&'static str),
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XmlError::Malformed => write!(f, "malformed XML"),
            XmlError::Missing(t) => write!(f, "missing element <{t}>"),
            XmlError::BadNumber(t) => write!(f, "element <{t}> is not a number"),
        }
    }
}

impl std::error::Error for XmlError {}

/// Text of the first element named `tag` (trimmed), `Ok(None)` if absent.
pub fn find_text(xml: &str, tag: &str) -> Result<Option<String>, XmlError> {
    let mut reader = Reader::from_str(xml);
    let mut inside = false;
    let mut text = String::new();
    loop {
        match reader.read_event().map_err(|_| XmlError::Malformed)? {
            Event::Start(e) if !inside && e.local_name().as_ref() == tag.as_bytes() => {
                inside = true;
            }
            Event::Empty(e) if !inside && e.local_name().as_ref() == tag.as_bytes() => {
                return Ok(Some(String::new()));
            }
            Event::Text(t) if inside => {
                text.push_str(&t.xml10_content().map_err(|_| XmlError::Malformed)?);
            }
            Event::End(_) if inside => return Ok(Some(text.trim().to_owned())),
            Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

fn find_number(xml: &str, tag: &'static str) -> Result<Option<u32>, XmlError> {
    match find_text(xml, tag)? {
        None => Ok(None),
        Some(s) => s.parse().map(Some).map_err(|_| XmlError::BadNumber(tag)),
    }
}

/// The nonce from the device's `<Encryption>` reply.
pub fn parse_nonce(xml: &str) -> Result<String, XmlError> {
    find_text(xml, "nonce")?
        .filter(|n| !n.is_empty())
        .ok_or(XmlError::Missing("nonce"))
}

/// `<remainTimes>` from a failed login's `<LoginErrInfo>`, if present.
pub fn parse_remain_times(xml: &str) -> Result<Option<u32>, XmlError> {
    find_number(xml, "remainTimes")
}

/// `<unlockTime>` (seconds until a lockout ends) from a failed login's
/// `<LoginErrInfo>`, if present.
pub fn parse_unlock_time(xml: &str) -> Result<Option<u32>, XmlError> {
    find_number(xml, "unlockTime")
}

/// What a failed login's status means (docs/protocol.md, "Login failures
/// and limits"), for messages to the user.
pub fn login_status_meaning(status: u16) -> &'static str {
    match status {
        401 => "wrong user name or password",
        406 => "login refused",
        407 => "account locked",
        408 => "device not initialized",
        409 => "too many users online",
        410 => "too many client addresses connected",
        411..=418 => "certificate, token, or password-reset condition",
        419 => "device occupied",
        501 => "device not authorized",
        _ => "login refused",
    }
}

/// Values read from an `<Extension>` on a media message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extension {
    /// `encryptLen`: how many leading payload bytes are encrypted.
    pub encrypt_len: Option<u32>,
    /// Whether `binaryData` is present and non-zero.
    pub binary_data: bool,
    /// `channelId`, if present.
    pub channel_id: Option<u32>,
}

/// Parses an extension; unknown elements are ignored.
pub fn parse_extension(xml: &str) -> Result<Extension, XmlError> {
    Ok(Extension {
        encrypt_len: find_number(xml, "encryptLen")?,
        binary_data: find_number(xml, "binaryData")?.is_some_and(|v| v != 0),
        channel_id: find_number(xml, "channelId")?,
    })
}

/// Collects, for every element named `record`, the text of the leaf
/// elements inside it (first occurrence of each name wins).
fn records(xml: &str, record: &str) -> Result<Vec<Vec<(String, String)>>, XmlError> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut current: Option<Vec<(String, String)>> = None;
    let mut leaf: Option<(String, String)> = None;
    loop {
        match reader.read_event().map_err(|_| XmlError::Malformed)? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if name == record {
                    current = Some(Vec::new());
                } else if current.is_some() {
                    leaf = Some((name, String::new()));
                }
            }
            Event::Text(t) => {
                if let Some((_, text)) = leaf.as_mut() {
                    text.push_str(&t.xml10_content().map_err(|_| XmlError::Malformed)?);
                }
            }
            Event::End(e) => {
                if e.local_name().as_ref() == record.as_bytes() {
                    out.extend(current.take());
                } else if let (Some(fields), Some((name, text))) = (current.as_mut(), leaf.take())
                    && !fields.iter().any(|(n, _)| *n == name)
                {
                    fields.push((name, text.trim().to_owned()));
                }
            }
            Event::Eof => return Ok(out),
            _ => {}
        }
    }
}

fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// One channel slot from a `ChannelInfoList` (command 145).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelInfo {
    /// Channel number (0-based).
    pub channel: u8,
    /// Whether a camera occupies the slot (`state` is `connect`).
    pub connected: bool,
    /// Stream tiers the channel offers, from `streamSupport`.
    pub tiers: Vec<StreamTier>,
}

/// Parses a `ChannelInfoList`. Entries without a valid `channelId` are
/// skipped; unknown tier names are ignored.
pub fn parse_channel_info_list(xml: &str) -> Result<Vec<ChannelInfo>, XmlError> {
    Ok(records(xml, "ChannelInfo")?
        .iter()
        .filter_map(|f| {
            let channel = field(f, "channelId")?.parse().ok()?;
            Some(ChannelInfo {
                channel,
                connected: field(f, "state") == Some("connect"),
                tiers: field(f, "streamSupport")
                    .unwrap_or("")
                    .split(',')
                    .filter_map(|t| StreamTier::from_name(t.trim()))
                    .collect(),
            })
        })
        .collect())
}

/// Name and model from a `VersionInfo` reply: the device's own (command 80)
/// or a channel's (command 318).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionInfo {
    /// `channelId`, present in per-channel replies.
    pub channel: Option<u8>,
    /// User-assigned name.
    pub name: String,
    /// Model (`type`).
    pub model: String,
    /// `firmwareVersion`, if present.
    pub firmware: Option<String>,
}

/// Parses a `VersionInfo` reply.
pub fn parse_version_info(xml: &str) -> Result<VersionInfo, XmlError> {
    let all = records(xml, "VersionInfo")?;
    let f = all.first().ok_or(XmlError::Missing("VersionInfo"))?;
    Ok(VersionInfo {
        channel: field(f, "channelId").and_then(|c| c.parse().ok()),
        name: field(f, "name").unwrap_or_default().to_owned(),
        model: field(f, "type").unwrap_or_default().to_owned(),
        firmware: field(f, "firmwareVersion").map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders() {
        let s = preview_start_body(2, StreamTier::Sub, PreviewPolicy::NewestIFrame);
        assert!(s.starts_with(XML_DECLARATION));
        assert!(s.contains("<body>") && s.contains("</body>"));
        assert_eq!(find_text(&s, "channelId").unwrap().as_deref(), Some("2"));
        assert_eq!(find_text(&s, "handle").unwrap().as_deref(), Some("258"));
        assert_eq!(
            find_text(&s, "streamType").unwrap().as_deref(),
            Some("subStream")
        );
        assert_eq!(
            find_text(&s, "policy").unwrap().as_deref(),
            Some("newestIFrame")
        );
        let s = preview_stop_body(2, StreamTier::Sub);
        assert_eq!(find_text(&s, "handle").unwrap().as_deref(), Some("258"));
        assert_eq!(find_text(&s, "policy").unwrap(), None);
        let s = login_body("AAA", "BBB");
        assert_eq!(find_text(&s, "userName").unwrap().as_deref(), Some("AAA"));
        assert_eq!(find_text(&s, "password").unwrap().as_deref(), Some("BBB"));
        assert_eq!(find_text(&s, "userVer").unwrap().as_deref(), Some("1"));
        assert_eq!(find_text(&s, "type").unwrap().as_deref(), Some("LAN"));
    }

    #[test]
    fn parsing() {
        let nonce = "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<body>\n<Encryption version=\"1.1\">\n\
                     <type>md5</type>\n<nonce>example-nonce-1</nonce>\n</Encryption>\n</body>";
        assert_eq!(parse_nonce(nonce).unwrap(), "example-nonce-1");
        assert_eq!(parse_nonce("<body/>"), Err(XmlError::Missing("nonce")));
        let err = "<body><LoginErrInfo><remainTimes>3</remainTimes></LoginErrInfo></body>";
        assert_eq!(parse_remain_times(err).unwrap(), Some(3));
        assert_eq!(parse_remain_times("<body/>").unwrap(), None);
        let locked = "<body><LoginErrInfo><remainTimes>0</remainTimes><unlockTime>300</unlockTime></LoginErrInfo></body>";
        assert_eq!(parse_unlock_time(locked).unwrap(), Some(300));
        assert_eq!(parse_unlock_time(err).unwrap(), None);
        assert_eq!(login_status_meaning(409), "too many users online");
        let s = logout_body();
        assert!(s.contains("<LoginUser") && s.contains("<userVer>1</userVer>"));
        assert_eq!(find_text(&s, "userName").unwrap(), None);
        assert_eq!(find_text(&s, "password").unwrap(), None);
        let ext = "<Extension version=\"1.1\"><binaryData>1</binaryData><encryptLen>1024</encryptLen></Extension>";
        assert_eq!(
            parse_extension(ext).unwrap(),
            Extension {
                encrypt_len: Some(1024),
                binary_data: true,
                channel_id: None
            }
        );
        assert_eq!(
            parse_extension("<Extension/>").unwrap(),
            Extension::default()
        );
        assert_eq!(
            parse_extension("<Extension><encryptLen>x</encryptLen></Extension>"),
            Err(XmlError::BadNumber("encryptLen"))
        );
        assert!(find_text("<a><b></a>", "b").is_err());
    }

    #[test]
    fn channel_info_list() {
        let xml = "<body><ChannelInfoList version=\"1.1\">\
                   <ChannelInfo><channelId>0</channelId><state>connect</state>\
                   <uid>UID-EXAMPLE</uid><streamSupport>mainStream,subStream,externStream</streamSupport></ChannelInfo>\
                   <ChannelInfo><channelId>1</channelId><state>none</state></ChannelInfo>\
                   <ChannelInfo><channelId>x</channelId><state>connect</state></ChannelInfo>\
                   </ChannelInfoList></body>";
        let list = parse_channel_info_list(xml).unwrap();
        assert_eq!(
            list,
            vec![
                ChannelInfo {
                    channel: 0,
                    connected: true,
                    tiers: vec![StreamTier::Main, StreamTier::Sub, StreamTier::Extern],
                },
                ChannelInfo {
                    channel: 1,
                    connected: false,
                    tiers: vec![],
                },
            ]
        );
        assert_eq!(parse_channel_info_list("<body/>").unwrap(), vec![]);
        assert!(parse_channel_info_list("<a><b></a>").is_err());
    }

    #[test]
    fn version_info() {
        let xml = "<body><VersionInfo version=\"1.1\"><channelId>3</channelId>\
                   <name>Example camera</name><type>EXAMPLE-MODEL</type>\
                   <detail><name>ignored</name></detail></VersionInfo></body>";
        assert_eq!(
            parse_version_info(xml).unwrap(),
            VersionInfo {
                channel: Some(3),
                name: "Example camera".into(),
                model: "EXAMPLE-MODEL".into(),
                firmware: None,
            }
        );
        assert_eq!(
            parse_version_info("<body/>"),
            Err(XmlError::Missing("VersionInfo"))
        );
    }
}
