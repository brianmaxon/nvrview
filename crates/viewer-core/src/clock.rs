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

    #[test]
    fn advances_with_real_time() {
        let (a, i) = (now_ns(), Instant::now());
        std::thread::sleep(Duration::from_millis(50));
        let (b, e) = (now_ns(), i.elapsed());
        let d = Duration::from_nanos(b - a);
        // Both clocks measured the same sleep (std's Instant reads the same
        // source on these platforms; this checks the units).
        assert!(d >= Duration::from_millis(50));
        let diff = d.abs_diff(e);
        assert!(diff < Duration::from_millis(2), "{d:?} vs {e:?}");
    }

    /// On macOS the clock is `mach_absolute_time()` in nanoseconds, which is
    /// what `AudioTimeStamp.mHostTime` counts.
    #[cfg(target_vendor = "apple")]
    #[test]
    #[allow(deprecated)]
    fn is_mach_absolute_time_on_apple() {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: writes one struct to a valid local.
        unsafe { libc::mach_timebase_info(&mut tb) };
        // SAFETY: no arguments; reads the host clock.
        let ticks = unsafe { libc::mach_absolute_time() };
        let ns = now_ns();
        let mach_ns = u128::from(ticks) * u128::from(tb.numer) / u128::from(tb.denom);
        let diff = (i128::from(ns) - mach_ns as i128).abs();
        assert!(diff < 1_000_000, "{ns} vs {mach_ns}");
    }
}
