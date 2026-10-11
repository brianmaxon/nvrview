//! One search lane per device: the index fills on demand and cover
//! thumbnails come first (docs/MILESTONE-P.md, slices C4 and C5).
//!
//! The device's own index is the real one; ours is a cache of what views
//! asked for (docs/FFI-RECORDINGS.md, decision 8). A [`DeviceLane`] is one
//! task per device that sends **one search at a time** over the device's
//! session, choosing each time what to do next, in this order:
//!
//! 1. **Cover thumbnails** ([`LaneHandle::request_thumbnail`]), newest
//!    request first, because they are what a person is looking at. At most
//!    [`LaneConfig::max_thumbnails`] wait; beyond that the oldest is dropped
//!    and reported. [`LaneHandle::cancel_thumbnails`] drops waiting ones
//!    (a fetch already sent completes and is cached).
//! 2. **Days a view asked for** ([`LaneHandle::request_days`]), newest day
//!    first, one unit at a time: a month's calendar (142, for every camera
//!    asked in that month at once), then each camera-day's files and events
//!    (14–16, 272–274). Days already indexed are not searched again unless
//!    the request forces it (a user's refresh).
//! 3. **Today, while held** ([`LaneHandle::hold_today`]): today's calendar
//!    and day are searched again once they are older than
//!    [`LaneConfig::refresh_today`], as long as some view holds them.
//!
//! Nothing runs in the background otherwise. Because a thumbnail can go
//! between any two units of index work, a long fill never delays what is
//! on screen by more than one search.
//!
//! **Device state.** While the session is not connected the lane waits
//! ([`LaneState::WaitingDevice`]); requests stay queued. After a refused
//! login or a closed session nothing can succeed: every waiting request is
//! reported failed and later ones fail at once, until the lane is dropped
//! and a new one is made for a new session.
//!
//! **Threads.** Index and cache writes (with their `fsync`) run on blocking
//! threads, never on an async worker that live view may share. Events go to
//! a callback, called from the lane's task without any lock held; it must
//! not block (the owner queues them, bounded).

use crate::index::DeviceIndex;
use crate::indexer::{
    IndexedChannel, day_needed, day_record_request, index_day, month_bounds, month_stale,
    recorded_days_of,
};
use crate::thumbs::{ThumbKey, ThumbnailCache, fetch_cover};
use crate::{Clock, DeviceCamera, Secs, day_start, secs};
use bc_session::{ConnState, DeviceSession, SearchError};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// Most distinct days one [`LaneHandle::request_days`] may ask for.
pub const MAX_REQUEST_DAYS: usize = 31;

/// Settings of a lane.
#[derive(Clone)]
pub struct LaneConfig {
    /// Search today's day and calendar again when older than this many
    /// seconds, while held (default 60).
    pub refresh_today: Secs,
    /// How often an idle lane looks whether a held today is due (default
    /// 5 s).
    pub hold_poll: Duration,
    /// Most thumbnail requests waiting (default 256).
    pub max_thumbnails: usize,
    /// Most camera-days waiting to be searched (default 4096).
    pub max_days: usize,
    /// The device's local time.
    pub clock: Clock,
}

impl LaneConfig {
    /// Defaults with the device's clock.
    pub fn new(clock: Clock) -> LaneConfig {
        LaneConfig {
            refresh_today: 60,
            hold_poll: Duration::from_secs(5),
            max_thumbnails: 256,
            max_days: 4096,
            clock,
        }
    }
}

impl fmt::Debug for LaneConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaneConfig")
            .field("refresh_today", &self.refresh_today)
            .field("hold_poll", &self.hold_poll)
            .field("max_thumbnails", &self.max_thumbnails)
            .field("max_days", &self.max_days)
            .finish_non_exhaustive()
    }
}

/// What became of a thumbnail request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThumbState {
    /// Cached: read it from the cache.
    Ready,
    /// No recording in the range (nothing cached).
    Empty,
    /// The fetch failed (English reason, for logs).
    Failed(String),
    /// Dropped from a full queue before it was fetched.
    Dropped,
}

