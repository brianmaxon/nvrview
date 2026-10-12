//! One playback's task: the shared clock, the plays of every camera
//! grouped per device, the paced release of pictures into the playback
//! tiles, synchronization, buffering, and the playback's events.
//!
//! **Plays per device.** A device keeps one seek position per session and a
//! play starts from the position current when its play request arrives; a
//! play whose recording does not hold it gets nothing (docs/protocol.md,
//! "Live view and plays on one session"). So the plays of one device are
//! always started together ([`bc_session::DeviceSession::play_group`]):
//! the running ones are stopped (7), every camera is seeked to one time
//! (123), and after about half a second all are played (5). An exact seek
//! asks the device for a little media before the target (the seek lead,
//! 2 s, kept inside the file that holds the target): devices may start a
//! play at a keyframe after the seek time (docs/protocol.md, "Rates,
//! keyframes, and how a play ends"), and with the lead a camera nearly
//! always has a picture at the target (a camera whose first picture is still
//! more than 250 ms after it is started once more, earlier). A seek, a rate
//! change, a tier switch, an offset change, and adding or removing a camera
//! restart the device's group that way. A camera whose recording does not
//! hold the time is not started; it shows `no_recording` and starts again
//! by itself (a group of one, at the start of its next file) where its
//! recording resumes. A play stopped because its buffer was full starts
//! again the same way, a group of one at the newest second it holds. Starts
//! on one device are serialized (one in flight; newer requests coalesce),
//! across playbacks too ([`super::HubCore`]'s per-device lock).
//!
//! **One playback per device.** Two playbacks cannot share a device's one
//! seek position, so a device belongs to the playback that most recently
//! started plays on it: starting takes it over, and the cameras of that
//! device in the playback that had it stop and report `failed`
//! (`device_in_use`) until that playback seeks again.
//!
//! **Which file.** Each camera plays the recording file that holds the seek
//! time: from the recordings index for the main stream (the index holds
//! main-stream files), from a search of the camera's sub-stream files of
//! that day (14 to 16, cached for the playback) for the sub-stream, whose
//! file IDs differ (docs/protocol.md, "Four plays at once").
//!
//! **The clock and pacing.** The position advances at the rate on the
//! core's clock. Each picture is released into its tile (a `Decode` soon
//! after) when the position plus the camera's offset reaches its recorded
//! time less the tile's measured decode latency (scaled by the rate), so a
//! renderer that shows the newest decoded picture shows it on time. After a
//! seek the pictures from the keyframe up to the target are released as
//! fast as the decoder answers (at most a small backlog), and the tile
//! shows the new generation once the target picture is decoded (the
//! keyframe, for a keyframe seek).
//!
//! **Synchronization.** The same moment shows on every camera. With
//! `wait`, the clock holds while a camera is not ready at the position:
//! after a start (a seek, a rate change, a restart) until it has media
//! there *and* has decoded up to its target, up to the first-media limit
//! (6 s); while playing, until it has media there again, up to 2 s. A
//! camera still behind then is `late` and the clock goes on without it.
//! Its play (stalled, or running behind the clock) alone starts again at
//! the position then (a group of one, with the seek lead), and again each
//! further 2 s it stays behind, at most `restarts_per_minute` times a
//! minute; it rejoins by dropping to its first keyframe at the position,
//! and otherwise stays `late`. A camera whose first picture after a start
//! lies after the target (the play began at a later keyframe) is not shown
//! ahead of the others: while playing, that picture waits for the clock
//! (the tile keeps its previous picture until then); paused, it is shown at
//! once, as the nearest picture there is. With `free` the clock never
//! holds. A camera that delivers nothing within the first-media limit is
//! `no_recording` (unavailable).
//!
//! **Rates.** 0.25, 0.5, 1, 2, 4, 8, and 16. The device is asked for
//! `playSpeed` 1 below 1 (paced slower here) and the rate itself from 1 up
//! (the observed values). A main-stream play carries keyframes only from 2x
//! (observed); from 8x the engine also decodes only keyframes of a stream
//! that carries every frame (a sub-stream keeps every frame at 4x,
//! observed), which keeps four cameras' decoding bounded. Audio is released
//! only at 1x while playing.
//!
//! **How a play ends.** The device was never seen ending a play, so the
//! end comes from the index: a camera ends after the end of its last
//! indexed file in the range, and the playback ends at the earlier of its
//! range's end and the last camera's end. A play continues across files by
//! itself (observed).

use super::buffer::{CamBuf, Push};
use super::clock::PlayClock;
use super::feed::{FeedItem, FeedTx, feed};
use super::{HubCore, PbShared, SeekMode, SyncMode, state_code};
use crate::clock::HostClock;
use crate::events::{EventSink, Priority};
use crate::tile::TileCommand;
use bc_protocol::archive::{FileSearch, PlayRequest, RecordFile};
use bc_session::{DeviceSession, DownloadError, Item, Play, PlayStart, QueryError, SearchError};
use recordings::index::{DeviceIndex, IndexedFile};
use recordings::{DeviceCamera, Secs, Tier, date_time, day_start};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{debug, info, warn};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The engine's timing and bounds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tuning {
    /// How often the clock is read and pictures released.
    pub(crate) tick: Duration,
    /// Longest the clock waits for a camera while playing (`wait`).
    pub(crate) wait_limit: Duration,
    /// A camera that delivers nothing this long after its play started is
    /// unavailable (`no_recording`).
    pub(crate) first_media: Duration,
    /// Media time a camera buffers ahead before its play is stopped, at 1x
    /// (times the rate above 1x).
    pub(crate) buffer_ms: i64,
    /// Bytes a camera buffers before its play is stopped.
    pub(crate) buffer_bytes: usize,
    /// Pictures before a seek's target are released while the tile holds
    /// fewer than this.
    pub(crate) preroll_backlog: u64,
    /// A picture this much older than the position (times the rate, at
    /// least this) is late: dropped to the next keyframe.
    pub(crate) late_ms: i64,
    /// A camera without recording at the position is started this long
    /// before its next file begins.
    pub(crate) prefetch_ms: i64,
    /// Longest wait for a play's stop (7) to be sent.
    pub(crate) stop_wait: Duration,
    /// Restarts after a stall or a lost connection, or of a camera left
    /// behind, per camera and minute.
    pub(crate) restarts_per_minute: u32,
    /// An exact seek asks the device for this much media time before the
    /// target (within the file that holds it), so that a play starts at a
    /// keyframe at or before the target although devices may start at one
    /// after the seek time.
    pub(crate) seek_lead_ms: i64,
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning {
            tick: Duration::from_millis(10),
            wait_limit: Duration::from_secs(2),
            first_media: Duration::from_secs(6),
            buffer_ms: 20_000,
            buffer_bytes: 16 << 20,
            preroll_backlog: 6,
            late_ms: 500,
            prefetch_ms: 1_000,
            stop_wait: Duration::from_secs(2),
            restarts_per_minute: 3,
            seek_lead_ms: 2_000,
        }
    }
}

