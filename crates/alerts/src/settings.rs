//! Settings: what the user chose, with the owner-approved defaults, and
//! validation that clamps every value into a safe range instead of failing.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::model::{CameraId, Class, Exposure, Mode, Severity};
use crate::sun::in_daily_window;
use crate::time::{MINUTE_MS, Millis, SECOND_MS};
use crate::trigger::valid_source_key;
use crate::zone::{Polygon, Zone};

/// The settings document (`alerts.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertSettings {
    pub version: u32,
    /// Per-camera settings; cameras not listed use [`CameraSettings::new`].
    pub cameras: Vec<CameraSettings>,
    pub always_alert: AlwaysAlertSettings,
    pub mode: Mode,
    /// A coarse location (rounded to 0.1°) for sunrise and sunset; `None`
    /// uses the fixed night hours.
    pub location: Option<Location>,
    pub night: NightSettings,
    /// External trigger sources and the camera angles that cover them.
    pub sources: Vec<SourceMapping>,
    pub timing: TimingSettings,
    /// Checks of a camera's own detections.
    pub verification: VerifySettings,
    /// The lowest severity that alerts; anything lower goes to the quiet log.
    pub min_alert_severity: Severity,
    pub outputs: Vec<OutputSettings>,
    pub standby: StandbySettings,
    pub learning: LearningSettings,
}

impl Default for AlertSettings {
    fn default() -> Self {
        AlertSettings {
            version: 1,
            cameras: Vec::new(),
            always_alert: AlwaysAlertSettings::default(),
            mode: Mode::Home,
            location: None,
            night: NightSettings::default(),
            sources: Vec::new(),
            timing: TimingSettings::default(),
            verification: VerifySettings::default(),
            min_alert_severity: Severity::Normal,
            outputs: vec![OutputSettings::desktop()],
            standby: StandbySettings::default(),
            learning: LearningSettings::default(),
        }
    }
}

/// Settings for one camera angle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraSettings {
    pub camera: CameraId,
    /// The user's own name for the camera, shown in explanations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub exposure: Exposure,
    /// Marked as an entrance (the same mark the alert insets use).
    #[serde(default)]
    pub entrance: bool,
    /// "Trust the camera's AI": when our check disagrees with the camera on
    /// an always-alert case, alert anyway, marked unverified.
    #[serde(default)]
    pub trust_camera_ai: bool,
    /// Classes that may alert on this camera.
    #[serde(default = "default_classes")]
    pub classes: Vec<Class>,
    #[serde(default)]
    pub zones: Vec<Zone>,
    #[serde(default)]
    pub schedule: Schedule,
    /// Whether device motion without a class asks our detector to look.
    #[serde(default)]
    pub verify_motion: bool,
}

fn default_classes() -> Vec<Class> {
    vec![
        Class::Person,
        Class::Vehicle,
        Class::Animal,
        Class::Package,
        Class::Visitor,
        Class::Smoke,
        Class::Co,
    ]
}

impl CameraSettings {
    pub fn new(camera: CameraId) -> Self {
        CameraSettings {
            camera,
            name: None,
            exposure: Exposure::Outside,
            entrance: false,
            trust_camera_ai: false,
            classes: default_classes(),
            zones: Vec::new(),
            schedule: Schedule::Always,
            verify_motion: false,
        }
    }

    /// The user's name, or "Camera N" (channel numbers count from 1), with
    /// the lens for a second lens.
    pub fn display_name(&self) -> String {
        match &self.name {
            Some(n) => n.clone(),
            None if self.camera.lens > 0 => format!(
                "Camera {} lens {}",
                u32::from(self.camera.channel) + 1,
                u32::from(self.camera.lens) + 1
            ),
            None => format!("Camera {}", u32::from(self.camera.channel) + 1),
        }
    }

    pub fn class_enabled(&self, class: Class) -> bool {
        self.classes.contains(&class)
    }
}

/// When a camera may alert (always-alert rules ignore it).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schedule {
    #[default]
    Always,
    /// Only at night (by the night rule in force).
    Night,
    /// Only within these local-time windows.
    Windows { windows: Vec<DailyWindow> },
}

/// A daily local-time window, [start, end), wrapping at midnight when
/// start > end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyWindow {
    /// Bit 0 Monday through bit 6 Sunday; the day the window starts on.
    #[serde(default = "all_days")]
    pub days: u8,
    pub start_min: u16,
    pub end_min: u16,
}

