//! The core's clock: the platform's host clock, in nanoseconds.
//!
//! Audio playout compares when a record arrived with when the app's audio
//! callback says a sample will be heard, so both must be on one clock, to
//! the microsecond, for hours. Rather than keep a private clock and make
//! apps estimate an offset to it (two reads that can be preempted apart,
//! and an offset that breaks if the two clocks ever differ in how they
//! count sleep), the core *is* on the clock the platform's audio APIs
//! already use:
//!
//! | platform | clock | what the audio API reports |
//! | --- | --- | --- |
//! | macOS | `CLOCK_UPTIME_RAW`, which Apple documents as `mach_absolute_time()` converted with the timebase; stops while asleep | `AudioTimeStamp.mHostTime` (mach ticks; convert with `AudioConvertHostTimeToNanos` or the timebase) |
//! | Windows | `QueryPerformanceCounter`, converted to ns | WASAPI's `IAudioClock::GetPosition` QPC position (100 ns units) |
//! | Linux, others | `CLOCK_MONOTONIC` | ALSA and PipeWire timestamps on the monotonic clock |
//!
//! `nv_clock_now_ns()` returns it for apps that want to check, and for tests.

// Two calls into the operating system's clock need `unsafe`; each is a plain
// read into a local value.
#![allow(unsafe_code)]

/// Now on the host clock, nanoseconds (from an arbitrary origin, usually
/// boot).
pub fn now_ns() -> u64 {
    imp::now_ns()
}

/// The clock one core runs on: the host clock, or, for a core on a test's
/// runtime ([`crate::TestRuntime`]), that runtime's (paused, simulated)
/// clock, so audio arrival times, render host times, and tile timing are on
/// the test's simulated time.
#[derive(Clone, Debug)]
pub(crate) enum HostClock {
    /// [`now_ns`].
    Host,
    /// The runtime's clock, as nanoseconds after a made-up origin; wall
    /// clock time moves with it from the real time at the start.
    Runtime {
        handle: tokio::runtime::Handle,
        origin: tokio::time::Instant,
        wall_origin_us: i64,
    },
}

/// Now on the wall clock, microseconds since the Unix epoch (0 if the
/// system clock is before it).
fn wall_now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_micros()).ok())
        .unwrap_or(0)
}

/// Where a simulated clock starts: an uptime of 1,000 s, so values look like
/// host clock values and an earlier time never goes below zero.
const SIMULATED_START_NS: u64 = 1_000_000_000_000;

impl HostClock {
    /// A clock on `handle`'s runtime, starting now; its wall clock starts
    /// at `wall_start_us` (the real time when `None`).
    pub(crate) fn runtime(handle: tokio::runtime::Handle, wall_start_us: Option<i64>) -> HostClock {
        let origin = {
            let _in = handle.enter();
            tokio::time::Instant::now()
        };
        HostClock::Runtime {
            handle,
            origin,
            wall_origin_us: wall_start_us.unwrap_or_else(wall_now_us),
        }
    }

    /// Now, nanoseconds.
    pub(crate) fn now_ns(&self) -> u64 {
        match self {
            HostClock::Host => now_ns(),
            HostClock::Runtime { handle, origin, .. } => {
                let _in = handle.enter();
                SIMULATED_START_NS + origin.elapsed().as_nanos() as u64
            }
        }
    }

    /// Now on the wall clock, microseconds since the Unix epoch: the
    /// system's, or on a test runtime, the real time at the start plus the
    /// simulated time since.
    pub(crate) fn wall_now_us(&self) -> i64 {
        match self {
            HostClock::Host => wall_now_us(),
            HostClock::Runtime {
                handle,
                origin,
                wall_origin_us,
            } => {
                let _in = handle.enter();
                wall_origin_us.saturating_add(origin.elapsed().as_micros() as i64)
            }
        }
    }

    /// Now, microseconds (the pipelines' unit).
    pub(crate) fn now_us(&self) -> u64 {
        self.now_ns() / 1000
    }
}

#[cfg(target_vendor = "apple")]
mod imp {
    pub fn now_ns() -> u64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid, writable timespec; CLOCK_UPTIME_RAW is
        // always available on Apple platforms (it cannot fail).
        unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
}

#[cfg(all(unix, not(target_vendor = "apple")))]
mod imp {
    pub fn now_ns() -> u64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid, writable timespec; CLOCK_MONOTONIC is
        // always available (it cannot fail).
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };

    fn frequency() -> u64 {
        static FREQ: OnceLock<u64> = OnceLock::new();
        *FREQ.get_or_init(|| {
            let mut f = 0i64;
            // SAFETY: writes one i64 to a valid local; cannot fail on
            // Windows XP and later.
            unsafe { QueryPerformanceFrequency(&mut f) };
            f.max(1) as u64
        })
    }

    pub fn now_ns() -> u64 {
        let mut c = 0i64;
        // SAFETY: writes one i64 to a valid local; cannot fail on Windows
        // XP and later.
        unsafe { QueryPerformanceCounter(&mut c) };
        (u128::from(c.max(0) as u64) * 1_000_000_000 / u128::from(frequency())) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The host clock and std's `Instant` measure the same interval (std's
    /// Instant reads the same source on these platforms; this checks the
    /// units). Each end is read between two host clock reads, so the check
    /// holds however the thread is preempted.
    #[test]
    fn advances_with_real_time() {
        let a0 = now_ns();
        let i0 = Instant::now();
        let a1 = now_ns();
        std::thread::sleep(Duration::from_millis(50));
        let b0 = now_ns();
        let i1 = Instant::now();
        let b1 = now_ns();
        let e = i1 - i0;
        assert!(e >= Duration::from_millis(50));
        // Rounding of either clock: a microsecond either way.
        let slack = Duration::from_micros(1);
        assert!(e + slack >= Duration::from_nanos(b0 - a1), "{e:?}");
        assert!(e <= Duration::from_nanos(b1 - a0) + slack, "{e:?}");
    }

    /// On macOS the clock is `mach_absolute_time()` in nanoseconds, which is
    /// what `AudioTimeStamp.mHostTime` counts. The mach read is bracketed by
    /// two host clock reads, so preemption cannot fail the check.
    #[cfg(target_vendor = "apple")]
    #[test]
    #[allow(deprecated)]
    fn is_mach_absolute_time_on_apple() {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: writes one struct to a valid local.
        unsafe { libc::mach_timebase_info(&mut tb) };
        let before = now_ns();
        // SAFETY: no arguments; reads the host clock.
        let ticks = unsafe { libc::mach_absolute_time() };
        let after = now_ns();
        let mach_ns = (u128::from(ticks) * u128::from(tb.numer) / u128::from(tb.denom)) as u64;
        // One tick of rounding either way (a tick is at most a few dozen ns).
        let tick = u64::from(tb.numer.div_ceil(tb.denom.max(1)));
        assert!(
            before <= mach_ns + tick && mach_ns <= after + tick,
            "{before} <= {mach_ns} <= {after}"
        );
    }

    /// A core on a test's runtime reads that runtime's paused clock.
    #[test]
    fn a_runtime_clock_follows_the_runtime() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        let c = HostClock::runtime(rt.handle().clone(), None);
        let t0 = c.now_ns();
        assert_eq!(t0, SIMULATED_START_NS);
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(c.now_ns(), t0, "paused");
        rt.block_on(async { tokio::time::sleep(Duration::from_millis(1500)).await });
        assert_eq!(c.now_ns() - t0, 1_500_000_000);
        assert_eq!(c.now_us(), (t0 + 1_500_000_000) / 1000);
    }
}
