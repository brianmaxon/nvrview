//! The free-space shim (docs/CAPTURE.md, "Bounds"): how much room is left
//! where a recording is written. The core reads it before a start, every
//! 5 s and at each fragment while recording, and before finalizing.

// One call into the operating system on each platform needs `unsafe`; each
// is a plain query with a buffer the call fills in.
#![allow(unsafe_code)]

use std::path::Path;

/// Answers the free bytes available to this process on the volume that
/// holds `dir`, or `None` when it cannot tell (the recording then carries
/// on as if there were room, and the failure is logged once). Platforms
/// may supply their own; tests inject a fake through
/// [`crate::Core::capture_set_free_space`].
pub trait FreeSpace: Send + Sync + 'static {
    /// Free bytes for `dir`.
    fn free_bytes(&self, dir: &Path) -> Option<u64>;
}

/// The system's answer: `statvfs` on Unix, `GetDiskFreeSpaceExW` on
/// Windows. The default.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemFreeSpace;

impl FreeSpace for SystemFreeSpace {
    fn free_bytes(&self, dir: &Path) -> Option<u64> {
        imp::free_bytes(dir)
    }
}

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    // The field types differ between platforms (32-bit block counts on
    // some), so the widening casts are needed on some and not others.
    #[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
    pub(super) fn free_bytes(dir: &Path) -> Option<u64> {
        let c = CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: `c` is a valid NUL-terminated path and `st` a properly
        // sized, writable statvfs that the call fills in.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
        if r != 0 {
            return None;
        }
        Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    pub(super) fn free_bytes(dir: &Path) -> Option<u64> {
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut avail = 0u64;
        // SAFETY: `wide` is NUL-terminated and outlives the call; the
        // output pointer is valid, the other two may be null.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut avail,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(avail)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::path::Path;

    pub(super) fn free_bytes(_dir: &Path) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_answers_for_a_real_directory() {
        let d = tempfile::tempdir().unwrap();
        assert!(SystemFreeSpace.free_bytes(d.path()).is_some_and(|b| b > 0));
        assert_eq!(
            SystemFreeSpace.free_bytes(&d.path().join("missing").join("deeper")),
            None
        );
    }
}
