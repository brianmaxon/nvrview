//! The work behind the sound calls of the recordings owner: headless
//! listeners for the "always listen" list (Decision 12) and the analysis of
//! recorded days on request (Decision 13). The envelope store and the labels
//! are in [`super::sound`].
//!
//! **Listeners.** One task per listed camera, on the core's runtime. It
//! subscribes to the camera's sub-stream (shared with any tile showing it:
//! the session keeps one preview per channel and tier), decodes only the
//! audio (video records are counted and dropped), and feeds the same
//! analysis a tile runs ([`crate::sound::TileSoundLink`]), so the envelope,
//! alarms, and alarm labels are the same as for a tile. While a tile shows
//! the channel the listener lets go of its subscription and waits, so the
//! audio is decoded once; it takes over again when the last tile closes.
//!
//! **Analysis of recorded days.** A request names a camera and a day; the
//! owner holds at most [`MAX_REQUESTS`], runs one per device at a time on a
//! blocking thread, and reads the day's decoded audio from a
//! [`DayAudioSource`]. The source is the seam for downloaded files and
//! playback (slices C10 and the downloads): the core has no way to read
//! recorded audio by itself yet, so without a source every request fails
//! with `no_source`. The analysis is plain: the loudness meter and alarm
//! detector of `crates/sound-events` over the chunks the source gives, with
//! sample numbers counted from the day's local midnight.

use super::sound::PATTERN_ANALYZER;
use super::{Hub, RecError, camera_json, lock};
use crate::audio::{AudioFrame, TileAudio, TileConfig};
use crate::sound::SoundHub;
use bc_protocol::media::Record;
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, Item};
use recordings::index::{EventSource, IndexedEvent};
use recordings::{DeviceCamera, Secs};
use serde_json::{Value, json};
use sound_events::{AlarmConfig, AlarmDetector, AlarmEvent, Level, LoudnessMeter, SAMPLE_RATE};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

/// Cameras on the "always listen" list.
pub const MAX_WATCHES: usize = 64;

/// Analysis requests held, queued and running together.
pub const MAX_REQUESTS: usize = 32;

/// A camera with video but no audio for this long is refused.
const NO_AUDIO_AFTER: Duration = Duration::from_secs(30);

/// How often a listener looks for tiles, stalls, and a missing audio track.
const LISTENER_TICK: Duration = Duration::from_secs(1);

const RATE: u64 = SAMPLE_RATE as u64;

/// A stretch of decoded audio of a recorded day.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioChunk {
    /// Number of the first sample: 16 kHz samples since the day's local
    /// midnight. Chunks come in increasing order and must not overlap
    /// (one that starts before the previous ended is skipped); gaps are
    /// allowed and mean "no audio".
    pub first_sample: u64,
    /// 16 kHz mono samples in -1..1.
    pub samples: Vec<f32>,
}

/// Why a [`DayAudioSource`] cannot give a day's audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceError {
    /// There is no recording of that camera-day to read, or none with audio.
    NoAudio,
    /// The source cannot read it now.
    Unavailable,
}

/// Where the audio of recorded days comes from: the seam between sound
/// analysis and whatever holds recordings (finished downloads, a playback
/// session). Implementations decode to 16 kHz mono and hand the audio over
/// in chunks of any size, in time order. Called on a blocking thread, so it
/// may read files.
pub trait DayAudioSource: Send + Sync + 'static {
    /// Opens the audio of `camera` on `device` (the stable device key) for
    /// the day that starts at `day` (device-local naive seconds).
    ///
    /// # Errors
    /// [`SourceError`] when there is nothing to read.
    fn open(
        &self,
        device: &str,
        camera: DeviceCamera,
        day: Secs,
    ) -> Result<Box<dyn Iterator<Item = AudioChunk> + Send>, SourceError>;
}