/// A camera as the engine gets it.
pub(crate) struct CamSetup {
    pub(crate) tile: u64,
    pub(crate) device: String,
    pub(crate) camera: DeviceCamera,
    pub(crate) tier: Tier,
    pub(crate) offset_ms: i64,
    pub(crate) commands: mpsc::Sender<TileCommand>,
}

/// A device as the engine gets it.
pub(crate) struct DevSetup {
    pub(crate) key: String,
    pub(crate) session: Option<Arc<DeviceSession>>,
    pub(crate) utc_offset_s: i64,
    pub(crate) index: Arc<Mutex<DeviceIndex>>,
}

/// Requests to the engine.
pub(crate) enum Cmd {
    Play,
    Pause,
    Seek(i64, SeekMode),
    Rate(f64),
    Offset(u64, i64),
    Tier(u64, Tier),
    Add(CamSetup, Option<DevSetup>),
    Remove(u64, oneshot::Sender<()>),
    /// Another playback takes the device over.
    Evict(String, oneshot::Sender<()>),
    /// The device was removed: its cameras leave the playback.
    DeviceGone(String, oneshot::Sender<()>),
    Close(oneshot::Sender<()>),
    Status(oneshot::Sender<Value>),
    /// The picture each tile shows (stills).
    Shown(oneshot::Sender<Vec<ShownTile>>),
}

/// What a tile shows, for a still: its generation and, once the app has
/// answered a picture of it as shown, that picture's recorded time
/// (device-local ms).
#[derive(Clone, Debug)]
pub(crate) struct ShownTile {
    pub tile: u64,
    pub device: String,
    pub camera: DeviceCamera,
    pub generation: u32,
    pub position_ms: Option<i64>,
}

/// Reports from the playback's own tasks.
enum Note {
    Started {
        device: String,
        token: u64,
        plays: Vec<(u64, u64, Play)>,
        missing: Vec<(u64, u64)>,
        failed: Option<(&'static str, Vec<(u64, u64)>)>,
    },
    Suspended {
        tile: u64,
        run: u64,
    },
    Ended {
        tile: u64,
        run: u64,
    },
    Error {
        tile: u64,
        run: u64,
        error: DownloadError,
    },
}

/// A running play's reader. Dropping it stops the play.
struct Reader {
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Reader {
    /// Stops the play and waits until its stop (7) went out.
    async fn stop(mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        let _ = (&mut self.task).await;
    }
}

/// Adds a record to a buffer (the lock is not held across an await).
fn push(buf: &Mutex<CamBuf>, run: u64, rec: bc_protocol::media::Record) -> Push {
    lock(buf).push(run, rec)
}

/// Reads a play into the camera's buffer until it is told to stop, the
/// buffer is full, or the play ends.
#[allow(clippy::too_many_arguments)]
async fn read(
    mut play: Play,
    buf: Arc<Mutex<CamBuf>>,
    tile: u64,
    run: u64,
    mut stop: oneshot::Receiver<()>,
    notes: mpsc::Sender<Note>,
    stop_wait: Duration,
) {
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => {
                play.stop(stop_wait).await;
                return;
            }
            r = play.next() => match r {
                Ok(Some(rec)) => match push(&buf, run, rec) {
                    Push::Kept => {}
                    Push::Full => {
                        play.stop(stop_wait).await;
                        let _ = notes.send(Note::Suspended { tile, run }).await;
                        return;
                    }
                    Push::Stale => {
                        play.stop(stop_wait).await;
                        return;
                    }
                },
                Ok(None) => {
                    let _ = notes.send(Note::Ended { tile, run }).await;
                    return;
                }
                Err(error) => {
                    drop(play);
                    let _ = notes.send(Note::Error { tile, run, error }).await;
                    return;
                }
            }
        }
    }
}

/// A camera's state, as `playback_camera` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CamState {
    Buffering,
    Playing,
    Late,
    NoRecording,
    Ended,
    Failed(&'static str),
}

impl CamState {
    fn name(self) -> &'static str {
        match self {
            CamState::Buffering => "buffering",
            CamState::Playing => "playing",
            CamState::Late => "late",
            CamState::NoRecording => "no_recording",
            CamState::Ended => "ended",
            CamState::Failed(_) => "failed",
        }
    }

    /// Whether the camera is expected to show pictures now.
    fn active(self) -> bool {
        matches!(
            self,
            CamState::Buffering | CamState::Playing | CamState::Late
        )
    }
}

struct Cam {
    tile: u64,
    device: String,
    camera: DeviceCamera,
    tier: Tier,
    offset_ms: i64,
    commands: mpsc::Sender<TileCommand>,
    feed: Option<FeedTx>,
    generation: u32,
    buf: Arc<Mutex<CamBuf>>,
    state: CamState,
    reported: Option<CamState>,
    run: u64,
    reader: Option<Reader>,
    /// Waiting to be started (in the device's queue).
    pending: bool,
    /// The generation's target (device-local ms) and whether it was reached.
    target_ms: i64,
    target_reached: bool,
    last_released_pts: Option<u64>,
    /// Late: dropping to the next keyframe.
    dropping: bool,
    /// Its first picture of the generation lies after the target (the play
    /// began at a later keyframe): held until the clock reaches it.
    ahead: bool,
    /// Its play was already started again for that, once (per generation).
    retried: bool,
    /// Where its play should start again, set by a release that found the
    /// first picture well after the target.
    retry_from: Option<i64>,
    /// When its play started (for the first-media limit).
    started: Option<Instant>,
    /// Since when it has had nothing at the position.
    behind_since: Option<Instant>,
    /// Its play was stopped because the buffer was full.
    suspended: bool,
    /// The device ended its play (an end message).
    stream_done: bool,
    /// Indexed files in the playback's range.
    files: Vec<IndexedFile>,
    /// Not started again before this position (ms): after a camera gave
    /// nothing, until its next file.
    quiet_until_ms: i64,
    restarts: VecDeque<Instant>,
}

impl Cam {
    fn covered(&self, local_ms: i64) -> bool {
        self.file_at(local_ms.div_euclid(1000)).is_some()
    }

    fn file_at(&self, s: Secs) -> Option<&IndexedFile> {
        self.files.iter().find(|f| f.start <= s && s <= f.end)
    }

    /// Start (ms) of the first file that begins after `local_ms`.
    fn next_start_ms(&self, local_ms: i64) -> Option<i64> {
        self.files
            .iter()
            .map(|f| f.start * 1000)
            .filter(|s| *s > local_ms)
            .min()
    }

    /// End (ms, exclusive) of the last file.
    fn end_ms(&self) -> Option<i64> {
        self.files.iter().map(|f| (f.end + 1) * 1000).max()
    }

    fn has_data_at(&self, local_ms: i64) -> bool {
        lock(&self.buf).horizon_ms().is_some_and(|h| h >= local_ms)
    }

    /// Whether the next picture held is due by `local_ms`.
    fn next_due_by(&self, local_ms: i64) -> bool {
        lock(&self.buf)
            .next_video_ms()
            .is_some_and(|n| n <= local_ms)
    }
}

