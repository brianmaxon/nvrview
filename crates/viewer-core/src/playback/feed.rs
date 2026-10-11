//! The link between the playback engine and one generation of a playback
//! tile: a bounded queue of media the engine releases on time, and what the
//! tile reports back (the decode latency, the backlog, when the generation
//! became the one shown, and the picture it waits for before it is shown).
//!
//! The engine never pushes past [`FEED_CAPACITY`]: it releases a picture
//! when it is due, and pictures before a seek's target only while the tile's
//! backlog is small. The queue still bounds itself (dropping to the next
//! keyframe, like a live subscription's queue) should that rule ever fail.

use bc_protocol::media::Record;
use bc_session::Item;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::Notify;

/// Items a feed holds at most.
pub(crate) const FEED_CAPACITY: usize = 256;

/// No target known yet: the generation is not shown until the engine
/// names one.
const NO_TARGET: u64 = u64::MAX;

/// What a playback tile's generation receives.
#[derive(Debug)]
pub(crate) enum FeedItem {
    /// Media or a discontinuity, as from a live subscription.
    Item(Item),
    /// Nothing to show at the position (no recording, or the camera became
    /// unavailable): the tile clears its picture (`NV_ACTION_CLEAR`). A
    /// generation that is still starting is shown at once, cleared.
    Clear,
    /// The target was named ([`FeedTx::set_target`]): a starting generation
    /// whose decoded pictures reached it already is shown now.
    Target,
}

#[derive(Default)]
struct Queue {
    items: VecDeque<FeedItem>,
    /// Dropping to the next keyframe after an overflow.
    gated: bool,
}

/// State shared by the engine (writer) and the tile (reader).
pub(crate) struct FeedShared {
    queue: Mutex<Queue>,
    notify: Notify,
    /// The generation is shown once a picture with this timestamp or a
    /// later one has been reported decoded.
    target_pts: AtomicU64,
    /// Decode latency (from `Decode` to `nv_tile_decoded`), smoothed, µs.
    latency_us: AtomicU64,
    /// Pictures handed to the pipeline and not yet answered, plus those
    /// it queues.
    backlog: AtomicU64,
    /// Set once the generation is the tile's shown one.
    shown: AtomicBool,
    /// Items dropped by the queue's own bound.
    dropped: AtomicU64,
}

impl FeedShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A new feed: the engine's end and the tile's end.
pub(crate) fn feed() -> (FeedTx, FeedRx) {
    let shared = Arc::new(FeedShared {
        queue: Mutex::new(Queue::default()),
        notify: Notify::new(),
        target_pts: AtomicU64::new(NO_TARGET),
        latency_us: AtomicU64::new(0),
        backlog: AtomicU64::new(0),
        shown: AtomicBool::new(false),
        dropped: AtomicU64::new(0),
    });
    (FeedTx(shared.clone()), FeedRx(shared))
}

/// The engine's end of a feed.
pub(crate) struct FeedTx(Arc<FeedShared>);

