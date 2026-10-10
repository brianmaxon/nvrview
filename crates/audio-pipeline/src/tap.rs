//! Taps: the decoded audio as 16 kHz mono f32 blocks with timestamps, for
//! speech recognition and recording.
//!
//! The tile pushes, a consumer on any other thread pops. The queue is a
//! bounded lock-free queue: when it is full the oldest block is dropped and
//! counted, so a slow consumer never holds up the tile or playout.

use crossbeam_queue::ArrayQueue;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Sample rate of tap blocks.
pub const TAP_RATE: u32 = 16_000;

/// One block of tapped audio.
#[derive(Clone, Debug, PartialEq)]
pub struct TapBlock {
    /// 16 kHz mono samples in -1..1.
    pub samples: Vec<f32>,
    /// Timestamp epoch; numbering and camera times restart with each.
    pub epoch: u32,
    /// Number of the first sample in this epoch, at 16 kHz.
    pub first_sample: u64,
    /// Camera time (video timestamp units, µs) of the first sample, once a
    /// video timestamp has been seen in this epoch.
    pub camera_time_us: Option<f64>,
    /// Wall-clock time (µs since the Unix epoch) at which the record that
    /// completed this block arrived, if the caller supplied one. The block's
    /// last sample was captured somewhat before that.
    pub wall_clock_us: Option<i64>,
    /// True for the first block after a reset (stream restart, format change,
    /// gap): there is a discontinuity before it.
    pub discontinuity: bool,
}

struct Shared {
    queue: ArrayQueue<TapBlock>,
    pushed: AtomicU64,
    dropped: AtomicU64,
}

/// Counters of one tap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TapStats {
    /// Blocks offered.
    pub pushed: u64,
    /// Blocks dropped because the consumer fell behind (oldest first).
    pub dropped: u64,
}

/// The tile's end of a tap.
#[derive(Clone)]
pub struct TapSender {
    shared: Arc<Shared>,
}

/// The consumer's end of a tap. `Send + Sync`; pop from any thread.
#[derive(Clone)]
pub struct TapReceiver {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for TapSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TapSender")
            .field("stats", &stats(&self.shared))
            .finish()
    }
}

impl std::fmt::Debug for TapReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TapReceiver")
            .field("stats", &stats(&self.shared))
            .finish()
    }
}

fn stats(s: &Shared) -> TapStats {
    TapStats {
        pushed: s.pushed.load(Ordering::Relaxed),
        dropped: s.dropped.load(Ordering::Relaxed),
    }
}

/// A tap holding at most `capacity` blocks (64 ms each for device AAC).
pub fn tap(capacity: usize) -> (TapSender, TapReceiver) {
    let shared = Arc::new(Shared {
        queue: ArrayQueue::new(capacity.max(1)),
        pushed: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
    });
    (
        TapSender {
            shared: Arc::clone(&shared),
        },
        TapReceiver { shared },
    )
}

impl TapSender {
    /// Offers a block; never blocks. Drops the oldest block when full.
    pub fn push(&self, block: TapBlock) {
        self.shared.pushed.fetch_add(1, Ordering::Relaxed);
        if self.shared.queue.force_push(block).is_some() {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Counters.
    pub fn stats(&self) -> TapStats {
        stats(&self.shared)
    }
}

impl TapReceiver {
    /// The oldest block waiting, if any.
    pub fn pop(&self) -> Option<TapBlock> {
        self.shared.queue.pop()
    }

    /// Blocks waiting.
    pub fn len(&self) -> usize {
        self.shared.queue.len()
    }

    /// True when nothing is waiting.
    pub fn is_empty(&self) -> bool {
        self.shared.queue.is_empty()
    }

    /// Counters.
    pub fn stats(&self) -> TapStats {
        stats(&self.shared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(n: u64) -> TapBlock {
        TapBlock {
            samples: vec![0.0; 4],
            epoch: 0,
            first_sample: n,
            camera_time_us: None,
            wall_clock_us: None,
            discontinuity: false,
        }
    }

    #[test]
    fn drops_oldest_when_full() {
        let (tx, rx) = tap(3);
        for n in 0..5 {
            tx.push(block(n));
        }
        assert_eq!(
            tx.stats(),
            TapStats {
                pushed: 5,
                dropped: 2
            }
        );
        let got: Vec<u64> = std::iter::from_fn(|| rx.pop())
            .map(|b| b.first_sample)
            .collect();
        assert_eq!(got, [2, 3, 4]);
    }
}
