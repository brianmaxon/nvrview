//! Counters describing what the logging pipeline had to drop.

use std::sync::atomic::{AtomicU64, Ordering};

/// Shared counters. Logging never fails loudly; instead it counts what it lost.
#[derive(Debug, Default)]
pub struct LogStats {
    io_errors: AtomicU64,
    dropped_bytes: AtomicU64,
    dropped_lines: AtomicU64,
}

/// A point-in-time copy of [`LogStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LogStatsSnapshot {
    /// File operations (open, write, rename, delete) that failed.
    pub io_errors: u64,
    /// Bytes lost because a write failed.
    pub dropped_bytes: u64,
    /// Lines dropped because the writer queue was full or the writer thread was gone.
    pub dropped_lines: u64,
}

impl LogStats {
    pub(crate) fn io_error(&self) {
        self.io_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn dropped_bytes(&self, n: usize) {
        self.dropped_bytes.fetch_add(n as u64, Ordering::Relaxed);
    }

    pub(crate) fn dropped_line(&self) {
        self.dropped_lines.fetch_add(1, Ordering::Relaxed);
    }

    /// Reads all counters.
    pub fn snapshot(&self) -> LogStatsSnapshot {
        LogStatsSnapshot {
            io_errors: self.io_errors.load(Ordering::Relaxed),
            dropped_bytes: self.dropped_bytes.load(Ordering::Relaxed),
            dropped_lines: self.dropped_lines.load(Ordering::Relaxed),
        }
    }
}
