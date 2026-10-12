//! Camera motion control: pan and tilt, preset recall, and lens zoom and
//! focus (docs/protocol.md, "Camera motion control").
//!
//! Only the forms observed from the official client are built, each from a
//! fixed template (protocol.md, "What nvrview will send"):
//!
//! - 18 `PtzControl` with `left`, `right`, `up` or `down` and a speed of 1
//!   to 64, and `stop` with speed 1 (motion);
//! - 19 `PtzPreset` with one preset whose `command` is `toPos` (recall;
//!   motion). Saving and deleting presets use the same command number with
//!   another `command` and change settings, so they are never built;
//! - 295 `StartZoomFocus` with `zoomPos` or `focusPos` and a position
//!   clamped to the range the camera reported (motion);
//! - 294 (zoom and focus ranges), 190 (the preset list) and 340 (the pan
//!   and tilt self-test state): read-only queries (viewing).
//!
//! The session ([`crate::session::Session`]) builds the messages and checks
//! them against what the device itself reported (its capabilities, lens
//! ranges and preset list); this module holds the texts and the reply
//! parsers, plus parsers of the request bodies for device simulators.

use crate::xml::{self, XML_DECLARATION, XmlError};
use std::fmt;

/// Slowest move speed.
pub const MIN_SPEED: u8 = 1;
/// Fastest move speed (the official client's slider runs 1 to 64).
pub const MAX_SPEED: u8 = 64;
/// The speed the official client sends with `stop`.
pub const STOP_SPEED: u8 = 1;

/// A pan or tilt direction. Only the four observed directions; the
/// diagonals are not sent until observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Pan left.
    Left,
    /// Pan right.
    Right,
    /// Tilt up.
    Up,
    /// Tilt down.
    Down,
}

impl Direction {
    /// Every direction.
    pub const ALL: [Direction; 4] = [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ];

    /// The `command` text.
    pub const fn name(self) -> &'static str {
        match self {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }

    /// Parses a `command` text (only the four directions).
    pub fn from_name(s: &str) -> Option<Direction> {
        Direction::ALL.into_iter().find(|d| d.name() == s)
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Which lens motor a 295 moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LensAxis {
    /// Zoom (`zoomPos`).
    Zoom,
    /// Focus (`focusPos`).
    Focus,
}

impl LensAxis {
    /// The `command` text of a 295.
    pub const fn command(self) -> &'static str {
        match self {
            LensAxis::Zoom => "zoomPos",
            LensAxis::Focus => "focusPos",
        }
    }

    /// A short name for logs and events.
    pub const fn name(self) -> &'static str {
        match self {
            LensAxis::Zoom => "zoom",
            LensAxis::Focus => "focus",
        }
    }

    /// Parses a short name (`zoom`, `focus`).
    pub fn from_name(s: &str) -> Option<LensAxis> {
        match s {
            "zoom" => Some(LensAxis::Zoom),
            "focus" => Some(LensAxis::Focus),
            _ => None,
        }
    }
}

/// One lens motor's range and position, in the camera's own steps (294).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LensRange {
    /// `minPos`.
    pub min: i32,
    /// `maxPos`.
    pub max: i32,
    /// `curPos`.
    pub cur: i32,
}

impl LensRange {
    /// `position` limited to `min..=max`.
    pub fn clamp(&self, position: i32) -> i32 {
        position.clamp(self.min.min(self.max), self.max.max(self.min))
    }
}

/// A 294 reply: the zoom and focus motors, each when reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ZoomFocus {
    /// The zoom motor.
    pub zoom: Option<LensRange>,
    /// The focus motor.
    pub focus: Option<LensRange>,
}

impl ZoomFocus {
    /// The range of one motor.
    pub fn axis(&self, axis: LensAxis) -> Option<LensRange> {
        match axis {
            LensAxis::Zoom => self.zoom,
            LensAxis::Focus => self.focus,
        }
    }
}

/// One preset of a 190 reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preset {
    /// `id` (from 0).
    pub id: u32,
    /// `name`.
    pub name: String,
}

fn wrap(inner: &str) -> String {
    format!("{XML_DECLARATION}\n<body>\n{inner}</body>\n")
}

/// Extension of a motion command (18, 19, 295): the channel only, so the
/// body goes in the payload (protocol.md, "Request shapes").
pub fn command_extension(channel: u8) -> String {
    xml::channel_extension(channel)
}

/// Extension of a read-only PTZ query (294, 190, 340): the channel and
/// `chnType` 0, no body.
pub fn query_extension(channel: u8) -> String {
    format!(
        "{XML_DECLARATION}\n<Extension version=\"1.1\">\n<channelId>{channel}</channelId>\n\
         <chnType>0</chnType>\n</Extension>\n"
    )
}

/// Payload of a move (18): `speed` is limited to 1–64.
pub fn move_body(channel: u8, direction: Direction, speed: u8) -> String {
    control_body(channel, speed.clamp(MIN_SPEED, MAX_SPEED), direction.name())
}

