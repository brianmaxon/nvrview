//! Plays of recordings on the device's session (docs/protocol.md,
//! "Playback"): a seek (123) to the starting time, then a play (5) of the
//! recording file, its media routed by message ID; a stop (7) on every exit
//! path.
//!
//! The rate is the request's `playSpeed` (1, 2, 4, 8, or 16, as observed).
//! A play continues across recording files by itself (observed); the device
//! was never seen ending a play (docs/protocol.md, "Rates, keyframes, and
//! how a play ends"), so a message with an empty body is taken as the end
//! only in case one comes; callers end plays themselves.
//!
//! The device sends a burst (a few MB) and then roughly real time; the
//! session never waits for a consumer, so a play's unread data is bounded
//! ([`crate::SessionConfig::play_buffer_bytes`]) and a play whose consumer
//! falls that far behind is stopped ([`DownloadError::Overflow`]); the
//! consumer starts it again from where it is.

use crate::archive::{TransferItem, TransferKind, TransferSink};
use crate::supervisor::{DeviceSession, Reply};
use crate::{DownloadError, QueryError, SearchError, Shared};
use bc_protocol::archive::{DateTime, PlayRequest};
use bc_protocol::media::Record;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::Poll;
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

/// A boxed future, for a list of requests to drive together.
type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Drives the futures together and returns their outputs in order. Each
/// round polls them in order, so requests that are ready to go out leave
/// in the order given.
async fn join_in_order<T>(mut futs: Vec<Boxed<'_, T>>) -> Vec<T> {
    let mut out: Vec<Option<T>> = futs.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (f, slot) in futs.iter_mut().zip(out.iter_mut()) {
            if slot.is_none() {
                match f.as_mut().poll(cx) {
                    Poll::Ready(v) => *slot = Some(v),
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    out.into_iter().flatten().collect()
}

impl DeviceSession {
    /// Starts a play: seeks (123), then plays (5) at the request's speed. Refused
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

    /// Starts several plays together, as the official client did
    /// (docs/protocol.md, "Four plays at once"): all seeks (123) back to
    /// back with consecutive `seq` numbers and the one `seekTime` `at`
    /// (`Some`: the official client's recipe; with `None` each start's own
    /// [`PlayStart::at`] is sent instead, an experiment: the device keeps
    /// one seek position per session, so the last seek is likely the one
    /// every play starts from), then their replies, then a pause of [`crate::SessionConfig::play_group_delay`] (about
    /// 0.5 s), then all plays (5) back to back, each with a message ID of
    /// its own. The returned plays are in the order of `starts`. The call
    /// stops nothing itself: stop running plays first.
    ///
    /// All or nothing: refused at once with [`QueryError::Busy`] when the
    /// group does not fit under [`crate::SessionConfig::max_plays`] beside
    /// the plays that run; a refused or failed seek is an error, and when
    /// anything fails after plays were sent, those plays are stopped
    /// (dropped) before the error returns. The plays' own refusals arrive
    /// later, from [`Play::next`].
    pub async fn play_group(
        &self,
        starts: &[PlayStart],
        at: Option<DateTime>,
    ) -> Result<Vec<Play>, SearchError> {
        if self.shared.is_closing() {
            return Err(SearchError::Query(QueryError::Closed));
        }
        if starts.is_empty() {
            return Ok(Vec::new());
        }
        let mut permits = Vec::with_capacity(starts.len());
        for _ in starts {
            permits.push(
                self.play_permits
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| SearchError::Query(QueryError::Busy))?,
            );
        }
        let n = starts.len() as u32;
        let base = self.play_seq.fetch_add(n, Ordering::Relaxed);
        let seeks: Vec<Boxed<'_, Result<Reply, QueryError>>> = (0..n)
            .map(|i| {
                let seq = base.wrapping_add(i);
                let at = at.unwrap_or(starts[i as usize].at);
                Box::pin(self.call(Box::new(move |s| s.replay_seek(seq, at)), None))
                    as Boxed<'_, Result<Reply, QueryError>>
            })
            .collect();
        for reply in join_in_order(seeks).await {
            let reply = reply?;
            if reply.status != 200 {
                return Err(SearchError::Refused(reply.status));
            }
        }
        tokio::time::sleep(self.play_group_delay).await;
        let mut plays = Vec::with_capacity(starts.len());
        for (i, (q, permit)) in starts.iter().zip(permits).enumerate() {
            let seq = base.wrapping_add(i as u32);
            let req = q.request.clone();
            // On an error the plays already sent are dropped with `plays`,
            // which stops them.
            let sink = self
                .transfer_with(
                    TransferKind::Play,
                    Box::new(move |s| s.start_play(&req, seq)),
                    Some(q.request.clone()),
                    self.play_buffer_bytes,
                )
                .await?;
            plays.push(Play {
                sink,
                shared: self.shared.clone(),
                stall: self.play_stall_timeout,
                first_wait: self.query_timeout,
                got_any: false,
                finished: None,
                seq,
                _permit: permit,
            });
        }
        Ok(plays)
    }
}
