//! Suppression of repeated events.
//!
//! [`RateLimitLayer`] is a `tracing_subscriber::Layer` that gates events per
//! call site (optionally per distinct message too): a burst is allowed, then
//! at most `max_per_interval` per `interval`. Suppressed events are counted and
//! summarized as one line, such as `suppressed 8,991 similar messages from
//! my_crate::decoder`, when the flood ends or periodically during a long one.
//!
//! The summary goes to a sink function rather than through `tracing`, because
//! `tracing` drops events emitted from inside another event's dispatch.
//! Summaries are produced when the next event arrives after the interval, on
//! [`RateLimitLayer::flush_idle`] (the setup API calls it from its writer
//! thread), and on [`RateLimitLayer::flush_all`].

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fmt::{self, Write as _};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber, callsite};
use tracing_subscriber::layer::{Context, Layer};

/// A monotonic time source, injectable for deterministic tests.
pub trait Clock: Send + Sync + 'static {
    /// Time elapsed since an arbitrary fixed origin.
    fn now(&self) -> Duration;
}

/// The real clock, based on [`Instant`].
#[derive(Debug)]
pub struct SystemClock(Instant);

impl SystemClock {
    /// Creates a clock whose origin is now.
    pub fn new() -> Self {
        Self(Instant::now())
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// A clock that only moves when told to. Clones share the same time.
#[derive(Debug, Clone, Default)]
pub struct ManualClock(Arc<AtomicU64>);

impl ManualClock {
    /// Creates a clock at time zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves time forward.
    pub fn advance(&self, by: Duration) {
        self.0.fetch_add(
            u64::try_from(by.as_nanos()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.0.load(Ordering::SeqCst))
    }
}

/// Rate limit parameters.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Events allowed back to back before limiting starts.
    pub burst: u32,
    /// Sustained events allowed per `interval` once the burst is spent.
    pub max_per_interval: u32,
    /// The refill and summary period.
    pub interval: Duration,
    /// Treat events from one call site with different messages as different
    /// streams. Off by default: a call site is one stream.
    pub by_message: bool,
    /// Upper bound on tracked streams, so the limiter itself stays bounded.
    pub max_tracked: usize,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            burst: 10,
            max_per_interval: 5,
            interval: Duration::from_secs(10),
            by_message: false,
            max_tracked: 1024,
        }
    }
}

/// Where summary lines are delivered.
pub type SummarySink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(PartialEq, Eq, Hash)]
struct Key {
    site: callsite::Identifier,
    message: u64,
}

struct State {
    tokens: f64,
    last_refill: Duration,
    last_seen: Duration,
    last_summary: Duration,
    suppressed: u64,
    target: String,
    location: String,
}

struct Inner {
    cfg: RateLimitConfig,
    clock: Box<dyn Clock>,
    sink: SummarySink,
    streams: Mutex<HashMap<Key, State>>,
}

/// A `Layer` that suppresses floods of identical events. Clones share state.
#[derive(Clone)]
pub struct RateLimitLayer(Arc<Inner>);

impl RateLimitLayer {
    /// Creates a limiter using the real clock.
    pub fn new(cfg: RateLimitConfig, sink: SummarySink) -> Self {
        Self::with_clock(cfg, SystemClock::new(), sink)
    }

    /// Creates a limiter with an injected clock.
    pub fn with_clock(cfg: RateLimitConfig, clock: impl Clock, sink: SummarySink) -> Self {
        Self(Arc::new(Inner {
            cfg,
            clock: Box::new(clock),
            sink,
            streams: Mutex::new(HashMap::new()),
        }))
    }

    /// Emits summaries for streams that have been quiet for a full interval and
    /// forgets streams that are quiet and have nothing pending.
    pub fn flush_idle(&self) {
        let now = self.0.clock.now();
        let interval = self.0.cfg.interval;
        let mut lines = Vec::new();
        {
            let mut map = lock(&self.0.streams);
            map.retain(|_, s| {
                if now.saturating_sub(s.last_seen) < interval {
                    return true;
                }
                if s.suppressed > 0 {
                    lines.push(summary_line(s));
                    s.suppressed = 0;
                }
                false
            });
        }
        self.emit(&lines);
    }

    /// Emits every pending summary immediately (use before shutdown).
    pub fn flush_all(&self) {
        let mut lines = Vec::new();
        {
            let mut map = lock(&self.0.streams);
            for s in map.values_mut() {
                if s.suppressed > 0 {
                    lines.push(summary_line(s));
                    s.suppressed = 0;
                }
            }
        }
        self.emit(&lines);
    }