#[derive(Default)]
pub(super) struct Work {
    watches: Mutex<HashMap<(String, u8), Watch>>,
    queue: Mutex<Queue>,
    pub(super) source: Mutex<Option<Arc<dyn DayAudioSource>>>,
}

struct Watch {
    camera: DeviceCamera,
    task: Option<JoinHandle<()>>,
    /// The state last reported.
    state: Option<&'static str>,
}

#[derive(Clone, Debug)]
struct Request {
    id: u64,
    device: String,
    camera: DeviceCamera,
    day: Secs,
}

#[derive(Default)]
struct Queue {
    next_id: u64,
    waiting: VecDeque<Request>,
    /// The request running for each device, and its cancel flag.
    running: HashMap<String, (Request, Arc<AtomicBool>)>,
}

impl Queue {
    fn held(&self) -> usize {
        self.waiting.len() + self.running.len()
    }
}

// ---- listeners -----------------------------------------------------------

fn watch_event(device: &str, camera: DeviceCamera, state: &str, reason: Option<&str>) -> Value {
    let mut v = json!({
        "type": "sound_watch", "camera": camera_json(device, camera), "state": state,
    });
    if let Some(r) = reason {
        v["reason"] = Value::from(r);
    }
    v
}

/// Reports a listener's state if it changed.
fn report(hub: &Hub, key: &str, channel: u8, state: &'static str, reason: Option<&str>) {
    let camera = {
        let mut watches = lock(&hub.sound.work.watches);
        let Some(w) = watches.get_mut(&(key.to_owned(), channel)) else {
            return;
        };
        if w.state == Some(state) {
            return;
        }
        w.state = Some(state);
        w.camera
    };
    hub.send(watch_event(key, camera, state, reason));
}

pub(super) fn watch(
    h: &Arc<Hub>,
    key: &str,
    camera: DeviceCamera,
    enabled: bool,
) -> Result<(), RecError> {
    let id = (key.to_owned(), camera.channel);
    if !enabled {
        let removed = lock(&h.sound.work.watches).remove(&id);
        if let Some(w) = removed {
            if let Some(t) = w.task {
                t.abort();
            }
            h.send(watch_event(key, w.camera, "stopped", None));
        }
        return Ok(());
    }
    let mut watches = lock(&h.sound.work.watches);
    if let Some(w) = watches.get(&id) {
        // Already listed; a refused one is tried again.
        if w.task.as_ref().is_some_and(|t| !t.is_finished()) || w.task.is_none() {
            return Ok(());
        }
    } else if watches.len() >= MAX_WATCHES {
        return Err(RecError::Limit);
    }
    watches.insert(
        id,
        Watch {
            camera,
            task: None,
            state: None,
        },
    );
    drop(watches);
    start_listener(h, key, camera.channel);
    Ok(())
}

/// Starts the listener of a listed camera whose device is added.
fn start_listener(h: &Arc<Hub>, key: &str, channel: u8) {
    let Some(sound) = h.sound.sound_hub().cloned() else {
        return;
    };
    let (session, device) = {
        let st = lock(&h.state);
        match st.sessions.get(key) {
            Some(a) => (a.session.clone(), a.id),
            None => return,
        }
    };
    let id = h.sound.listener_id();
    let task = h.rt.spawn(listen(
        Arc::downgrade(h),
        sound,
        session,
        key.to_owned(),
        device,
        channel,
        id,
    ));
    let mut watches = lock(&h.sound.work.watches);
    match watches.get_mut(&(key.to_owned(), channel)) {
        Some(w) if w.task.is_none() || w.task.as_ref().is_some_and(|t| t.is_finished()) => {
            w.task = Some(task);
        }
        // Removed meanwhile, or already running.
        _ => task.abort(),
    }
}

pub(super) fn device_added(h: &Arc<Hub>, key: &str) {
    let channels: Vec<u8> = lock(&h.sound.work.watches)
        .iter()
        .filter(|((k, _), w)| k == key && w.task.is_none())
        .map(|((_, c), _)| *c)
        .collect();
    for c in channels {
        start_listener(h, key, c);
    }
}

