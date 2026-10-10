//! A tokio runtime on a paused clock, driven from a test's own thread.
//!
//! Async tests can simply use `#[tokio::test(start_paused = true)]`: the
//! test is a task on the runtime, and tokio moves the paused clock forward
//! whenever every task waits for a timer. Code with a blocking interface
//! (the viewer core and its C interface, called from app threads) cannot be
//! a task, so its tests need the clock held still while they make calls and
//! check results, and moved forward only when they ask. [`SimRuntime`] does
//! that:
//!
//! - its runtime (current-thread, paused, on a thread of its own) runs every
//!   task: the code under test and the devices it talks to (`mock_nvr::
//!   spawn_in`), over in-memory pipes, so nothing waits on real time or real
//!   sockets;
//! - the clock is *held* between steps (with tokio's documented mechanism: a
//!   running `spawn_blocking` task stops the paused clock from advancing),
//!   so a test thread can look at the state without it changing;
//! - [`SimRuntime::advance`] lets the clock run to a later instant, with
//!   every timer in between firing in order, and returns once all work due
//!   at that instant is done (the runtime is idle again);
//! - [`SimRuntime::settle`] waits, without moving the clock, until the work
//!   the test's own calls handed to the runtime is done.
//!
//! The result is deterministic up to what tokio itself randomizes (the
//! branch order of an unbiased `select!` among branches ready at the same
//! instant) and runs as fast as the CPU allows: an hour of device time is
//! the CPU time it needs, not an hour.
//!
//! Work that runs on threads the runtime does not know about (a thread of
//! the code under test, or a test's own helper thread) is not waited for:
//! keep such work out of what a simulated test measures.

use std::future::Future;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::thread;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::time::Instant;

/// Counts the runtime thread's parks (each time it runs out of ready
/// tasks).
#[derive(Default)]
struct Parks {
    count: Mutex<u64>,
    changed: Condvar,
}

impl Parks {
    fn lock(&self) -> MutexGuard<'_, u64> {
        self.count.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn parked(&self) {
        *self.lock() += 1;
        self.changed.notify_all();
    }

    fn count(&self) -> u64 {
        *self.lock()
    }

