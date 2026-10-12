//! The event log, the snapshot cache, and feedback, under
//! `data_dir/alerts/` (or in memory without a data directory). Everything
//! is bounded: log files rotate by size and line count with a cap on the
//! number of files and a retention period; snapshots are capped by count,
//! total size, and age; feedback by count.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Most bytes of one log file before the next one starts.
pub(crate) const LOG_FILE_BYTES: u64 = 1 << 20;
/// Most lines of one log file.
pub(crate) const LOG_FILE_LINES: u32 = 4000;
/// Most log files kept (so at most 16 MiB of log).
pub(crate) const LOG_FILES: usize = 16;
/// Log entries older than this are dropped with their file (90 days).
pub(crate) const LOG_RETENTION_MS: i64 = 90 * 86_400_000;
/// Lines kept without a data directory.
const MEMORY_LINES: usize = 2000;

/// Largest snapshot accepted.
pub(crate) const SNAPSHOT_MAX_BYTES: usize = 1 << 20;
/// Most snapshots kept.
pub(crate) const SNAPSHOT_FILES: usize = 256;
/// Most bytes of snapshots kept.
pub(crate) const SNAPSHOT_TOTAL_BYTES: u64 = 64 << 20;
/// Snapshots older than this are dropped (7 days).
pub(crate) const SNAPSHOT_RETENTION_MS: i64 = 7 * 86_400_000;
/// Snapshots kept without a data directory.
const MEMORY_SNAPSHOTS: usize = 16;

/// Most feedback entries remembered.
pub(crate) const FEEDBACK_ENTRIES: usize = 4096;

/// The decision log: one JSON object per line, appended in time order.
pub(crate) struct AlertLog {
    dir: Option<PathBuf>,
    memory: VecDeque<String>,
    /// The file being appended to: sequence number, bytes, lines.
    current: Option<(u64, u64, u32)>,
}

fn log_name(seq: u64) -> String {
    format!("log-{seq:08}.jsonl")
}

fn log_seq(name: &str) -> Option<u64> {
    name.strip_prefix("log-")?
        .strip_suffix(".jsonl")?
        .parse()
        .ok()
}

impl AlertLog {
    pub(crate) fn open(dir: Option<PathBuf>) -> AlertLog {
        let dir = dir.map(|d| d.join("log"));
        if let Some(d) = &dir
            && let Err(e) = fs::create_dir_all(d)
        {
            warn!(error = %e, "alert log directory unusable; keeping the log in memory");
            return AlertLog {
                dir: None,
                memory: VecDeque::new(),
                current: None,
            };
        }
        let mut log = AlertLog {
            dir,
            memory: VecDeque::new(),
            current: None,
        };
        if let Some(&seq) = log.files().last()
            && let Some(p) = log.path(seq)
        {
            let (bytes, lines) = fs::read(&p)
                .map(|b| (b.len() as u64, b.iter().filter(|&&c| c == b'\n').count()))
                .unwrap_or((0, 0));
            log.current = Some((seq, bytes, u32::try_from(lines).unwrap_or(u32::MAX)));
        }
        log
    }