fn all_days() -> u8 {
    0x7f
}

impl DailyWindow {
    /// Whether a local minute on a weekday (0 = Monday) is in the window.
    /// For a window crossing midnight the early-morning part belongs to the
    /// previous day's bit.
    pub fn contains(&self, weekday: u8, minute: u16) -> bool {
        if !in_daily_window(minute, self.start_min, self.end_min) {
            return false;
        }
        let day = if self.start_min > self.end_min && minute < self.end_min {
            (weekday + 6) % 7
        } else {
            weekday
        };
        self.days & (1 << day) != 0
    }
}

/// Which always-alert rules are on. Nothing learned, no feedback, no model,
/// and no schedule can lower these; only an explicit, time-limited mute of
/// one camera stops them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlwaysAlertSettings {
    pub smoke_co: bool,
    pub night_outside_person: bool,
    pub doorbell_night: bool,
}

impl Default for AlwaysAlertSettings {
    fn default() -> Self {
        AlwaysAlertSettings {
            smoke_co: true,
            night_outside_person: true,
            doorbell_night: true,
        }
    }
}

/// A coarse location: rounded to 0.1° (about 11 km), stored locally.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NightSettings {
    pub after_sunset_min: u16,
    pub before_sunrise_min: u16,
    /// Fixed night hours when no location is set (local minutes).
    pub fallback_start_min: u16,
    pub fallback_end_min: u16,
}

impl Default for NightSettings {
    fn default() -> Self {
        NightSettings {
            after_sunset_min: 30,
            before_sunrise_min: 30,
            fallback_start_min: 22 * 60,
            fallback_end_min: 6 * 60,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimingSettings {
    /// Cooldown after an episode's first alert.
    pub initial_cooldown_s: u32,
    /// Cooldown after each later alert while activity continues.
    pub escalated_cooldown_s: u32,
    /// A trigger that arrives later than this after the event is late.
    pub stale_after_s: u32,
    /// An episode with no activity for this long settles.
    pub settle_after_s: u32,
    /// A pending episode with no decision for this long is dropped.
    pub pending_timeout_s: u32,
    /// "Still present" re-check period and limit (used by the host).
    pub recheck_period_s: u32,
    pub recheck_limit_s: u32,
    /// Consecutive re-check misses that settle an episode.
    pub settle_misses: u8,
}

impl Default for TimingSettings {
    fn default() -> Self {
        TimingSettings {
            initial_cooldown_s: 60,
            escalated_cooldown_s: 300,
            stale_after_s: 30,
            settle_after_s: 120,
            pending_timeout_s: 30,
            recheck_period_s: 10,
            recheck_limit_s: 600,
            settle_misses: 2,
        }
    }
}

impl TimingSettings {
    pub fn initial_cooldown_ms(&self) -> Millis {
        Millis::from(self.initial_cooldown_s) * SECOND_MS
    }
    pub fn escalated_cooldown_ms(&self) -> Millis {
        Millis::from(self.escalated_cooldown_s) * SECOND_MS
    }
    pub fn stale_ms(&self) -> Millis {
        Millis::from(self.stale_after_s) * SECOND_MS
    }
    pub fn settle_ms(&self) -> Millis {
        Millis::from(self.settle_after_s) * SECOND_MS
    }
    pub fn pending_timeout_ms(&self) -> Millis {
        Millis::from(self.pending_timeout_s) * SECOND_MS
    }
}

/// How a camera's own detection is checked against our detector.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VerifySettings {
    /// Pictures with a hit needed (k in k-of-n); 1 picture is "weak".
    pub k: u8,
    /// The detector score floor (per-class, day and night thresholds come
    /// with the verification slice).
    pub min_score: f32,
    /// Pictures from this long before the trigger count (a pre-trigger
    /// picture from the recording).
    pub window_before_s: u32,
    /// Pictures up to this long after the trigger count.
    pub window_after_s: u32,
}

impl Default for VerifySettings {
    fn default() -> Self {
        VerifySettings {
            k: 2,
            min_score: 0.5,
            window_before_s: 2,
            window_after_s: 10,
        }
    }
}

/// What an external trigger needs before it may alert.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Require {
    /// A mapped camera must see an allowed class in the region (default).
    #[default]
    Verified,
    /// The sensor alone may alert (an explicit per-source choice).
    None,
}

/// What happens when no picture could be checked (cameras offline, the
/// detector timed out).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailPolicy {
    /// Quiet log: "sensor fired, could not look" (default).
    #[default]
    Quiet,
    /// Alert, marked unverified.
    AlertUnverified,
}