/// Sub-stream files found per camera and day.
type SubFiles = Arc<Mutex<HashMap<(DeviceCamera, Secs), Vec<RecordFile>>>>;

/// A queued start of plays on one device.
struct Launch {
    at: Secs,
    tiles: Vec<u64>,
}

struct Dev {
    session: Option<Arc<DeviceSession>>,
    utc_offset_s: i64,
    index: Arc<Mutex<DeviceIndex>>,
    token: Arc<AtomicU64>,
    busy: bool,
    /// The start in flight (or the last one). It holds the session, so it
    /// is canceled before the device is given up: a session cannot close
    /// while a task the engine left behind still holds it.
    job: Option<JoinHandle<()>>,
    queue: VecDeque<Launch>,
    /// Readers to stop before the next start.
    stopping: Vec<Reader>,
    sub_files: SubFiles,
}

impl Dev {
    /// Cancels the start in flight and waits until it has let go of the
    /// session. A canceled start drops the plays it got, which stops them;
    /// waiting for it needs no time to pass.
    async fn cancel_start(&mut self) {
        if let Some(j) = self.job.take() {
            j.abort();
            let _ = j.await;
        }
    }
}

/// The seek being reported.
struct SeekReport {
    generation: u32,
    position_ms: i64,
    done: bool,
}

pub(crate) struct Engine {
    pub(crate) id: u64,
    hub: Arc<HubCore>,
    events: EventSink,
    clock: HostClock,
    tuning: Tuning,
    shared: Arc<PbShared>,
    play_clock: PlayClock,
    start_ms: i64,
    end_ms: i64,
    sync: SyncMode,
    mode: SeekMode,
    playing: bool,
    ended: bool,
    state: &'static str,
    reported_state: Option<(&'static str, i64)>,
    cams: Vec<Cam>,
    devices: HashMap<String, Dev>,
    next_gen: u32,
    next_run: u64,
    seek: Option<SeekReport>,
    notes_tx: mpsc::Sender<Note>,
    notes: mpsc::Receiver<Note>,
}

/// Capacity of the notes channel (each running task sends a few).
const NOTES_CAPACITY: usize = 64;

/// The rates a playback accepts.
pub(crate) const RATES: [f64; 7] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];

/// The `playSpeed` asked of the device for a rate.
pub(crate) fn device_speed(rate: f64) -> u32 {
    if rate < 1.0 { 1 } else { rate as u32 }
}

pub(crate) struct EngineSetup {
    pub(crate) id: u64,
    pub(crate) hub: Arc<HubCore>,
    pub(crate) events: EventSink,
    pub(crate) clock: HostClock,
    pub(crate) tuning: Tuning,
    pub(crate) shared: Arc<PbShared>,
    pub(crate) start_ms: i64,
    pub(crate) end_ms: i64,
    pub(crate) position_ms: i64,
    pub(crate) rate: f64,
    pub(crate) sync: SyncMode,
    pub(crate) cams: Vec<CamSetup>,
    pub(crate) devices: Vec<DevSetup>,
}

impl Engine {
    pub(crate) fn new(s: EngineSetup) -> Engine {
        let (notes_tx, notes) = mpsc::channel(NOTES_CAPACITY);
        let mut play_clock = PlayClock::new(s.position_ms);
        play_clock.set_rate(s.rate, 0);
        let mut e = Engine {
            id: s.id,
            hub: s.hub,
            events: s.events,
            clock: s.clock,
            tuning: s.tuning,
            shared: s.shared,
            play_clock,
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            sync: s.sync,
            mode: SeekMode::Exact,
            playing: false,
            ended: false,
            state: "paused",
            reported_state: None,
            cams: Vec::new(),
            devices: HashMap::new(),
            next_gen: 1,
            next_run: 1,
            seek: None,
            notes_tx,
            notes,
        };
        for d in s.devices {
            e.add_device(d);
        }
        for c in s.cams {
            e.add_cam(c);
        }
        e
    }

    fn add_device(&mut self, d: DevSetup) {
        self.devices.entry(d.key).or_insert(Dev {
            session: d.session,
            utc_offset_s: d.utc_offset_s,
            index: d.index,
            token: Arc::new(AtomicU64::new(0)),
            busy: false,
            job: None,
            queue: VecDeque::new(),
            stopping: Vec::new(),
            sub_files: Arc::new(Mutex::new(HashMap::new())),
        });
    }

    fn add_cam(&mut self, c: CamSetup) {
        let offset = self.devices.get(&c.device).map_or(0, |d| d.utc_offset_s);
        let t = &self.tuning;
        self.cams.push(Cam {
            tile: c.tile,
            device: c.device,
            camera: c.camera,
            tier: c.tier,
            offset_ms: c.offset_ms,
            commands: c.commands,
            feed: None,
            generation: 0,
            buf: Arc::new(Mutex::new(CamBuf::new(offset, t.buffer_ms, t.buffer_bytes))),
            state: CamState::Buffering,
            reported: None,
            run: 0,
            reader: None,
            pending: false,
            target_ms: 0,
            target_reached: false,
            last_released_pts: None,
            dropping: false,
            ahead: false,
            retried: false,
            retry_from: None,
            started: None,
            behind_since: None,
            suspended: false,
            stream_done: false,
            files: Vec::new(),
            quiet_until_ms: i64::MIN,
            restarts: VecDeque::new(),
        });
    }

    fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }

    fn position(&self) -> i64 {
        self.play_clock.position(self.now_ns())
    }

    /// Runs until closed. `gate` opens once the app has the playback's and
    /// tiles' IDs.
    pub(crate) async fn run(mut self, mut cmds: mpsc::Receiver<Cmd>, gate: oneshot::Receiver<()>) {
        let _ = gate.await;
        info!(
            playback = self.id,
            cameras = self.cams.len(),
            "playback opened"
        );
        let p = self.position();
        self.seek_all(p, SeekMode::Exact);
        let mut tick = tokio::time::interval(self.tuning.tick);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                c = cmds.recv() => match c {
                    Some(Cmd::Close(ack)) => {
                        self.shut().await;
                        let _ = ack.send(());
                        return;
                    }
                    Some(c) => self.command(c).await,
                    None => {
                        self.shut().await;
                        return;
                    }
                },
                Some(n) = self.notes.recv() => self.note(n),
                _ = tick.tick() => {}
            }
            self.step();
        }
    }

    /// Stops every play and gives the devices up.
    async fn shut(&mut self) {
        let mut readers = Vec::new();
        for d in self.devices.values_mut() {
            d.token.fetch_add(1, Ordering::SeqCst);
            d.cancel_start().await;
            readers.append(&mut d.stopping);
        }
        for c in &mut self.cams {
            readers.extend(c.reader.take());
        }
        for r in readers {
            r.stop().await;
        }
        for key in self.devices.keys() {
            self.hub.release(key, self.id);
        }
        info!(playback = self.id, "playback stopped");
    }

    async fn command(&mut self, c: Cmd) {
        match c {
            Cmd::Play => {
                if self.ended {
                    self.ended = false;
                    self.seek_all(self.start_ms, SeekMode::Exact);
                }
                self.playing = true;
            }
            Cmd::Pause => self.playing = false,
            Cmd::Seek(p, mode) => {
                let p = p.clamp(self.start_ms, self.end_ms);
                self.ended = false;
                self.seek_all(p, mode);
            }
            Cmd::Rate(r) => {
                let now = self.now_ns();
                self.play_clock.set_rate(r, now);
                let p = self.position();
                info!(playback = self.id, rate = r, "rate changed");
                if !self.ended {
                    self.restart_all(p, None);
                }
            }
            Cmd::Offset(tile, ms) => {
                if let Some(c) = self.cams.iter_mut().find(|c| c.tile == tile) {
                    c.offset_ms = ms;
                    let key = c.device.clone();
                    self.restart_one_device(&key);
                }
            }
            Cmd::Tier(tile, tier) => {
                if let Some(c) = self.cams.iter_mut().find(|c| c.tile == tile) {
                    c.tier = tier;
                    let key = c.device.clone();
                    self.restart_one_device(&key);
                }
            }
            Cmd::Add(c, d) => {
                if let Some(d) = d {
                    self.add_device(d);
                }
                let key = c.device.clone();
                self.add_cam(c);
                self.restart_one_device(&key);
            }
            Cmd::Remove(tile, ack) => {
                if let Some(i) = self.cams.iter().position(|c| c.tile == tile) {
                    let mut c = self.cams.remove(i);
                    if let Some(r) = c.reader.take() {
                        r.stop().await;
                    }
                    if self.cams.iter().any(|o| o.device == c.device) {
                        self.restart_one_device(&c.device);
                    } else {
                        self.drop_device(&c.device).await;
                    }
                }
                let _ = ack.send(());
            }
            Cmd::Evict(key, ack) => {
                let readers = self.give_up(&key, CamState::Failed("device_in_use"));
                info!(playback = self.id, "device taken over by a newer playback");
                tokio::spawn(async move {
                    for r in readers {
                        r.stop().await;
                    }
                    let _ = ack.send(());
                });
            }
            Cmd::DeviceGone(key, ack) => {
                let readers = self.give_up(&key, CamState::Failed("session_closed"));
                for r in readers {
                    r.stop().await;
                }
                self.cams.retain(|c| c.device != key);
                self.drop_device(&key).await;
                let _ = ack.send(());
            }
            Cmd::Status(ack) => {
                let _ = ack.send(self.status());
            }
            Cmd::Shown(ack) => {
                let _ = ack.send(self.shown());
            }
            Cmd::Close(_) => unreachable!("handled by the loop"),
        }
    }

    /// Stops a device's plays and marks its cameras; returns the readers to
    /// stop.
    fn give_up(&mut self, key: &str, state: CamState) -> Vec<Reader> {
        let mut readers = Vec::new();
        if let Some(d) = self.devices.get_mut(key) {
            d.token.fetch_add(1, Ordering::SeqCst);
            d.queue.clear();
            readers.append(&mut d.stopping);
        }
        for c in self.cams.iter_mut().filter(|c| c.device == key) {
            readers.extend(c.reader.take());
            c.pending = false;
            c.suspended = false;
            if c.state != state {
                if let Some(f) = &c.feed {
                    f.push(FeedItem::Clear);
                }
                c.state = state;
            }
        }
        readers
    }

    async fn drop_device(&mut self, key: &str) {
        if let Some(mut d) = self.devices.remove(key) {
            d.token.fetch_add(1, Ordering::SeqCst);
            d.cancel_start().await;
            for r in d.stopping.drain(..) {
                r.stop().await;
            }
        }
        self.hub.release(key, self.id);
    }

    fn note(&mut self, n: Note) {
        match n {
            Note::Started {
                device,
                token,
                plays,
                missing,
                failed,
            } => {
                let current = self
                    .devices
                    .get(&device)
                    .is_some_and(|d| d.token.load(Ordering::SeqCst) == token);
                if let Some(d) = self.devices.get_mut(&device) {
                    d.busy = false;
                }
                if !current {
                    drop(plays);
                    self.launch_next(&device);
                    return;
                }
                let now = Instant::now();
                for (tile, run, play) in plays {
                    let Some(c) = self
                        .cams
                        .iter_mut()
                        .find(|c| c.tile == tile && c.run == run)
                    else {
                        continue;
                    };
                    let (stop_tx, stop_rx) = oneshot::channel();
                    let task = tokio::spawn(read(
                        play,
                        c.buf.clone(),
                        tile,
                        run,
                        stop_rx,
                        self.notes_tx.clone(),
                        self.tuning.stop_wait,
                    ));
                    c.reader = Some(Reader {
                        stop: Some(stop_tx),
                        task,
                    });
                    c.started = Some(now);
                }
                for (tile, run) in missing {
                    if let Some(c) = self
                        .cams
                        .iter_mut()
                        .find(|c| c.tile == tile && c.run == run)
                    {
                        no_recording(c, i64::MIN);
                    }
                }
                if let Some((reason, items)) = failed {
                    for (tile, run) in items {
                        if let Some(c) = self
                            .cams
                            .iter_mut()
                            .find(|c| c.tile == tile && c.run == run)
                        {
                            fail(c, reason);
                        }
                    }
                }
                self.launch_next(&device);
            }
            Note::Suspended { tile, run } => {
                if let Some(c) = self
                    .cams
                    .iter_mut()
                    .find(|c| c.tile == tile && c.run == run)
                {
                    debug!(playback = self.id, tile, "buffer full; play stopped");
                    c.reader = None;
                    c.suspended = true;
                }
            }
            Note::Ended { tile, run } => {
                if let Some(c) = self
                    .cams
                    .iter_mut()
                    .find(|c| c.tile == tile && c.run == run)
                {
                    c.reader = None;
                    c.stream_done = true;
                }
            }
            Note::Error { tile, run, error } => {
                let restarts = self.tuning.restarts_per_minute;
                let Some(c) = self
                    .cams
                    .iter_mut()
                    .find(|c| c.tile == tile && c.run == run)
                else {
                    return;
                };
                c.reader = None;
                match error {
                    DownloadError::Closed => fail(c, "session_closed"),
                    DownloadError::Refused(_) | DownloadError::NotAllowed => {
                        fail(c, "start_rejected");
                    }
                    DownloadError::BadMedia => fail(c, "undecodable"),
                    DownloadError::Overflow => c.suspended = true,
                    DownloadError::Stalled | DownloadError::ConnectionLost => {
                        let now = Instant::now();
                        while c
                            .restarts
                            .front()
                            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
                        {
                            c.restarts.pop_front();
                        }
                        if c.restarts.len() < restarts as usize {
                            c.restarts.push_back(now);
                            c.suspended = true;
                        } else {
                            fail(c, "stream_ended");
                        }
                    }
                    DownloadError::Busy | DownloadError::Cancelled => fail(c, "limit"),
                }
            }
        }
    }

    /// A seek of the whole playback: every device's group restarts at
    /// `position` as one new generation of every tile.
    fn seek_all(&mut self, position: i64, mode: SeekMode) {
        let now = self.now_ns();
        self.play_clock.set(position, now);
        self.mode = mode;
        let generation = self.restart_all(position, Some(mode));
        self.seek = Some(SeekReport {
            generation,
            position_ms: position,
            done: false,
        });
        self.events.send(
            json!({"type": "playback_seek", "playback": self.id, "position_ms": position,
                   "generation": generation, "state": "starting"})
            .to_string(),
            Priority::State,
        );
    }

    /// Restarts every device's group at `position` with one new generation;
    /// returns it.
    fn restart_all(&mut self, position: i64, mode: Option<SeekMode>) -> u32 {
        if let Some(m) = mode {
            self.mode = m;
        }
        let generation = self.next_gen;
        self.next_gen = self.next_gen.wrapping_add(1);
        let keys: Vec<String> = self.devices.keys().cloned().collect();
        for k in keys {
            self.restart_device(&k, position, generation);
        }
        generation
    }

    fn restart_one_device(&mut self, key: &str) {
        if self.ended {
            return;
        }
        let generation = self.next_gen;
        self.next_gen = self.next_gen.wrapping_add(1);
        let p = self.position();
        self.restart_device(key, p, generation);
    }

    /// Stops a device's plays, gives each of its cameras a new generation
    /// at `position`, and queues one group start at a common seek time.
    fn restart_device(&mut self, key: &str, position: i64, generation: u32) {
        let Some(d) = self.devices.get_mut(key) else {
            return;
        };
        d.token.fetch_add(1, Ordering::SeqCst);
        d.queue.clear();
        let index = d.index.clone();
        let has_session = d.session.is_some();
        let (start_s, end_s) = (
            self.start_ms.div_euclid(1000),
            (self.end_ms + 999).div_euclid(1000),
        );
        let mut seek_ms = i64::MAX;
        // The latest start of the files that hold the cameras' targets: the
        // seek time stays inside every one of them.
        let mut floor_ms = i64::MIN;
        let lead = self.seek_lead_ms();
        let mut tiles = Vec::new();
        let mut stopping = Vec::new();
        let max_ms = (self.tuning.buffer_ms as f64 * self.play_clock.rate().max(1.0)) as i64;
        for c in self.cams.iter_mut().filter(|c| c.device == key) {
            stopping.extend(c.reader.take());
            c.files = lock(&index).files(c.camera, start_s, end_s);
            c.run = self.next_run;
            self.next_run += 1;
            let target = position + c.offset_ms;
            {
                let mut b = lock(&c.buf);
                b.set_max_ms(max_ms);
                b.restart(c.run, target);
            }
            let (tx, rx) = feed();
            let _ = c.commands.try_send(TileCommand::Playback {
                generation,
                tier: c.camera.stream(c.tier),
                feed: rx,
            });
            c.feed = Some(tx);
            c.generation = generation;
            c.target_ms = target;
            c.target_reached = false;
            c.last_released_pts = None;
            c.dropping = false;
            c.ahead = false;
            c.retried = false;
            c.retry_from = None;
            c.started = None;
            c.behind_since = None;
            c.suspended = false;
            c.stream_done = false;
            c.quiet_until_ms = i64::MIN;
            c.pending = false;
            if !has_session {
                fail(c, "session_closed");
            } else if let Some(f) = c.file_at(target.div_euclid(1000)) {
                floor_ms = floor_ms.max(f.start * 1000);
                c.state = CamState::Buffering;
                c.pending = true;
                seek_ms = seek_ms.min(target);
                tiles.push(c.tile);
            } else {
                no_recording(c, i64::MIN);
            }
        }
        let seek_ms = seek_ms.saturating_sub(lead).max(floor_ms).min(seek_ms);
        let d = self.devices.get_mut(key).expect("checked");
        d.stopping.append(&mut stopping);
        if !tiles.is_empty() {
            d.queue.push_back(Launch {
                at: seek_ms.div_euclid(1000),
                tiles,
            });
        }
        let key = key.to_owned();
        self.launch_next(&key);
    }

    /// How far before its target an exact seek starts the device's plays.
    fn seek_lead_ms(&self) -> i64 {
        if self.mode == SeekMode::Exact {
            self.tuning.seek_lead_ms
        } else {
            0
        }
    }

    /// Starts the device's next queued launch unless one is in flight.
    fn launch_next(&mut self, key: &str) {
        let speed = device_speed(self.play_clock.rate());
        let Some(d) = self.devices.get_mut(key) else {
            return;
        };
        if d.busy {
            return;
        }
        let Some(session) = d.session.clone() else {
            d.queue.clear();
            return;
        };
        let Some(launch) = d.queue.pop_front() else {
            // Nothing to start: stop what waits to be stopped anyway.
            let stopping = std::mem::take(&mut d.stopping);
            if !stopping.is_empty() {
                tokio::spawn(async move {
                    for r in stopping {
                        r.stop().await;
                    }
                });
            }
            return;
        };
        let mut items = Vec::new();
        for c in self
            .cams
            .iter_mut()
            .filter(|c| c.device == key && c.pending && launch.tiles.contains(&c.tile))
        {
            c.pending = false;
            items.push(StartItem {
                tile: c.tile,
                run: c.run,
                camera: c.camera,
                tier: c.tier,
                main_file: c.file_at(launch.at).cloned(),
            });
        }
        if items.is_empty() {
            return self.launch_next(key);
        }
        let d = self.devices.get_mut(key).expect("checked");
        d.busy = true;
        let job = StartJob {
            hub: self.hub.clone(),
            playback: self.id,
            key: key.to_owned(),
            session,
            token: d.token.clone(),
            my_token: d.token.load(Ordering::SeqCst),
            sub_files: d.sub_files.clone(),
            speed,
            at: launch.at,
            items,
            stopping: std::mem::take(&mut d.stopping),
            notes: self.notes_tx.clone(),
        };
        d.job = Some(tokio::spawn(job.run()));
    }

    /// Queues a group of one: a camera whose play stopped (buffer full,
    /// stall) or whose recording resumes.
    fn start_single(&mut self, i: usize, at: Secs, continue_buffer: bool) {
        let run = self.next_run;
        self.next_run += 1;
        let c = &mut self.cams[i];
        c.run = run;
        if continue_buffer {
            lock(&c.buf).resume(run, at * 1000);
        } else {
            lock(&c.buf).restart(run, at * 1000);
        }
        c.pending = true;
        c.started = None;
        c.suspended = false;
        c.ahead = false;
        let (key, tile) = (c.device.clone(), c.tile);
        if let Some(d) = self.devices.get_mut(&key) {
            d.queue.push_back(Launch {
                at,
                tiles: vec![tile],
            });
        }
        self.launch_next(&key);
    }

    /// One step: the clock, the cameras' waits and ends, releases,
    /// restarts, and the events.
    fn step(&mut self) {
        let now = Instant::now();
        let now_ns = self.now_ns();
        let mut pos = self.play_clock.position(now_ns);
        // The end of the playback: its range, or the last camera's end.
        let last_end = self
            .cams
            .iter()
            .filter_map(|c| c.end_ms().map(|e| e - c.offset_ms))
            .max();
        let end = last_end.map_or(self.end_ms, |e| e.min(self.end_ms));
        if self.playing && !self.ended && pos >= end {
            self.play_clock.set(end, now_ns);
            self.play_clock.hold(now_ns);
            pos = end;
            self.end_all();
        }
        // Whether the clock waits for a camera.
        let mut wait = false;
        if self.playing && !self.ended {
            for c in &mut self.cams {
                if !c.state.active() || c.stream_done {
                    c.behind_since = None;
                    continue;
                }
                let at = pos + c.offset_ms;
                // Ready: it holds media at the position and has reached its
                // generation's target (or holds a first picture after it
                // for its time). A camera still decoding up to its target
                // is waited for like one with nothing yet, so a seek starts
                // every camera at the same moment.
                let ready = c.has_data_at(at) && (c.target_reached || c.ahead);
                if ready || !c.covered(at) {
                    c.behind_since = None;
                    if c.state == CamState::Late {
                        c.state = CamState::Playing;
                    }
                    continue;
                }
                let since = *c.behind_since.get_or_insert(now);
                let limit = if c.target_reached {
                    self.tuning.wait_limit
                } else {
                    self.tuning.first_media
                };
                if self.sync == SyncMode::Wait
                    && c.state != CamState::Late
                    && now.duration_since(since) < limit
                {
                    wait = true;
                } else if c.target_reached && c.state == CamState::Playing {
                    c.state = CamState::Late;
                }
            }
        }
        if self.playing && !self.ended && !wait {
            self.play_clock.run(now_ns);
        } else {
            self.play_clock.hold(now_ns);
        }
        let pos = self.play_clock.position(now_ns);
        let running = self.play_clock.running();
        for i in 0..self.cams.len() {
            self.camera_step(i, pos, running, now);
        }
        self.state = if self.ended {
            "ended"
        } else if !self.cams.is_empty()
            && self
                .cams
                .iter()
                .all(|c| matches!(c.state, CamState::Failed(_)))
        {
            "failed"
        } else if !self.playing {
            "paused"
        } else if wait {
            "buffering"
        } else {
            "playing"
        };
        self.shared.publish(pos, state_code(self.state));
        self.report(pos);
    }

    fn end_all(&mut self) {
        self.ended = true;
        let mut readers = Vec::new();
        for d in self.devices.values_mut() {
            d.token.fetch_add(1, Ordering::SeqCst);
            d.queue.clear();
            readers.append(&mut d.stopping);
        }
        for c in &mut self.cams {
            readers.extend(c.reader.take());
            c.pending = false;
            if c.state.active() || c.state == CamState::NoRecording {
                c.state = CamState::Ended;
            }
        }
        info!(playback = self.id, "playback ended");
        tokio::spawn(async move {
            for r in readers {
                r.stop().await;
            }
        });
    }

    fn camera_step(&mut self, i: usize, pos: i64, running: bool, now: Instant) {
        let rate = self.play_clock.rate();
        let tuning = self.tuning;
        let c = &mut self.cams[i];
        let at = pos + c.offset_ms;
        // Gave nothing within the first-media limit: unavailable.
        if c.state == CamState::Buffering
            && c.started
                .is_some_and(|s| now.duration_since(s) >= tuning.first_media)
            && !lock(&c.buf).got_media
        {
            warn!(playback = self.id, tile = c.tile, "no media from the play");
            let quiet = c
                .file_at(at.div_euclid(1000))
                .map_or(i64::MIN, |f| (f.end + 1) * 1000);
            if let Some(r) = c.reader.take() {
                drop(r);
            }
            no_recording(c, quiet);
            return;
        }
        if c.state.active() {
            self.release(i, pos, running, rate);
        }
        let c = &mut self.cams[i];
        // The play began at a keyframe well after the target although the
        // seek lead was asked for: once, it starts again earlier, so the
        // camera shows the target like the others (paused, or playing).
        if let Some(from) = c.retry_from.take()
            && c.state.active()
            && !c.pending
            && !self.ended
        {
            if let Some(r) = c.reader.take()
                && let Some(d) = self.devices.get_mut(&c.device)
            {
                d.stopping.push(r);
            }
            info!(
                playback = self.id,
                tile = c.tile,
                "play began after the target; it starts again earlier"
            );
            self.start_single(i, from.div_euclid(1000), false);
            return;
        }
        let c = &mut self.cams[i];
        // The camera's own end.
        if c.state.active() && c.end_ms().is_some_and(|e| at >= e) && lock(&c.buf).is_empty() {
            if let Some(r) = c.reader.take() {
                drop(r);
            }
            c.state = CamState::Ended;
            return;
        }
        // Nothing recorded at the position (a gap between files); the play
        // may go on across it by itself.
        if matches!(c.state, CamState::Playing | CamState::Late)
            && !c.covered(at)
            && !c.next_due_by(at)
            && c.target_reached
        {
            c.state = CamState::NoRecording;
            if let Some(f) = &c.feed {
                f.push(FeedItem::Clear);
            }
            return;
        }
        // The recording resumes: a group of one at its next file.
        if c.state == CamState::NoRecording && c.reader.is_none() && !c.pending {
            let next = if c.covered(at) && at >= c.quiet_until_ms {
                Some(at)
            } else {
                c.next_start_ms(at)
                    .filter(|s| *s <= at + tuning.prefetch_ms * rate.max(1.0) as i64)
                    .filter(|s| *s >= c.quiet_until_ms)
            };
            if let Some(s) = next.filter(|_| !self.ended) {
                let c = &mut self.cams[i];
                c.state = CamState::Buffering;
                c.target_reached = true;
                if let Some(f) = &c.feed {
                    f.push(FeedItem::Item(Item::Discontinuity));
                }
                self.start_single(i, s.div_euclid(1000), false);
            }
            return;
        }
        if c.state == CamState::NoRecording
            && (c.reader.is_some() || c.pending)
            && c.covered(at)
            && c.next_due_by(at + 1000)
        {
            // The play went on across the gap by itself.
            c.state = CamState::Playing;
        }
        // Left behind: nothing at the position for the wait limit (its
        // play stalled or runs behind the clock), so it is `late`. Its play
        // alone starts again at the position, so it rejoins the others
        // instead of trailing them; again after each further wait limit
        // behind, a few times a minute at most, then it stays `late`.
        let lead = self.seek_lead_ms();
        let c = &mut self.cams[i];
        if c.state == CamState::Late
            && !c.pending
            && !c.suspended
            && !c.stream_done
            && !self.ended
            && c.behind_since
                .is_some_and(|s| now.duration_since(s) >= tuning.wait_limit)
        {
            c.behind_since = Some(now);
            while c
                .restarts
                .front()
                .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
            {
                c.restarts.pop_front();
            }
            if c.restarts.len() < tuning.restarts_per_minute as usize {
                c.restarts.push_back(now);
                let floor = c
                    .file_at(at.div_euclid(1000))
                    .map_or(at, |f| f.start * 1000);
                let from = (at - lead).max(floor).min(at);
                if let Some(r) = c.reader.take()
                    && let Some(d) = self.devices.get_mut(&c.device)
                {
                    d.stopping.push(r);
                }
                info!(
                    playback = self.id,
                    tile = c.tile,
                    "camera left behind; its play restarts"
                );
                self.start_single(i, from.div_euclid(1000), false);
                return;
            }
        }
        // A stopped play (buffer full, stall) starts again from the newest
        // second held once the buffer has drained.
        let c = &mut self.cams[i];
        if c.suspended && c.state.active() && !c.pending && !self.ended {
            let (low, horizon) = {
                let b = lock(&c.buf);
                (b.low_water(), b.horizon_ms())
            };
            if low {
                // From the newest picture held, less the seek lead (its
                // recorded time is known to within a second, and a device
                // may start at a later keyframe); what is held already is
                // skipped.
                let newest = horizon.unwrap_or(at).max(at - 1000);
                if c.end_ms().is_some_and(|e| newest >= e) {
                    c.suspended = false;
                    c.stream_done = true;
                } else {
                    let floor = c
                        .file_at(newest.div_euclid(1000))
                        .map_or(newest, |f| f.start * 1000);
                    let from = (newest - tuning.seek_lead_ms).max(floor).min(newest);
                    self.start_single(i, from.div_euclid(1000), true);
                }
            }
        }
    }

    /// Releases a camera's due pictures into its tile.
    fn release(&mut self, i: usize, pos: i64, running: bool, rate: f64) {
        let mode = self.mode;
        let late_ms = (self.tuning.late_ms as f64 * rate.max(1.0)) as i64;
        let preroll = self.tuning.preroll_backlog;
        let lead = self.seek_lead_ms();
        let c = &mut self.cams[i];
        let Some(feed) = c.feed.as_ref() else {
            return;
        };
        let at = pos + c.offset_ms;
        let latency_ms = if running {
            (feed.latency_us() as f64 / 1000.0 * rate) as i64
        } else {
            0
        };
        let due = at + latency_ms;
        let audio_ok = running && (rate - 1.0).abs() < f64::EPSILON;
        let playing = self.playing;
        let keyframes_only = rate >= 8.0;
        let buf = c.buf.clone();
        let mut b = lock(&buf);
        while let Some(front) = b.front() {
            let Some(pts) = front.pts() else {
                // Audio and stream info travel with the pictures before
                // them.
                let e = b.pop().expect("front");
                let keep = match e.record {
                    bc_protocol::media::Record::Audio { .. } => audio_ok && c.target_reached,
                    _ => true,
                };
                if keep {
                    feed.push(FeedItem::Item(Item::Media(e.record)));
                }
                continue;
            };
            let key = front.keyframe();
            let rec = b.recorded_ms(pts).unwrap_or(due);
            if !c.target_reached {
                if rec < c.target_ms {
                    // Before the target: as fast as the decoder answers.
                    if feed.pending() as u64 + feed.backlog() >= preroll {
                        break;
                    }
                    let e = b.pop().expect("front");
                    if keyframes_only && !key {
                        continue;
                    }
                    feed.push(FeedItem::Item(Item::Media(e.record)));
                    c.last_released_pts = Some(pts);
                    if mode == SeekMode::Keyframe && key && !feed.has_target() {
                        feed.set_target(pts);
                    }
                    continue;
                }
                if lead > 0
                    && !c.retried
                    && c.last_released_pts.is_none()
                    && rec > c.target_ms + 250
                {
                    // The play began at a keyframe well after the target: ask
                    // again from as much earlier as it was late.
                    c.retried = true;
                    let floor = c
                        .file_at(c.target_ms.div_euclid(1000))
                        .map_or(i64::MIN, |f| f.start * 1000);
                    let late = (rec - c.target_ms + 999) / 1000 * 1000;
                    let from = (c.target_ms - lead - late).max(floor);
                    if from.div_euclid(1000) < (c.target_ms - lead).max(floor).div_euclid(1000) {
                        c.retry_from = Some(from);
                        break;
                    }
                }
                if playing && rec > due && c.last_released_pts.is_none() {
                    // The first picture lies after the target (the play
                    // began at a later keyframe): shown when the clock
                    // reaches it, not ahead of the other cameras. Paused, it
                    // is shown at once (the nearest picture there is).
                    c.ahead = true;
                    break;
                }
                c.ahead = false;
                c.target_reached = true;
                if (rec == c.target_ms || c.last_released_pts.is_none())
                    && !(keyframes_only && !key)
                {
                    let e = b.pop().expect("front");
                    feed.push(FeedItem::Item(Item::Media(e.record)));
                    c.last_released_pts = Some(pts);
                    if !feed.has_target() {
                        feed.set_target(pts);
                    }
                    continue;
                }
                if !feed.has_target()
                    && let Some(last) = c.last_released_pts
                {
                    feed.set_target(last);
                }
            }
            if rec > due {
                break;
            }
            if running && rec < at - late_ms && !c.dropping {
                c.dropping = true;
                feed.push(FeedItem::Item(Item::Discontinuity));
            }
            if c.dropping {
                if key && rec >= at - late_ms {
                    c.dropping = false;
                } else {
                    b.pop();
                    continue;
                }
            }
            let e = b.pop().expect("front");
            if keyframes_only && !key {
                continue;
            }
            feed.push(FeedItem::Item(Item::Media(e.record)));
            c.last_released_pts = Some(pts);
            if c.state == CamState::Buffering {
                c.state = CamState::Playing;
            }
        }
    }

    /// Sends the events whose state changed.
    fn report(&mut self, pos: i64) {
        for c in &mut self.cams {
            if c.reported != Some(c.state) {
                c.reported = Some(c.state);
                let mut o = json!({"type": "playback_camera", "playback": self.id,
                                   "tile": c.tile, "state": c.state.name()});
                if let CamState::Failed(r) = c.state {
                    o["reason"] = json!(r);
                }
                self.events.send(o.to_string(), Priority::State);
            }
        }
        let mut seek_done = false;
        if let Some(s) = self.seek.as_mut().filter(|s| !s.done) {
            let done = self.cams.iter().all(|c| {
                c.generation != s.generation
                    || c.feed.as_ref().is_some_and(FeedTx::shown)
                    || !c.state.active()
            });
            if done {
                s.done = true;
                seek_done = true;
                info!(playback = self.id, generation = s.generation, "seek done");
                self.events.send(
                    json!({"type": "playback_seek", "playback": self.id,
                           "position_ms": s.position_ms, "generation": s.generation,
                           "state": "active"})
                    .to_string(),
                    Priority::State,
                );
            }
        }
        let rate = self.play_clock.rate();
        if seek_done || self.reported_state.is_none_or(|(st, _)| st != self.state) {
            self.reported_state = Some((self.state, pos));
            self.events.send(
                json!({"type": "playback_state", "playback": self.id, "state": self.state,
                       "position_ms": pos, "rate": rate})
                .to_string(),
                Priority::State,
            );
        }
    }

    /// Each tile's newest answered picture of the generation it shows: a
    /// generation not yet shown (a seek in progress) has none.
    fn shown(&self) -> Vec<ShownTile> {
        self.cams
            .iter()
            .map(|c| {
                let pts = c
                    .feed
                    .as_ref()
                    .filter(|f| f.shown())
                    .and_then(FeedTx::decoded);
                ShownTile {
                    tile: c.tile,
                    device: c.device.clone(),
                    camera: c.camera,
                    generation: c.generation,
                    position_ms: pts.and_then(|p| lock(&c.buf).recorded_ms(p)),
                }
            })
            .collect()
    }

    fn status(&self) -> Value {
        let cams: Vec<Value> = self
            .cams
            .iter()
            .map(|c| {
                let mut o = json!({
                    "tile": c.tile,
                    "camera": {"device": c.device, "channel": c.camera.channel, "lens": c.camera.lens},
                    "tier": match c.tier { Tier::Main => "main", Tier::Sub => "sub" },
                    "offset_ms": c.offset_ms,
                    "state": c.state.name(),
                    "generation": c.generation,
                });
                if let CamState::Failed(r) = c.state {
                    o["reason"] = json!(r);
                }
                o
            })
            .collect();
        json!({
            "playback": self.id,
            "state": self.state,
            "position_ms": self.position(),
            "rate": self.play_clock.rate(),
            "sync": match self.sync { SyncMode::Wait => "wait", SyncMode::Free => "free" },
            "start_ms": self.start_ms,
            "end_ms": self.end_ms,
            "cameras": cams,
        })
    }
}