/// What the lane is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneState {
    /// Nothing to do. Also reported again after requests that needed no
    /// search, so every batch of requests ends with it.
    Idle,
    /// Working through requests.
    Busy,
    /// The device is not connected; requests wait.
    WaitingDevice,
    /// The session can never succeed again (login refused, or closed).
    Ended,
}

/// What the lane reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaneEvent {
    /// A camera's day was searched and stored; the index is now at
    /// `revision`.
    DayIndexed {
        /// The camera.
        camera: DeviceCamera,
        /// Start of the day.
        day: Secs,
        /// The index's revision after storing it.
        revision: u64,
    },
    /// A camera's day (or its month's calendar) could not be searched. It
    /// is not retried by itself; ask again.
    DayFailed {
        /// The camera.
        camera: DeviceCamera,
        /// Start of the day.
        day: Secs,
        /// English reason, for logs.
        reason: String,
    },
    /// A thumbnail request ended.
    Thumbnail {
        /// What was asked.
        key: ThumbKey,
        /// How it ended.
        state: ThumbState,
    },
    /// The lane's state changed.
    State(LaneState),
}

/// Why a request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneError {
    /// More than [`MAX_REQUEST_DAYS`] days, or the queue is full.
    Limit,
}

impl fmt::Display for LaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaneError::Limit => f.write_str("too many days requested"),
        }
    }
}

impl std::error::Error for LaneError {}

/// Receives the lane's events.
pub type LaneSink = Arc<dyn Fn(LaneEvent) + Send + Sync>;

#[derive(Clone, Debug)]
struct Want {
    uid: Option<String>,
    force: bool,
}

#[derive(Default)]
struct Queues {
    /// Newest first.
    thumbs: VecDeque<(ThumbKey, Option<String>)>,
    /// Newest day first, then camera.
    days: BTreeMap<(Reverse<Secs>, DeviceCamera), Want>,
    /// Month calendars to ask again even if fresh: (month start, camera).
    force_months: BTreeSet<(Secs, DeviceCamera)>,
    /// Cameras whose today is held, per hold.
    holds: HashMap<u64, Vec<IndexedChannel>>,
    stop: bool,
}

struct Shared {
    q: Mutex<Queues>,
    changed: Notify,
    next_hold: AtomicU64,
    max_thumbnails: usize,
    max_days: usize,
}

impl Shared {
    fn q(&self) -> std::sync::MutexGuard<'_, Queues> {
        self.q.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A device's search lane. Dropping it stops the task once the search in
/// flight, if any, has completed (so the device's search is closed, never
/// abandoned half-way); [`DeviceLane::stop`] also waits for that. Requests
/// go through its [`LaneHandle`] (also reached through `Deref`).
pub struct DeviceLane {
    handle: LaneHandle,
    join: Option<JoinHandle<()>>,
}

/// Makes requests to a lane; cheap to clone, usable from any thread. After
/// the lane stops, requests are accepted and never answered.
#[derive(Clone)]
pub struct LaneHandle {
    shared: Arc<Shared>,
    sink: LaneSink,
}

impl fmt::Debug for LaneHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let q = self.shared.q();
        f.debug_struct("LaneHandle")
            .field("thumbnails", &q.thumbs.len())
            .field("days", &q.days.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for DeviceLane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceLane")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl std::ops::Deref for DeviceLane {
    type Target = LaneHandle;

    fn deref(&self) -> &LaneHandle {
        &self.handle
    }
}

/// Keeps a device's today fresh while it lives (see
/// [`LaneHandle::hold_today`]).
#[must_use = "the hold ends when dropped"]
pub struct TodayHold {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for TodayHold {
    fn drop(&mut self) {
        self.shared.q().holds.remove(&self.id);
    }
}

impl fmt::Debug for TodayHold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TodayHold").field("id", &self.id).finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl DeviceLane {
    /// Starts the lane on the current runtime.
    pub fn spawn(
        session: Arc<DeviceSession>,
        index: Arc<Mutex<DeviceIndex>>,
        thumbs: Arc<Mutex<ThumbnailCache>>,
        cfg: LaneConfig,
        sink: LaneSink,
    ) -> DeviceLane {
        let shared = Arc::new(Shared {
            q: Mutex::new(Queues::default()),
            changed: Notify::new(),
            next_hold: AtomicU64::new(1),
            max_thumbnails: cfg.max_thumbnails.max(1),
            max_days: cfg.max_days.max(1),
        });
        let worker = Worker {
            session,
            index,
            thumbs,
            cfg,
            sink: sink.clone(),
            shared: shared.clone(),
            state: None,
        };
        let join = tokio::spawn(worker.run());
        DeviceLane {
            handle: LaneHandle { shared, sink },
            join: Some(join),
        }
    }

