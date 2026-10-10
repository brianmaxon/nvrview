//! Recordings on the device: searches, cover images, and downloads, carried
//! by the same session as live view and routed by message ID.
//!
//! Every request here is read-only (viewing tier). Searches hold a search
//! permit for their whole open–fetch–close sequence, so by default they run
//! one at a time per device; downloads hold a download permit for as long as
//! the [`Download`] lives.

use crate::Shared;
use crate::supervisor::{Build, DeviceSession, QueryError, Reply, Request, TransferReq};
use bc_protocol::archive::{
    self, CoverRequest, DateTime, DayRecordChannel, DayRecords, DownloadRequest, Element,
    EventRecord, EventSearch, FileSearch, RecordFile,
};
use bc_protocol::media::{Codec, Record};
use bc_protocol::xml::XmlError;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::{Notify, OwnedSemaphorePermit};
use tokio::time::{Instant, timeout_at};

/// Most 15 requests one file search sends (a day of continuous recording
/// came back in a single reply).
const MAX_FILE_PAGES: usize = 100;
/// Most 273 pages one event search fetches (50 events each).
const MAX_EVENT_PAGES: usize = 2000;

/// Reads each channel's camera device ID (`uid`) from a channel list (145).
pub(crate) fn parse_channel_uids(xml: &str) -> HashMap<u8, String> {
    let Ok(root) = Element::parse(xml) else {
        return HashMap::new();
    };
    root.find_all("ChannelInfo")
        .into_iter()
        .filter_map(|c| {
            let ch = c.num::<u8>("channelId").ok().flatten()?;
            let uid = c.text_of("uid").filter(|u| !u.is_empty())?;
            Some((ch, uid.to_owned()))
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransferKind {
    Download,
    Cover,
}

/// What the supervisor hands a transfer's owner.
#[derive(Debug)]
pub(crate) enum TransferItem {
    Estimate(u64),
    Record(Record),
    End(u16),
    ConnectionLost,
    Closed,
    Cancelled,
    Busy,
    NotAllowed,
    BadMedia,
    Overflow,
}

#[derive(Default)]
struct SinkState {
    items: VecDeque<TransferItem>,
    bytes: usize,
    /// No more items will be accepted (ended, failed, or overflowed).
    done: bool,
    /// The supervisor stopped the transfer (sent 144 for a download).
    stopped: bool,
    /// Ended by an overflow or bad media: the supervisor will stop it.
    stop_pending: bool,
    id: Option<u32>,
}

/// The queue between the supervisor and a transfer's owner. Bounded in
/// bytes: on overflow the transfer is stopped rather than the session
/// blocked (blocking would stall live view on the same connection).
pub(crate) struct TransferSink {
    state: Mutex<SinkState>,
    notify: Notify,
    cancelled: AtomicBool,
    max_bytes: usize,
}

fn record_len(r: &Record) -> usize {
    match r {
        Record::Video { data, .. } | Record::Audio { data, .. } => data.len() + 32,
        Record::Info { .. } => 32,
    }
}

impl TransferSink {
    pub(crate) fn new(max_bytes: usize) -> TransferSink {
        TransferSink {
            state: Mutex::new(SinkState::default()),
            notify: Notify::new(),
            cancelled: AtomicBool::new(false),
            max_bytes: max_bytes.max(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SinkState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Queues an item; false when the queue is over its byte limit (the
    /// transfer then ends with an overflow and must be stopped).
    pub(crate) fn push(&self, item: TransferItem) -> bool {
        let mut s = self.lock();
        if s.done {
            return true;
        }
        if let TransferItem::Record(r) = &item {
            let n = record_len(r);
            if s.bytes + n > self.max_bytes {
                s.done = true;
                s.stop_pending = true;
                s.items.push_back(TransferItem::Overflow);
                drop(s);
                self.notify.notify_one();
                return false;
            }
            s.bytes += n;
        }
        if matches!(item, TransferItem::End(_)) {
            s.done = true;
        }
        s.items.push_back(item);
        drop(s);
        self.notify.notify_one();
        true
    }

    /// Ends the transfer with `item` unless it already ended.
    pub(crate) fn fail(&self, item: TransferItem) {
        let mut s = self.lock();
        if !s.done {
            s.done = true;
            s.stop_pending = matches!(item, TransferItem::BadMedia | TransferItem::Overflow);
            s.items.push_back(item);
        }
        drop(s);
        self.notify.notify_one();
    }

    pub(crate) fn started(&self, id: u32) {
        self.lock().id = Some(id);
    }

    pub(crate) fn mark_stopped(&self) {
        self.lock().stopped = true;
        self.notify.notify_one();
    }

    fn pop(&self) -> Option<TransferItem> {
        let mut s = self.lock();
        let item = s.items.pop_front();
        if let Some(TransferItem::Record(r)) = &item {
            s.bytes = s.bytes.saturating_sub(record_len(r));
        }
        item
    }

    /// The next item, waiting until `deadline`; `None` on timeout.
    async fn next(&self, deadline: Instant) -> Option<TransferItem> {
        loop {
            let notified = self.notify.notified();
            if let Some(i) = self.pop() {
                return Some(i);
            }
            if timeout_at(deadline, notified).await.is_err() {
                return self.pop();
            }
        }
    }
}

/// Why a recording search, cover image, or download could not be done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchError {
    /// A request failed (session closed, connection lost, timed out, ...).
    Query(QueryError),
    /// The device answered with this status.
    Refused(u16),
    /// The reply could not be read.
    Xml(XmlError),
    /// The device kept sending pages beyond any plausible number.
    TooManyPages,
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SearchError::Query(e) => write!(f, "{e}"),
            SearchError::Refused(s) => write!(f, "refused by the device (status {s})"),
            SearchError::Xml(e) => write!(f, "unreadable reply: {e}"),
            SearchError::TooManyPages => write!(f, "too many result pages"),
        }
    }
}

impl std::error::Error for SearchError {}

impl From<QueryError> for SearchError {
    fn from(e: QueryError) -> Self {
        SearchError::Query(e)
    }
}

impl From<XmlError> for SearchError {
    fn from(e: XmlError) -> Self {
        SearchError::Xml(e)
    }
}

/// 200, or 300 (which some replies carry with a complete body).
fn ok_status(r: &Reply) -> Result<(), SearchError> {
    if matches!(r.status, 200 | 300) {
        Ok(())
    } else {
        Err(SearchError::Refused(r.status))
    }
}

/// One cover image: the stream info and the keyframe a 298 reply carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverImage {
    /// Width from the stream info record (may be 0; take it from the SPS).
    pub width: u32,
    /// Height from the stream info record (may be 0).
    pub height: u32,
    /// Codec of the keyframe.
    pub codec: Codec,
    /// The keyframe's timestamp in microseconds.
    pub pts_us: u64,
    /// The keyframe as Annex-B NAL units, parameter sets included.
    pub keyframe: Vec<u8>,
}

/// Something a download delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadEvent {
    /// The device's estimate of the download's size in bytes.
    Estimate(u64),
    /// A media record: stream info, video, or audio, in stream order.
    Record(Record),
}

/// Why a download did not end normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadError {
    /// Nothing arrived for [`crate::SessionConfig::download_stall_timeout`].
    /// The download is still running on the device: drop or
    /// [`Download::cancel`] it (which sends the stop) and fetch again.
    Stalled,
    /// The connection dropped; the session reconnects on its own.
    ConnectionLost,
    /// The session is closed.
    Closed,
    /// The device ended the download with this status (not 200 or 300).
    Refused(u16),
    /// The consumer left too much data unread; the download was stopped.
    Overflow,
    /// The media could not be parsed; the download was stopped.
    BadMedia,
    /// Too many requests outstanding on the session.
    Busy,
    /// The request could not be built (not logged in yet, for example).
    NotAllowed,
    /// The download was cancelled.
    Cancelled,
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DownloadError::Stalled => "download stalled",
            DownloadError::ConnectionLost => "connection lost during the download",
            DownloadError::Closed => "device session is closed",
            DownloadError::Refused(_) => "download refused by the device",
            DownloadError::Overflow => "download data was not read in time",
            DownloadError::BadMedia => "unreadable media in the download",
            DownloadError::Busy => "too many requests outstanding",
            DownloadError::NotAllowed => "download request not allowed",
            DownloadError::Cancelled => "download cancelled",
        })
    }
}