/// Marks a camera without recording at the position: its picture is
/// cleared; it starts again where its recording resumes (not before
/// `quiet_until_ms`).
fn no_recording(c: &mut Cam, quiet_until_ms: i64) {
    c.state = CamState::NoRecording;
    c.quiet_until_ms = quiet_until_ms;
    c.target_reached = true;
    c.suspended = false;
    lock(&c.buf).clear();
    if let Some(f) = &c.feed {
        f.push(FeedItem::Clear);
    }
}

fn fail(c: &mut Cam, reason: &'static str) {
    c.state = CamState::Failed(reason);
    c.pending = false;
    c.suspended = false;
    if let Some(f) = &c.feed {
        f.push(FeedItem::Clear);
    }
}

/// One camera of a start.
struct StartItem {
    tile: u64,
    run: u64,
    camera: DeviceCamera,
    tier: Tier,
    main_file: Option<IndexedFile>,
}

/// A start of plays on one device, run as a task.
struct StartJob {
    hub: Arc<HubCore>,
    playback: u64,
    key: String,
    session: Arc<DeviceSession>,
    token: Arc<AtomicU64>,
    my_token: u64,
    sub_files: SubFiles,
    speed: u32,
    at: Secs,
    items: Vec<StartItem>,
    stopping: Vec<Reader>,
    notes: mpsc::Sender<Note>,
}