    /// A handle for requests.
    pub fn handle(&self) -> LaneHandle {
        self.handle.clone()
    }

    /// Stops the task once the search in flight, if any, has completed,
    /// and waits for it. Waiting requests are dropped unanswered.
    pub async fn stop(mut self) {
        self.handle.shared.q().stop = true;
        self.handle.shared.changed.notify_one();
        if let Some(j) = self.join.take() {
            let _ = j.await;
        }
    }
}

impl Drop for DeviceLane {
    fn drop(&mut self) {
        self.handle.shared.q().stop = true;
        self.handle.shared.changed.notify_one();
    }
}

impl LaneHandle {
    /// Asks for these cameras' days (any time within each day) to be in
    /// the index. Days already indexed are skipped unless `force` (a
    /// user's refresh), which also asks the months' calendars again.
    /// [`LaneEvent::DayIndexed`] follows for each day searched.
    pub fn request_days(
        &self,
        cameras: &[IndexedChannel],
        days: &[Secs],
        force: bool,
    ) -> Result<(), LaneError> {
        let days: BTreeSet<Secs> = days.iter().map(|d| day_start(*d)).collect();
        if days.len() > MAX_REQUEST_DAYS {
            return Err(LaneError::Limit);
        }
        {
            let mut q = self.shared.q();
            let new = days
                .iter()
                .flat_map(|d| cameras.iter().map(move |c| (Reverse(*d), c.camera)))
                .filter(|k| !q.days.contains_key(k))
                .count();
            if q.days.len() + new > self.shared.max_days {
                return Err(LaneError::Limit);
            }
            for d in &days {
                for c in cameras {
                    let w = q
                        .days
                        .entry((Reverse(*d), c.camera))
                        .or_insert_with(|| Want {
                            uid: c.uid.clone(),
                            force: false,
                        });
                    w.force |= force;
                    if c.uid.is_some() {
                        w.uid = c.uid.clone();
                    }
                    if force {
                        let m = secs(month_bounds(*d).0);
                        q.force_months.insert((m, c.camera));
                    }
                }
            }
        }
        self.shared.changed.notify_one();
        Ok(())
    }

    /// Keeps today's day and calendar of these cameras fresh until the
    /// returned hold is dropped (a view showing today holds it).
    pub fn hold_today(&self, cameras: &[IndexedChannel]) -> TodayHold {
        let id = self.shared.next_hold.fetch_add(1, Ordering::Relaxed);
        self.shared.q().holds.insert(id, cameras.to_vec());
        self.shared.changed.notify_one();
        TodayHold {
            shared: self.shared.clone(),
            id,
        }
    }

    /// Asks for a cover thumbnail. A request for a key already waiting
    /// moves it to the front. [`LaneEvent::Thumbnail`] follows (`Ready`
    /// also when it was cached already).
    pub fn request_thumbnail(&self, key: ThumbKey, uid: Option<String>) {
        let dropped = {
            let mut q = self.shared.q();
            q.thumbs.retain(|(k, _)| *k != key);
            q.thumbs.push_front((key, uid));
            let mut dropped = Vec::new();
            while q.thumbs.len() > self.shared.max_thumbnails {
                if let Some((k, _)) = q.thumbs.pop_back() {
                    dropped.push(k);
                }
            }
            dropped
        };
        for key in dropped {
            (self.sink)(LaneEvent::Thumbnail {
                key,
                state: ThumbState::Dropped,
            });
        }
        self.shared.changed.notify_one();
    }

