//! A rotating file writer bounded by file count and total size.
//!
//! Layout: the active file is `<stem>.log`; rotated files are `<stem>.1.log`
//! (newest) through `<stem>.<n>.log` (oldest). Unlike an ever-increasing index,
//! rotated files are renumbered and anything beyond the caps is deleted.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::stats::LogStats;

/// Limits for [`RotatingFileWriter`].
#[derive(Debug, Clone)]
pub struct RotationConfig {
    /// Directory holding the log files (created if missing).
    pub dir: PathBuf,
    /// File stem: the active file is `<stem>.log`.
    pub stem: String,
    /// Rotate when the active file would exceed this many bytes.
    pub max_file_bytes: u64,
    /// Maximum number of files, counting the active one.
    pub max_files: usize,
    /// Maximum total bytes across all files.
    pub max_total_bytes: u64,
}

impl RotationConfig {
    /// Largest size the active file may reach: the per-file cap, never more
    /// than the total cap. This guarantees `active + rotated <= total cap`.
    fn file_limit(&self) -> u64 {
        self.max_file_bytes.min(self.max_total_bytes).max(1)
    }

    fn max_rotated_files(&self) -> usize {
        self.max_files.max(1) - 1
    }

    fn max_rotated_bytes(&self) -> u64 {
        self.max_total_bytes.saturating_sub(self.file_limit())
    }

    fn active_path(&self) -> PathBuf {
        self.dir.join(format!("{}.log", self.stem))
    }

    fn rotated_path(&self, index: u32) -> PathBuf {
        self.dir.join(format!("{}.{}.log", self.stem, index))
    }
}

/// Writes to `<stem>.log`, rotating and deleting to stay within the caps.
///
/// Invariant: the bytes on disk for the stem never exceed `max_total_bytes` by
/// more than the size of one write. I/O errors never panic and never surface
/// from [`Write`]: the data is dropped and counted in [`LogStats`].
///
/// The active file is always closed before it is renamed or deleted, which
/// Windows requires.
pub struct RotatingFileWriter {
    cfg: RotationConfig,
    file: Option<File>,
    size: u64,
    stats: Arc<LogStats>,
}

impl RotatingFileWriter {
    /// Creates the directory, enforces the caps on files left by a previous
    /// run, and prepares to append to the existing active file if it fits.
    ///
    /// # Errors
    /// Fails only if the log directory cannot be created.
    pub fn new(cfg: RotationConfig, stats: Arc<LogStats>) -> io::Result<Self> {
        fs::create_dir_all(&cfg.dir)?;
        let mut w = Self {
            cfg,
            file: None,
            size: 0,
            stats,
        };
        if fs::metadata(w.cfg.active_path()).is_ok_and(|m| m.len() >= w.cfg.file_limit()) {
            w.rotate();
        } else {
            w.enforce();
        }
        Ok(w)
    }

    /// Path of the active log file.
    pub fn active_path(&self) -> PathBuf {
        self.cfg.active_path()
    }

