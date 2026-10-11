//! Queries that answer from memory (docs/FFI-RECORDINGS.md, sections 2 and
//! 3): the calendar, spans, events, and incidents, shaped as the C
//! interface's JSON. Nothing here searches; a view asks for its days first
//! (`recordings_want`), then queries.

use super::{
    Dev, Hub, MAX_ANSWER_BYTES, RecError, State, camera_json, camera_name, config, kind_in,
    kind_out, lock, tier_name,
};
use recordings::incident::{CameraId, Incident};
use recordings::index::{EventQuery, EventSource, IndexedEvent};
use recordings::{DeviceCamera, Secs, day_start};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};

/// A parsed query (all members optional except `start` and `end`).
pub(super) struct Query {
    pub start: Secs,
    pub end: Secs,
    pub cameras: Vec<(String, DeviceCamera)>,
    /// Device kind names (`motion` already mapped to `md`).
    pub kinds: Vec<String>,
    pub limit: usize,
    pub group: Option<String>,
}

impl Query {
    pub fn parse(json: &str) -> Result<Query, RecError> {
        let m = config::request(json)?;
        let (start, end) = (config::secs(&m, "start")?, config::secs(&m, "end")?);
        if end < start {
            return Err(RecError::InvalidArgument);
        }
        let group = match m.get("group") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(RecError::InvalidArgument),
        };
        Ok(Query {
            start,
            end,
            cameras: config::cameras(&m)?,
            kinds: config::strings(&m, "kinds")?
                .iter()
                .map(|k| kind_in(k).to_owned())
                .collect(),
            limit: config::limit(&m)?,
            group,
        })
    }

    /// The cameras of `device` the query names (`None`: every camera).
    fn of_device(&self, device: &str) -> Option<Vec<DeviceCamera>> {
        if self.cameras.is_empty() {
            return None;
        }
        Some(
            self.cameras
                .iter()
                .filter(|(d, _)| d == device)
                .map(|(_, c)| *c)
                .collect(),
        )
    }

    fn names_device(&self, device: &str) -> bool {
        self.cameras.is_empty() || self.cameras.iter().any(|(d, _)| d == device)
    }
}

/// What a query reads.
pub(super) struct Ctx<'a> {
    pub devices: &'a HashMap<String, Dev>,
    pub revision: u64,
}

impl Ctx<'_> {
    fn keys(&self) -> Vec<&String> {
        let mut k: Vec<&String> = self.devices.keys().collect();
        k.sort();
        k
    }
}

/// Keeps items while the answer stays within its byte limit and the
/// query's count; says whether any were left out.
struct Fit {
    items: Vec<Value>,
    count: usize,
    bytes: usize,
    limit: usize,
    truncated: bool,
}

impl Fit {
    fn new(limit: usize) -> Fit {
        Fit {
            items: Vec::new(),
            count: 0,
            bytes: 1024,
            limit,
            truncated: false,
        }
    }

    /// Counts one item of `n` bytes; false (and nothing counted) once full.
    fn take(&mut self, n: usize) -> bool {
        if self.truncated || self.count >= self.limit || self.bytes + n + 1 > MAX_ANSWER_BYTES {
            self.truncated = true;
            return false;
        }
        self.count += 1;
        self.bytes += n + 1;
        true
    }

    /// Keeps `v`; false (and `v` left out) once full.
    fn push(&mut self, v: Value) -> bool {
        let ok = self.take(v.to_string().len());
        if ok {
            self.items.push(v);
        }
        ok
    }
}

/// Days with recordings per camera.
pub(super) fn calendar(ctx: &Ctx<'_>, q: &Query) -> Value {
    let mut out = Fit::new(q.limit);
    let first = day_start(q.start);
    'devices: for key in ctx.keys() {
        if !q.names_device(key) {
            continue;
        }
        let idx = lock(&ctx.devices[key].index);
        let cams = q
            .of_device(key)
            .unwrap_or_else(|| idx.indexed_cameras().into_iter().collect());
        for cam in cams {
            let days: Vec<Secs> = idx
                .recorded_days(cam)
                .into_iter()
                .filter(|d| *d >= first && *d <= q.end)
                .collect();
            if !out.push(json!({"camera": camera_json(key, cam), "days": days})) {
                break 'devices;
            }
        }
    }
    json!({"revision": ctx.revision, "truncated": out.truncated, "days": out.items})
}

/// Recording files per camera, as spans.
pub(super) fn spans(ctx: &Ctx<'_>, q: &Query) -> Value {
    let mut out = Fit::new(q.limit);
    let mut lanes: Vec<Value> = Vec::new();
    'devices: for key in ctx.keys() {
        if !q.names_device(key) {
            continue;
        }
        let idx = lock(&ctx.devices[key].index);
        let cams = q
            .of_device(key)
            .unwrap_or_else(|| idx.indexed_cameras().into_iter().collect());
        for cam in cams {
            let mut spans: Vec<Value> = Vec::new();
            let mut full = false;
            for f in idx.files(cam, q.start, q.end) {
                let tier = f
                    .stream
                    .as_deref()
                    .and_then(bc_protocol::stream::StreamTier::from_name)
                    .map_or(recordings::Tier::Main, |s| recordings::lens_and_tier(s).1);
                let span = json!({"start": f.start, "end": f.end, "bytes": f.size,
                                  "causes": f.record_types, "tier": tier_name(tier)});
                if !out.take(span.to_string().len()) {
                    full = true;
                    break;
                }
                spans.push(span);
            }
            lanes.push(json!({"camera": camera_json(key, cam), "spans": spans}));
            if full {
                break 'devices;
            }
        }
    }
    json!({"revision": ctx.revision, "truncated": out.truncated, "spans": lanes})
}