pub(super) fn device_removed(h: &Hub, key: &str, id: u64) {
    if lock(&h.state).sessions.get(key).is_none_or(|a| a.id != id) {
        return;
    }
    let gone: Vec<Watch> = {
        let mut watches = lock(&h.sound.work.watches);
        let keys: Vec<_> = watches.keys().filter(|(k, _)| k == key).cloned().collect();
        keys.into_iter()
            .filter_map(|k| watches.remove(&k))
            .collect()
    };
    for w in gone {
        if let Some(t) = w.task {
            t.abort();
        }
        h.send(watch_event(key, w.camera, "stopped", None));
    }
    // Analyses of the device stop at their next chunk.
    let queue = lock(&h.sound.work.queue);
    if let Some((_, cancel)) = queue.running.get(key) {
        cancel.store(true, Ordering::SeqCst);
    }
}

pub(super) fn shutdown(h: &Hub) {
    for (_, w) in lock(&h.sound.work.watches).drain() {
        if let Some(t) = w.task {
            t.abort();
        }
    }
    let mut q = lock(&h.sound.work.queue);
    q.waiting.clear();
    for (_, cancel) in q.running.values() {
        cancel.store(true, Ordering::SeqCst);
    }
}

/// One listener: waits while a tile shows its channel, otherwise decodes
/// the sub-stream's audio into the analysis.
async fn listen(
    hub: Weak<Hub>,
    sound: Arc<SoundHub>,
    session: Arc<DeviceSession>,
    key: String,
    device: u64,
    channel: u8,
    id: u64,
) {
    let refuse = |reason: &str| {
        if let Some(h) = hub.upgrade() {
            report(&h, &key, channel, "refused", Some(reason));
        }
    };
    let (clock, started) = match hub.upgrade() {
        Some(h) => (h.clock.clone(), h.clock.now_ns()),
        None => return,
    };
    let mut tick = tokio::time::interval(LISTENER_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        // Standing by while a tile does the work.
        if sound.channel_shown(device, channel) {
            if let Some(h) = hub.upgrade() {
                report(&h, &key, channel, "watching", None);
            }
            tick.tick().await;
            continue;
        }
        let Ok(mut sub) = session.subscribe(channel, StreamTier::Sub).await else {
            refuse("device_closed");
            return;
        };
        let Some(mut link) = sound.add_listener(id, device, channel) else {
            refuse("sound_off");
            return;
        };
        let mut audio = TileAudio::new(TileConfig::default());
        audio.set_owner(id);
        audio.set_mono_output(true);
        if let Some(h) = hub.upgrade() {
            report(&h, &key, channel, "watching", None);
        }
        let (mut sequence, mut last_video_pts) = (0u64, None::<u64>);
        let (mut heard, mut video) = (false, false);
        loop {
            tokio::select! {
                item = sub.recv() => {
                    let Some(item) = item else {
                        refuse("device_closed");
                        return;
                    };
                    match item {
                        Item::Media(Record::Audio { stream_index, data }) => {
                            sequence += 1;
                            heard = true;
                            let frame = AudioFrame {
                                stream_index,
                                data,
                                sequence,
                                after_video_pts_us: last_video_pts,
                                arrived_us: clock.now_us(),
                                wall_clock_us: Some(clock.wall_now_us()).filter(|w| *w > 0),
                            };
                            if frame.is_talkback() {
                                continue;
                            }
                            audio.push(frame.record());
                            if let Some(b) = audio.mono_block() {
                                link.push(&b);
                            }
                        }
                        Item::Media(Record::Video { pts_us, .. }) => {
                            sequence += 1;
                            video = true;
                            last_video_pts = Some(pts_us);
                        }
                        Item::Discontinuity => {
                            last_video_pts = None;
                            audio.discontinuity();
                        }
                        _ => {}
                    }
                }
                _ = tick.tick() => {
                    link.tick(clock.now_ns());
                    if sound.channel_shown(device, channel) {
                        break;
                    }
                    if !heard && video && clock.now_ns().saturating_sub(started)
                        > NO_AUDIO_AFTER.as_nanos() as u64
                    {
                        refuse("no_audio");
                        return;
                    }
                }
            }
        }
        // A tile took over: stop the preview this listener held.
        link.end();
        drop(link);
        drop(sub);
    }
}