impl FeedTx {
    /// Queues an item. Past [`FEED_CAPACITY`] the queue is dropped and
    /// gated to the next keyframe (never reached when the engine keeps to
    /// [`FeedTx::pending`]).
    pub(crate) fn push(&self, item: FeedItem) {
        let mut q = self.0.lock();
        let gateable = matches!(
            item,
            FeedItem::Item(Item::Media(
                Record::Audio { .. }
                    | Record::Video {
                        keyframe: false,
                        ..
                    }
            ))
        );
        if matches!(
            item,
            FeedItem::Item(Item::Media(Record::Video { keyframe: true, .. }))
        ) {
            q.gated = false;
        } else if gateable && q.gated {
            self.0.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if q.items.len() >= FEED_CAPACITY {
            self.0
                .dropped
                .fetch_add(q.items.len() as u64, Ordering::Relaxed);
            q.items.clear();
            q.items.push_back(FeedItem::Item(Item::Discontinuity));
            if gateable {
                q.gated = true;
                drop(q);
                self.0.notify.notify_one();
                return;
            }
        }
        q.items.push_back(item);
        drop(q);
        self.0.notify.notify_one();
    }

    /// Items queued and not yet taken by the tile.
    pub(crate) fn pending(&self) -> usize {
        self.0.lock().items.len()
    }

    /// Pictures the tile's pipeline holds or has in flight.
    pub(crate) fn backlog(&self) -> u64 {
        self.0.backlog.load(Ordering::Relaxed)
    }

    /// The tile's smoothed decode latency, µs.
    pub(crate) fn latency_us(&self) -> u64 {
        self.0.latency_us.load(Ordering::Relaxed)
    }

    /// Names the picture that makes the generation ready to be shown.
    pub(crate) fn set_target(&self, pts: u64) {
        self.0.target_pts.store(pts, Ordering::SeqCst);
        let mut q = self.0.lock();
        q.items.push_back(FeedItem::Target);
        drop(q);
        self.0.notify.notify_one();
    }

    /// Whether the target was named.
    pub(crate) fn has_target(&self) -> bool {
        self.0.target_pts.load(Ordering::SeqCst) != NO_TARGET
    }

    /// Whether the generation is the one the tile shows.
    pub(crate) fn shown(&self) -> bool {
        self.0.shown.load(Ordering::SeqCst)
    }
}

/// The tile's end of a feed.
pub(crate) struct FeedRx(Arc<FeedShared>);

impl FeedRx {
    /// The next item; waits while there is none (a feed never ends: the
    /// tile replaces or closes it).
    pub(crate) async fn recv(&mut self) -> FeedItem {
        loop {
            let notified = self.0.notify.notified();
            if let Some(i) = self.0.lock().items.pop_front() {
                return i;
            }
            notified.await;
        }
    }

    /// The target picture's timestamp, if named.
    pub(crate) fn target(&self) -> Option<u64> {
        Some(self.0.target_pts.load(Ordering::SeqCst)).filter(|t| *t != NO_TARGET)
    }

    /// Records one measured decode latency (µs) into the smoothed value.
    pub(crate) fn note_latency(&self, us: u64) {
        let old = self.0.latency_us.load(Ordering::Relaxed);
        let new = if old == 0 { us } else { (old * 7 + us) / 8 };
        self.0.latency_us.store(new, Ordering::Relaxed);
    }

    /// Publishes the pipeline's backlog.
    pub(crate) fn set_backlog(&self, n: u64) {
        self.0.backlog.store(n, Ordering::Relaxed);
    }

    /// Marks the generation shown.
    pub(crate) fn set_shown(&self) {
        self.0.shown.store(true, Ordering::SeqCst);
    }

    /// Items the queue's own bound dropped.
    pub(crate) fn dropped(&self) -> u64 {
        self.0.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bc_protocol::media::Codec;

    fn video(keyframe: bool) -> FeedItem {
        FeedItem::Item(Item::Media(Record::Video {
            codec: Codec::H264,
            keyframe,
            pts_us: 0,
            data: vec![0],
            extra_header: Vec::new(),
        }))
    }

    #[tokio::test]
    async fn bounded_and_gated_after_an_overflow() {
        let (tx, mut rx) = feed();
        tx.push(video(true));
        for _ in 0..FEED_CAPACITY + 5 {
            tx.push(video(false));
        }
        // Overflowed: a discontinuity, then predicted frames are dropped
        // until a keyframe.
        assert!(tx.pending() <= FEED_CAPACITY);
        assert!(matches!(
            rx.recv().await,
            FeedItem::Item(Item::Discontinuity)
        ));
        assert_eq!(tx.pending(), 0);
        tx.push(video(false));
        assert_eq!(tx.pending(), 0);
        tx.push(video(true));
        assert_eq!(tx.pending(), 1);
        assert!(rx.dropped() > FEED_CAPACITY as u64);
    }

    #[test]
    fn target_latency_and_shown() {
        let (tx, rx) = feed();
        assert!(!tx.has_target() && rx.target().is_none());
        tx.set_target(42);
        assert_eq!(rx.target(), Some(42));
        assert_eq!(tx.pending(), 1);
        rx.note_latency(8000);
        rx.note_latency(16000);
        assert_eq!(tx.latency_us(), 9000);
        rx.set_backlog(3);
        assert_eq!(tx.backlog(), 3);
        assert!(!tx.shown());
        rx.set_shown();
        assert!(tx.shown());
    }
}