/// One external source and the camera angles that cover its area.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceMapping {
    /// The key the source posts as (`[a-z0-9_-]`, at most 32 bytes).
    pub key: String,
    /// The user's own name for it, shown in explanations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub cameras: Vec<SourceCamera>,
    #[serde(default = "default_before")]
    pub window_before_ms: Millis,
    #[serde(default = "default_after")]
    pub window_after_ms: Millis,
    /// Retriggers extend the window, up to this long after the first.
    #[serde(default = "default_max")]
    pub window_max_ms: Millis,
    #[serde(default = "default_source_classes")]
    pub classes: Vec<Class>,
    #[serde(default)]
    pub require: Require,
    /// Weighted hits needed (k in k-of-n).
    #[serde(default = "default_k")]
    pub k: f32,
    #[serde(default = "default_min_score")]
    pub min_score: f32,
    #[serde(default)]
    pub on_unavailable: FailPolicy,
}

fn default_before() -> Millis {
    2 * SECOND_MS
}
fn default_after() -> Millis {
    8 * SECOND_MS
}
fn default_max() -> Millis {
    30 * SECOND_MS
}
fn default_source_classes() -> Vec<Class> {
    vec![Class::Person, Class::Vehicle, Class::Animal]
}
fn default_k() -> f32 {
    2.0
}
fn default_min_score() -> f32 {
    0.5
}

impl SourceMapping {
    pub fn new(key: &str, cameras: Vec<SourceCamera>) -> Self {
        SourceMapping {
            key: key.to_owned(),
            name: None,
            cameras,
            window_before_ms: default_before(),
            window_after_ms: default_after(),
            window_max_ms: default_max(),
            classes: default_source_classes(),
            require: Require::Verified,
            k: default_k(),
            min_score: default_min_score(),
            on_unavailable: FailPolicy::Quiet,
        }
    }

    /// The user's name, or "Sensor N" by position (set by the caller).
    pub fn display_name(&self, index: usize) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("Sensor {}", index + 1))
    }
}

/// One camera angle that covers a source, with its region of interest (an
/// empty polygon means the whole picture) and its weight in k-of-n.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceCamera {
    pub camera: CameraId,
    #[serde(default)]
    pub roi: Polygon,
    #[serde(default = "one")]
    pub weight: f32,
}

fn one() -> f32 {
    1.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    Desktop,
    /// A self-hosted ntfy server.
    Ntfy,
    /// The user's own MQTT broker.
    Mqtt,
    /// A generic outgoing webhook.
    Webhook,
    /// A Home Assistant instance on the local network (an output module
    /// the host registers; see docs/NOTIFICATIONS.md, "Delivery").
    HomeAssistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputSettings {
    pub kind: OutputKind,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default = "normal")]
    pub min_severity: Severity,
    /// Attach a picture. On by default (the owner's choice); the settings
    /// screen warns when the endpoint may be outside the local network.
    #[serde(default = "yes")]
    pub pictures: bool,
}

fn yes() -> bool {
    true
}
fn normal() -> Severity {
    Severity::Normal
}

impl OutputSettings {
    pub fn desktop() -> Self {
        OutputSettings {
            kind: OutputKind::Desktop,
            enabled: true,
            endpoint: None,
            min_severity: Severity::Normal,
            pictures: true,
        }
    }

    /// Whether this output sends pictures to an endpoint that may be
    /// outside the local network, so the settings screen should warn.
    pub fn picture_warning(&self) -> bool {
        self.pictures
            && self.kind != OutputKind::Desktop
            && self
                .endpoint
                .as_deref()
                .is_some_and(endpoint_may_leave_network)
    }
}