    fn rotated_files(&self) -> Vec<(u32, PathBuf, u64)> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.cfg.dir) else {
            self.stats.io_error();
            return out;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(index) = parse_rotated(&name.to_string_lossy(), &self.cfg.stem) else {
                continue;
            };
            let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((index, entry.path(), len));
        }
        out.sort_by_key(|(i, _, _)| *i);
        out
    }

    fn remove(&self, path: &Path) {
        if fs::remove_file(path).is_err() && path.exists() {
            self.stats.io_error();
        }
    }

    fn rename(&self, from: &Path, to: &Path) -> bool {
        if fs::rename(from, to).is_err() {
            self.stats.io_error();
            return false;
        }
        true
    }

    /// Deletes rotated files beyond the count and byte budgets (oldest first)
    /// and renumbers the survivors to `1..=k`.
    fn enforce(&self) {
        let max_files = self.cfg.max_rotated_files();
        let max_bytes = self.cfg.max_rotated_bytes();
        let mut kept = 0usize;
        let mut sum = 0u64;
        for (index, path, len) in self.rotated_files() {
            if kept < max_files && sum + len <= max_bytes {
                kept += 1;
                sum += len;
                let target = self.cfg.rotated_path(kept as u32);
                if index as usize != kept {
                    self.rename(&path, &target);
                }
            } else {
                self.remove(&path);
            }
        }
    }

    /// Closes the active file, shifts rotated files up, and enforces the caps.
    fn rotate(&mut self) {
        self.file = None; // close first: Windows cannot rename or delete open files
        self.size = 0;
        let active = self.cfg.active_path();
        if active.exists() {
            let mut ok = true;
            for (index, path, _) in self.rotated_files().into_iter().rev() {
                ok &= self.rename(&path, &self.cfg.rotated_path(index.saturating_add(1)));
            }
            if !(ok && self.rename(&active, &self.cfg.rotated_path(1))) {
                // Could not rotate; make room by discarding the active file.
                self.remove(&active);
            }
        }
        self.enforce();
    }

    fn open(&mut self) -> io::Result<()> {
        if self.file.is_none() {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.cfg.active_path())?;
            self.size = file.metadata()?.len();
            self.file = Some(file);
        }
        Ok(())
    }

    fn try_write(&mut self, buf: &[u8]) -> io::Result<()> {
        let len = buf.len() as u64;
        if self.size > 0 && self.size + len > self.cfg.file_limit() {
            self.rotate();
        }
        self.open()?;
        let Some(file) = self.file.as_mut() else {
            return Err(io::Error::other("log file not open"));
        };
        file.write_all(buf)?;
        self.size += len;
        Ok(())
    }
}

impl Write for RotatingFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.try_write(buf).is_err() {
            self.stats.io_error();
            self.stats.dropped_bytes(buf.len());
            self.file = None; // reopen (and re-measure) on the next write
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.as_mut()
            && file.flush().is_err()
        {
            self.stats.io_error();
        }
        Ok(())
    }
}

fn parse_rotated(name: &str, stem: &str) -> Option<u32> {
    let rest = name.strip_prefix(stem)?.strip_prefix('.')?;
    let digits = rest.strip_suffix(".log")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok().filter(|i| *i >= 1)
}

