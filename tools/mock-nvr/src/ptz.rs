//! A camera that moves: pan and tilt, presets, and lens zoom and focus, as
//! observed from the official client (docs/protocol.md, "Camera motion
//! control").
//!
//! Channels with a [`PtzCamera`] in [`crate::Config::ptz`] answer:
//!
//! - 18 (`PtzControl`): a direction starts a continuous move and `stop` ends
//!   it, each with an empty status-200 reply. The camera keeps moving until
//!   a stop arrives, across disconnects too (the protocol's presumed
//!   behavior, which is what makes a missing stop dangerous);
//! - 19 (`PtzPreset`) with `toPos`: jumps to the preset, empty status 200;
//! - 295 (`StartZoomFocus`): sets the zoom or focus position, empty status
//!   200;
//! - 294, 190, 340: the zoom and focus ranges, the preset list, and the
//!   self-test state, in the observed reply shapes.
//!
//! Every PTZ request, accepted or not, is logged ([`PtzLogEntry`]) so tests
//! can check what was sent and in what order. What a real device answers to
//! a command it does not support, a diagonal, an out-of-range position, a
//! preset save or delete, or an unknown preset is not known; the mock
//! answers status 400 to each (its own choice), and counts a save or delete
//! as a configuration request ([`PtzBoard::config_requests`]) that nvrview
//! must never send.

use bc_protocol::ptz::{self, Direction, LensRange};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use tokio::time::Instant;

/// Most PTZ requests remembered by [`PtzBoard::log`].
const LOG_MAX: usize = 4096;
/// Pan and tilt, in the mock's own degrees per second, per unit of speed.
const DEGREES_PER_SECOND_PER_SPEED: f64 = 1.0;

/// One preset of a mock camera.
#[derive(Clone, Debug, PartialEq)]
pub struct MockPreset {
    /// `id` (from 0).
    pub id: u32,
    /// `name`.
    pub name: String,
    /// Where a recall points the camera: pan and tilt.
    pub pan: f64,
    /// Tilt.
    pub tilt: f64,
}

/// What a mock camera can do and where it starts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PtzCamera {
    /// It pans and tilts (18, 340).
    pub pan_tilt: bool,
    /// Its zoom motor (294, 295), with the start position in `cur`.
    pub zoom: Option<LensRange>,
    /// Its focus motor.
    pub focus: Option<LensRange>,
    /// Its presets (190, 19).
    pub presets: Vec<MockPreset>,
}

impl PtzCamera {
    /// A pan-and-tilt camera with a 1000–6000 zoom, a 1–100 focus (the
    /// ranges observed on a PTZ lens), and `presets` named presets at
    /// made-up positions.
    pub fn pan_tilt_zoom(presets: u32) -> PtzCamera {
        PtzCamera {
            pan_tilt: true,
            zoom: Some(LensRange {
                min: 1000,
                max: 6000,
                cur: 1000,
            }),
            focus: Some(LensRange {
                min: 1,
                max: 100,
                cur: 50,
            }),
            presets: (0..presets)
                .map(|id| MockPreset {
                    id,
                    name: format!("Preset {}", id + 1),
                    pan: f64::from(id) * 30.0,
                    tilt: -10.0,
                })
                .collect(),
        }
    }

    /// A camera with only a motorized zoom and focus lens, 0–28 and 0–164
    /// (the ranges observed on such a camera).
    pub fn lens_only() -> PtzCamera {
        PtzCamera {
            pan_tilt: false,
            zoom: Some(LensRange {
                min: 0,
                max: 28,
                cur: 0,
            }),
            focus: Some(LensRange {
                min: 0,
                max: 164,
                cur: 80,
            }),
            presets: Vec::new(),
        }
    }

    /// The 199 `ptzType` this camera reports: 3 when it pans and tilts, 1
    /// with only a lens, else 0.
    pub fn ptz_type(&self) -> u32 {
        if self.pan_tilt {
            3
        } else if self.zoom.is_some() || self.focus.is_some() {
            1
        } else {
            0
        }
    }

    /// The 199 `ptzPreset` this camera reports (bit 0: presets).
    pub fn ptz_preset(&self) -> u32 {
        u32::from(!self.presets.is_empty())
    }
}

/// A continuous move in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moving {
    /// The direction.
    pub direction: Direction,
    /// The speed (1–64).
    pub speed: u32,
    /// When it started.
    pub since: Instant,
}

/// Where a mock camera points and whether it is moving.
#[derive(Clone, Debug, PartialEq)]
pub struct PtzState {
    /// Pan, degrees (wraps at 360).
    pub pan: f64,
    /// Tilt, degrees (-90 to 90).
    pub tilt: f64,
    /// Zoom position, if it has a zoom.
    pub zoom: Option<i32>,
    /// Focus position, if it has a focus motor.
    pub focus: Option<i32>,
    /// The move in progress.
    pub moving: Option<Moving>,
    /// The last preset recalled.
    pub last_preset: Option<u32>,
    /// Moves started so far.
    pub moves: u64,
    /// Stops received so far.
    pub stops: u64,
}

