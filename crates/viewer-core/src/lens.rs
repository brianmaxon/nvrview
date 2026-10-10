//! Second lenses that the capabilities (199) do not reveal: one short
//! `externStream` preview per channel, run at most one at a time per device,
//! and its answer kept per device key and channel in `lenses.json` next to
//! `devices.json` (docs/protocol.md, "Second lenses"; docs/FFI.md,
//! `channels`).

use crate::store;
use bc_session::{DeviceSession, LensProbe};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
// tokio's Instant: the runtime's clock, simulated on a test runtime.
use tokio::time::Instant;
use tracing::{debug, info, warn};

const LENSES_FILE: &str = "lenses.json";
const FORMAT: u64 = 1;
/// Keys of devices added without an address last one run; never stored.
const RUN_KEY_PREFIX: &str = "run-";
/// Longest text kept from a camera's identity.
const MAX_TEXT: usize = 256;

/// When and how long a probe runs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProbeTiming {
    /// How long to wait for the first picture. A second lens's
    /// `externStream` was observed to start within about 0.2 s, and once
    /// took 1.18 s (docs/protocol.md, "Second lenses"); the default leaves
    /// room above that.
    pub(crate) window: Duration,
    /// Pause before each probe, so tiles opened when the channel list
    /// arrives go first.
    pub(crate) delay: Duration,
    /// A probe waits until no tile of its channel has started for this
    /// long ...
    pub(crate) quiet: Duration,
    /// ... but no longer than this.
    pub(crate) max_defer: Duration,
}

impl Default for ProbeTiming {
    fn default() -> ProbeTiming {
        ProbeTiming {
            window: Duration::from_secs(3),
            delay: Duration::from_millis(500),
            quiet: Duration::from_secs(3),
            max_defer: Duration::from_secs(15),
        }
    }
}

/// Which camera a channel holds, as far as the device says: the channel
/// list's `uid` (145) and the channel's name and model (318).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Camera {
    pub(crate) uid: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) name: Option<String>,
}

impl Camera {
    /// Whether `other` is the same camera: by `uid` when either has one,
    /// otherwise by model and name.
    fn same(&self, other: &Camera) -> bool {
        if self.uid.is_some() || other.uid.is_some() {
            self.uid == other.uid
        } else {
            self.model == other.model && self.name == other.name
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    camera: Camera,
    lenses: u8,
}

/// Probe answers by device key and channel, shared by every device of the
/// core. Without a data directory, or for a device without a stable key, it
/// lives in memory only.
#[derive(Debug)]
pub(crate) struct LensCache {
    dir: Option<PathBuf>,
    devices: BTreeMap<String, BTreeMap<u8, Entry>>,
}

fn text(o: &Map<String, Value>, k: &str) -> Option<String> {
    o.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= MAX_TEXT)
        .map(str::to_owned)
}

impl LensCache {
    /// Loads `lenses.json` from `dir`.
    pub(crate) fn open(dir: Option<PathBuf>) -> LensCache {
        let mut devices: BTreeMap<String, BTreeMap<u8, Entry>> = BTreeMap::new();
        if let Some(Value::Object(o)) = dir.as_deref().and_then(|d| store::read(d, LENSES_FILE))
            && o.get("format").and_then(Value::as_u64) == Some(FORMAT)
            && let Some(Value::Object(list)) = o.get("devices")
        {
            for (key, chans) in list {
                let Value::Object(chans) = chans else {
                    continue;
                };
                if key.is_empty() || key.len() > MAX_TEXT {
                    continue;
                }
                for (ch, e) in chans {
                    let (Ok(ch), Value::Object(e)) = (ch.parse::<u8>(), e) else {
                        continue;
                    };
                    let Some(lenses) = e
                        .get("lenses")
                        .and_then(Value::as_u64)
                        .filter(|n| (1..=8).contains(n))
                    else {
                        continue;
                    };
                    let camera = Camera {
                        uid: text(e, "uid"),
                        model: text(e, "model"),
                        name: text(e, "name"),
                    };
                    devices.entry(key.clone()).or_default().insert(
                        ch,
                        Entry {
                            camera,
                            lenses: lenses as u8,
                        },
                    );
                }
            }
        }
        LensCache { dir, devices }
    }