    /// Drops waiting thumbnail requests: one key, or all of them (the view
    /// scrolled away). No event follows for them.
    pub fn cancel_thumbnails(&self, key: Option<&ThumbKey>) {
        let mut q = self.shared.q();
        match key {
            Some(k) => q.thumbs.retain(|(x, _)| x != k),
            None => q.thumbs.clear(),
        }
    }

    /// Thumbnail requests and camera-days waiting.
    pub fn pending(&self) -> (usize, usize) {
        let q = self.shared.q();
        (q.thumbs.len(), q.days.len())
    }
}

/// One unit of work.
enum Unit {
    Thumb(ThumbKey, Option<String>),
    /// Calendars of one month for these cameras.
    Month(Secs, Vec<(DeviceCamera, Option<String>)>),
    /// One camera-day.
    Day(Secs, DeviceCamera, Option<String>),
}

struct Worker {
    session: Arc<DeviceSession>,
    index: Arc<Mutex<DeviceIndex>>,
    thumbs: Arc<Mutex<ThumbnailCache>>,
    cfg: LaneConfig,
    sink: LaneSink,
    shared: Arc<Shared>,
    state: Option<LaneState>,
}

impl Worker {
    fn set_state(&mut self, s: LaneState) {
        if self.state != Some(s) {
            self.state = Some(s);
            (self.sink)(LaneEvent::State(s));
        }
    }

    async fn run(mut self) {
        let mut conn = self.session.watch_state();
        let shared = self.shared.clone();
        loop {
            let notified = shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.shared.q().stop {
                return;
            }
            match self.session.state() {
                ConnState::Connected => {}
                ConnState::LoginFailed { .. } | ConnState::Closed => {
                    self.set_state(LaneState::Ended);
                    self.fail_everything("device session ended");
                    notified.await;
                    continue;
                }
                ConnState::Connecting | ConnState::Backoff { .. } => {
                    let waiting = {
                        let q = self.shared.q();
                        !q.thumbs.is_empty() || !q.days.is_empty()
                    };
                    if waiting {
                        self.set_state(LaneState::WaitingDevice);
                    }
                    tokio::select! {
                        _ = conn.changed() => {}
                        _ = &mut notified => {}
                    }
                    continue;
                }
            }
            let had_requests = {
                let q = self.shared.q();
                !q.thumbs.is_empty() || !q.days.is_empty()
            };
            match self.next_unit() {
                Some(unit) => {
                    self.set_state(LaneState::Busy);
                    self.run_unit(unit).await;
                }
                None => {
                    if self.add_due_holds() {
                        continue;
                    }
                    if self.state == Some(LaneState::Busy) {
                        self.prune().await;
                    }
                    if had_requests && self.state == Some(LaneState::Idle) {
                        // Requests that needed no search are answered too.
                        (self.sink)(LaneEvent::State(LaneState::Idle));
                    }
                    self.set_state(LaneState::Idle);
                    tokio::select! {
                        _ = &mut notified => {}
                        _ = tokio::time::sleep(self.cfg.hold_poll) => {}
                        _ = conn.changed() => {}
                    }
                }
            }
        }
    }

    /// Fails every waiting request (the session can never succeed).
    fn fail_everything(&self, reason: &str) {
        let (thumbs, days) = {
            let mut q = self.shared.q();
            q.force_months.clear();
            (std::mem::take(&mut q.thumbs), std::mem::take(&mut q.days))
        };
        for (key, _) in thumbs {
            (self.sink)(LaneEvent::Thumbnail {
                key,
                state: ThumbState::Failed(reason.to_owned()),
            });
        }
        for ((Reverse(day), camera), _) in days {
            (self.sink)(LaneEvent::DayFailed {
                camera,
                day,
                reason: reason.to_owned(),
            });
        }
    }