impl PtzState {
    fn new(cam: &PtzCamera) -> PtzState {
        PtzState {
            pan: 0.0,
            tilt: 0.0,
            zoom: cam.zoom.map(|r| r.cur),
            focus: cam.focus.map(|r| r.cur),
            moving: None,
            last_preset: None,
            moves: 0,
            stops: 0,
        }
    }

    /// Brings the position up to `now`.
    fn settle(&mut self, now: Instant) {
        let Some(m) = self.moving.as_mut() else {
            return;
        };
        let dt = now.saturating_duration_since(m.since).as_secs_f64();
        let d = dt * f64::from(m.speed) * DEGREES_PER_SECOND_PER_SPEED;
        match m.direction {
            Direction::Left => self.pan = (self.pan - d).rem_euclid(360.0),
            Direction::Right => self.pan = (self.pan + d).rem_euclid(360.0),
            Direction::Up => self.tilt = (self.tilt + d).min(90.0),
            Direction::Down => self.tilt = (self.tilt - d).max(-90.0),
        }
        m.since = now;
    }
}

/// What a logged request was.
#[derive(Clone, Debug, PartialEq)]
pub enum PtzRequest {
    /// A motion request (18, 19, 295), as parsed.
    Motion(ptz::Request),
    /// A read-only query (294, 190, 340).
    Query(u32),
    /// A body the mock could not read.
    Unreadable(u32),
}

/// One PTZ request the mock received.
#[derive(Clone, Debug, PartialEq)]
pub struct PtzLogEntry {
    /// When it arrived.
    pub at: Instant,
    /// The connection (one per login).
    pub connection: u64,
    /// The command number.
    pub command: u32,
    /// The channel it named, if readable.
    pub channel: Option<u8>,
    /// What it asked.
    pub request: PtzRequest,
    /// The status answered.
    pub status: u16,
}

impl PtzLogEntry {
    /// The 18 `command` text (a direction or `stop`), if this is an 18.
    pub fn control(&self) -> Option<&str> {
        match &self.request {
            PtzRequest::Motion(ptz::Request::Control { command, .. }) => Some(command),
            _ => None,
        }
    }
}

struct Inner {
    state: HashMap<u8, PtzState>,
    log: VecDeque<PtzLogEntry>,
    config_requests: u64,
    status: u16,
}

/// Every mock camera's motion state, shared by all connections (the
/// cameras do not care which session moved them).
pub struct PtzBoard {
    cameras: HashMap<u8, PtzCamera>,
    inner: Mutex<Inner>,
}