/// Events matching the query, by start.
pub(super) fn events(ctx: &Ctx<'_>, q: &Query) -> Value {
    let mut all: Vec<(Secs, &String, IndexedEvent)> = Vec::new();
    for key in ctx.keys() {
        if !q.names_device(key) {
            continue;
        }
        let idx = lock(&ctx.devices[key].index);
        let eq = EventQuery {
            cameras: q.of_device(key).unwrap_or_default(),
            kinds: q.kinds.clone(),
            start: q.start,
            end: q.end,
        };
        if q.of_device(key).is_some_and(|c| c.is_empty()) {
            continue;
        }
        all.extend(idx.events(&eq).into_iter().map(|e| (e.start, key, e)));
    }
    all.sort_by(|a, b| {
        let k = |t: &(Secs, &String, IndexedEvent)| {
            (
                t.0,
                t.1.clone(),
                t.2.channel,
                t.2.lens,
                t.2.end,
                t.2.kind.clone(),
            )
        };
        k(a).cmp(&k(b))
    });
    let mut out = Fit::new(q.limit);
    for (start, key, e) in all {
        let mut v = json!({"camera": camera_json(key, e.camera()),
                           "kind": kind_out(&e.kind), "source": e.source.as_str(),
                           "start": start, "end": e.end});
        if e.source == EventSource::LocalAudio {
            v["analyzer"] = json!(e.analyzer);
            if let Some(l) = &e.label {
                v["label"] = json!(l);
            }
            if let Some(c) = e.confidence_pm {
                v["confidence"] = json!(f64::from(c) / 1000.0);
            }
        }
        if !out.push(v) {
            break;
        }
    }
    json!({"revision": ctx.revision, "truncated": out.truncated, "events": out.items})
}

/// One incident as the C interface shows it.
fn incident_json(
    h: &Hub,
    st: &State,
    inc: &Incident,
    downloads: &HashMap<String, Vec<String>>,
) -> Value {
    let stores = st.stores.as_ref().expect("configured");
    let key = inc.key();
    let offset = inc
        .events
        .first()
        .and_then(|e| st.devices.get(&e.camera.device))
        .map_or(0, |d| d.enable.utc_offset_s);
    let growing = inc.is_growing(h.device_now(offset), stores.settings.gap_s);
    let ranges = inc.download_ranges();
    let cam = |c: &CameraId| camera_json(&c.device, c.local());
    let cameras: Vec<Value> = inc
        .intervals()
        .into_iter()
        .map(|(c, iv)| {
            let r = ranges.get(&c).copied().unwrap_or((inc.start(), inc.end()));
            json!({
                "camera": cam(&c), "name": camera_name(&stores.groups, &c),
                "intervals": iv.iter().map(|(s, e)| json!([s, e])).collect::<Vec<_>>(),
                "download": [r.0, r.1], "added": inc.extra_cameras.contains(&c),
            })
        })
        .collect();
    let events: Vec<Value> = inc
        .events
        .iter()
        .map(|e| {
            let local =
                st.incidents
                    .local
                    .contains(&(e.camera.clone(), e.kind.clone(), e.start, e.end));
            json!({"camera": cam(&e.camera), "kind": kind_out(&e.kind),
                   "source": if local { "local_audio" } else { "device" },
                   "start": e.start, "end": e.end})
        })
        .collect();
    let kinds: BTreeSet<&str> = inc.events.iter().map(|e| kind_out(&e.kind)).collect();
    json!({
        "key": key, "id": inc.id, "name": inc.name, "group": inc.group,
        "start": inc.start(), "end": inc.end(), "margin_s": inc.margin,
        "kinds": kinds, "growing": growing,
        "edited": st.incidents.edited.edited.contains(&key), "locked": false,
        "cameras": cameras, "events": events,
        "downloads": downloads.get(&key).cloned().unwrap_or_default(),
    })
}

/// Incidents overlapping the query's range, newest first.
pub(super) fn incidents(h: &Hub, st: &State, q: &Query) -> Value {
    let downloads = h.downloads_by_key(st);
    let wanted: BTreeSet<CameraId> = q
        .cameras
        .iter()
        .map(|(d, c)| CameraId::with_lens(d.clone(), c.channel, c.lens))
        .collect();
    let mut out = Fit::new(q.limit);
    for inc in &st.incidents.edited.incidents {
        if inc.end() < q.start || inc.start() > q.end {
            continue;
        }
        if !wanted.is_empty() && inc.cameras().is_disjoint(&wanted) {
            continue;
        }
        if !q.kinds.is_empty() && !inc.events.iter().any(|e| q.kinds.contains(&e.kind)) {
            continue;
        }
        if q.group.is_some() && inc.group != q.group {
            continue;
        }
        if !out.push(incident_json(h, st, inc, &downloads)) {
            break;
        }
    }
    json!({"revision": st.incidents.revision, "truncated": out.truncated, "incidents": out.items})
}

/// One incident by key, if any has it now.
pub(super) fn incident_by_key(h: &Hub, st: &State, key: &str) -> Option<Value> {
    let inc = st
        .incidents
        .edited
        .incidents
        .iter()
        .find(|i| i.key() == key)?;
    Some(incident_json(h, st, inc, &h.downloads_by_key(st)))
}

/// The incident with this key, if any has it now.
pub(super) fn find<'a>(st: &'a State, key: &str) -> Option<&'a Incident> {
    st.incidents
        .edited
        .incidents
        .iter()
        .find(|i| i.key() == key)
}