    /// Picks the next unit: a thumbnail, else the newest wanted day (its
    /// month's calendar first when stale), skipping days that need nothing.
    fn next_unit(&self) -> Option<Unit> {
        let now = (self.cfg.clock)();
        let mut q = self.shared.q();
        if let Some((key, uid)) = q.thumbs.pop_front() {
            return Some(Unit::Thumb(key, uid));
        }
        let idx = lock(&self.index);
        loop {
            let (&(Reverse(day), camera), want) = q.days.iter().next()?;
            let want = want.clone();
            let (m_start, m_end) = month_bounds(day);
            let (ms, me) = (secs(m_start), secs(m_end));
            let month_due = |q: &Queues, c: DeviceCamera| {
                q.force_months.contains(&(ms, c))
                    || month_stale(idx.month_fetched_at(c, ms), me, now, self.cfg.refresh_today)
            };
            if month_due(&q, camera) {
                // Every waiting camera of this month whose calendar is due,
                // in one 142.
                let cams: Vec<(DeviceCamera, Option<String>)> = q
                    .days
                    .iter()
                    .filter(|((Reverse(d), _), _)| *d >= ms && *d <= me)
                    .map(|((_, c), w)| (*c, w.uid.clone()))
                    .filter(|(c, _)| month_due(&q, *c))
                    .fold(Vec::new(), |mut v, (c, u)| {
                        if !v.iter().any(|(x, _)| *x == c) {
                            v.push((c, u));
                        }
                        v
                    });
                return Some(Unit::Month(ms, cams));
            }
            q.days.remove(&(Reverse(day), camera));
            if want.force || day_needed(&idx, camera, day, now, self.cfg.refresh_today) {
                return Some(Unit::Day(day, camera, want.uid));
            }
        }
    }

    /// Queues held todays that are due. True if any was queued.
    fn add_due_holds(&self) -> bool {
        let now = (self.cfg.clock)();
        let today = day_start(now);
        let (m_start, m_end) = month_bounds(today);
        let (ms, me) = (secs(m_start), secs(m_end));
        let mut q = self.shared.q();
        let held: Vec<IndexedChannel> = q.holds.values().flatten().cloned().collect();
        if held.is_empty() {
            return false;
        }
        let idx = lock(&self.index);
        let mut added = false;
        for c in held {
            let due = month_stale(
                idx.month_fetched_at(c.camera, ms),
                me,
                now,
                self.cfg.refresh_today,
            ) || day_needed(&idx, c.camera, today, now, self.cfg.refresh_today);
            if due && q.days.len() < self.shared.max_days {
                q.days.entry((Reverse(today), c.camera)).or_insert(Want {
                    uid: c.uid.clone(),
                    force: false,
                });
                added = true;
            }
        }
        added
    }

    fn uid(&self, camera: DeviceCamera, uid: Option<String>) -> Option<String> {
        uid.or_else(|| self.session.channel_uids().get(&camera.channel).cloned())
    }

    async fn run_unit(&mut self, unit: Unit) {
        match unit {
            Unit::Thumb(key, uid) => {
                let state = self.thumbnail(&key, uid).await;
                (self.sink)(LaneEvent::Thumbnail { key, state });
            }
            Unit::Month(ms, cams) => self.month(ms, cams).await,
            Unit::Day(day, camera, uid) => self.day(day, camera, uid).await,
        }
    }

    async fn thumbnail(&self, key: &ThumbKey, uid: Option<String>) -> ThumbState {
        let cached = lock(&self.thumbs).contains(key);
        if cached {
            return ThumbState::Ready;
        }
        let uid = uid.or_else(|| self.session.channel_uids().get(&key.channel).cloned());
        match fetch_cover(&self.session, key, uid).await {
            Ok(Some(t)) => {
                let (cache, key) = (self.thumbs.clone(), key.clone());
                let put = tokio::task::spawn_blocking(move || lock(&cache).put(&key, &t)).await;
                match put {
                    Ok(Ok(true)) => ThumbState::Ready,
                    Ok(Ok(false)) => ThumbState::Failed("larger than the cache".into()),
                    Ok(Err(e)) => ThumbState::Failed(format!("cannot cache: {e}")),
                    Err(e) => ThumbState::Failed(format!("cannot cache: {e}")),
                }
            }
            Ok(None) => ThumbState::Empty,
            Err(e) => {
                debug!(error = %e, "cover image failed");
                ThumbState::Failed(e.to_string())
            }
        }
    }