impl std::error::Error for DownloadError {}

fn item_error(i: &TransferItem) -> DownloadError {
    match i {
        TransferItem::ConnectionLost => DownloadError::ConnectionLost,
        TransferItem::Closed => DownloadError::Closed,
        TransferItem::Cancelled => DownloadError::Cancelled,
        TransferItem::Busy => DownloadError::Busy,
        TransferItem::NotAllowed => DownloadError::NotAllowed,
        TransferItem::BadMedia => DownloadError::BadMedia,
        TransferItem::Overflow => DownloadError::Overflow,
        TransferItem::End(s) => DownloadError::Refused(*s),
        TransferItem::Estimate(_) | TransferItem::Record(_) => DownloadError::Closed,
    }
}

/// A running download (command 143) of one stream's time range.
///
/// Read it with [`Download::next`] until `Ok(None)` (the device's end
/// message). Dropping it before the end, or calling [`Download::cancel`],
/// sends the stop (144). It holds one of the device's download permits.
pub struct Download {
    sink: Arc<TransferSink>,
    shared: Arc<Shared>,
    stall: Duration,
    /// Extra time allowed for the first item (the session may still be
    /// logging in).
    first_wait: Duration,
    got_any: bool,
    finished: Option<Result<(), DownloadError>>,
    _permit: OwnedSemaphorePermit,
}