/// Payload of a stop (18 `stop`, speed 1 as the official client sent it).
pub fn stop_body(channel: u8) -> String {
    control_body(channel, STOP_SPEED, "stop")
}

fn control_body(channel: u8, speed: u8, command: &str) -> String {
    wrap(&format!(
        "<PtzControl version=\"1.1\">\n<channelId>{channel}</channelId>\n<speed>{speed}</speed>\n\
         <command>{command}</command>\n</PtzControl>\n"
    ))
}

/// Payload of a preset recall (19 `toPos`), with the fields the official
/// client sent: the preset's name from 190, zero coordinates, and `mode`
/// `global`. There is no way to build a save or delete.
pub fn recall_body(channel: u8, id: u32, name: &str) -> String {
    const ZERO: &str = "0.000000e+00";
    wrap(&format!(
        "<PtzPreset version=\"1.1\">\n<channelId>{channel}</channelId>\n<presetList>\n<preset>\n\
         <id>{id}</id>\n<command>toPos</command>\n<name>{}</name>\n<xpos>{ZERO}</xpos>\n\
         <ypos>{ZERO}</ypos>\n<height>{ZERO}</height>\n<width>{ZERO}</width>\n\
         <mode>global</mode>\n</preset>\n</presetList>\n</PtzPreset>\n",
        crate::archive::escape(name)
    ))
}

/// Payload of a lens move (295) to an absolute position. The caller clamps
/// it ([`crate::session::Session::ptz_lens`] does).
pub fn lens_body(channel: u8, axis: LensAxis, position: i32) -> String {
    wrap(&format!(
        "<StartZoomFocus version=\"1.1\">\n<channelId>{channel}</channelId>\n\
         <command>{}</command>\n<movePos>{position}</movePos>\n</StartZoomFocus>\n",
        axis.command()
    ))
}

fn number<T: std::str::FromStr>(
    fields: &[(String, String)],
    name: &'static str,
) -> Result<T, XmlError> {
    xml::field(fields, name)
        .ok_or(XmlError::Missing(name))?
        .parse()
        .map_err(|_| XmlError::BadNumber(name))
}

fn range(fields: &[(String, String)]) -> Result<LensRange, XmlError> {
    Ok(LensRange {
        min: number(fields, "minPos")?,
        max: number(fields, "maxPos")?,
        cur: number(fields, "curPos")?,
    })
}

/// Parses a 294 reply (`PtzZoomFocus`).
pub fn parse_zoom_focus(xml_text: &str) -> Result<ZoomFocus, XmlError> {
    let one = |name: &str| -> Result<Option<LensRange>, XmlError> {
        xml::records(xml_text, name)?
            .first()
            .map(|f| range(f))
            .transpose()
    };
    Ok(ZoomFocus {
        zoom: one("zoom")?,
        focus: one("focus")?,
    })
}

/// Parses a 190 reply (`PtzPreset`): the presets of `presetList`. Entries
/// without a numeric `id` are skipped.
pub fn parse_presets(xml_text: &str) -> Result<Vec<Preset>, XmlError> {
    Ok(xml::records(xml_text, "preset")?
        .iter()
        .filter_map(|f| {
            Some(Preset {
                id: xml::field(f, "id")?.parse().ok()?,
                name: xml::field(f, "name").unwrap_or("").to_owned(),
            })
        })
        .collect())
}

/// Parses a 340 reply (`PtzAutoTest`): `testStat`.
pub fn parse_self_test(xml_text: &str) -> Result<u32, XmlError> {
    xml::find_text(xml_text, "testStat")?
        .ok_or(XmlError::Missing("testStat"))?
        .parse()
        .map_err(|_| XmlError::BadNumber("testStat"))
}

/// A motion request body as a device reads it. For device simulators and
/// tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// 18: `channelId`, `command`, `speed`.
    Control {
        /// `channelId`.
        channel: u8,
        /// `command`, as sent.
        command: String,
        /// `speed`.
        speed: u32,
    },
    /// 19: `channelId`, and the first preset's `id` and `command`.
    Preset {
        /// `channelId`.
        channel: u8,
        /// The preset's `id`.
        id: u32,
        /// The preset's `command`, as sent.
        command: String,
        /// The preset's `name`, if any.
        name: Option<String>,
    },
    /// 295: `channelId`, `command`, `movePos`.
    Lens {
        /// `channelId`.
        channel: u8,
        /// `command`, as sent.
        command: String,
        /// `movePos`.
        position: i32,
    },
}

fn text(xml_text: &str, name: &'static str) -> Result<String, XmlError> {
    xml::find_text(xml_text, name)?.ok_or(XmlError::Missing(name))
}

fn num<T: std::str::FromStr>(xml_text: &str, name: &'static str) -> Result<T, XmlError> {
    text(xml_text, name)?
        .trim()
        .parse()
        .map_err(|_| XmlError::BadNumber(name))
}