/// Total bytes on disk for a stem (active plus rotated files).
pub fn total_bytes_on_disk(dir: &Path, stem: &str) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name == format!("{stem}.log") || parse_rotated(&name, stem).is_some()
        })
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &Path, file: u64, files: usize, total: u64) -> RotationConfig {
        RotationConfig {
            dir: dir.to_path_buf(),
            stem: "app".into(),
            max_file_bytes: file,
            max_files: files,
            max_total_bytes: total,
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn writer(c: RotationConfig) -> (RotatingFileWriter, Arc<LogStats>) {
        let stats = Arc::new(LogStats::default());
        (RotatingFileWriter::new(c, stats.clone()).unwrap(), stats)
    }

    #[test]
    fn rotates_and_shifts() {
        let dir = tempfile::tempdir().unwrap();
        let (mut w, _) = writer(cfg(dir.path(), 10, 4, 1000));
        for i in 0..3 {
            w.write_all(format!("line-{i}....").as_bytes()).unwrap(); // 10 bytes each
        }
        assert_eq!(names(dir.path()), ["app.1.log", "app.2.log", "app.log"]);
        assert_eq!(
            fs::read_to_string(dir.path().join("app.log")).unwrap(),
            "line-2...."
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("app.1.log")).unwrap(),
            "line-1...."
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("app.2.log")).unwrap(),
            "line-0...."
        );
    }

    #[test]
    fn file_count_cap_holds() {
        let dir = tempfile::tempdir().unwrap();
        let (mut w, stats) = writer(cfg(dir.path(), 10, 3, 1000));
        for _ in 0..50 {
            w.write_all(b"0123456789").unwrap();
        }
        assert_eq!(names(dir.path()), ["app.1.log", "app.2.log", "app.log"]);
        assert_eq!(stats.snapshot().io_errors, 0);
    }

    #[test]
    fn total_cap_holds() {
        let dir = tempfile::tempdir().unwrap();
        // Per-file 10, up to 100 files, but only 35 bytes in total.
        let (mut w, _) = writer(cfg(dir.path(), 10, 100, 35));
        for _ in 0..200 {
            w.write_all(b"abcde").unwrap();
            assert!(total_bytes_on_disk(dir.path(), "app") <= 35);
        }
        assert!(names(dir.path()).len() <= 4);
    }

    #[test]
    fn oversized_single_write_stays_within_one_write() {
        let dir = tempfile::tempdir().unwrap();
        let (mut w, _) = writer(cfg(dir.path(), 10, 3, 30));
        w.write_all(&[b'x'; 100]).unwrap();
        assert!(total_bytes_on_disk(dir.path(), "app") <= 30 + 100);
        w.write_all(b"y").unwrap();
        assert!(total_bytes_on_disk(dir.path(), "app") <= 30 + 1);
    }

    #[test]
    fn single_file_mode() {
        let dir = tempfile::tempdir().unwrap();
        let (mut w, _) = writer(cfg(dir.path(), 10, 1, 1000));
        for _ in 0..10 {
            w.write_all(b"0123456789").unwrap();
        }
        assert_eq!(names(dir.path()), ["app.log"]);
    }

    #[test]
    fn startup_cleanup_enforces_caps() {
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=8 {
            fs::write(dir.path().join(format!("app.{i}.log")), vec![b'x'; 100]).unwrap();
        }
        fs::write(dir.path().join("app.log"), vec![b'x'; 5000]).unwrap(); // oversized active
        fs::write(dir.path().join("other.1.log"), b"keep").unwrap();
        fs::write(dir.path().join("app.notes.log"), b"keep").unwrap();
        let (_w, _) = writer(cfg(dir.path(), 100, 4, 400));
        let n = names(dir.path());
        assert_eq!(
            n,
            [
                "app.1.log",
                "app.2.log",
                "app.3.log",
                "app.notes.log",
                "other.1.log"
            ]
        );
        assert!(total_bytes_on_disk(dir.path(), "app") <= 400);
        // The oversized active file was dropped for the byte budget without
        // evicting the older files that still fit.
        assert_eq!(
            fs::metadata(dir.path().join("app.1.log")).unwrap().len(),
            100
        );
    }

    #[test]
    fn startup_appends_to_small_active_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("app.log"), b"old\n").unwrap();
        let (mut w, _) = writer(cfg(dir.path(), 100, 3, 300));
        w.write_all(b"new\n").unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("app.log")).unwrap(),
            "old\nnew\n"
        );
    }

    #[test]
    fn startup_renumbers_gaps() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("app.3.log"), b"a").unwrap();
        fs::write(dir.path().join("app.9.log"), b"b").unwrap();
        let (_w, _) = writer(cfg(dir.path(), 100, 5, 500));
        assert_eq!(names(dir.path()), ["app.1.log", "app.2.log"]);
        assert_eq!(
            fs::read_to_string(dir.path().join("app.1.log")).unwrap(),
            "a"
        );
    }

    #[test]
    fn io_errors_are_counted_not_raised() {
        let dir = tempfile::tempdir().unwrap();
        let (mut w, stats) = writer(cfg(dir.path(), 100, 3, 300));
        w.write_all(b"ok").unwrap();
        // Make the active path unopenable: replace it with a directory.
        w.file = None;
        fs::remove_file(dir.path().join("app.log")).unwrap();
        fs::create_dir(dir.path().join("app.log")).unwrap();
        assert_eq!(w.write(b"lost").unwrap(), 4);
        w.flush().unwrap();
        let s = stats.snapshot();
        assert!(s.io_errors >= 1);
        assert_eq!(s.dropped_bytes, 4);
    }

    #[test]
    fn parse_rotated_names() {
        assert_eq!(parse_rotated("app.1.log", "app"), Some(1));
        assert_eq!(parse_rotated("app.12.log", "app"), Some(12));
        assert_eq!(parse_rotated("app.0.log", "app"), None);
        assert_eq!(parse_rotated("app.log", "app"), None);
        assert_eq!(parse_rotated("app.x.log", "app"), None);
        assert_eq!(parse_rotated("app2.1.log", "app"), None);
    }
}