    fn path(&self, seq: u64) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(log_name(seq)))
    }

    /// The log files' sequence numbers, oldest first.
    fn files(&self) -> Vec<u64> {
        self.dir.as_deref().map(files_in).unwrap_or_default()
    }

    /// What a query reads, taken under the service's lock and read outside
    /// it.
    pub(crate) fn reader(&self) -> LogReader {
        match &self.dir {
            None => LogReader::Memory(self.memory.iter().rev().cloned().collect()),
            Some(d) => LogReader::Dir(d.clone()),
        }
    }

    /// Every line, newest first.
    #[cfg(test)]
    pub(crate) fn lines_newest_first(&self) -> Vec<String> {
        self.reader().lines_newest_first()
    }

    /// Appends one line (a JSON object without a newline). `now_ms` drives
    /// retention.
    pub(crate) fn append(&mut self, line: &str, now_ms: i64) {
        if self.dir.is_none() {
            if self.memory.len() >= MEMORY_LINES {
                self.memory.pop_front();
            }
            self.memory.push_back(line.to_owned());
            return;
        }
        let needed = line.len() as u64 + 1;
        let (seq, bytes, lines) = match self.current {
            Some((seq, bytes, lines))
                if bytes + needed <= LOG_FILE_BYTES && lines < LOG_FILE_LINES =>
            {
                (seq, bytes, lines)
            }
            Some((seq, ..)) => {
                self.prune(now_ms, true);
                (seq + 1, 0, 0)
            }
            None => (1, 0, 0),
        };
        let Some(p) = self.path(seq) else { return };
        let r = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .and_then(|mut f| {
                let mut buf = Vec::with_capacity(line.len() + 1);
                buf.extend_from_slice(line.as_bytes());
                buf.push(b'\n');
                f.write_all(&buf)
            });
        match r {
            Ok(()) => self.current = Some((seq, bytes + needed, lines + 1)),
            Err(e) => warn!(error = %e, "alert log write failed"),
        }
    }

    /// Drops the oldest files beyond the count (one slot kept free when a
    /// new file is about to start) and files whose newest entry is past
    /// retention.
    fn prune(&self, now_ms: i64, starting_new: bool) {
        let files = self.files();
        let keep = if starting_new {
            LOG_FILES - 1
        } else {
            LOG_FILES
        };
        let excess = files.len().saturating_sub(keep);
        for (i, &seq) in files.iter().enumerate() {
            let Some(p) = self.path(seq) else { continue };
            let old = i < excess || newest_ms(&p).is_some_and(|t| now_ms - t > LOG_RETENTION_MS);
            if old {
                let _ = fs::remove_file(&p);
            }
        }
    }

    /// The highest trigger and alert IDs in the newest entries, so a new
    /// run continues after them.
    pub(crate) fn highest_ids(&self) -> (u64, u64) {
        let (mut trig, mut alert) = (0, 0);
        let lines: Vec<String> = if self.dir.is_none() {
            self.memory.iter().cloned().collect()
        } else {
            let files = self.files();
            files
                .iter()
                .rev()
                .take(2)
                .filter_map(|&s| self.path(s))
                .filter_map(|p| fs::read_to_string(p).ok())
                .flat_map(|t| t.lines().map(str::to_owned).collect::<Vec<_>>())
                .collect()
        };
        for l in lines {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&l) else {
                continue;
            };
            let d = &v["decision"];
            trig = trig.max(d["trigger_id"].as_u64().unwrap_or(0));
            alert = alert.max(d["alert_id"].as_u64().unwrap_or(0));
        }
        (trig, alert)
    }
}

/// The log's lines, newest first.
pub(crate) type LogLines = Vec<String>;

/// A copy of the in-memory log, or where the files are.
pub(crate) enum LogReader {
    Memory(Vec<String>),
    Dir(PathBuf),
}

impl LogReader {
    pub(crate) fn lines_newest_first(self) -> LogLines {
        match self {
            LogReader::Memory(v) => v,
            LogReader::Dir(d) => {
                let mut out = Vec::new();
                for seq in files_in(&d).into_iter().rev() {
                    let Ok(text) = fs::read_to_string(d.join(log_name(seq))) else {
                        continue;
                    };
                    out.extend(
                        text.lines()
                            .rev()
                            .filter(|l| !l.trim().is_empty())
                            .map(str::to_owned),
                    );
                }
                out
            }
        }
    }
}

/// The log files' sequence numbers in `d`, oldest first.
fn files_in(d: &Path) -> Vec<u64> {
    let mut v: Vec<u64> = fs::read_dir(d)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| log_seq(&e.file_name().to_string_lossy()))
                .collect()
        })
        .unwrap_or_default();
    v.sort_unstable();
    v
}

/// The `logged_ms` of a log file's last line.
fn newest_ms(p: &Path) -> Option<i64> {
    let text = fs::read_to_string(p).ok()?;
    let last = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let v: serde_json::Value = serde_json::from_str(last).ok()?;
    v["logged_ms"].as_i64()
}

/// Snapshots by alert ID (the newest revision's picture).
pub(crate) struct SnapshotCache {
    dir: Option<PathBuf>,
    /// Alert ID to (bytes, put time); ordered so the oldest alerts go first.
    index: BTreeMap<u64, (u64, i64)>,
    memory: BTreeMap<u64, Vec<u8>>,
}

fn snap_name(id: u64) -> String {
    format!("{id}.jpg")
}