    fn emit(&self, lines: &[String]) {
        for l in lines {
            (self.0.sink)(l);
        }
    }

    /// Returns true if the event should be logged.
    fn admit(&self, event: &Event<'_>) -> bool {
        let meta = event.metadata();
        let message = if self.0.cfg.by_message {
            let mut v = MessageHash(None);
            event.record(&mut v);
            v.0.unwrap_or(0)
        } else {
            0
        };
        let key = Key {
            site: meta.callsite(),
            message,
        };
        let cfg = &self.0.cfg;
        let now = self.0.clock.now();
        let mut lines = Vec::new();
        let allowed;
        {
            let mut map = lock(&self.0.streams);
            if map.len() >= cfg.max_tracked.max(1) && !map.contains_key(&key) {
                Self::evict_idle(&mut map, now, cfg.interval, &mut lines);
                if map.len() >= cfg.max_tracked.max(1) {
                    drop(map);
                    self.emit(&lines);
                    return true; // cannot track more; fail open rather than grow
                }
            }
            let st = map.entry(key).or_insert_with(|| State {
                tokens: f64::from(cfg.burst.max(1)),
                last_refill: now,
                last_seen: now,
                last_summary: now,
                suppressed: 0,
                target: meta.target().to_owned(),
                location: match (meta.file(), meta.line()) {
                    (Some(f), Some(l)) => format!("{f}:{l}"),
                    _ => String::new(),
                },
            });
            let elapsed = now.saturating_sub(st.last_refill);
            if !cfg.interval.is_zero() && !elapsed.is_zero() {
                let add = elapsed.as_secs_f64() / cfg.interval.as_secs_f64()
                    * f64::from(cfg.max_per_interval);
                st.tokens = (st.tokens + add).min(f64::from(cfg.burst.max(1)));
                st.last_refill = now;
            }
            st.last_seen = now;
            if st.tokens >= 1.0 {
                st.tokens -= 1.0;
                allowed = true;
                if st.suppressed > 0 && now.saturating_sub(st.last_summary) >= cfg.interval {
                    lines.push(summary_line(st));
                    st.suppressed = 0;
                    st.last_summary = now;
                }
            } else {
                allowed = false;
                st.suppressed += 1;
                if now.saturating_sub(st.last_summary) >= cfg.interval {
                    lines.push(summary_line(st));
                    st.suppressed = 0;
                    st.last_summary = now;
                }
            }
        }
        self.emit(&lines);
        allowed
    }

    fn evict_idle(
        map: &mut HashMap<Key, State>,
        now: Duration,
        interval: Duration,
        lines: &mut Vec<String>,
    ) {
        map.retain(|_, s| {
            if now.saturating_sub(s.last_seen) < interval {
                return true;
            }
            if s.suppressed > 0 {
                lines.push(summary_line(s));
            }
            false
        });
    }
}

impl<S: Subscriber> Layer<S> for RateLimitLayer {
    fn event_enabled(&self, event: &Event<'_>, _ctx: Context<'_, S>) -> bool {
        self.admit(event)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct MessageHash(Option<u64>);

impl Visit for MessageHash {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let mut h = DefaultHasher::new();
            let mut s = String::new();
            let _ = write!(s, "{value:?}");
            s.hash(&mut h);
            self.0 = Some(h.finish());
        }
    }
}