/// Whether an endpoint URL's host may be outside the local network: true
/// unless it is a loopback, private, or link-local address, a `.local`,
/// `.lan`, or `.home.arpa` name, or a single-label name. A heuristic for a
/// warning only.
pub fn endpoint_may_leave_network(endpoint: &str) -> bool {
    let rest = endpoint.split_once("://").map_or(endpoint, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    }
    .to_ascii_lowercase();
    if host.is_empty() {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                !(v4.is_loopback() || v4.is_private() || v4.is_link_local())
            }
            std::net::IpAddr::V6(v6) => {
                let seg0 = v6.segments()[0];
                !(v6.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80)
            }
        };
    }
    let local_suffix = [".local", ".lan", ".home.arpa", ".internal"]
        .iter()
        .any(|s| host.ends_with(s));
    !(local_suffix || host == "localhost" || !host.contains('.'))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StandbySettings {
    /// Switch to standby automatically while on battery power.
    pub auto_on_battery: bool,
    /// The most cameras one device wakes at once for checks.
    pub max_wake_per_device: u8,
}

impl Default for StandbySettings {
    fn default() -> Self {
        StandbySettings {
            auto_on_battery: false,
            max_wake_per_device: 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LearningSettings {
    pub enabled: bool,
    /// A low-severity event this rare ("about 1 in N" slots) is promoted.
    pub promote_one_in: u32,
    /// A normal-severity event at least this common is demoted.
    pub demote_one_in: u32,
}

impl Default for LearningSettings {
    fn default() -> Self {
        LearningSettings {
            enabled: true,
            promote_one_in: 20,
            demote_one_in: 2,
        }
    }
}

/// Something validation changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Issue {
    LocationRounded,
    LocationClamped,
    ValueClamped(&'static str),
    NameCleaned,
    DuplicateCamera(CameraId),
    PolygonCleaned,
    BadSourceKey(usize),
    DuplicateSource(String),
    SourceWithoutCameras(String),
    EmptyEndpoint(usize),
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Issue::LocationRounded => f.write_str("location rounded to 0.1 degree"),
            Issue::LocationClamped => f.write_str("location clamped to valid degrees"),
            Issue::ValueClamped(what) => write!(f, "{what} clamped to its allowed range"),
            Issue::NameCleaned => f.write_str("a name was trimmed or shortened"),
            Issue::DuplicateCamera(c) => write!(
                f,
                "duplicate settings for device {} channel {} lens {} (first kept)",
                c.device, c.channel, c.lens
            ),
            Issue::PolygonCleaned => f.write_str("a zone or region was clamped to the picture"),
            Issue::BadSourceKey(i) => write!(f, "source {} has an invalid key (removed)", i + 1),
            Issue::DuplicateSource(k) => write!(f, "duplicate source {k} (first kept)"),
            Issue::SourceWithoutCameras(k) => write!(f, "source {k} has no cameras"),
            Issue::EmptyEndpoint(i) => write!(f, "output {} needs an endpoint (disabled)", i + 1),
        }
    }
}

/// The longest kept name, in characters.
pub const MAX_NAME_CHARS: usize = 40;

fn clean_name(name: &mut Option<String>, issues: &mut Vec<Issue>) {
    if let Some(n) = name {
        let cleaned: String = n
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
            .trim()
            .chars()
            .take(MAX_NAME_CHARS)
            .collect();
        if cleaned != *n {
            issues.push(Issue::NameCleaned);
        }
        *name = if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        };
    }
}

fn clamp_u32(v: &mut u32, lo: u32, hi: u32, what: &'static str, issues: &mut Vec<Issue>) {
    let c = (*v).clamp(lo, hi);
    if c != *v {
        *v = c;
        issues.push(Issue::ValueClamped(what));
    }
}

fn clamp_u16(v: &mut u16, lo: u16, hi: u16, what: &'static str, issues: &mut Vec<Issue>) {
    let c = (*v).clamp(lo, hi);
    if c != *v {
        *v = c;
        issues.push(Issue::ValueClamped(what));
    }
}

fn clamp_i64(v: &mut i64, lo: i64, hi: i64, what: &'static str, issues: &mut Vec<Issue>) {
    let c = (*v).clamp(lo, hi);
    if c != *v {
        *v = c;
        issues.push(Issue::ValueClamped(what));
    }
}

fn clamp_f32(v: &mut f32, lo: f32, hi: f32, what: &'static str, issues: &mut Vec<Issue>) {
    let c = if v.is_finite() { v.clamp(lo, hi) } else { lo };
    if c != *v {
        *v = c;
        issues.push(Issue::ValueClamped(what));
    }
}