    /// Waits until the runtime has parked after the `seen`th park.
    fn wait_beyond(&self, seen: u64) {
        let mut c = self.lock();
        while *c <= seen {
            c = self.changed.wait(c).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Holds the paused clock still while it lives: a blocking task that waits
/// for this to be dropped.
struct Hold {
    _release: std::sync::mpsc::Sender<()>,
}

fn hold(handle: &Handle) -> Hold {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    // Auto-advance is inhibited from this call until the task returns.
    drop(handle.spawn_blocking(move || {
        let _ = rx.recv();
    }));
    Hold { _release: tx }
}

/// A current-thread tokio runtime with a paused clock that a test thread
/// steps. See the crate documentation.
pub struct SimRuntime {
    handle: Handle,
    hold: Mutex<Option<Hold>>,
    parks: Arc<Parks>,
    origin: Instant,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    driver: Option<thread::JoinHandle<()>>,
}

impl Default for SimRuntime {
    fn default() -> SimRuntime {
        SimRuntime::new()
    }
}

impl SimRuntime {
    /// Starts the runtime with its clock held at its start.
    ///
    /// # Panics
    /// If the runtime or its thread cannot start.
    pub fn new() -> SimRuntime {
        let parks = Arc::new(Parks::default());
        let p = Arc::clone(&parks);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .on_thread_park(move || p.parked())
            .build()
            .expect("simulated runtime");
        let handle = rt.handle().clone();
        let first = hold(&handle);
        let origin = {
            let _g = handle.enter();
            Instant::now()
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let driver = thread::Builder::new()
            .name("sim-runtime".into())
            .spawn(move || {
                rt.block_on(async {
                    let _ = stopped.await;
                });
                rt.shutdown_timeout(Duration::from_secs(1));
            })
            .expect("simulated runtime thread");
        SimRuntime {
            handle,
            hold: Mutex::new(Some(first)),
            parks,
            origin,
            stop: Some(stop),
            driver: Some(driver),
        }
    }

    /// The runtime: give it to the code under test and to the devices.
    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    /// Now on the simulated clock.
    pub fn now(&self) -> Instant {
        let _g = self.handle.enter();
        Instant::now()
    }

    /// Simulated time since the runtime started.
    pub fn elapsed(&self) -> Duration {
        self.now() - self.origin
    }

    fn hold_slot(&self) -> MutexGuard<'_, Option<Hold>> {
        self.hold.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `f` on the runtime and waits for its result, with the clock
    /// held (it must not need time to pass).
    pub fn block_on<F: Future>(&self, f: F) -> F::Output {
        self.handle.block_on(f)
    }

    /// Waits, without moving the clock, until the runtime has done all the
    /// work that is ready: what the test's calls handed it, and everything
    /// that work started in turn.
    pub fn settle(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        let parks = Arc::clone(&self.parks);
        // Runs after everything queued before it; the next park comes once
        // the queue is empty.
        self.handle.spawn(async move {
            let _ = tx.send(parks.count());
        });
        let seen = rx.recv().expect("simulated runtime stopped");
        self.parks.wait_beyond(seen);
    }

    /// Lets the clock run for `d`: every timer due in between fires in
    /// order, and the work it causes runs, as on a real clock but without
    /// waiting. Returns with the clock held at the new time once the work
    /// due then is done. The clock moves in whole milliseconds (tokio's
    /// timer resolution).
    pub fn advance(&self, d: Duration) {
        if d.is_zero() {
            self.settle();
            return;
        }
        let target = self.now() + d;
        let (armed_tx, armed_rx) = std::sync::mpsc::channel::<()>();
        let (held_tx, held_rx) = std::sync::mpsc::channel::<(Hold, u64)>();
        let (handle, parks) = (self.handle.clone(), Arc::clone(&self.parks));
        self.handle.spawn(async move {
            let sleep = tokio::time::sleep_until(target);
            tokio::pin!(sleep);
            // Register the timer before the clock is let go, so it cannot
            // run past the target.
            std::future::poll_fn(|cx| {
                let _ = sleep.as_mut().poll(cx);
                Poll::Ready(())
            })
            .await;
            let _ = armed_tx.send(());
            sleep.await;
            let h = hold(&handle);
            let _ = held_tx.send((h, parks.count()));
        });
        armed_rx.recv().expect("simulated runtime stopped");
        drop(self.hold_slot().take());
        let (h, seen) = held_rx.recv().expect("simulated runtime stopped");
        *self.hold_slot() = Some(h);
        self.parks.wait_beyond(seen);
    }

    /// Advances in steps of `step` until `cond` holds (checked before the
    /// first step and after each); panics naming `what` once `limit` of
    /// simulated time has passed without it.
    pub fn advance_until(
        &self,
        what: &str,
        limit: Duration,
        step: Duration,
        mut cond: impl FnMut() -> bool,
    ) {
        self.settle();
        let end = self.now() + limit;
        while !cond() {
            assert!(
                self.now() < end,
                "timed out after {limit:?} of simulated time waiting for {what}"
            );
            self.advance(step);
        }
    }

    /// Runs `f` (a blocking call that may need time to pass, such as a
    /// shutdown that waits for timeouts) with the clock free to run, then
    /// holds it again wherever it got to.
    pub fn run_free<R>(&self, f: impl FnOnce() -> R) -> R {
        let h = self.hold_slot().take();
        drop(h);
        let r = f();
        *self.hold_slot() = Some(hold(&self.handle));
        self.settle();
        r
    }
}

impl Drop for SimRuntime {
    fn drop(&mut self) {
        drop(self.hold_slot().take());
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(d) = self.driver.take() {
            let _ = d.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn the_clock_moves_only_when_advanced() {
        let sim = SimRuntime::new();
        let t0 = sim.now();
        std::thread::sleep(Duration::from_millis(20));
        sim.settle();
        assert_eq!(sim.now(), t0);
        sim.advance(Duration::from_millis(250));
        assert_eq!(sim.now() - t0, Duration::from_millis(250));
        assert_eq!(sim.elapsed(), Duration::from_millis(250));
    }

    #[test]
    fn timers_fire_in_order_and_their_work_is_done_on_return() {
        let sim = SimRuntime::new();
        let ticks = Arc::new(AtomicU64::new(0));
        let t = Arc::clone(&ticks);
        sim.handle().spawn(async move {
            let mut i = tokio::time::interval(Duration::from_millis(40));
            loop {
                i.tick().await;
                t.fetch_add(1, Ordering::SeqCst);
            }
        });
        sim.settle();
        // The first tick is immediate.
        assert_eq!(ticks.load(Ordering::SeqCst), 1);
        sim.advance(Duration::from_millis(39));
        assert_eq!(ticks.load(Ordering::SeqCst), 1);
        sim.advance(Duration::from_millis(1));
        assert_eq!(ticks.load(Ordering::SeqCst), 2);
        // Ten minutes of ticks, without waiting ten minutes.
        sim.advance(Duration::from_secs(600));
        assert_eq!(ticks.load(Ordering::SeqCst), 2 + 15_000);
    }

    #[test]
    fn advance_until_stops_at_the_first_step_that_satisfies() {
        let sim = SimRuntime::new();
        let start = sim.now();
        let at = start + Duration::from_millis(1234);
        let flag = Arc::new(AtomicU64::new(0));
        let f = Arc::clone(&flag);
        sim.handle().spawn(async move {
            tokio::time::sleep_until(at).await;
            f.store(1, Ordering::SeqCst);
        });
        sim.advance_until(
            "the flag",
            Duration::from_secs(5),
            Duration::from_millis(100),
            || flag.load(Ordering::SeqCst) == 1,
        );
        assert_eq!(sim.now() - start, Duration::from_millis(1300));
    }

    #[test]
    fn settle_waits_for_work_the_test_started() {
        let sim = SimRuntime::new();
        let n = Arc::new(AtomicU64::new(0));
        for _ in 0..100 {
            let n = Arc::clone(&n);
            sim.handle().spawn(async move {
                // A chain of tasks, none needing time.
                tokio::spawn(async move {
                    tokio::task::yield_now().await;
                    n.fetch_add(1, Ordering::SeqCst);
                });
            });
        }
        sim.settle();
        assert_eq!(n.load(Ordering::SeqCst), 100);
    }
}
