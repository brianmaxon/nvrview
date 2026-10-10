//! Detection events (docs/protocol.md, "Detection events").
//!
//! After a client subscribes with command 192, the device pushes command 33:
//! a snapshot of every channel slot's alarm state, sent whenever any of it
//! changes. A detection starts when a slot's value leaves `none` and stops
//! when it returns to `none` in a later push; [`parse_alarm_event_list`]
//! reads one snapshot and the caller compares consecutive ones.
//!
//! Which detection types a channel supports comes from the `aitype` bitmask
//! of each channel `item` in the capabilities reply (command 199), and per
//! lens from the `subItem`s of a multi-lens camera, read by
//! [`parse_channel_ai`] and named by [`ai_kinds`].

use crate::xml::{XmlError, field, records};

/// One channel slot's state in an alarm-state push (command 33).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct AlarmState {
    /// `channelId` (0-based).
    pub channel: u8,
    /// `subChannel`: the lens of a multi-lens camera (0 when absent).
    pub sub_channel: u8,
    /// The names in `status` other than `none` (such as `MD` for motion),
    /// split at commas, in the order given.
    pub status: Vec<String>,
    /// The names in `AItype` other than `none` (such as `people`), split at
    /// commas, in the order given.
    pub ai_types: Vec<String>,
}

impl AlarmState {
    /// Whether the slot reports motion (`MD` in `status`, any case).
    pub fn motion(&self) -> bool {
        self.status.iter().any(|s| s.eq_ignore_ascii_case("MD"))
    }

    /// Nothing is active on this slot.
    pub fn is_idle(&self) -> bool {
        self.status.is_empty() && self.ai_types.is_empty()
    }
}

/// Splits a `status` or `AItype` value into its names, leaving out `none`
/// and empty parts. Devices may list several names separated by commas.
fn names(value: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in value.unwrap_or("").split(',') {
        let part = part.trim();
        if part.is_empty() || part.eq_ignore_ascii_case("none") {
            continue;
        }
        if !out.iter().any(|n| n.eq_ignore_ascii_case(part)) {
            out.push(part.to_owned());
        }
    }
    out
}

/// Parses an `AlarmEventList` (command 33). Entries without a valid
/// `channelId` are skipped; unknown elements are ignored.
pub fn parse_alarm_event_list(xml: &str) -> Result<Vec<AlarmState>, XmlError> {
    Ok(records(xml, "AlarmEvent")?
        .iter()
        .filter_map(|f| {
            Some(AlarmState {
                channel: field(f, "channelId")?.parse().ok()?,
                sub_channel: field(f, "subChannel")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                status: names(field(f, "status")),
                ai_types: names(field(f, "AItype")),
            })
        })
        .collect())
}

/// One channel's AI capabilities from the capabilities reply (199).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelAi {
    /// `chnID` (0-based, as `channelId` elsewhere).
    pub channel: u8,
    /// The item's own `aitype`, a bitmask ([`ai_kinds`]).
    pub aitype: u32,
    /// The lenses of a multi-lens camera, from the item's `subItem`s (each
    /// with its own `chnID` and `aitype`), by lens number; empty when the
    /// item has none (a single-lens camera, or older firmware).
    pub lenses: Vec<LensAi>,
}

/// One lens of a multi-lens camera in the capabilities reply (199).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LensAi {
    /// The `subItem`'s `chnID`: 0 for the first lens, 1 for the second
    /// (apparently the lens that 33 calls `subChannel`).
    pub lens: u8,
    /// The lens's `aitype`.
    pub aitype: u32,
}

type Fields = Vec<(String, String)>;