// ---- recorded days -------------------------------------------------------

fn analyze_event(r: &Request, state: &str, extra: Value) -> Value {
    let mut v = json!({
        "type": "sound_analyze", "request": r.id,
        "camera": camera_json(&r.device, r.camera), "day": r.day, "state": state,
    });
    if let (Value::Object(m), Value::Object(e)) = (&mut v, extra) {
        m.extend(e);
    }
    v
}

pub(super) fn analyze_day(
    h: &Arc<Hub>,
    key: &str,
    camera: DeviceCamera,
    day: Secs,
) -> Result<String, RecError> {
    let (req, start) = {
        let mut q = lock(&h.sound.work.queue);
        let same = |r: &Request| r.device == key && r.camera == camera && r.day == day;
        if let Some(r) = q
            .waiting
            .iter()
            .chain(q.running.values().map(|(r, _)| r))
            .find(|r| same(r))
        {
            let state = if q.running.contains_key(key) && q.running[key].0.id == r.id {
                "running"
            } else {
                "queued"
            };
            return Ok(json!({"request": r.id, "state": state}).to_string());
        }
        if q.held() >= MAX_REQUESTS {
            return Err(RecError::Limit);
        }
        q.next_id += 1;
        let req = Request {
            id: q.next_id,
            device: key.to_owned(),
            camera,
            day,
        };
        q.waiting.push_back(req.clone());
        (req, !q.running.contains_key(key))
    };
    h.send(analyze_event(&req, "queued", json!({})));
    if start {
        let weak = Arc::downgrade(h);
        let key = key.to_owned();
        h.rt.spawn(run_device(weak, key));
    }
    Ok(json!({"request": req.id, "state": "queued"}).to_string())
}

pub(super) fn cancel(h: &Arc<Hub>, request: u64) -> Result<(), RecError> {
    let mut q = lock(&h.sound.work.queue);
    if let Some(i) = q.waiting.iter().position(|r| r.id == request) {
        let r = q.waiting.remove(i).expect("found");
        drop(q);
        h.send(analyze_event(&r, "canceled", json!({})));
        return Ok(());
    }
    match q.running.values().find(|(r, _)| r.id == request) {
        Some((_, cancel)) => {
            cancel.store(true, Ordering::SeqCst);
            Ok(())
        }
        None => Err(RecError::UnknownId),
    }
}

/// Runs the device's waiting requests one after another.
async fn run_device(hub: Weak<Hub>, key: String) {
    loop {
        let Some(h) = hub.upgrade() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let req = {
            let mut q = lock(&h.sound.work.queue);
            if q.running.contains_key(&key) {
                return;
            }
            let Some(i) = q.waiting.iter().position(|r| r.device == key) else {
                return;
            };
            let r = q.waiting.remove(i).expect("found");
            q.running.insert(key.clone(), (r.clone(), cancel.clone()));
            r
        };
        h.send(analyze_event(&req, "running", json!({})));
        let outcome = run_request(&h, &req, &cancel).await;
        lock(&h.sound.work.queue).running.remove(&key);
        match outcome {
            Ok((seconds, labels)) => {
                info!(request = req.id, seconds, labels, "sound analysis done");
                h.send(analyze_event(
                    &req,
                    "done",
                    json!({"seconds": seconds, "labels": labels}),
                ));
            }
            Err(Failure::Canceled) => h.send(analyze_event(&req, "canceled", json!({}))),
            Err(Failure::Reason(r)) => {
                warn!(request = req.id, reason = r, "sound analysis failed");
                h.send(analyze_event(&req, "failed", json!({"reason": r})));
            }
        }
        if h.closing.load(Ordering::SeqCst) {
            return;
        }
    }
}

