//! Control events: a bounded queue and the one thread that delivers them, so
//! `on_event` is serialized across the whole core and never runs on a
//! session or tile thread.

use crate::{Callbacks, in_callback};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

/// Most events waiting for the app. State changes are rare (reconnects back
/// off), so this is only reached by an app that stopped returning from
/// `on_event`; the oldest events are then dropped and counted.
const MAX_QUEUED: usize = 1024;
/// Diagnostics (`tile_stats`) are queued only while fewer than this many
/// events wait, so they never crowd out state changes.
const MAX_QUEUED_FOR_DIAGNOSTICS: usize = 64;

/// How important an event is when the queue is long.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Priority {
    /// State the app must see (device state, channels, tile closed).
    State,
    /// Periodic diagnostics that the next period replaces.
    Diagnostics,
}

#[derive(Default)]
struct Queue {
    items: VecDeque<String>,
    closed: bool,
    /// An `on_event` is running.
    delivering: bool,
}

/// Sees every event before it is queued (the alert service's trigger
/// intake). Must not block.
pub(crate) type Tap = Arc<dyn Fn(&str) + Send + Sync>;

struct Inner {
    queue: Mutex<Queue>,
    tap: std::sync::OnceLock<Tap>,
    ready: Condvar,
    /// Signaled when the queue empties after a delivery ([`EventSink::flush`]).
    idle: Condvar,
    dropped: AtomicU64,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Where core code sends events. Cheap to clone; never blocks.
#[derive(Clone)]
pub(crate) struct EventSink(Arc<Inner>);

impl EventSink {
    /// Queues one JSON object for `on_event`. Returns false when it was
    /// dropped (queue full of diagnostics, or the core is shutting down).
    pub(crate) fn send(&self, json: String, priority: Priority) -> bool {
        if let Some(tap) = self.0.tap.get() {
            tap(&json);
        }
        let mut q = self.0.lock();
        if q.closed {
            return false;
        }
        let limit = match priority {
            Priority::State => MAX_QUEUED,
            Priority::Diagnostics => MAX_QUEUED_FOR_DIAGNOSTICS,
        };
        if q.items.len() >= limit {
            if priority == Priority::Diagnostics {
                self.0.dropped.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            q.items.pop_front();
            self.0.dropped.fetch_add(1, Ordering::Relaxed);
        }
        q.items.push_back(json);
        drop(q);
        self.0.ready.notify_one();
        true
    }

    /// Installs the tap (once; later calls are ignored).
    pub(crate) fn set_tap(&self, tap: Tap) {
        let _ = self.0.tap.set(tap);
    }

    /// Events dropped so far because the app was not keeping up.
    pub(crate) fn dropped(&self) -> u64 {
        self.0.dropped.load(Ordering::Relaxed)
    }

    /// Waits until every event queued so far has been delivered (or the
    /// thread has stopped).
    pub(crate) fn flush(&self) {
        let mut q = self.0.lock();
        while (!q.items.is_empty() || q.delivering) && !(q.closed && q.items.is_empty()) {
            q = self.0.idle.wait(q).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// The delivery thread. Dropping it without [`EventThread::stop`] also stops
/// and joins it.
pub(crate) struct EventThread {
    sink: EventSink,
    join: Option<JoinHandle<()>>,
}

impl EventThread {
    pub(crate) fn spawn(callbacks: Arc<dyn Callbacks>) -> std::io::Result<EventThread> {
        let sink = EventSink(Arc::new(Inner {
            queue: Mutex::new(Queue::default()),
            tap: std::sync::OnceLock::new(),
            ready: Condvar::new(),
            idle: Condvar::new(),
            dropped: AtomicU64::new(0),
        }));
        let inner = sink.0.clone();
        let join = std::thread::Builder::new()
            .name("nvrview-events".into())
            .spawn(move || {
                in_callback::mark_core_thread();
                loop {
                    let json = {
                        let mut q = inner.lock();
                        loop {
                            if let Some(j) = q.items.pop_front() {
                                q.delivering = true;
                                break Some(j);
                            }
                            if q.closed {
                                break None;
                            }
                            q = inner.ready.wait(q).unwrap_or_else(PoisonError::into_inner);
                        }
                    };
                    let Some(j) = json else {
                        inner.idle.notify_all();
                        return;
                    };
                    callbacks.on_event(&j);
                    let mut q = inner.lock();
                    q.delivering = false;
                    if q.items.is_empty() {
                        drop(q);
                        inner.idle.notify_all();
                    }
                }
            })?;
        Ok(EventThread {
            sink,
            join: Some(join),
        })
    }

    pub(crate) fn sink(&self) -> EventSink {
        self.sink.clone()
    }

    /// Delivers what is already queued, refuses anything new, and joins the
    /// thread. No `on_event` runs after this returns.
    pub(crate) fn stop(&mut self) {
        self.sink.0.lock().closed = true;
        self.sink.0.ready.notify_all();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for EventThread {
    fn drop(&mut self) {
        self.stop();
    }
}