/// Formats a count with thousands separators: `8991` becomes `8,991`.
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn summary_line(s: &State) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let loc = if s.location.is_empty() {
        String::new()
    } else {
        format!(" ({})", s.location)
    };
    format!(
        "{secs} WARN viewer_log::ratelimit: suppressed {} similar messages from {}{loc}\n",
        group_digits(s.suppressed),
        s.target
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::subscriber::with_default;
    use tracing_subscriber::layer::SubscriberExt;

    type Lines = Arc<Mutex<Vec<String>>>;

    fn setup(
        cfg: RateLimitConfig,
    ) -> (
        RateLimitLayer,
        ManualClock,
        Lines,
        impl Subscriber + Send + Sync,
    ) {
        let clock = ManualClock::new();
        let lines: Lines = Arc::default();
        let l2 = lines.clone();
        let sink: SummarySink = Arc::new(move |s| l2.lock().unwrap().push(s.to_owned()));
        let layer = RateLimitLayer::with_clock(cfg, clock.clone(), sink);
        let sub = tracing_subscriber::registry()
            .with(layer.clone())
            .with(Counter);
        (layer, clock, lines, sub)
    }

    /// Counts events that pass the limiter.
    struct Counter;
    impl<S: Subscriber> Layer<S> for Counter {
        fn on_event(&self, _e: &Event<'_>, _c: Context<'_, S>) {
            PASSED.with(|p| p.set(p.get() + 1));
        }
    }
    thread_local! { static PASSED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
    fn passed() -> u64 {
        PASSED.with(std::cell::Cell::get)
    }
    fn reset() {
        PASSED.with(|p| p.set(0));
    }

    fn cfg() -> RateLimitConfig {
        RateLimitConfig {
            burst: 3,
            max_per_interval: 2,
            interval: Duration::from_secs(10),
            ..Default::default()
        }
    }

    fn flood(n: usize) {
        for _ in 0..n {
            tracing::error!("decoder rejected frame");
        }
    }

    #[test]
    fn burst_then_limit() {
        reset();
        let (_l, _c, lines, sub) = setup(cfg());
        with_default(sub, || flood(100));
        assert_eq!(passed(), 3);
        assert!(lines.lock().unwrap().is_empty());
    }

    #[test]
    fn refills_over_time() {
        reset();
        let (_l, clock, _lines, sub) = setup(cfg());
        with_default(sub, || {
            flood(10);
            assert_eq!(passed(), 3);
            clock.advance(Duration::from_secs(5)); // 1 token
            flood(10);
            assert_eq!(passed(), 4);
            clock.advance(Duration::from_secs(100)); // refills to burst only
            flood(10);
            assert_eq!(passed(), 7);
        });
    }

    #[test]
    fn summary_after_flood_ends_on_next_event() {
        reset();
        let (_l, clock, lines, sub) = setup(cfg());
        with_default(sub, || {
            flood(100); // 3 pass, 97 suppressed
            clock.advance(Duration::from_secs(10));
            flood(1);
        });
        let got = lines.lock().unwrap().clone();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].contains("suppressed 97 similar messages from viewer_log::ratelimit::tests"),
            "{}",
            got[0]
        );
    }

    #[test]
    fn periodic_summary_during_continuous_flood() {
        reset();
        let (_l, clock, lines, sub) = setup(cfg());
        with_default(sub, || {
            for _ in 0..3 {
                flood(50);
                clock.advance(Duration::from_secs(10));
            }
        });
        assert!(lines.lock().unwrap().len() >= 2);
    }

    #[test]
    fn flush_idle_emits_after_quiet_interval() {
        reset();
        let (layer, clock, lines, sub) = setup(cfg());
        with_default(sub, || flood(1003));
        layer.flush_idle();
        assert!(lines.lock().unwrap().is_empty(), "not idle yet");
        clock.advance(Duration::from_secs(10));
        layer.flush_idle();
        let got = lines.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("suppressed 1,000 similar"), "{}", got[0]);
        layer.flush_idle();
        assert_eq!(lines.lock().unwrap().len(), 1, "summary is emitted once");
    }

    #[test]
    fn flush_all_emits_immediately() {
        reset();
        let (layer, _c, lines, sub) = setup(cfg());
        with_default(sub, || flood(8));
        layer.flush_all();
        assert!(lines.lock().unwrap()[0].contains("suppressed 5 similar"));
    }

    #[test]
    fn call_sites_are_independent() {
        reset();
        let (_l, _c, _lines, sub) = setup(cfg());
        with_default(sub, || {
            flood(10);
            for _ in 0..10 {
                tracing::error!("other site");
            }
        });
        assert_eq!(passed(), 6);
    }

    #[test]
    fn by_message_separates_streams() {
        reset();
        let (_l, _c, _lines, sub) = setup(RateLimitConfig {
            by_message: true,
            ..cfg()
        });
        with_default(sub, || {
            for i in 0..10 {
                tracing::error!("failure {}", i % 2);
            }
        });
        // Two distinct messages, 3 allowed each at one call site.
        assert_eq!(passed(), 6);
    }

    #[test]
    fn tracked_streams_are_bounded() {
        reset();
        let (layer, _c, _l, sub) = setup(RateLimitConfig {
            max_tracked: 1,
            by_message: true,
            ..cfg()
        });
        with_default(sub, || {
            for i in 0..20 {
                tracing::info!("unique {i}");
            }
        });
        assert!(layer.0.streams.lock().unwrap().len() <= 1);
        assert_eq!(passed(), 20, "fails open when the table is full");
    }

    #[test]
    fn digits_grouping() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(8991), "8,991");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }
}