    async fn month(&mut self, ms: Secs, cams: Vec<(DeviceCamera, Option<String>)>) {
        let (m_start, m_end) = month_bounds(ms);
        let me = secs(m_end);
        let req = day_record_request(cams.iter().map(|(c, u)| (*c, self.uid(*c, u.clone()))));
        let result = self.session.day_records(m_start, m_end, &req).await;
        {
            let mut q = self.shared.q();
            for (c, _) in &cams {
                q.force_months.remove(&(ms, *c));
            }
        }
        match result {
            Ok(list) => {
                let now = (self.cfg.clock)();
                let idx = self.index.clone();
                let months: Vec<(DeviceCamera, Vec<Secs>)> = cams
                    .iter()
                    .map(|(c, _)| (*c, recorded_days_of(&list, c.channel, m_start)))
                    .collect();
                let stored = tokio::task::spawn_blocking(move || {
                    let mut idx = lock(&idx);
                    for (c, days) in months {
                        idx.set_month(c, ms, me, days, now)?;
                    }
                    Ok::<(), std::io::Error>(())
                })
                .await;
                if let Some(e) = match stored {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(e) => Some(e.to_string()),
                } {
                    warn!(error = %e, "cannot store the recording calendar");
                    self.fail_month(ms, me, &cams, &format!("cannot store: {e}"));
                }
            }
            Err(e) => {
                debug!(error = %e, "day records failed");
                self.fail_month(ms, me, &cams, &reason(&e));
            }
        }
    }

    /// Reports and drops the waiting days of these cameras in a month whose
    /// calendar could not be read.
    fn fail_month(&self, ms: Secs, me: Secs, cams: &[(DeviceCamera, Option<String>)], why: &str) {
        let failed: Vec<(Secs, DeviceCamera)> = {
            let mut q = self.shared.q();
            let keys: Vec<(Reverse<Secs>, DeviceCamera)> = q
                .days
                .keys()
                .filter(|(Reverse(d), c)| *d >= ms && *d <= me && cams.iter().any(|(x, _)| x == c))
                .copied()
                .collect();
            for k in &keys {
                q.days.remove(k);
            }
            keys.into_iter().map(|(Reverse(d), c)| (d, c)).collect()
        };
        for (day, camera) in failed {
            (self.sink)(LaneEvent::DayFailed {
                camera,
                day,
                reason: why.to_owned(),
            });
        }
    }

    async fn day(&mut self, day: Secs, camera: DeviceCamera, uid: Option<String>) {
        let c = IndexedChannel { camera, uid: None };
        let uid = self.uid(camera, uid);
        match index_day(&self.session, &c, uid, day).await {
            Ok((files, events)) => {
                let (idx, clock) = (self.index.clone(), self.cfg.clock.clone());
                let stored = tokio::task::spawn_blocking(move || {
                    let mut idx = lock(&idx);
                    idx.put_day(day, camera, files, events, clock())
                        .map(|()| idx.revision())
                })
                .await;
                let event = match stored {
                    Ok(Ok(revision)) => LaneEvent::DayIndexed {
                        camera,
                        day,
                        revision,
                    },
                    Ok(Err(e)) => LaneEvent::DayFailed {
                        camera,
                        day,
                        reason: format!("cannot store: {e}"),
                    },
                    Err(e) => LaneEvent::DayFailed {
                        camera,
                        day,
                        reason: format!("cannot store: {e}"),
                    },
                };
                (self.sink)(event);
            }
            Err(e) => {
                debug!(error = %e, channel = camera.channel, "day search failed");
                (self.sink)(LaneEvent::DayFailed {
                    camera,
                    day,
                    reason: reason(&e),
                });
            }
        }
    }

    async fn prune(&self) {
        let (idx, now) = (self.index.clone(), (self.cfg.clock)());
        let r = tokio::task::spawn_blocking(move || lock(&idx).prune(now)).await;
        if let Ok(Err(e)) = r {
            warn!(error = %e, "cannot prune the index");
        }
    }
}

fn reason(e: &SearchError) -> String {
    e.to_string()
}