impl SnapshotCache {
    pub(crate) fn open(dir: Option<PathBuf>, now_ms: i64) -> SnapshotCache {
        let dir = dir.map(|d| d.join("snapshots"));
        let dir = dir.filter(|d| fs::create_dir_all(d).is_ok());
        let mut index = BTreeMap::new();
        if let Some(d) = &dir
            && let Ok(rd) = fs::read_dir(d)
        {
            for e in rd.filter_map(Result::ok) {
                let name = e.file_name().to_string_lossy().into_owned();
                let Some(id) = name
                    .strip_suffix(".jpg")
                    .and_then(|s| s.parse::<u64>().ok())
                else {
                    continue;
                };
                let Ok(meta) = e.metadata() else { continue };
                let at = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(now_ms, |d| i64::try_from(d.as_millis()).unwrap_or(now_ms));
                index.insert(id, (meta.len(), at));
            }
        }
        let mut c = SnapshotCache {
            dir,
            index,
            memory: BTreeMap::new(),
        };
        c.prune(now_ms);
        c
    }

    /// Stores the picture of an alert (replacing an earlier one).
    pub(crate) fn put(&mut self, id: u64, data: &[u8], now_ms: i64) -> bool {
        let Some(d) = &self.dir else {
            self.memory.insert(id, data.to_vec());
            while self.memory.len() > MEMORY_SNAPSHOTS {
                self.memory.pop_first();
            }
            return true;
        };
        if fs::write(d.join(snap_name(id)), data).is_err() {
            return false;
        }
        self.index.insert(id, (data.len() as u64, now_ms));
        self.prune(now_ms);
        true
    }

    pub(crate) fn get(&self, id: u64) -> Option<Vec<u8>> {
        match &self.dir {
            None => self.memory.get(&id).cloned(),
            Some(d) => {
                if !self.index.contains_key(&id) {
                    return None;
                }
                fs::read(d.join(snap_name(id))).ok()
            }
        }
    }

    pub(crate) fn has(&self, id: u64) -> bool {
        match &self.dir {
            None => self.memory.contains_key(&id),
            Some(_) => self.index.contains_key(&id),
        }
    }

    fn prune(&mut self, now_ms: i64) {
        let Some(d) = self.dir.clone() else { return };
        let mut drop_ids: Vec<u64> = self
            .index
            .iter()
            .filter(|(_, (_, at))| now_ms - at > SNAPSHOT_RETENTION_MS)
            .map(|(&id, _)| id)
            .collect();
        let mut total: u64 = self.index.values().map(|(b, _)| b).sum();
        let mut count = self.index.len();
        for (&id, &(bytes, _)) in &self.index {
            if count <= SNAPSHOT_FILES && total <= SNAPSHOT_TOTAL_BYTES {
                break;
            }
            if !drop_ids.contains(&id) {
                drop_ids.push(id);
            }
            count -= 1;
            total = total.saturating_sub(bytes);
        }
        for id in drop_ids {
            self.index.remove(&id);
            let _ = fs::remove_file(d.join(snap_name(id)));
        }
    }
}

/// Feedback by alert ID: the latest vote and reason.
pub(crate) struct FeedbackStore {
    path: Option<PathBuf>,
    entries: BTreeMap<u64, serde_json::Value>,
}