/// Reads each channel `item` that has a `chnID` and an `aitype` from a
/// `Support` reply (199), with the per-lens `subItem`s of a multi-lens
/// camera (docs/protocol.md, "Which channels detect what"). Items without
/// them are skipped, and so are sub-items without them.
pub fn parse_channel_ai(xml: &str) -> Result<Vec<ChannelAi>, XmlError> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    // The open item's own fields and its finished sub-items, the open
    // sub-item's fields, and the open leaf element.
    let mut item: Option<(Fields, Vec<Fields>)> = None;
    let mut sub: Option<Fields> = None;
    let mut leaf: Option<(String, String)> = None;
    loop {
        match reader.read_event().map_err(|_| XmlError::Malformed)? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if item.is_none() {
                    if name == "item" {
                        item = Some((Vec::new(), Vec::new()));
                    }
                } else if name == "subItem" && sub.is_none() {
                    sub = Some(Vec::new());
                } else {
                    leaf = Some((name, String::new()));
                }
            }
            Event::Text(t) => {
                if let Some((_, text)) = leaf.as_mut() {
                    text.push_str(&t.xml10_content().map_err(|_| XmlError::Malformed)?);
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                let name = name.as_ref();
                if name == b"subItem" && sub.is_some() {
                    if let (Some(s), Some((_, subs))) = (sub.take(), item.as_mut()) {
                        subs.push(s);
                    }
                } else if name == b"item" && sub.is_none() {
                    if let Some((fields, subs)) = item.take() {
                        out.extend(channel_ai(&fields, &subs));
                    }
                } else if let Some((n, text)) = leaf.take() {
                    let fields = match (sub.as_mut(), item.as_mut()) {
                        (Some(s), _) => Some(s),
                        (None, Some((f, _))) => Some(f),
                        _ => None,
                    };
                    if let Some(f) = fields
                        && !f.iter().any(|(x, _)| *x == n)
                    {
                        f.push((n, text.trim().to_owned()));
                    }
                }
            }
            Event::Eof => return Ok(out),
            _ => {}
        }
    }
}

fn channel_ai(item: &[(String, String)], subs: &[Fields]) -> Option<ChannelAi> {
    let mut lenses: Vec<LensAi> = subs
        .iter()
        .filter_map(|s| {
            Some(LensAi {
                lens: field(s, "chnID")?.parse().ok()?,
                aitype: field(s, "aitype")?.parse().ok()?,
            })
        })
        .collect();
    lenses.sort_by_key(|l| l.lens);
    lenses.dedup_by_key(|l| l.lens);
    Some(ChannelAi {
        channel: field(item, "chnID")?.parse().ok()?,
        aitype: field(item, "aitype")?.parse().ok()?,
        lenses,
    })
}

/// `aitype` bits that name a detection type, with the name the alarm-state
/// pushes use for it (reported (client), docs/protocol.md). Bit 0 means the
/// channel has AI at all.
const AI_BITS: [(u32, &str); 6] = [
    (1, "people"),
    (2, "vehicle"),
    (3, "face"),
    (4, "dog_cat"),
    (17, "package"),
    (22, "cry"),
];