impl PtzBoard {
    pub(crate) fn new(cameras: HashMap<u8, PtzCamera>) -> PtzBoard {
        let state = cameras
            .iter()
            .map(|(c, cam)| (*c, PtzState::new(cam)))
            .collect();
        PtzBoard {
            cameras,
            inner: Mutex::new(Inner {
                state,
                log: VecDeque::new(),
                config_requests: 0,
                status: 200,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A camera's state now, if the channel has one.
    /// The position is brought up to now on a copy, so a caller on
    /// another clock (a test thread outside a simulated runtime) cannot
    /// disturb the camera's own.
    pub fn state(&self, channel: u8) -> Option<PtzState> {
        let mut s = self.lock().state.get(&channel)?.clone();
        s.settle(Instant::now());
        Some(s)
    }

    /// Every PTZ request so far (the latest [`LOG_MAX`]), oldest first.
    pub fn log(&self) -> Vec<PtzLogEntry> {
        self.lock().log.iter().cloned().collect()
    }

    /// Preset saves or deletes received (configuration; must stay 0).
    pub fn config_requests(&self) -> u64 {
        self.lock().config_requests
    }

    /// Answers later motion requests (18 directions, 19, 295) with this
    /// status instead of acting (200 acts). A stop is always obeyed.
    pub fn set_status(&self, status: u16) {
        self.lock().status = status;
    }

    /// Handles one PTZ request: `channel` from the extension, `body` the
    /// decrypted payload. Returns the status and the reply's inner XML.
    pub(crate) fn handle(
        &self,
        connection: u64,
        command: u32,
        channel: Option<u8>,
        body: &str,
    ) -> (u16, String) {
        let now = Instant::now();
        let mut g = self.lock();
        let (request, status, reply) = match command {
            18 | 19 | 295 => match ptz::parse_request(command, body) {
                Ok(r) => {
                    let (status, reply) = self.motion(&mut g, &r, now);
                    (PtzRequest::Motion(r), status, reply)
                }
                Err(_) => (PtzRequest::Unreadable(command), 400, String::new()),
            },
            _ => {
                let (status, reply) = self.query(&mut g, command, channel, now);
                (PtzRequest::Query(command), status, reply)
            }
        };
        let channel = match &request {
            PtzRequest::Motion(
                ptz::Request::Control { channel, .. }
                | ptz::Request::Preset { channel, .. }
                | ptz::Request::Lens { channel, .. },
            ) => Some(*channel),
            _ => channel,
        };
        if g.log.len() >= LOG_MAX {
            g.log.pop_front();
        }
        g.log.push_back(PtzLogEntry {
            at: now,
            connection,
            command,
            channel,
            request,
            status,
        });
        (status, reply)
    }

    fn motion(&self, g: &mut Inner, r: &ptz::Request, now: Instant) -> (u16, String) {
        let forced = g.status;
        match r {
            ptz::Request::Control {
                channel,
                command,
                speed,
            } => {
                if !self.cameras.get(channel).is_some_and(|c| c.pan_tilt) {
                    return (400, String::new());
                }
                let st = g.state.get_mut(channel).expect("state per camera");
                st.settle(now);
                if command == "stop" {
                    st.moving = None;
                    st.stops += 1;
                    return (200, String::new());
                }
                if forced != 200 {
                    return (forced, String::new());
                }
                match Direction::from_name(command) {
                    Some(direction) if (1..=64).contains(speed) => {
                        st.moving = Some(Moving {
                            direction,
                            speed: *speed,
                            since: now,
                        });
                        st.moves += 1;
                        (200, String::new())
                    }
                    _ => (400, String::new()),
                }
            }
            ptz::Request::Preset {
                channel,
                id,
                command,
                ..
            } => {
                if command != "toPos" {
                    g.config_requests += 1;
                    return (400, String::new());
                }
                let Some(p) = self
                    .cameras
                    .get(channel)
                    .and_then(|c| c.presets.iter().find(|p| p.id == *id))
                else {
                    return (400, String::new());
                };
                if forced != 200 {
                    return (forced, String::new());
                }
                let st = g.state.get_mut(channel).expect("state per camera");
                st.settle(now);
                st.moving = None;
                st.pan = p.pan;
                st.tilt = p.tilt;
                st.last_preset = Some(*id);
                (200, String::new())
            }
            ptz::Request::Lens {
                channel,
                command,
                position,
            } => {
                let Some(cam) = self.cameras.get(channel) else {
                    return (400, String::new());
                };
                let (range, zoom) = match command.as_str() {
                    "zoomPos" => (cam.zoom, true),
                    "focusPos" => (cam.focus, false),
                    _ => return (400, String::new()),
                };
                if !range.is_some_and(|r| (r.min..=r.max).contains(position)) {
                    return (400, String::new());
                }
                if forced != 200 {
                    return (forced, String::new());
                }
                let st = g.state.get_mut(channel).expect("state per camera");
                if zoom {
                    st.zoom = Some(*position);
                } else {
                    st.focus = Some(*position);
                }
                (200, String::new())
            }
        }
    }

    fn query(
        &self,
        g: &mut Inner,
        command: u32,
        channel: Option<u8>,
        now: Instant,
    ) -> (u16, String) {
        let Some((ch, cam)) = channel.and_then(|c| self.cameras.get(&c).map(|cam| (c, cam))) else {
            return (400, String::new());
        };
        let st = g.state.get_mut(&ch).expect("state per camera");
        st.settle(now);
        match command {
            294 if cam.zoom.is_some() || cam.focus.is_some() => {
                let motor = |name: &str, r: Option<LensRange>, cur: Option<i32>| {
                    r.map(|r| {
                        format!(
                            "<{name}>\n<maxPos>{}</maxPos>\n<minPos>{}</minPos>\n<curPos>{}</curPos>\n</{name}>\n",
                            r.max,
                            r.min,
                            cur.unwrap_or(r.cur)
                        )
                    })
                    .unwrap_or_default()
                };
                (
                    200,
                    format!(
                        "<PtzZoomFocus version=\"1.1\">\n<channelId>{ch}</channelId>\n{}{}</PtzZoomFocus>\n",
                        motor("zoom", cam.zoom, st.zoom),
                        motor("focus", cam.focus, st.focus)
                    ),
                )
            }
            190 if !cam.presets.is_empty() => {
                let mut list = String::new();
                for p in &cam.presets {
                    list.push_str(&format!(
                        "<preset>\n<id>{}</id>\n<name>{}</name>\n<imageName></imageName>\n</preset>\n",
                        p.id, p.name
                    ));
                }
                (
                    200,
                    format!(
                        "<PtzPreset version=\"1.1\">\n<channelId>{ch}</channelId>\n<separateCfg>\n\
                         <preset>0</preset>\n</separateCfg>\n<maxPresetNum>64</maxPresetNum>\n\
                         <maxPresetPicNum>64</maxPresetPicNum>\n<presetList>\n{list}</presetList>\n\
                         </PtzPreset>\n"
                    ),
                )
            }
            340 if cam.pan_tilt => (
                200,
                "<PtzAutoTest version=\"1.1\">\n<testStat>0</testStat>\n</PtzAutoTest>\n"
                    .to_owned(),
            ),
            _ => (400, String::new()),
        }
    }
}