impl FeedbackStore {
    pub(crate) fn open(dir: Option<PathBuf>) -> FeedbackStore {
        let path = dir.map(|d| d.join("feedback.jsonl"));
        let mut entries = BTreeMap::new();
        if let Some(p) = &path
            && let Ok(text) = fs::read_to_string(p)
        {
            for l in text.lines() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(l)
                    && let Some(id) = v["alert_id"].as_u64()
                {
                    entries.insert(id, v);
                }
            }
        }
        let mut f = FeedbackStore { path, entries };
        f.bound(true);
        f
    }

    pub(crate) fn put(&mut self, alert_id: u64, entry: serde_json::Value) {
        let line = entry.to_string();
        self.entries.insert(alert_id, entry);
        let over = self.bound(false);
        if let Some(p) = &self.path {
            if over {
                self.rewrite();
            } else if let Err(e) = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .and_then(|mut f| writeln!(f, "{line}"))
            {
                warn!(error = %e, "feedback write failed");
            }
        }
    }

    pub(crate) fn get(&self, alert_id: u64) -> Option<&serde_json::Value> {
        self.entries.get(&alert_id)
    }

    /// Keeps the newest entries; rewrites the file when it holds many
    /// superseded lines. Returns whether entries were dropped.
    fn bound(&mut self, at_open: bool) -> bool {
        let mut over = false;
        while self.entries.len() > FEEDBACK_ENTRIES {
            self.entries.pop_first();
            over = true;
        }
        if at_open
            && let Some(p) = &self.path
            && fs::metadata(p).is_ok_and(|m| m.len() > (FEEDBACK_ENTRIES as u64) * 256)
        {
            self.rewrite();
        }
        over
    }

    fn rewrite(&self) {
        let Some(p) = &self.path else { return };
        let mut text = String::new();
        for v in self.entries.values() {
            text.push_str(&v.to_string());
            text.push('\n');
        }
        let tmp = p.with_extension("jsonl.tmp");
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_rotates_and_keeps_a_bounded_number_of_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = AlertLog::open(Some(dir.path().to_path_buf()));
        let filler = "x".repeat(4000);
        for i in 0..(LOG_FILES as u64 * 300) {
            let line = format!(
                r#"{{"logged_ms":{i},"decision":{{"trigger_id":{i},"alert_id":{i}}},"pad":"{filler}"}}"#
            );
            log.append(&line, i as i64);
        }
        let files = log.files();
        assert!(files.len() <= LOG_FILES, "{} files", files.len());
        let total: u64 = files
            .iter()
            .filter_map(|&s| log.path(s))
            .map(|p| fs::metadata(p).unwrap().len())
            .sum();
        assert!(total <= LOG_FILES as u64 * LOG_FILE_BYTES);
        let lines = log.lines_newest_first();
        assert!(lines[0].contains(&format!(r#""trigger_id":{}"#, LOG_FILES * 300 - 1)));
        let (t, a) = log.highest_ids();
        assert_eq!(
            (t, a),
            (LOG_FILES as u64 * 300 - 1, LOG_FILES as u64 * 300 - 1)
        );
        // A new run continues the same files.
        let log2 = AlertLog::open(Some(dir.path().to_path_buf()));
        assert_eq!(log2.lines_newest_first().len(), lines.len());
    }

    #[test]
    fn old_log_files_expire() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = AlertLog::open(Some(dir.path().to_path_buf()));
        let filler = "y".repeat(100_000);
        // Fill two files at time 0, then write far in the future.
        for i in 0..25 {
            log.append(&format!(r#"{{"logged_ms":0,"n":{i},"p":"{filler}"}}"#), 0);
        }
        assert!(log.files().len() >= 2);
        let later = LOG_RETENTION_MS + 1;
        for i in 0..11 {
            log.append(
                &format!(r#"{{"logged_ms":{later},"m":{i},"p":"{filler}"}}"#),
                later,
            );
        }
        assert!(
            log.lines_newest_first()
                .iter()
                .all(|l| l.contains(&format!(r#""logged_ms":{later}"#)) || !l.contains(r#""n":0,"#)),
        );
    }

    #[test]
    fn memory_log_is_bounded() {
        let mut log = AlertLog::open(None);
        for i in 0..(MEMORY_LINES + 10) {
            log.append(&format!(r#"{{"n":{i}}}"#), 0);
        }
        assert_eq!(log.lines_newest_first().len(), MEMORY_LINES);
    }

    #[test]
    fn snapshots_are_bounded_by_count_and_age() {
        let dir = tempfile::tempdir().unwrap();
        let now = 1_000_000_000_000;
        let mut c = SnapshotCache::open(Some(dir.path().to_path_buf()), now);
        for id in 0..(SNAPSHOT_FILES as u64 + 5) {
            assert!(c.put(id, b"jpeg", now));
        }
        assert!(!c.has(0));
        assert!(c.has(SNAPSHOT_FILES as u64 + 4));
        assert_eq!(c.get(SNAPSHOT_FILES as u64).as_deref(), Some(&b"jpeg"[..]));
        c.put(9999, b"later", now + SNAPSHOT_RETENTION_MS + 1);
        assert!(!c.has(10));
        assert!(c.has(9999));
    }

    #[test]
    fn feedback_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = FeedbackStore::open(Some(dir.path().to_path_buf()));
        f.put(4, serde_json::json!({"alert_id":4,"vote":"down"}));
        f.put(4, serde_json::json!({"alert_id":4,"vote":"up"}));
        let f2 = FeedbackStore::open(Some(dir.path().to_path_buf()));
        assert_eq!(f2.get(4).unwrap()["vote"], "up");
    }
}