impl Download {
    /// The next event; `Ok(None)` once the device ended the download
    /// normally. After an error, the same error is returned again.
    pub async fn next(&mut self) -> Result<Option<DownloadEvent>, DownloadError> {
        if let Some(f) = self.finished {
            return f.map(|()| None);
        }
        let wait = if self.got_any {
            self.stall
        } else {
            self.stall + self.first_wait
        };
        let Some(item) = self.sink.next(Instant::now() + wait).await else {
            // Not final: the caller decides to cancel and retry.
            return Err(DownloadError::Stalled);
        };
        self.got_any = true;
        match item {
            TransferItem::Estimate(n) => Ok(Some(DownloadEvent::Estimate(n))),
            TransferItem::Record(r) => Ok(Some(DownloadEvent::Record(r))),
            TransferItem::End(200 | 300) => {
                self.finished = Some(Ok(()));
                Ok(None)
            }
            other => {
                let e = item_error(&other);
                self.finished = Some(Err(e));
                Err(e)
            }
        }
    }

    /// The download's message ID once it was sent.
    pub fn message_id(&self) -> Option<u32> {
        self.sink.lock().id
    }

    /// Stops the download if it is still running and waits (up to `limit`)
    /// until the stop (144) was sent. True if a stop was sent.
    pub async fn cancel(self, limit: Duration) -> bool {
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
                // Ended (or failed) with nothing left to stop.
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

impl Drop for Download {
    fn drop(&mut self) {
        self.sink.cancel();
        self.shared.wake.notify_one();
    }
}

impl fmt::Debug for Download {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Download")
            .field("message_id", &self.message_id())
            .finish_non_exhaustive()
    }
}

impl DeviceSession {
    async fn search_permit(&self) -> Result<tokio::sync::SemaphorePermit<'_>, SearchError> {
        if self.shared.is_closing() {
            return Err(SearchError::Query(QueryError::Closed));
        }
        self.search_permits
            .acquire()
            .await
            .map_err(|_| SearchError::Query(QueryError::Closed))
    }

    async fn step(&self, build: Build, conn: Option<u64>) -> Result<Reply, SearchError> {
        Ok(self.call(build, conn).await?)
    }

    /// Which days have recordings (command 142) for each channel, within
    /// `start`..`end` (the official client asks one month at a time).
    pub async fn day_records(
        &self,
        start: DateTime,
        end: DateTime,
        channels: &[DayRecordChannel],
    ) -> Result<Vec<DayRecords>, SearchError> {
        let _p = self.search_permit().await?;
        let channels = channels.to_vec();
        let r = self
            .step(
                Box::new(move |s| s.day_records(start, end, &channels)),
                None,
            )
            .await?;
        ok_status(&r)?;
        Ok(archive::parse_day_records(&r.xml)?)
    }