/// Tries of a group start the session refuses as busy (the permits of
/// plays just stopped are freed a moment later).
const BUSY_TRIES: u32 = 5;

impl StartJob {
    fn stale(&self) -> bool {
        self.token.load(Ordering::SeqCst) != self.my_token
    }

    async fn run(mut self) {
        for r in std::mem::take(&mut self.stopping) {
            r.stop().await;
        }
        let lock = self.hub.device_lock(&self.key);
        let guard = lock.lock().await;
        let note = self.start().await;
        drop(guard);
        let _ = self.notes.send(note).await;
    }

    fn note(&self, plays: Vec<(u64, u64, Play)>, missing: Vec<(u64, u64)>) -> Note {
        Note::Started {
            device: self.key.clone(),
            token: self.my_token,
            plays,
            missing,
            failed: None,
        }
    }

    async fn start(&mut self) -> Note {
        if self.stale() {
            return self.note(Vec::new(), Vec::new());
        }
        self.hub.claim(&self.key, self.playback).await;
        if self.stale() {
            return self.note(Vec::new(), Vec::new());
        }
        let at = date_time(self.at);
        let mut starts = Vec::new();
        let mut ids = Vec::new();
        let mut missing = Vec::new();
        for it in &self.items {
            let file = match it.tier {
                Tier::Main => it
                    .main_file
                    .as_ref()
                    .map(|f| (f.id.clone(), f.name.clone())),
                Tier::Sub => self.sub_file(it.camera).await,
            };
            match file {
                Some((file_id, file_name)) => {
                    starts.push(PlayStart {
                        request: PlayRequest {
                            channel: it.camera.channel,
                            uid: None,
                            stream: it.camera.stream(it.tier),
                            file_id,
                            file_name,
                            speed: self.speed,
                        },
                        at,
                    });
                    ids.push((it.tile, it.run));
                }
                None => missing.push((it.tile, it.run)),
            }
        }
        if starts.is_empty() || self.stale() {
            return self.note(Vec::new(), missing);
        }
        let mut tries = 0;
        let result = loop {
            match self.session.play_group(&starts, Some(at)).await {
                Err(SearchError::Query(QueryError::Busy)) if tries + 1 < BUSY_TRIES => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                r => break r,
            }
        };
        match result {
            Ok(plays) => {
                if self.stale() {
                    for p in plays {
                        p.stop(Duration::from_secs(2)).await;
                    }
                    return self.note(Vec::new(), Vec::new());
                }
                info!(
                    playback = self.playback,
                    plays = plays.len(),
                    speed = self.speed,
                    "plays started"
                );
                let plays = ids
                    .into_iter()
                    .zip(plays)
                    .map(|((t, r), p)| (t, r, p))
                    .collect();
                self.note(plays, missing)
            }
            Err(e) => {
                warn!(playback = self.playback, error = %e, "plays could not start");
                let reason = match e {
                    SearchError::Query(QueryError::Closed) => "session_closed",
                    SearchError::Query(QueryError::Busy) => "limit",
                    _ => "start_rejected",
                };
                Note::Started {
                    device: self.key.clone(),
                    token: self.my_token,
                    plays: Vec::new(),
                    missing,
                    failed: Some((reason, ids)),
                }
            }
        }
    }

    /// The camera's sub-stream file that holds the seek time (its day's
    /// sub-stream files are searched once per playback).
    async fn sub_file(&self, camera: DeviceCamera) -> Option<(String, String)> {
        let day = day_start(self.at);
        let cached = lock(&self.sub_files).get(&(camera, day)).cloned();
        let files = match cached {
            Some(f) => f,
            None => {
                let start = date_time(day);
                let q = FileSearch {
                    channel: camera.channel,
                    uid: None,
                    stream: camera.stream(Tier::Sub),
                    start,
                    end: start.end_of_day(),
                };
                match self.session.search_files(&q).await {
                    Ok(f) => {
                        lock(&self.sub_files).insert((camera, day), f.clone());
                        f
                    }
                    Err(e) => {
                        warn!(error = %e, "sub-stream file search failed");
                        return None;
                    }
                }
            }
        };
        files
            .iter()
            .find(|f| recordings::secs(f.start) <= self.at && self.at <= recordings::secs(f.end))
            .map(|f| (f.id.clone(), f.name.clone()))
    }
}