enum Failure {
    Canceled,
    Reason(&'static str),
}

async fn run_request(
    h: &Arc<Hub>,
    req: &Request,
    cancel: &Arc<AtomicBool>,
) -> Result<(usize, usize), Failure> {
    let source = lock(&h.sound.work.source)
        .clone()
        .ok_or(Failure::Reason("no_source"))?;
    let (device, camera, day) = (req.device.clone(), req.camera, req.day);
    let flag = cancel.clone();
    let done =
        h.rt.spawn_blocking(move || {
            let chunks = source.open(&device, camera, day).map_err(|e| match e {
                SourceError::NoAudio => "no_audio",
                SourceError::Unavailable => "source_unavailable",
            })?;
            Ok::<_, &'static str>(analyze(day, chunks, &flag))
        })
        .await
        .map_err(|_| Failure::Reason("internal"))?
        .map_err(Failure::Reason)?;
    if cancel.load(Ordering::SeqCst) || h.closing.load(Ordering::SeqCst) {
        return Err(Failure::Canceled);
    }
    store_analysis(h, req, done)
}

/// Puts an analysis into the envelope store and the index.
fn store_analysis(h: &Arc<Hub>, req: &Request, a: Analysis) -> Result<(usize, usize), Failure> {
    let seconds = a.seconds.len();
    let labels = a.labels.len();
    let Some((first, last)) = a.span else {
        return Ok((0, 0));
    };
    let (offset, dir, cfg) = {
        let st = lock(&h.state);
        let stores = st
            .stores
            .as_ref()
            .ok_or(Failure::Reason("not_configured"))?;
        let dev = st
            .devices
            .get(&req.device)
            .ok_or(Failure::Reason("not_enabled"))?;
        (
            dev.enable.utc_offset_s,
            h.data_dir
                .is_some()
                .then(|| stores.root.join("sound").join(super::dir_name(&req.device))),
            stores.settings.index.into(),
        )
    };
    let target = super::sound::Target::new(req.device.clone(), offset, dir, cfg);
    h.with_envelope(&target, |store| {
        for (second, peak, rms) in &a.seconds {
            store.put(req.camera, *second, *peak, *rms);
        }
    });
    // Alarm labels replace the detector's earlier ones for the audio
    // analyzed, so analyzing a day again does not duplicate them.
    h.put_labels(
        &req.device,
        req.camera,
        PATTERN_ANALYZER,
        (first, last),
        a.labels,
    )
    .map_err(|_| Failure::Reason("index"))?;
    Ok((seconds, labels))
}

/// What the analysis of a day found.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Analysis {
    /// Seconds with audio: (device-local second, peak, rms).
    pub seconds: Vec<(Secs, Level, Level)>,
    /// `smoke_alarm` and `co_alarm` labels.
    pub labels: Vec<IndexedEvent>,
    /// First and last second the audio covered.
    pub span: Option<(Secs, Secs)>,
}

