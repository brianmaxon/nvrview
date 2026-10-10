//! Bounded per-channel frame queue and the subscription handle.

use crate::Shared;
use bc_protocol::media::Record;
use bc_protocol::stream::StreamTier;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::Notify;

/// One thing delivered to a subscriber.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// A demultiplexed media record (stream info, video frame, or audio).
    Media(Record),
    /// The stream was interrupted (connection lost, or the queue overflowed
    /// and was flushed). Flush decoders; video resumes at the next keyframe.
    Discontinuity,
    /// The device answered the preview start with status 400, which may
    /// not be final (a third-party camera behind an NVR does this before
    /// its stream starts): the session keeps the preview open, restarts it
    /// every [`crate::SessionConfig::preview_retry_interval`], and either
    /// media follows (the wait is over) or, after
    /// [`crate::SessionConfig::preview_wait_limit`],
    /// [`Item::StartRejected`] does. Sent once per wait.
    WaitingForDevice {
        /// Reply status of the start request (400).
        status: u16,
    },
    /// The device refused to start this preview (reply status). Final for
    /// this subscription until the connection is reestablished.
    StartRejected {
        /// Reply status of the start request.
        status: u16,
    },
}

#[derive(Default)]
struct State {
    items: VecDeque<Item>,
    /// True until a keyframe arrives: predicted frames and audio are
    /// discarded meanwhile.
    gated: bool,
    closed: bool,
}

/// Fixed-capacity queue. When full it drops everything queued and waits for
/// the next keyframe, so a slow consumer sees a clean gap, never a growing
/// backlog or a stream of frames with missing references.
pub(crate) struct FrameQueue {
    state: Mutex<State>,
    notify: Notify,
    capacity: usize,
    pub(crate) cancelled: AtomicBool,
    dropped: AtomicU64,
}

impl FrameQueue {
    pub(crate) fn new(capacity: usize) -> FrameQueue {
        FrameQueue {
            state: Mutex::new(State {
                gated: true,
                ..State::default()
            }),
            notify: Notify::new(),
            capacity: capacity.max(1),
            cancelled: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether [`FrameQueue::push_media`] would keep `rec` right now (not
    /// discarded by keyframe gating).
    pub(crate) fn accepts(&self, rec: &Record) -> bool {
        match rec {
            Record::Video { keyframe: true, .. } | Record::Info { .. } => true,
            Record::Video { .. } | Record::Audio { .. } => !self.lock().gated,
        }
    }

    pub(crate) fn push_media(&self, rec: Record) {
        let keyframe = matches!(rec, Record::Video { keyframe: true, .. });
        let gateable = matches!(
            rec,
            Record::Audio { .. }
                | Record::Video {
                    keyframe: false,
                    ..
                }
        );
        let mut s = self.lock();
        if keyframe {
            s.gated = false;
        } else if gateable && s.gated {
            return;
        }
        if s.items.len() >= self.capacity {
            self.dropped
                .fetch_add(s.items.len() as u64, Ordering::Relaxed);
            s.items.clear();
            if gateable {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                s.gated = true;
                s.items.push_back(Item::Discontinuity);
                drop(s);
                self.notify.notify_one();
                return;
            }
            s.items.push_back(Item::Discontinuity);
        }
        s.items.push_back(Item::Media(rec));
        drop(s);
        self.notify.notify_one();
    }

    /// Flush and gate; queue a [`Item::Discontinuity`].
    pub(crate) fn discontinuity(&self) {
        let mut s = self.lock();
        self.dropped
            .fetch_add(s.items.len() as u64, Ordering::Relaxed);
        s.items.clear();
        s.gated = true;
        s.items.push_back(Item::Discontinuity);
        drop(s);
        self.notify.notify_one();
    }

    /// Discard queued frames and wait for the next keyframe (after a demux
    /// error), without a visible discontinuity marker beyond the gap itself.
    pub(crate) fn gate(&self) {
        let mut s = self.lock();
        s.gated = true;
    }

    pub(crate) fn push_control(&self, item: Item) {
        let mut s = self.lock();
        if s.items.len() >= self.capacity {
            s.items.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        s.items.push_back(item);
        drop(s);
        self.notify.notify_one();
    }

    pub(crate) fn close(&self) {
        self.lock().closed = true;
        self.notify.notify_one();
    }

    async fn recv(&self) -> Option<Item> {
        loop {
            {
                let mut s = self.lock();
                if let Some(i) = s.items.pop_front() {
                    return Some(i);
                }
                if s.closed {
                    return None;
                }
            }
            self.notify.notified().await;
        }
    }
}

/// A live subscription to one channel and stream tier. Dropping it stops the
/// preview on the device (as long as the session is still running).
pub struct Subscription {
    pub(crate) channel: u8,
    pub(crate) tier: StreamTier,
    pub(crate) queue: Arc<FrameQueue>,
    pub(crate) shared: Arc<Shared>,
}

impl Subscription {
    /// Waits for the next item; `None` once the session has ended for good
    /// (closed, or login refused) and the queue is drained.
    pub async fn recv(&mut self) -> Option<Item> {
        self.queue.recv().await
    }

    /// Channel number.
    pub fn channel(&self) -> u8 {
        self.channel
    }

    /// Stream tier.
    pub fn tier(&self) -> StreamTier {
        self.tier
    }

    /// Frames discarded so far because the consumer was too slow or the
    /// connection dropped.
    pub fn dropped(&self) -> u64 {
        self.queue.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.queue.cancelled.store(true, Ordering::SeqCst);
        self.shared.wake.notify_one();
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("channel", &self.channel)
            .field("tier", &self.tier)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bc_protocol::media::Codec;

    fn video(key: bool, pts: u64) -> Record {
        Record::Video {
            codec: Codec::H264,
            keyframe: key,
            pts_us: pts,
            data: vec![0],
            extra_header: Vec::new(),
        }
    }

    #[tokio::test]
    async fn gates_until_keyframe_and_flushes_on_overflow() {
        let q = FrameQueue::new(3);
        q.push_media(video(false, 1)); // gated: ignored
        q.push_media(video(true, 2));
        q.push_media(video(false, 3));
        q.push_media(video(false, 4));
        // Full: the next predicted frame flushes everything and gates.
        q.push_media(video(false, 5));
        assert_eq!(q.dropped.load(Ordering::Relaxed), 4);
        assert_eq!(q.recv().await, Some(Item::Discontinuity));
        q.push_media(video(false, 6)); // gated
        q.push_media(video(true, 7));
        assert_eq!(q.recv().await, Some(Item::Media(video(true, 7))));
        q.close();
        assert_eq!(q.recv().await, None);
    }

    #[tokio::test]
    async fn never_exceeds_capacity() {
        let q = FrameQueue::new(8);
        for i in 0..10_000u64 {
            q.push_media(video(i % 30 == 0, i));
            assert!(q.lock().items.len() <= 8 + 1);
        }
    }
}