/// Parses the payload of a motion request (18, 19 or 295) as a device
/// would. For device simulators and tests.
pub fn parse_request(command: u32, xml_text: &str) -> Result<Request, XmlError> {
    let channel = num(xml_text, "channelId")?;
    match command {
        18 => Ok(Request::Control {
            channel,
            command: text(xml_text, "command")?,
            speed: num(xml_text, "speed")?,
        }),
        19 => {
            let p = xml::records(xml_text, "preset")?;
            let f = p.first().ok_or(XmlError::Missing("preset"))?;
            Ok(Request::Preset {
                channel,
                id: number(f, "id")?,
                command: xml::field(f, "command")
                    .ok_or(XmlError::Missing("command"))?
                    .to_owned(),
                name: xml::field(f, "name").map(str::to_owned),
            })
        }
        295 => Ok(Request::Lens {
            channel,
            command: text(xml_text, "command")?,
            position: num(xml_text, "movePos")?,
        }),
        _ => Err(XmlError::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_and_stop_match_the_observed_form() {
        assert_eq!(
            move_body(3, Direction::Left, 40),
            "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<body>\n<PtzControl version=\"1.1\">\n\
             <channelId>3</channelId>\n<speed>40</speed>\n<command>left</command>\n\
             </PtzControl>\n</body>\n"
        );
        assert!(stop_body(3).contains("<speed>1</speed>\n<command>stop</command>"));
        // Speeds outside 1–64 are limited.
        assert!(move_body(0, Direction::Up, 0).contains("<speed>1</speed>"));
        assert!(move_body(0, Direction::Up, 200).contains("<speed>64</speed>"));
        for d in Direction::ALL {
            let r = parse_request(18, &move_body(7, d, 20)).unwrap();
            assert_eq!(
                r,
                Request::Control {
                    channel: 7,
                    command: d.name().to_owned(),
                    speed: 20
                }
            );
            assert_eq!(Direction::from_name(d.name()), Some(d));
        }
        // No diagonal can be named.
        assert_eq!(Direction::from_name("leftUp"), None);
        // The motion extension has no chnType (the body is the payload).
        assert!(!command_extension(3).contains("chnType"));
        assert!(query_extension(3).contains("<chnType>0</chnType>"));
    }

    #[test]
    fn recall_is_to_pos_only_with_the_observed_fields() {
        let b = recall_body(2, 5, "Gate & <path>");
        assert!(b.contains("<command>toPos</command>"));
        assert!(!b.contains("setPos") && !b.contains("delPos"));
        assert!(b.contains("<name>Gate &amp; &lt;path&gt;</name>"));
        assert!(b.contains("<xpos>0.000000e+00</xpos>") && b.contains("<mode>global</mode>"));
        assert_eq!(
            parse_request(19, &recall_body(2, 5, "Gate")).unwrap(),
            Request::Preset {
                channel: 2,
                id: 5,
                command: "toPos".to_owned(),
                name: Some("Gate".to_owned())
            }
        );
    }

    #[test]
    fn lens_body_and_replies_parse() {
        let b = lens_body(1, LensAxis::Zoom, 11);
        assert_eq!(
            parse_request(295, &b).unwrap(),
            Request::Lens {
                channel: 1,
                command: "zoomPos".to_owned(),
                position: 11
            }
        );
        let zf = parse_zoom_focus(
            "<body><PtzZoomFocus version=\"1.1\"><channelId>1</channelId>\
             <zoom><maxPos>28</maxPos><minPos>0</minPos><curPos>11</curPos></zoom>\
             <focus><maxPos>164</maxPos><minPos>0</minPos><curPos>90</curPos></focus>\
             </PtzZoomFocus></body>",
        )
        .unwrap();
        assert_eq!(
            zf.zoom,
            Some(LensRange {
                min: 0,
                max: 28,
                cur: 11
            })
        );
        assert_eq!(zf.axis(LensAxis::Focus).unwrap().max, 164);
        assert_eq!(zf.zoom.unwrap().clamp(40), 28);
        assert_eq!(zf.zoom.unwrap().clamp(-3), 0);
        let none = parse_zoom_focus("<body><PtzZoomFocus/></body>").unwrap();
        assert_eq!(none, ZoomFocus::default());
        let p = parse_presets(
            "<body><PtzPreset version=\"1.1\"><channelId>1</channelId><separateCfg><preset>0\
             </preset></separateCfg><maxPresetNum>64</maxPresetNum><presetList>\
             <preset><id>0</id><name>Door</name><imageName>a</imageName></preset>\
             <preset><id>3</id><name>Yard</name></preset></presetList></PtzPreset></body>",
        )
        .unwrap();
        // The separateCfg `preset` has no id element and is skipped.
        assert_eq!(
            p,
            vec![
                Preset {
                    id: 0,
                    name: "Door".to_owned()
                },
                Preset {
                    id: 3,
                    name: "Yard".to_owned()
                }
            ]
        );
        assert_eq!(
            parse_self_test("<body><PtzAutoTest><testStat>0</testStat></PtzAutoTest></body>"),
            Ok(0)
        );
    }
}