    /// The recording files of one channel and window (commands 14, 15, 16).
    /// Fetches pages until one adds nothing new, then closes the search
    /// (also after an error).
    pub async fn search_files(&self, q: &FileSearch) -> Result<Vec<RecordFile>, SearchError> {
        let _p = self.search_permit().await?;
        let q2 = q.clone();
        let open = self
            .step(Box::new(move |s| s.file_search_open(&q2)), None)
            .await?;
        ok_status(&open)?;
        let handle = archive::parse_file_search_opened(&open.xml)?.handle;
        let (id, conn, ch, uid) = (open.message_id, open.conn, q.channel, q.uid.clone());
        let mut files: Vec<RecordFile> = Vec::new();
        let mut seen = HashSet::new();
        let mut result = Err(SearchError::TooManyPages);
        for page in 0..MAX_FILE_PAGES {
            let u = uid.clone();
            let r = match self
                .step(
                    Box::new(move |s| {
                        s.file_search_step(id, ch, u.as_deref(), handle, false)
                            .map(|b| (id, b))
                    }),
                    Some(conn),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            if !matches!(r.status, 200 | 300) {
                // A further page refused: the list is complete. A refused
                // first page is an error.
                result = if page == 0 {
                    Err(SearchError::Refused(r.status))
                } else {
                    Ok(())
                };
                break;
            }
            let list = match archive::parse_file_list(&r.xml) {
                Ok(l) => l,
                Err(e) => {
                    result = Err(e.into());
                    break;
                }
            };
            let before = files.len();
            for f in list {
                if seen.insert((f.id.clone(), f.name.clone(), f.start)) {
                    files.push(f);
                }
            }
            if files.len() == before {
                result = Ok(());
                break;
            }
        }
        let u = uid.clone();
        let _ = self
            .step(
                Box::new(move |s| {
                    s.file_search_step(id, ch, u.as_deref(), handle, true)
                        .map(|b| (id, b))
                }),
                Some(conn),
            )
            .await;
        result.map(|()| files)
    }

    /// The motion and detection events of one channel and window (commands
    /// 272, 273 until `bFinished`, 274), in device order. The search is
    /// closed also after an error.
    pub async fn search_events(&self, q: &EventSearch) -> Result<Vec<EventRecord>, SearchError> {
        let _p = self.search_permit().await?;
        let q2 = q.clone();
        let open = self
            .step(Box::new(move |s| s.event_search_open(&q2)), None)
            .await?;
        ok_status(&open)?;
        let fh = archive::parse_event_search_opened(&open.xml)?;
        let (id, conn, ch) = (open.message_id, open.conn, q.channel);
        let mut events = Vec::new();
        let mut result = Err(SearchError::TooManyPages);
        for _ in 0..MAX_EVENT_PAGES {
            let r = match self
                .step(
                    Box::new(move |s| s.event_search_step(id, ch, fh, false).map(|b| (id, b))),
                    Some(conn),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            if let Err(e) = ok_status(&r) {
                result = Err(e);
                break;
            }
            match archive::parse_event_page(&r.xml) {
                Ok(p) => {
                    let empty = p.events.is_empty();
                    events.extend(p.events);
                    if p.finished || empty {
                        result = Ok(());
                        break;
                    }
                }
                Err(e) => {
                    result = Err(e.into());
                    break;
                }
            }
        }
        let _ = self
            .step(
                Box::new(move |s| s.event_search_step(id, ch, fh, true).map(|b| (id, b))),
                Some(conn),
            )
            .await;
        result.map(|()| events)
    }

    async fn transfer(
        &self,
        kind: TransferKind,
        build: Build,
    ) -> Result<Arc<TransferSink>, SearchError> {
        if self.shared.is_closing() {
            return Err(SearchError::Query(QueryError::Closed));
        }
        let sink = Arc::new(TransferSink::new(self.download_buffer_bytes));
        self.cmd_tx
            .send(Request::Transfer(TransferReq {
                kind,
                build,
                sink: sink.clone(),
            }))
            .await
            .map_err(|_| SearchError::Query(QueryError::Closed))?;
        self.shared.wake.notify_one();
        Ok(sink)
    }

    /// One cover image for a time range (command 298); `None` when the
    /// device sent no picture (no recording in the range).
    pub async fn cover_image(&self, q: &CoverRequest) -> Result<Option<CoverImage>, SearchError> {
        let _p = self.search_permit().await?;
        let q2 = q.clone();
        let sink = self
            .transfer(TransferKind::Cover, Box::new(move |s| s.cover(&q2)))
            .await?;
        let deadline = Instant::now() + self.query_timeout;
        let (mut width, mut height) = (0, 0);
        let mut picture = None;
        let result = loop {
            match sink.next(deadline).await {
                None => break Err(SearchError::Query(QueryError::TimedOut)),
                Some(TransferItem::Record(Record::Info {
                    width: w,
                    height: h,
                    ..
                })) => (width, height) = (w, h),
                Some(TransferItem::Record(Record::Video {
                    codec,
                    keyframe: true,
                    pts_us,
                    data,
                    ..
                })) if picture.is_none() => picture = Some((codec, pts_us, data)),
                Some(TransferItem::Record(_) | TransferItem::Estimate(_)) => {}
                Some(TransferItem::End(200 | 300)) => break Ok(()),
                Some(TransferItem::End(s)) => break Err(SearchError::Refused(s)),
                Some(TransferItem::ConnectionLost) => {
                    break Err(SearchError::Query(QueryError::ConnectionLost));
                }
                Some(TransferItem::Busy) => break Err(SearchError::Query(QueryError::Busy)),
                Some(_) => break Err(SearchError::Query(QueryError::Closed)),
            }
        };
        sink.cancel();
        result.map(|()| {
            picture.map(|(codec, pts_us, keyframe)| CoverImage {
                width,
                height,
                codec,
                pts_us,
                keyframe,
            })
        })
    }

    /// Starts a download of one stream's time range (command 143). Waits
    /// for a download permit first (one at a time per device by default).
    pub async fn download(&self, q: &DownloadRequest) -> Result<Download, SearchError> {
        let permit = self
            .download_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| SearchError::Query(QueryError::Closed))?;
        let q2 = q.clone();
        let sink = self
            .transfer(
                TransferKind::Download,
                Box::new(move |s| s.start_download(&q2)),
            )
            .await?;
        Ok(Download {
            sink,
            shared: self.shared.clone(),
            stall: self.download_stall_timeout,
            first_wait: self.query_timeout,
            got_any: false,
            finished: None,
            _permit: permit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_uids_are_read_from_the_channel_list() {
        let xml = "<body><ChannelInfoList><ChannelInfo><channelId>0</channelId><uid>UID-EXAMPLE</uid>\
                   </ChannelInfo><ChannelInfo><channelId>1</channelId><uid></uid></ChannelInfo>\
                   </ChannelInfoList></body>";
        let m = parse_channel_uids(xml);
        assert_eq!(m.len(), 1);
        assert_eq!(m[&0], "UID-EXAMPLE");
        assert!(parse_channel_uids("<a><b></a>").is_empty());
    }

    #[tokio::test]
    async fn sink_is_bounded_and_ends_once() {
        let s = TransferSink::new(100);
        let rec = |n: usize| {
            TransferItem::Record(Record::Audio {
                stream_index: 5,
                data: vec![0; n],
            })
        };
        assert!(s.push(rec(10)));
        assert!(!s.push(rec(100)));
        // Ended by the overflow: nothing more is queued.
        assert!(s.push(rec(1)));
        s.fail(TransferItem::Closed);
        let far = Instant::now() + Duration::from_secs(1);
        assert!(matches!(s.next(far).await, Some(TransferItem::Record(_))));
        assert!(matches!(s.next(far).await, Some(TransferItem::Overflow)));
        let soon = Instant::now() + Duration::from_millis(20);
        assert!(s.next(soon).await.is_none());
    }
}