/// The detection types an `aitype` bitmask advertises, as the names the
/// alarm-state pushes use (`people`, `vehicle`, `face`, `dog_cat`,
/// `package`, `cry`). Empty when bit 0 (AI present) is clear. Bits without
/// a known push name (6 "other", 23 user-defined objects) and the bits of
/// configuration features are left out.
pub fn ai_kinds(aitype: u32) -> Vec<&'static str> {
    if aitype & 1 == 0 {
        return Vec::new();
    }
    AI_BITS
        .iter()
        .filter(|(bit, _)| aitype & (1 << bit) != 0)
        .map(|(_, name)| *name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alarm_snapshot_with_and_without_sub_channels() {
        let xml = "<body><AlarmEventList version=\"1.1\">\
            <AlarmEvent version=\"1.1\"><channelId>0</channelId><status>none</status>\
            <AItype>none</AItype><recording>0</recording><timeStamp>0</timeStamp></AlarmEvent>\
            <AlarmEvent version=\"1.1\"><channelId>1</channelId><status>MD</status>\
            <AItype>people</AItype><recording>0</recording><timeStamp>0</timeStamp></AlarmEvent>\
            <AlarmEvent version=\"1.1\"><channelId>2</channelId><subChannel>1</subChannel>\
            <status>none</status><AItype>dog_cat, people,People</AItype></AlarmEvent>\
            <AlarmEvent version=\"1.1\"><channelId>x</channelId><status>MD</status></AlarmEvent>\
            <range><start>0</start><end>2</end></range></AlarmEventList></body>";
        let list = parse_alarm_event_list(xml).unwrap();
        assert_eq!(list.len(), 3);
        assert!(list[0].is_idle() && !list[0].motion());
        assert_eq!(list[1].channel, 1);
        assert!(list[1].motion());
        assert_eq!(list[1].ai_types, vec!["people"]);
        assert_eq!(list[2].sub_channel, 1);
        assert!(!list[2].motion());
        assert_eq!(list[2].ai_types, vec!["dog_cat", "people"]);
        assert_eq!(parse_alarm_event_list("<body/>").unwrap(), vec![]);
        assert!(parse_alarm_event_list("<a><b></a>").is_err());
    }

    #[test]
    fn status_lists_split_at_commas() {
        let xml = "<AlarmEventList><AlarmEvent><channelId>3</channelId>\
                   <status>IO,md,visitor</status><AItype></AItype></AlarmEvent></AlarmEventList>";
        let s = &parse_alarm_event_list(xml).unwrap()[0];
        assert_eq!(s.status, vec!["IO", "md", "visitor"]);
        assert!(s.motion());
        assert!(s.ai_types.is_empty());
    }

    #[test]
    fn channel_ai_bits() {
        let xml = "<body><Support version=\"1.1\"><channelNum>3</channelNum>\
            <item><chnID>0</chnID><aitype>0</aitype><aiAnimalType>0</aiAnimalType></item>\
            <item><chnID>1</chnID><aitype>7</aitype></item>\
            <item><chnID>2</chnID><aitype>23</aitype>\
            <subItem><chnID>0</chnID><aitype>7</aitype></subItem>\
            <subItem><chnID>1</chnID><aitype>1</aitype></subItem></item>\
            <item><chnID>9</chnID></item></Support></body>";
        let ai = parse_channel_ai(xml).unwrap();
        assert_eq!(
            ai,
            vec![
                ChannelAi {
                    channel: 0,
                    aitype: 0,
                    lenses: vec![]
                },
                ChannelAi {
                    channel: 1,
                    aitype: 7,
                    lenses: vec![]
                },
                ChannelAi {
                    channel: 2,
                    aitype: 23,
                    lenses: vec![LensAi { lens: 0, aitype: 7 }, LensAi { lens: 1, aitype: 1 }]
                },
            ]
        );
        // Sub-items in another order, one incomplete, other fields among
        // them: lenses by number, the item's own fields untouched.
        let xml = "<body><Support><item><chnID>4</chnID>\
            <subItem><chnID>1</chnID><aitype>3</aitype><other>2</other></subItem>\
            <subItem><aitype>5</aitype></subItem>\
            <subItem><chnID>0</chnID><aitype>23</aitype></subItem>\
            <aitype>19</aitype></item></Support></body>";
        let ai = parse_channel_ai(xml).unwrap();
        assert_eq!(ai.len(), 1);
        assert_eq!((ai[0].channel, ai[0].aitype), (4, 19));
        assert_eq!(
            ai[0]
                .lenses
                .iter()
                .map(|l| (l.lens, l.aitype))
                .collect::<Vec<_>>(),
            vec![(0, 23), (1, 3)]
        );
        assert!(ai_kinds(0).is_empty());
        assert_eq!(ai_kinds(7), vec!["people", "vehicle"]);
        assert_eq!(ai_kinds(23), vec!["people", "vehicle", "dog_cat"]);
        // Without bit 0 (AI present) nothing is advertised.
        assert!(ai_kinds(6).is_empty());
        assert_eq!(
            ai_kinds(1 | (1 << 17) | (1 << 22) | (1 << 3) | (1 << 7)),
            vec!["face", "package", "cry"]
        );
    }
}
