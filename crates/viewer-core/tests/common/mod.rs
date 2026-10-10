//! Test helpers: an in-process mock NVR, a fake platform decoder that
//! records every action and answers through the results API, and a
//! connector that watches what the core sends.

#![allow(dead_code)]

use bc_protocol::framer::Decoder;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use viewer_core::{
    Action, Callbacks, Codec, Core, CoreConfig, DecodeFailureKind, PipelineConfig, TileStatus,
};

pub const USER: &str = "example-user";
pub const PASSWORD: &str = "example-password";

pub fn mock(channels: u8) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = channels;
    mock_nvr::spawn(cfg).unwrap()
}

/// Polls `cond` every 10 ms; panics after `secs` seconds.
pub fn wait_until(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// One action as the fake decoder saw it.
#[derive(Clone, Debug, PartialEq)]
pub enum Seen {
    Configure {
        codec: Codec,
        width: u32,
        height: u32,
        sets: usize,
    },
    Decode {
        pts: u64,
        keyframe: bool,
        len: usize,
    },
    Flush,
    Reset,
    Clear,
    Status(TileStatus),
}

/// How the fake decoder treats a tile.
#[derive(Clone, Debug, Default)]
pub struct Behavior {
    /// Answer from a separate thread after this delay instead of inside the
    /// callback.
    pub answer_from_thread: Option<Duration>,
    /// Fail every `Configure` whose index (0-based) is in this list.
    pub fail_configures: Vec<(usize, DecodeFailureKind)>,
    /// Fail every `Decode` whose index (0-based) is in this range.
    pub fail_decodes: Option<(std::ops::Range<usize>, DecodeFailureKind)>,
    /// Sleep this long in every `Decode` callback (a slow app).
    pub slow: Option<Duration>,
    /// Never answer.
    pub silent: bool,
}

#[derive(Default)]
struct TileRec {
    seen: Vec<Seen>,
    configures: usize,
    decodes: usize,
    answered: usize,
}

#[derive(Default)]
pub struct Recorder {
    core: OnceLock<Weak<Core>>,
    tiles: Mutex<HashMap<u64, TileRec>>,
    behavior: Mutex<HashMap<u64, Behavior>>,
    busy: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    events: Mutex<Vec<serde_json::Value>>,
    pub actions: AtomicUsize,
    pub events_count: AtomicUsize,
    pub overlap: AtomicBool,
    /// Results of API calls attempted from inside a callback.
    pub reentrant: Mutex<Vec<Result<(), viewer_core::CoreError>>>,
    pub try_reentry: AtomicBool,
    /// Every finished `on_action` (by tile) and every event, in the order
    /// they happened across threads.
    timeline: Mutex<Vec<Entry>>,
}

/// One entry of [`Recorder::timeline`].
#[derive(Clone, Debug)]
pub enum Entry {
    /// An `on_action` for this tile returned.
    ActionDone(u64),
    Event(serde_json::Value),
}

impl Recorder {
    pub fn set_behavior(&self, tile: u64, b: Behavior) {
        self.behavior.lock().unwrap().insert(tile, b);
    }

    pub fn seen(&self, tile: u64) -> Vec<Seen> {
        self.tiles
            .lock()
            .unwrap()
            .get(&tile)
            .map(|t| t.seen.clone())
            .unwrap_or_default()
    }

    pub fn decodes(&self, tile: u64) -> usize {
        self.tiles
            .lock()
            .unwrap()
            .get(&tile)
            .map_or(0, |t| t.decodes)
    }

    pub fn last_status(&self, tile: u64) -> Option<TileStatus> {
        self.seen(tile).iter().rev().find_map(|s| match s {
            Seen::Status(s) => Some(*s),
            _ => None,
        })
    }

    pub fn timeline(&self) -> Vec<Entry> {
        self.timeline.lock().unwrap().clone()
    }

    /// Asserts that every tile in `tiles` got exactly one `tile_closed`, and
    /// that no `on_action` for it finished after that event; returns the
    /// reasons.
    pub fn check_closed_once(&self, tiles: &[u64]) -> Vec<String> {
        let tl = self.timeline();
        tiles
            .iter()
            .map(|&t| {
                let closes: Vec<usize> = tl
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| {
                        matches!(e, Entry::Event(v) if v["type"] == "tile_closed" && v["tile"] == t)
                    })
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(closes.len(), 1, "tile_closed for tile {t}: {tl:?}");
                let at = closes[0];
                assert!(
                    !tl[at..]
                        .iter()
                        .any(|e| matches!(e, Entry::ActionDone(x) if *x == t)),
                    "on_action for tile {t} after its tile_closed"
                );
                match &tl[at] {
                    Entry::Event(v) => v["reason"].as_str().unwrap().to_owned(),
                    Entry::ActionDone(_) => unreachable!(),
                }
            })
            .collect()
    }

    pub fn events(&self) -> Vec<serde_json::Value> {
        self.events.lock().unwrap().clone()
    }

    pub fn events_of(&self, kind: &str) -> Vec<serde_json::Value> {
        self.events()
            .into_iter()
            .filter(|e| e["type"] == kind)
            .collect()
    }

    fn core(&self) -> Option<Arc<Core>> {
        self.core.get().and_then(Weak::upgrade)
    }

    fn answer(&self, tile: u64, r: Answer, from_thread: Option<Duration>) {
        let Some(core) = self.core() else { return };
        let send = move |core: &Core| {
            let res = match r {
                Answer::Decoded(pts) => core.decoded(tile, pts),
                Answer::Failed(kind) => core.decode_failed(tile, kind),
            };
            // A tile closed meanwhile is the only expected failure.
            if let Err(e) = res {
                assert_eq!(e, viewer_core::CoreError::UnknownId);
            }
        };
        match from_thread {
            None => send(&core),
            Some(d) => {
                std::thread::spawn(move || {
                    std::thread::sleep(d);
                    send(&core);
                });
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Answer {
    Decoded(u64),
    Failed(DecodeFailureKind),
}

impl Callbacks for Recorder {
    fn on_action(&self, tile: u64, action: &Action<'_>) {
        self.actions.fetch_add(1, Ordering::SeqCst);
        let busy = self.busy.lock().unwrap().entry(tile).or_default().clone();
        if busy.swap(true, Ordering::SeqCst) {
            self.overlap.store(true, Ordering::SeqCst);
        }
        let b = self
            .behavior
            .lock()
            .unwrap()
            .get(&tile)
            .cloned()
            .unwrap_or_default();
        let mut answer = None;
        {
            let mut tiles = self.tiles.lock().unwrap();
            let rec = tiles.entry(tile).or_default();
            let seen = match *action {
                Action::Configure {
                    codec,
                    parameter_sets,
                    coded_size,
                    ..
                } => {
                    let i = rec.configures;
                    rec.configures += 1;
                    if let Some((_, kind)) = b.fail_configures.iter().find(|(n, _)| *n == i) {
                        answer = Some(Answer::Failed(*kind));
                    }
                    Seen::Configure {
                        codec,
                        width: coded_size.width,
                        height: coded_size.height,
                        sets: parameter_sets.len(),
                    }
                }
                Action::Decode {
                    access_unit,
                    keyframe,
                    pts,
                } => {
                    let i = rec.decodes;
                    rec.decodes += 1;
                    answer = Some(match &b.fail_decodes {
                        Some((r, kind)) if r.contains(&i) => Answer::Failed(*kind),
                        _ => Answer::Decoded(pts),
                    });
                    Seen::Decode {
                        pts,
                        keyframe,
                        len: access_unit.len(),
                    }
                }
                Action::Flush => Seen::Flush,
                Action::Reset => Seen::Reset,
                Action::Clear => Seen::Clear,
                Action::Status(s) => Seen::Status(s),
            };
            rec.seen.push(seen);
            if answer.is_some() {
                rec.answered += 1;
            }
        }
        if let (Some(d), Action::Decode { .. }) = (b.slow, action) {
            std::thread::sleep(d);
        }
        if let (Some(a), false) = (answer, b.silent) {
            self.answer(tile, a, b.answer_from_thread);
        }
        if self.try_reentry.load(Ordering::SeqCst) {
            if let Some(core) = self.core() {
                self.reentrant.lock().unwrap().push(core.close_tile(tile));
            }
        }
        self.timeline.lock().unwrap().push(Entry::ActionDone(tile));
        busy.store(false, Ordering::SeqCst);
    }

    fn on_event(&self, json: &str) {
        self.events_count.fetch_add(1, Ordering::SeqCst);
        let v: serde_json::Value = serde_json::from_str(json)
            .unwrap_or_else(|e| panic!("event is not JSON ({e}): {json}"));
        assert!(v["type"].is_string(), "{json}");
        self.timeline.lock().unwrap().push(Entry::Event(v.clone()));
        self.events.lock().unwrap().push(v);
        if self.try_reentry.load(Ordering::SeqCst) {
            if let Some(core) = self.core() {
                self.reentrant.lock().unwrap().push(core.remove_device(1));
            }
        }
    }
}

/// Short cooldowns and frequent stats, so recovery is visible in a test.
pub fn fast_config() -> CoreConfig {
    CoreConfig {
        pipeline: PipelineConfig {
            reset_cooldown_us: 100_000,
            max_reset_cooldown_us: 400_000,
            ..PipelineConfig::default()
        },
        backoff: viewer_core::BackoffConfig {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(200),
            jitter: 0.0,
            stable_after: Duration::from_secs(30),
        },
        stats_interval: Duration::from_millis(200),
        ..CoreConfig::default()
    }
}

pub fn start(config: CoreConfig) -> (Arc<Core>, Arc<Recorder>) {
    let rec = Arc::new(Recorder::default());
    let core = Arc::new(Core::new(config, rec.clone()).unwrap());
    rec.core.set(Arc::downgrade(&core)).unwrap();
    (core, rec)
}

/// Checks the action protocol on one tile: nothing before a `Configure`
/// except status, no `Decode` without a configured decoder, and the first
/// `Decode` after a `Configure` or `Flush` is a keyframe.
pub fn check_protocol(seen: &[Seen]) {
    let mut configured = false;
    let mut need_key = true;
    for s in seen {
        match s {
            Seen::Configure { sets, .. } => {
                assert!(*sets >= 2, "parameter sets in Configure");
                configured = true;
                need_key = true;
            }
            Seen::Decode { keyframe, len, .. } => {
                assert!(configured, "Decode before Configure: {seen:?}");
                assert!(*len > 0);
                if need_key {
                    assert!(
                        *keyframe,
                        "first Decode after Configure/Flush is not a keyframe"
                    );
                    need_key = false;
                }
            }
            Seen::Flush => need_key = true,
            Seen::Reset => configured = false,
            Seen::Clear | Seen::Status(_) => {}
        }
    }
}

/// A connector to a mock NVR that counts connections and decodes the
/// command numbers the core sends.
#[derive(Clone, Default)]
pub struct Tap {
    pub addr: Option<SocketAddr>,
    pub connects: Arc<AtomicUsize>,
    pub commands: Arc<Mutex<Vec<u32>>>,
}

impl Tap {
    pub fn new(addr: SocketAddr) -> Tap {
        Tap {
            addr: Some(addr),
            ..Tap::default()
        }
    }

    pub fn count(&self, command: u32) -> usize {
        self.commands
            .lock()
            .unwrap()
            .iter()
            .filter(|c| **c == command)
            .count()
    }
}

pub struct TapStream {
    inner: TcpStream,
    dec: Decoder,
    commands: Arc<Mutex<Vec<u32>>>,
}

impl viewer_core::Connector for Tap {
    type Stream = TapStream;

    fn connect(
        &self,
    ) -> Pin<Box<dyn std::future::Future<Output = io::Result<TapStream>> + Send + '_>> {
        Box::pin(async move {
            self.connects.fetch_add(1, Ordering::SeqCst);
            let inner = TcpStream::connect(self.addr.unwrap()).await?;
            Ok(TapStream {
                inner,
                dec: Decoder::new(),
                commands: self.commands.clone(),
            })
        })
    }
}

impl AsyncRead for TapStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TapStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        let r = Pin::new(&mut this.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &r {
            this.dec.push(&buf[..*n]);
            while let Ok(Some(m)) = this.dec.next_message() {
                this.commands.lock().unwrap().push(m.header.command);
            }
        }
        r
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
