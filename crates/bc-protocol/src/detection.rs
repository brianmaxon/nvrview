//! Detection events (docs/protocol.md, "Detection events").
//!
//! After a client subscribes with command 192, the device pushes command 33:
//! a snapshot of every channel slot's alarm state, sent whenever any of it
//! changes. A detection starts when a slot's value leaves `none` and stops
//! when it returns to `none` in a later push; [`parse_alarm_event_list`]
//! reads one snapshot and the caller compares consecutive ones.
//!
//! Which detection types a channel supports comes from the `aitype` bitmask
//! of each channel `item` in the capabilities reply (command 199), read by
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChannelAi {
    /// `chnID` (0-based, as `channelId` elsewhere).
    pub channel: u8,
    /// `aitype`, a bitmask ([`ai_kinds`]).
    pub aitype: u32,
}

/// Reads each channel `item` that has a `chnID` and an `aitype` from a
/// `Support` reply (199). Items without them are skipped. The per-lens
/// `subItem`s of a multi-lens camera are not read; the item's own `chnID`
/// and `aitype` come before them.
pub fn parse_channel_ai(xml: &str) -> Result<Vec<ChannelAi>, XmlError> {
    Ok(records(xml, "item")?
        .iter()
        .filter_map(|f| {
            Some(ChannelAi {
                channel: field(f, "chnID")?.parse().ok()?,
                aitype: field(f, "aitype")?.parse().ok()?,
            })
        })
        .collect())
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
                    aitype: 0
                },
                ChannelAi {
                    channel: 1,
                    aitype: 7
                },
                ChannelAi {
                    channel: 2,
                    aitype: 23
                },
            ]
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