/// Runs the loudness meter and the alarm detector over a day's chunks.
/// Stops early (returning what it has found) when `cancel` is set.
pub(super) fn analyze(
    day: Secs,
    chunks: impl Iterator<Item = AudioChunk>,
    cancel: &AtomicBool,
) -> Analysis {
    let mut meter = LoudnessMeter::new();
    let mut detector = AlarmDetector::new(AlarmConfig::default());
    let mut out = Analysis::default();
    let mut next = 0u64;
    let mut first = None::<u64>;
    let second = |s: u64| day + (s / RATE) as i64;
    let label = |e: AlarmEvent, out: &mut Analysis| {
        if let AlarmEvent::Ended {
            kind, start, end, ..
        } = e
        {
            out.labels.push(IndexedEvent {
                kind: kind.as_str().to_owned(),
                start: second(start),
                end: second(end.max(start)),
                source: EventSource::LocalAudio,
                ..IndexedEvent::default()
            });
        }
    };
    for c in chunks {
        if cancel.load(Ordering::SeqCst) {
            return Analysis::default();
        }
        if c.samples.is_empty() || c.first_sample < next {
            continue;
        }
        first.get_or_insert(c.first_sample);
        next = c.first_sample + c.samples.len() as u64;
        meter.push(c.first_sample, &c.samples, |s| {
            out.seconds.push((second(s.second * RATE), s.peak, s.rms));
        });
        detector.push(c.first_sample, &c.samples, |e| label(e, &mut out));
    }
    meter.flush(|s| out.seconds.push((second(s.second * RATE), s.peak, s.rms)));
    detector.flush(|e| label(e, &mut out));
    if let Some(f) = first {
        out.span = Some((second(f), second(next.saturating_sub(1))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secs` seconds of a 3.1 kHz tone at 0.3 in the T3 rhythm.
    fn t3(secs: u64) -> Vec<f32> {
        (0..secs * RATE)
            .map(|n| {
                let t = n as f64 / RATE as f64;
                let c = t % 4.0;
                if c < 3.0 && c % 1.0 < 0.5 {
                    0.3 * (2.0 * std::f64::consts::PI * 3100.0 * t).sin() as f32
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn chunks(first_sample: u64, pcm: &[f32]) -> Vec<AudioChunk> {
        pcm.chunks(4000)
            .enumerate()
            .map(|(i, c)| AudioChunk {
                first_sample: first_sample + (i * 4000) as u64,
                samples: c.to_vec(),
            })
            .collect()
    }

    #[test]
    fn a_day_with_an_alarm_gives_seconds_and_a_label_in_device_local_time() {
        let day = 1_791_590_400;
        let start = 3 * 3600 * RATE + 7 * RATE / 2;
        let mut input = chunks(start, &t3(20));
        // A gap, then a second stretch, and a chunk that overlaps: skipped.
        input.extend(chunks(start + 60 * RATE, &vec![0.01; 3 * RATE as usize]));
        input.push(AudioChunk {
            first_sample: start,
            samples: vec![0.9; 100],
        });
        let a = analyze(day, input.into_iter(), &AtomicBool::new(false));
        let first = day + 3 * 3600 + 3;
        assert_eq!(a.span, Some((first, day + 3 * 3600 + 3 + 63)));
        assert_eq!(a.seconds.first().unwrap().0, first);
        // 21 seconds touched by the alarm, 4 by the quiet stretch.
        assert!((24..=26).contains(&a.seconds.len()), "{}", a.seconds.len());
        assert!(a.seconds.windows(2).all(|w| w[0].0 < w[1].0));
        let loud = a.seconds[5];
        assert!((-13..=-7).contains(&loud.1.db().unwrap()), "{loud:?}");
        assert_eq!(a.labels.len(), 1, "{:?}", a.labels);
        let l = &a.labels[0];
        assert_eq!(l.kind, "smoke_alarm");
        assert_eq!(l.source, EventSource::LocalAudio);
        assert!(
            (first..=first + 2).contains(&l.start),
            "{}",
            l.start - first
        );
        assert!(l.end > l.start + 8 && l.end <= first + 21);
    }

    #[test]
    fn nothing_and_cancel() {
        let none = analyze(0, std::iter::empty(), &AtomicBool::new(false));
        assert_eq!(none, Analysis::default());
        let cancel = AtomicBool::new(true);
        let a = analyze(0, chunks(0, &t3(5)).into_iter(), &cancel);
        assert_eq!(a, Analysis::default());
    }
}