    /// The lens count known for `channel` of device `key`, if it was found
    /// for the same camera.
    fn get(&self, key: &str, channel: u8, camera: &Camera) -> Option<u8> {
        let e = self.devices.get(key)?.get(&channel)?;
        e.camera.same(camera).then_some(e.lenses)
    }

    fn put(&mut self, key: &str, channel: u8, camera: &Camera, lenses: u8) {
        let entry = Entry {
            camera: camera.clone(),
            lenses,
        };
        let chans = self.devices.entry(key.to_owned()).or_default();
        if chans.get(&channel) == Some(&entry) {
            return;
        }
        chans.insert(channel, entry);
        if key.starts_with(RUN_KEY_PREFIX) {
            return;
        }
        let Some(dir) = &self.dir else { return };
        let devices: Map<String, Value> = self
            .devices
            .iter()
            .filter(|(k, _)| !k.starts_with(RUN_KEY_PREFIX))
            .map(|(k, chans)| {
                let chans: Map<String, Value> = chans
                    .iter()
                    .map(|(ch, e)| {
                        let mut o = Map::new();
                        o.insert("lenses".into(), json!(e.lenses));
                        for (name, v) in [
                            ("uid", &e.camera.uid),
                            ("model", &e.camera.model),
                            ("name", &e.camera.name),
                        ] {
                            if let Some(v) = v {
                                o.insert(name.into(), json!(v));
                            }
                        }
                        (ch.to_string(), Value::Object(o))
                    })
                    .collect();
                (k.clone(), Value::Object(chans))
            })
            .collect();
        let doc = json!({"format": FORMAT, "devices": devices});
        if let Err(e) = store::write_atomic(dir, LENSES_FILE, &doc) {
            warn!(error = %e, "cannot store lens probe results");
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// When the app last started a tile (or a tier switch) on each channel of
/// one device, so a probe can keep out of its way.
#[derive(Debug, Default)]
pub(crate) struct TileStarts(Mutex<HashMap<u8, Instant>>);

impl TileStarts {
    pub(crate) fn note(&self, channel: u8) {
        lock(&self.0).insert(channel, Instant::now());
    }

    /// How much longer until `channel` has been free of tile starts for
    /// `quiet`; `None` when it already has.
    fn busy_for(&self, channel: u8, quiet: Duration) -> Option<Duration> {
        let at = *lock(&self.0).get(&channel)?;
        quiet.checked_sub(at.elapsed()).filter(|d| !d.is_zero())
    }
}

/// What every device's probes share.
#[derive(Clone, Debug)]
pub(crate) struct LensContext {
    pub(crate) cache: Arc<Mutex<LensCache>>,
    pub(crate) timing: ProbeTiming,
}

/// One probe for a device's watcher to run.
#[derive(Clone, Debug)]
pub(crate) struct ProbeJob {
    pub(crate) channel: u8,
    pub(crate) camera: Camera,
}

/// One device's probe bookkeeping: what is known, what is queued, and
/// whether a probe is running (never more than one).
#[derive(Debug)]
pub(crate) struct LensProbes {
    key: String,
    ctx: LensContext,
    /// Lens counts found by a probe or the cache, for the channels' current
    /// cameras.
    known: HashMap<u8, u8>,
    /// Channels probed (or being probed) on this connection; an
    /// inconclusive probe is tried again on the next one.
    tried: HashSet<u8>,
    queue: VecDeque<ProbeJob>,
    busy: bool,
}

impl LensProbes {
    pub(crate) fn new(key: String, ctx: LensContext) -> LensProbes {
        LensProbes {
            key,
            ctx,
            known: HashMap::new(),
            tried: HashSet::new(),
            queue: VecDeque::new(),
            busy: false,
        }
    }

    /// The lens count a probe (or the cache) found for `channel`.
    pub(crate) fn known(&self) -> &HashMap<u8, u8> {
        &self.known
    }

    /// The connection changed: queued probes are dropped, and channels whose
    /// probe was inconclusive may be probed again.
    pub(crate) fn reset_connection(&mut self) {
        self.queue.clear();
        self.tried.clear();
    }

    /// Takes the channels that need an answer (`candidates`: connected,
    /// not listing `externStream`, not already shown as multi-lens by 199,
    /// each with its camera): applies cached answers and queues probes for
    /// the rest. Returns true when what the app should see changed.
    pub(crate) fn plan(&mut self, candidates: Vec<ProbeJob>) -> bool {
        let wanted: HashSet<u8> = candidates.iter().map(|c| c.channel).collect();
        let mut changed = false;
        self.known.retain(|ch, n| {
            let keep = wanted.contains(ch);
            changed |= !keep && *n >= 2;
            keep
        });
        self.queue.clear();
        let cache = lock(&self.ctx.cache);
        for job in candidates {
            match cache.get(&self.key, job.channel, &job.camera) {
                Some(n) => {
                    let old = self.known.insert(job.channel, n);
                    changed |= old != Some(n) && (n >= 2 || old.is_some_and(|o| o >= 2));
                }
                None => {
                    if let Some(old) = self.known.remove(&job.channel) {
                        changed |= old >= 2;
                    }
                    if !self.tried.contains(&job.channel) {
                        self.queue.push_back(job);
                    }
                }
            }
        }
        changed
    }

    /// The next probe to start, unless one is running.
    pub(crate) fn next(&mut self) -> Option<ProbeJob> {
        if self.busy {
            return None;
        }
        let job = self.queue.pop_front()?;
        self.tried.insert(job.channel);
        self.busy = true;
        Some(job)
    }

    /// A probe ended: its answer goes into the cache (an inconclusive one
    /// does not). Call [`LensProbes::plan`] afterwards to apply it.
    pub(crate) fn finish(&mut self, job: &ProbeJob, result: LensProbe) {
        self.busy = false;
        let lenses = match result {
            LensProbe::SecondLens { after } => {
                info!(
                    channel = job.channel,
                    after_ms = after.as_millis() as u64,
                    "second lens found"
                );
                2
            }
            LensProbe::NoVideo { info } => {
                debug!(channel = job.channel, info, "no second lens: no video");
                1
            }
            LensProbe::Refused { status } => {
                debug!(channel = job.channel, status, "no second lens: refused");
                1
            }
            LensProbe::Inconclusive => {
                debug!(channel = job.channel, "lens probe inconclusive");
                return;
            }
        };
        lock(&self.ctx.cache).put(&self.key, job.channel, &job.camera, lenses);
    }
}

/// Runs one probe: waits a moment, keeps out of the way of tiles starting
/// on the channel (boundedly), then probes on the device's own session.
pub(crate) async fn run_probe(
    session: Arc<DeviceSession>,
    tiles: Arc<TileStarts>,
    timing: ProbeTiming,
    job: ProbeJob,
) -> (ProbeJob, LensProbe) {
    tokio::time::sleep(timing.delay).await;
    let give_up = Instant::now() + timing.max_defer;
    while let Some(wait) = tiles.busy_for(job.channel, timing.quiet) {
        let left = give_up.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        tokio::time::sleep(wait.min(left)).await;
    }
    let r = session.probe_second_lens(job.channel, timing.window).await;
    (job, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(uid: Option<&str>, model: &str, name: &str) -> Camera {
        Camera {
            uid: uid.map(str::to_owned),
            model: Some(model.into()),
            name: Some(name.into()),
        }
    }

    fn job(channel: u8, camera: Camera) -> ProbeJob {
        ProbeJob { channel, camera }
    }

    fn probes(dir: Option<PathBuf>) -> LensProbes {
        LensProbes::new(
            "d-1".into(),
            LensContext {
                cache: Arc::new(Mutex::new(LensCache::open(dir))),
                timing: ProbeTiming::default(),
            },
        )
    }

    #[test]
    fn cameras_match_by_uid_else_by_model_and_name() {
        let a = cam(Some("UID-EXAMPLE-1"), "EXAMPLE-CAM", "Camera 1");
        assert!(a.same(&cam(Some("UID-EXAMPLE-1"), "OTHER", "Camera 2")));
        assert!(!a.same(&cam(Some("UID-EXAMPLE-2"), "EXAMPLE-CAM", "Camera 1")));
        assert!(!a.same(&cam(None, "EXAMPLE-CAM", "Camera 1")));
        let b = cam(None, "EXAMPLE-CAM", "Camera 1");
        assert!(b.same(&cam(None, "EXAMPLE-CAM", "Camera 1")));
        assert!(!b.same(&cam(None, "EXAMPLE-CAM-2", "Camera 1")));
        assert!(!b.same(&cam(None, "EXAMPLE-CAM", "Camera 2")));
    }

    #[test]
    fn one_probe_at_a_time_and_answers_are_cached_per_camera() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_owned();
        let mut p = probes(Some(dir.clone()));
        let c1 = cam(Some("UID-EXAMPLE-1"), "EXAMPLE-CAM", "Camera 1");
        let c2 = cam(None, "EXAMPLE-CAM", "Camera 2");
        assert!(!p.plan(vec![job(1, c1.clone()), job(2, c2.clone())]));
        let first = p.next().unwrap();
        assert_eq!(first.channel, 1);
        assert!(p.next().is_none(), "never two probes at once");
        p.finish(
            &first,
            LensProbe::SecondLens {
                after: Duration::from_millis(150),
            },
        );
        assert!(p.plan(vec![job(1, c1.clone()), job(2, c2.clone())]));
        assert_eq!(p.known().get(&1), Some(&2));
        let second = p.next().unwrap();
        assert_eq!(second.channel, 2);
        p.finish(&second, LensProbe::NoVideo { info: true });
        // A single lens changes nothing the app sees.
        assert!(!p.plan(vec![job(1, c1.clone()), job(2, c2.clone())]));
        assert_eq!(p.known().get(&2), Some(&1));
        assert!(p.next().is_none());

        // A later run: both answers come from the file, no probe.
        let mut p = probes(Some(dir.clone()));
        assert!(p.plan(vec![job(1, c1.clone()), job(2, c2.clone())]));
        assert_eq!(p.known().get(&1), Some(&2));
        assert!(p.next().is_none());
        // Another camera on channel 1: the answer no longer holds.
        let other = cam(Some("UID-EXAMPLE-9"), "EXAMPLE-CAM", "Camera 1");
        assert!(p.plan(vec![job(1, other.clone()), job(2, c2.clone())]));
        assert!(p.known().get(&1).is_none());
        let j = p.next().unwrap();
        assert_eq!((j.channel, &j.camera), (1, &other));
        // Inconclusive: not cached, not retried on this connection ...
        p.finish(&j, LensProbe::Inconclusive);
        assert!(!p.plan(vec![job(1, other.clone())]));
        assert!(p.next().is_none());
        // ... but on the next one.
        p.reset_connection();
        p.plan(vec![job(1, other.clone())]);
        assert_eq!(p.next().unwrap().channel, 1);
    }

    #[test]
    fn bad_entries_are_skipped_and_run_keys_are_not_stored() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_owned();
        std::fs::write(
            dir.join(LENSES_FILE),
            r#"{"format":1,"devices":{"d-1":{"0":{"lenses":2,"model":"M"},"x":{"lenses":2},"1":{"lenses":0},"2":"no"}}}"#,
        )
        .unwrap();
        let mut c = LensCache::open(Some(dir.clone()));
        let m = Camera {
            model: Some("M".into()),
            ..Camera::default()
        };
        assert_eq!(c.get("d-1", 0, &m), Some(2));
        assert_eq!(c.get("d-1", 1, &m), None);
        c.put("run-7", 0, &m, 2);
        assert_eq!(c.get("run-7", 0, &m), Some(2));
        let again = LensCache::open(Some(dir.clone()));
        assert_eq!(again.get("run-7", 0, &m), None);
    }

    #[test]
    fn tile_starts_hold_a_probe_back() {
        let t = TileStarts::default();
        assert!(t.busy_for(3, Duration::from_secs(3)).is_none());
        t.note(3);
        assert!(t.busy_for(3, Duration::from_secs(3)).is_some());
        assert!(t.busy_for(4, Duration::from_secs(3)).is_none());
        assert!(t.busy_for(3, Duration::ZERO).is_none());
    }
}
