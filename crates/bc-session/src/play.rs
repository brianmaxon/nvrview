//! Plays of recordings on the device's session (docs/protocol.md,
//! "Playback"): a seek (123) to the starting time, then a play (5) of the
//! recording file, its media routed by message ID; a stop (7) on every exit
//! path.
//!
//! Only normal speed is played: the `playSpeed` values of other rates were
//! seen only encrypted (docs/MILESTONE-P.md, B1). A play continues across
//! recording files by itself (observed); how it ends at the end of the
//! last file is not known (B6): a message with an empty body is taken as
//! the end.
//!
//! The device sends a burst (a few MB) and then roughly real time; the
//! session never waits for a consumer, so a play's unread data is bounded
//! ([`crate::SessionConfig::play_buffer_bytes`]) and a play whose consumer
//! falls that far behind is stopped ([`DownloadError::Overflow`]); the
//! consumer starts it again from where it is.

use crate::archive::{TransferItem, TransferKind, TransferSink};
use crate::supervisor::DeviceSession;
use crate::{DownloadError, QueryError, SearchError, Shared};
use bc_protocol::archive::{DateTime, PlayRequest};
use bc_protocol::media::Record;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::OwnedSemaphorePermit;
use tokio::time::{Instant, timeout_at};

/// What to play: a recording file, from a time within it (or after it: a
/// play continues into the next files).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayStart {
    /// The file (from the file search) and the stream to play it on.
    pub request: PlayRequest,
    /// Where to start: the play begins at the keyframe at or before it.
    pub at: DateTime,
}

/// A running play. Read it with [`Play::next`]; dropping it, or
/// [`Play::stop`], sends the stop (7). It holds one of the device's play
/// permits.
pub struct Play {
    sink: Arc<TransferSink>,
    shared: Arc<Shared>,
    stall: Duration,
    first_wait: Duration,
    got_any: bool,
    finished: Option<Result<(), DownloadError>>,
    seq: u32,
    _permit: OwnedSemaphorePermit,
}

impl Play {
    /// The next media record; `Ok(None)` once the play ended. After an
    /// error, the same error is returned again. Nothing for
    /// [`crate::SessionConfig::play_stall_timeout`] is
    /// [`DownloadError::Stalled`] (not final: stop and start again).
    pub async fn next(&mut self) -> Result<Option<Record>, DownloadError> {
        if let Some(f) = self.finished {
            return f.map(|()| None);
        }
        let wait = if self.got_any {
            self.stall
        } else {
            self.stall + self.first_wait
        };
        let Some(item) = self.sink.next(Instant::now() + wait).await else {
            return Err(DownloadError::Stalled);
        };
        self.got_any = true;
        match item {
            TransferItem::Record(r) => Ok(Some(r)),
            TransferItem::Estimate(_) => Box::pin(self.next()).await,
            TransferItem::End(200 | 300) => {
                self.finished = Some(Ok(()));
                Ok(None)
            }
            TransferItem::End(s) => {
                self.finished = Some(Err(DownloadError::Refused(s)));
                Err(DownloadError::Refused(s))
            }
            other => {
                let e = crate::archive::item_error(&other);
                self.finished = Some(Err(e));
                Err(e)
            }
        }
    }

    /// The play's message ID once it was sent.
    pub fn message_id(&self) -> Option<u32> {
        self.sink.lock().id
    }

    /// The replay `seq` its media carry.
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// Stops the play if it is still running and waits (up to `limit`)
    /// until the stop (7) was sent. True if a stop was sent.
    pub async fn stop(self, limit: Duration) -> bool {
        self.sink.cancel();
        self.shared.wake.notify_one();
        let deadline = Instant::now() + limit;
        loop {
            let notified = self.sink.notify.notified();
            {
                let s = self.sink.lock();
                if s.stopped {
                    return true;
                }
                if s.done && !s.stop_pending {
                    return false;
                }
            }
            if timeout_at(deadline, notified).await.is_err() {
                return self.sink.lock().stopped;
            }
        }
    }
}

impl Drop for Play {
    fn drop(&mut self) {
        self.sink.cancel();
        self.shared.wake.notify_one();
    }
}

impl fmt::Debug for Play {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Play")
            .field("message_id", &self.message_id())
            .field("seq", &self.seq)
            .finish_non_exhaustive()
    }
}

impl DeviceSession {
    /// Starts a play: seeks (123), then plays (5) at normal speed. Refused
    /// at once with [`QueryError::Busy`] when
    /// [`crate::SessionConfig::max_plays`] plays already run; the seek's
    /// refusal is [`SearchError::Refused`].
    pub async fn play(&self, q: &PlayStart) -> Result<Play, SearchError> {
        if self.shared.is_closing() {
            return Err(SearchError::Query(QueryError::Closed));
        }
        let permit = self
            .play_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| SearchError::Query(QueryError::Busy))?;
        let seq = self.play_seq.fetch_add(1, Ordering::Relaxed);
        let at = q.at;
        let reply = self
            .call(Box::new(move |s| s.replay_seek(seq, at)), None)
            .await?;
        if reply.status != 200 {
            return Err(SearchError::Refused(reply.status));
        }
        let req = q.request.clone();
        let sink = self
            .transfer_with(
                TransferKind::Play,
                Box::new(move |s| s.start_play(&req, seq)),
                Some(q.request.clone()),
                self.play_buffer_bytes,
            )
            .await?;
        Ok(Play {
            sink,
            shared: self.shared.clone(),
            stall: self.play_stall_timeout,
            first_wait: self.query_timeout,
            got_any: false,
            finished: None,
            seq,
            _permit: permit,
        })
    }
}
