//! Fills and refreshes a device's [`DeviceIndex`] in the background.
//!
//! A pass asks which days have recordings (142, one request per month for
//! every indexed channel), then searches files (14–16) and events (272–274)
//! day by day, **newest day first**, for every channel and day that is not
//! indexed yet, was indexed before the day was over, or is today and older
//! than [`IndexerConfig::refresh_today`]. Then it applies the index limits.
//! The background task repeats passes every [`IndexerConfig::pass_interval`].
//! Searches go through the device session, which serializes them and keeps
//! live view running.

use crate::index::{DeviceIndex, IndexedEvent, IndexedFile};
use crate::{Clock, Secs, date_time, day_start, secs};
use bc_protocol::archive::{DateTime, DayRecordChannel, EventSearch, FileSearch, days_in_month};
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, SearchError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// A channel to index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedChannel {
    /// Channel number.
    pub channel: u8,
    /// The camera's device ID; when `None`, the session's channel list is
    /// used.
    pub uid: Option<String>,
    /// Stream (lens) to search: `Main` for a camera's first lens, `Extern`
    /// for a second lens.
    pub stream: StreamTier,
}

/// Indexer settings.
#[derive(Clone)]
pub struct IndexerConfig {
    /// Channels to index.
    pub channels: Vec<IndexedChannel>,
    /// How many days before today to index.
    pub days_back: u32,
    /// Refetch today's files and events (and the current month's days)
    /// when older than this many seconds.
    pub refresh_today: Secs,
    /// Pause between passes of the background task.
    pub pass_interval: Duration,
    /// The device's local time.
    pub clock: Clock,
}

impl IndexerConfig {
    /// Defaults: 14 days back, today refreshed every 60 s, a pass every 60 s.
    pub fn new(channels: Vec<IndexedChannel>, clock: Clock) -> IndexerConfig {
        IndexerConfig {
            channels,
            days_back: 14,
            refresh_today: 60,
            pass_interval: Duration::from_secs(60),
            clock,
        }
    }
}

impl std::fmt::Debug for IndexerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexerConfig")
            .field("channels", &self.channels)
            .field("days_back", &self.days_back)
            .field("refresh_today", &self.refresh_today)
            .finish_non_exhaustive()
    }
}

/// What one pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassReport {
    /// Months asked with 142.
    pub months: usize,
    /// Channel-days searched (files and events) and stored, newest first.
    pub days: Vec<(u8, Secs)>,
    /// Searches that failed (retried next pass).
    pub errors: usize,
}

fn lock(index: &Mutex<DeviceIndex>) -> std::sync::MutexGuard<'_, DeviceIndex> {
    index.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Start of the month containing `s`, and its last second.
fn month_bounds(s: Secs) -> (DateTime, DateTime) {
    let t = date_time(s);
    let first = DateTime::date(t.year, t.month, 1).unwrap_or(t);
    let last = DateTime::date(t.year, t.month, days_in_month(t.year, t.month))
        .unwrap_or(t)
        .end_of_day();
    (first, last)
}