fn round_tenth(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

impl AlertSettings {
    /// Settings for a camera, or the defaults for an unlisted one.
    pub fn camera(&self, camera: CameraId) -> CameraSettings {
        self.cameras
            .iter()
            .find(|c| c.camera == camera)
            .cloned()
            .unwrap_or_else(|| CameraSettings::new(camera))
    }

    /// The mapping for a source key, with its position.
    pub fn source(&self, key: &str) -> Option<(usize, &SourceMapping)> {
        self.sources.iter().enumerate().find(|(_, s)| s.key == key)
    }

    /// Clamps every value into its allowed range and drops what cannot be
    /// used. Never fails; lists what it changed.
    pub fn validated(mut self) -> (AlertSettings, Vec<Issue>) {
        let mut issues = Vec::new();
        self.version = 1;

        if let Some(loc) = &mut self.location {
            if !loc.lat.is_finite() || !loc.lon.is_finite() {
                self.location = None;
                issues.push(Issue::LocationClamped);
            } else {
                let lat = loc.lat.clamp(-90.0, 90.0);
                let lon = loc.lon.clamp(-180.0, 180.0);
                if lat != loc.lat || lon != loc.lon {
                    issues.push(Issue::LocationClamped);
                }
                let (rl, rn) = (round_tenth(lat), round_tenth(lon));
                if (rl - lat).abs() > 1e-9 || (rn - lon).abs() > 1e-9 {
                    issues.push(Issue::LocationRounded);
                }
                loc.lat = rl;
                loc.lon = rn;
            }
        }

        let n = &mut self.night;
        clamp_u16(
            &mut n.after_sunset_min,
            0,
            180,
            "night margin after sunset",
            &mut issues,
        );
        clamp_u16(
            &mut n.before_sunrise_min,
            0,
            180,
            "night margin before sunrise",
            &mut issues,
        );
        clamp_u16(
            &mut n.fallback_start_min,
            0,
            1439,
            "fallback night start",
            &mut issues,
        );
        clamp_u16(
            &mut n.fallback_end_min,
            0,
            1439,
            "fallback night end",
            &mut issues,
        );

        let t = &mut self.timing;
        clamp_u32(
            &mut t.initial_cooldown_s,
            10,
            3600,
            "initial cooldown",
            &mut issues,
        );
        let lo = t.initial_cooldown_s;
        clamp_u32(
            &mut t.escalated_cooldown_s,
            lo,
            3600,
            "escalated cooldown",
            &mut issues,
        );
        clamp_u32(&mut t.stale_after_s, 5, 600, "staleness bound", &mut issues);
        clamp_u32(&mut t.settle_after_s, 20, 3600, "settle time", &mut issues);
        clamp_u32(
            &mut t.pending_timeout_s,
            5,
            120,
            "pending timeout",
            &mut issues,
        );
        clamp_u32(
            &mut t.recheck_period_s,
            2,
            120,
            "re-check period",
            &mut issues,
        );
        clamp_u32(
            &mut t.recheck_limit_s,
            10,
            3600,
            "re-check limit",
            &mut issues,
        );
        if !(1..=10).contains(&t.settle_misses) {
            t.settle_misses = t.settle_misses.clamp(1, 10);
            issues.push(Issue::ValueClamped("settle misses"));
        }

        let v = &mut self.verification;
        if !(1..=6).contains(&v.k) {
            v.k = v.k.clamp(1, 6);
            issues.push(Issue::ValueClamped("pictures needed"));
        }
        clamp_f32(
            &mut v.min_score,
            0.05,
            0.99,
            "verification score floor",
            &mut issues,
        );
        clamp_u32(
            &mut v.window_before_s,
            0,
            30,
            "verification window before",
            &mut issues,
        );
        clamp_u32(
            &mut v.window_after_s,
            2,
            60,
            "verification window after",
            &mut issues,
        );

        let mut seen = Vec::new();
        let mut cameras = Vec::new();
        for mut c in std::mem::take(&mut self.cameras) {
            if seen.contains(&c.camera) {
                issues.push(Issue::DuplicateCamera(c.camera));
                continue;
            }
            seen.push(c.camera);
            clean_name(&mut c.name, &mut issues);
            c.zones.truncate(16);
            for z in &mut c.zones {
                if z.polygon.sanitize() {
                    issues.push(Issue::PolygonCleaned);
                }
            }
            c.classes.sort();
            c.classes.dedup();
            if let Schedule::Windows { windows } = &mut c.schedule {
                windows.truncate(16);
                for w in windows.iter_mut() {
                    clamp_u16(&mut w.start_min, 0, 1439, "schedule start", &mut issues);
                    clamp_u16(&mut w.end_min, 0, 1439, "schedule end", &mut issues);
                    w.days &= 0x7f;
                }
            }
            cameras.push(c);
        }
        self.cameras = cameras;

        let mut keys: Vec<String> = Vec::new();
        let mut sources = Vec::new();
        for (i, mut s) in std::mem::take(&mut self.sources).into_iter().enumerate() {
            if !valid_source_key(&s.key) {
                issues.push(Issue::BadSourceKey(i));
                continue;
            }
            if keys.contains(&s.key) {
                issues.push(Issue::DuplicateSource(s.key.clone()));
                continue;
            }
            keys.push(s.key.clone());
            clean_name(&mut s.name, &mut issues);
            if s.cameras.is_empty() {
                issues.push(Issue::SourceWithoutCameras(s.key.clone()));
            }
            s.cameras.truncate(8);
            for c in &mut s.cameras {
                if c.roi.sanitize() {
                    issues.push(Issue::PolygonCleaned);
                }
                clamp_f32(&mut c.weight, 0.0, 4.0, "camera weight", &mut issues);
            }
            clamp_i64(
                &mut s.window_before_ms,
                0,
                30 * SECOND_MS,
                "window before",
                &mut issues,
            );
            clamp_i64(
                &mut s.window_after_ms,
                SECOND_MS,
                60 * SECOND_MS,
                "window after",
                &mut issues,
            );
            let lo = s.window_after_ms;
            clamp_i64(
                &mut s.window_max_ms,
                lo,
                2 * MINUTE_MS,
                "window limit",
                &mut issues,
            );
            clamp_f32(&mut s.k, 1.0, 6.0, "required hits", &mut issues);
            clamp_f32(&mut s.min_score, 0.05, 0.99, "minimum score", &mut issues);
            s.classes.sort();
            s.classes.dedup();
            sources.push(s);
        }
        self.sources = sources;

        for (i, o) in self.outputs.iter_mut().enumerate() {
            if let Some(e) = &mut o.endpoint {
                *e = e.trim().chars().take(512).collect();
            }
            let needs_endpoint = o.kind != OutputKind::Desktop;
            if needs_endpoint && o.endpoint.as_deref().is_none_or(str::is_empty) && o.enabled {
                o.enabled = false;
                issues.push(Issue::EmptyEndpoint(i));
            }
        }

        let s = &mut self.standby;
        if !(1..=16).contains(&s.max_wake_per_device) {
            s.max_wake_per_device = s.max_wake_per_device.clamp(1, 16);
            issues.push(Issue::ValueClamped("cameras woken per device"));
        }
        let l = &mut self.learning;
        clamp_u32(
            &mut l.promote_one_in,
            2,
            10_000,
            "rarity to promote",
            &mut issues,
        );
        clamp_u32(
            &mut l.demote_one_in,
            1,
            l.promote_one_in - 1,
            "rarity to demote",
            &mut issues,
        );

        (self, issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_owner_choices() {
        let s = AlertSettings::default();
        assert!(s.always_alert.smoke_co);
        assert!(s.always_alert.night_outside_person);
        assert!(s.always_alert.doorbell_night);
        assert_eq!(s.night.after_sunset_min, 30);
        assert_eq!(s.night.before_sunrise_min, 30);
        assert_eq!(
            (s.night.fallback_start_min, s.night.fallback_end_min),
            (1320, 360)
        );
        assert_eq!(s.timing.initial_cooldown_s, 60);
        assert_eq!(s.timing.escalated_cooldown_s, 300);
        assert!(s.outputs[0].pictures);
        let m = SourceMapping::new("sensor-1", vec![]);
        assert_eq!(
            (m.window_before_ms, m.window_after_ms, m.window_max_ms),
            (2000, 8000, 30_000)
        );
        assert_eq!(m.require, Require::Verified);
        // An empty document gives the same defaults.
        let parsed: AlertSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, s);
    }

    #[test]
    fn json_round_trip() {
        let mut s = AlertSettings::default();
        let mut c = CameraSettings::new(CameraId::new(1, 2, 0));
        c.schedule = Schedule::Windows {
            windows: vec![DailyWindow {
                days: 0x1f,
                start_min: 22 * 60,
                end_min: 7 * 60,
            }],
        };
        s.cameras.push(c);
        s.sources.push(SourceMapping::new(
            "sensor-1",
            vec![SourceCamera {
                camera: CameraId::new(1, 2, 0),
                roi: Polygon(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]),
                weight: 1.0,
            }],
        ));
        let json = serde_json::to_string(&s).unwrap();
        let back: AlertSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        assert!(json.contains(r#""schedule":{"kind":"windows""#));
    }

    #[test]
    fn validation_clamps() {
        let mut s = AlertSettings {
            location: Some(Location {
                lat: 95.04,
                lon: 12.345,
            }),
            ..AlertSettings::default()
        };
        s.timing.initial_cooldown_s = 1;
        s.timing.escalated_cooldown_s = 5;
        let mut c = CameraSettings::new(CameraId::new(1, 0, 0));
        c.name = Some("  Camera\u{7} with a very long name that goes on and on and on  ".into());
        s.cameras.push(c.clone());
        s.cameras.push(c);
        let mut src = SourceMapping::new("ok", vec![]);
        src.k = f32::NAN;
        src.window_after_ms = 0;
        s.sources.push(src);
        s.sources.push(SourceMapping::new("Not OK", vec![]));
        s.outputs.push(OutputSettings {
            kind: OutputKind::Ntfy,
            enabled: true,
            endpoint: None,
            min_severity: Severity::High,
            pictures: true,
        });
        let (v, issues) = s.validated();
        let loc = v.location.unwrap();
        assert_eq!((loc.lat, loc.lon), (90.0, 12.3));
        assert_eq!(v.timing.initial_cooldown_s, 10);
        assert_eq!(v.timing.escalated_cooldown_s, 10);
        assert_eq!(v.cameras.len(), 1);
        let name = v.cameras[0].name.clone().unwrap();
        assert_eq!(name.chars().count(), MAX_NAME_CHARS);
        assert!(!name.contains('\u{7}'));
        assert_eq!(v.sources.len(), 1);
        assert_eq!(v.sources[0].k, 1.0);
        assert_eq!(v.sources[0].window_after_ms, 1000);
        assert!(!v.outputs[1].enabled);
        assert!(issues.contains(&Issue::LocationClamped));
        assert!(issues.contains(&Issue::LocationRounded));
        assert!(issues.contains(&Issue::DuplicateCamera(CameraId::new(1, 0, 0))));
        assert!(issues.contains(&Issue::BadSourceKey(1)));
        assert!(issues.contains(&Issue::SourceWithoutCameras("ok".into())));
        assert!(issues.contains(&Issue::EmptyEndpoint(1)));
        // Validating again changes nothing more (a source without cameras
        // is a warning, not a change).
        let (again, more) = v.clone().validated();
        assert_eq!(again, v);
        assert_eq!(more, vec![Issue::SourceWithoutCameras("ok".into())]);
    }

    #[test]
    fn windows_wrap_with_days() {
        // Monday 22:00 to Tuesday 07:00, Monday only.
        let w = DailyWindow {
            days: 0b1,
            start_min: 22 * 60,
            end_min: 7 * 60,
        };
        assert!(w.contains(0, 23 * 60));
        assert!(w.contains(1, 6 * 60)); // Tuesday morning belongs to Monday
        assert!(!w.contains(0, 6 * 60)); // Monday morning belongs to Sunday
        assert!(!w.contains(1, 23 * 60));
    }

    #[test]
    fn picture_warnings() {
        for local in [
            "http://192.0.2.10:8080/topic",
            "https://127.0.0.1/x",
            "https://ntfy.local/alerts",
            "http://server/alerts",
            "mqtt://[fd00::1]:1883",
        ] {
            // 192.0.2.x is documentation space, not private: it warns.
            let expect = local.contains("192.0.2.");
            assert_eq!(endpoint_may_leave_network(local), expect, "{local}");
        }
        assert!(endpoint_may_leave_network("https://push.example.com/topic"));
        assert!(endpoint_may_leave_network(""));
        let mut o = OutputSettings {
            kind: OutputKind::Ntfy,
            enabled: true,
            endpoint: Some("https://push.example.com/t".into()),
            min_severity: Severity::Normal,
            pictures: true,
        };
        assert!(o.picture_warning());
        o.pictures = false;
        assert!(!o.picture_warning());
        assert!(!OutputSettings::desktop().picture_warning());
    }
}