/// Runs one indexing pass.
pub async fn run_pass(
    session: &DeviceSession,
    index: &Mutex<DeviceIndex>,
    cfg: &IndexerConfig,
) -> PassReport {
    let mut report = PassReport::default();
    let now = (cfg.clock)();
    let today = day_start(now);
    let first = today - i64::from(cfg.days_back) * 86_400;
    let uids = session.channel_uids();
    let uid_of = |c: &IndexedChannel| c.uid.clone().or_else(|| uids.get(&c.channel).cloned());

    // 1. Days with recordings, one 142 per month for all channels.
    let mut month = month_bounds(first).0;
    loop {
        let (m_start, m_end) = month_bounds(secs(month));
        if secs(m_start) > today {
            break;
        }
        let (ms, me) = (secs(m_start), secs(m_end));
        let stale = |fetched: Option<Secs>| match fetched {
            None => true,
            // The current month gains days; a past month is final once it
            // was read after it ended.
            Some(at) if me >= today => now - at >= cfg.refresh_today,
            Some(at) => at <= me,
        };
        let wanted: Vec<&IndexedChannel> = {
            let idx = lock(index);
            cfg.channels
                .iter()
                .filter(|c| stale(idx.month_fetched_at(c.channel, ms)))
                .collect()
        };
        if !wanted.is_empty() {
            let req: Vec<DayRecordChannel> = wanted
                .iter()
                .map(|c| DayRecordChannel {
                    channel: c.channel,
                    uid: uid_of(c),
                })
                .collect();
            match session.day_records(m_start, m_end, &req).await {
                Ok(list) => {
                    report.months += 1;
                    let mut idx = lock(index);
                    for c in &wanted {
                        let days: Vec<Secs> = list
                            .iter()
                            .filter(|d| d.channel == c.channel)
                            .flat_map(|d| d.days.iter())
                            .filter(|t| t.has_recordings())
                            .filter_map(|t| {
                                DateTime::date(m_start.year, m_start.month, t.day_of_month() as u8)
                            })
                            .map(secs)
                            .collect();
                        if let Err(e) = idx.set_month(c.channel, ms, me, days, now) {
                            warn!(error = %e, "cannot store the recording calendar");
                        }
                    }
                }
                Err(e) => {
                    report.errors += 1;
                    debug!(error = %e, "day records failed");
                }
            }
        }
        let next = me + 1;
        month = date_time(next);
    }

    // 2. Files and events, newest day first.
    let mut day = today;
    while day >= first {
        for c in &cfg.channels {
            let needed = {
                let idx = lock(index);
                if !idx.recorded_days(c.channel).contains(&day) {
                    false
                } else {
                    match idx.channel_day(day, c.channel) {
                        None => true,
                        Some(cd) if day == today => now - cd.fetched_at >= cfg.refresh_today,
                        // Fetched before the day was over: once more.
                        Some(cd) => cd.fetched_at < day + 86_400,
                    }
                }
            };
            if !needed {
                continue;
            }
            match index_day(session, c, uid_of(c), day).await {
                Ok((files, events)) => {
                    let mut idx = lock(index);
                    if let Err(e) = idx.put_day(day, c.channel, files, events, (cfg.clock)()) {
                        warn!(error = %e, "cannot store the index");
                        report.errors += 1;
                    } else {
                        report.days.push((c.channel, day));
                    }
                }
                Err(e) => {
                    report.errors += 1;
                    debug!(error = %e, channel = c.channel, "day search failed");
                }
            }
        }
        day -= 86_400;
    }

    if let Err(e) = lock(index).prune(now) {
        warn!(error = %e, "cannot prune the index");
    }
    report
}

async fn index_day(
    session: &DeviceSession,
    c: &IndexedChannel,
    uid: Option<String>,
    day: Secs,
) -> Result<(Vec<IndexedFile>, Vec<IndexedEvent>), SearchError> {
    let start = date_time(day);
    let end = start.end_of_day();
    let files = session
        .search_files(&FileSearch {
            channel: c.channel,
            uid: uid.clone(),
            stream: c.stream,
            start,
            end,
        })
        .await?;
    let events = session
        .search_events(&EventSearch {
            channel: c.channel,
            uid,
            stream: c.stream,
            start,
            end,
        })
        .await?;
    Ok((
        files.iter().map(IndexedFile::from).collect(),
        events.iter().map(IndexedEvent::from).collect(),
    ))
}

/// A running background indexer. Dropping it stops the task at its next
/// await point.
pub struct Indexer {
    stop: watch::Sender<bool>,
    reports: watch::Receiver<Option<PassReport>>,
    join: Option<JoinHandle<()>>,
}

impl Indexer {
    /// Starts passes on the current runtime: one at once, then one every
    /// [`IndexerConfig::pass_interval`].
    pub fn spawn(
        session: Arc<DeviceSession>,
        index: Arc<Mutex<DeviceIndex>>,
        cfg: IndexerConfig,
    ) -> Indexer {
        let (stop, mut stop_rx) = watch::channel(false);
        let (report_tx, reports) = watch::channel(None);
        let join = tokio::spawn(async move {
            let mut failures = 0u64;
            loop {
                let pass = run_pass(&session, &index, &cfg);
                let report = tokio::select! {
                    r = pass => r,
                    _ = stop_rx.changed() => return,
                };
                if report.errors > 0 {
                    failures += 1;
                    // One line per pass at most, and only every tenth pass
                    // while the failures continue.
                    if failures % 10 == 1 {
                        warn!(
                            errors = report.errors,
                            passes = failures,
                            "index pass had failures"
                        );
                    }
                } else {
                    failures = 0;
                }
                report_tx.send_replace(Some(report));
                tokio::select! {
                    _ = tokio::time::sleep(cfg.pass_interval) => {}
                    _ = stop_rx.changed() => return,
                }
            }
        });
        Indexer {
            stop,
            reports,
            join: Some(join),
        }
    }

    /// The latest pass report.
    pub fn reports(&self) -> watch::Receiver<Option<PassReport>> {
        self.reports.clone()
    }

    /// Stops the task and waits for it.
    pub async fn stop(mut self) {
        let _ = self.stop.send(true);
        if let Some(j) = self.join.take() {
            let _ = j.await;
        }
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months() {
        let (a, b) = month_bounds(secs(DateTime::new(2026, 2, 14, 5, 0, 0).unwrap()));
        assert_eq!(a, DateTime::date(2026, 2, 1).unwrap());
        assert_eq!(b, DateTime::new(2026, 2, 28, 23, 59, 59).unwrap());
    }
}
