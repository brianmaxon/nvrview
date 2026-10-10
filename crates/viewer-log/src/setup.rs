//! One-call logging setup.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::nonblocking::{NonBlockingWriter, WriterThread};
use crate::ratelimit::{Clock, RateLimitConfig, RateLimitLayer, SummarySink, SystemClock};
use crate::rotate::{RotatingFileWriter, RotationConfig};
use crate::stats::{LogStats, LogStatsSnapshot};

/// Default per-file cap: 5 MB.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Default file count cap (including the active file).
pub const DEFAULT_MAX_FILES: usize = 5;
/// Default total cap: 25 MB.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 25 * 1024 * 1024;
/// Default writer queue length, in lines.
pub const DEFAULT_QUEUE_LINES: usize = 4096;

/// Logging configuration. Start from [`LogConfig::new`].
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Directory for log files.
    pub dir: PathBuf,
    /// File stem; the active file is `<stem>.log`.
    pub stem: String,
    /// Rotate when a file would exceed this many bytes.
    pub max_file_bytes: u64,
    /// Maximum files kept, counting the active one.
    pub max_files: usize,
    /// Maximum bytes across all files.
    pub max_total_bytes: u64,
    /// Most verbose level recorded.
    pub level: LevelFilter,
    /// Also write to stderr (default: only in debug builds).
    pub stderr: bool,
    /// Flood suppression settings.
    pub rate_limit: RateLimitConfig,
    /// Writer queue length in lines; lines beyond it are dropped and counted.
    pub queue_lines: usize,
}

impl LogConfig {
    /// Defaults: 5 MB per file, 5 files, 25 MB total, info level, stderr in
    /// debug builds, 4096-line queue, default rate limit.
    pub fn new(dir: impl Into<PathBuf>, stem: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            stem: stem.into(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_files: DEFAULT_MAX_FILES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            level: LevelFilter::INFO,
            stderr: cfg!(debug_assertions),
            rate_limit: RateLimitConfig::default(),
            queue_lines: DEFAULT_QUEUE_LINES,
        }
    }

    fn rotation(&self) -> RotationConfig {
        RotationConfig {
            dir: self.dir.clone(),
            stem: self.stem.clone(),
            max_file_bytes: self.max_file_bytes,
            max_files: self.max_files,
            max_total_bytes: self.max_total_bytes,
        }
    }
}

/// Why [`init`] failed.
#[derive(Debug)]
pub enum InitError {
    /// The log directory or writer thread could not be created.
    Io(io::Error),
    /// A global `tracing` subscriber is already installed.
    AlreadyInitialized,
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot set up log files: {e}"),
            Self::AlreadyInitialized => {
                f.write_str("a global tracing subscriber is already installed")
            }
        }
    }
}

impl std::error::Error for InitError {}

impl From<io::Error> for InitError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Keeps logging alive. Dropping it emits pending summaries, drains the queue,
/// and flushes the file.
pub struct LogGuard {
    limiter: RateLimitLayer,
    thread: Option<WriterThread>,
    stats: Arc<LogStats>,
}

impl LogGuard {
    /// Counters for I/O errors and dropped lines.
    pub fn stats(&self) -> LogStatsSnapshot {
        self.stats.snapshot()
    }

    /// Emits pending rate-limit summaries and waits for queued lines to reach disk.
    pub fn flush(&self) {
        self.limiter.flush_all();
        if let Some(t) = &self.thread {
            t.flush();
        }
    }
}

impl Drop for LogGuard {
    fn drop(&mut self) {
        self.limiter.flush_all();
        self.thread.take(); // joins the writer thread after draining
    }
}

/// The pieces of the logging stack, for callers (and tests) that install the
/// subscriber themselves.
pub struct Pipeline {
    /// Producer handle usable as a `MakeWriter`.
    pub writer: NonBlockingWriter,
    /// The rate limiter layer.
    pub limiter: RateLimitLayer,
    /// Guard that owns the writer thread.
    pub guard: LogGuard,
}

/// Builds the writer thread and rate limiter without installing a subscriber.
///
/// # Errors
/// Fails if the directory cannot be created or the thread cannot spawn.
pub fn build_pipeline(cfg: &LogConfig) -> Result<Pipeline, io::Error> {
    build_pipeline_with_clock(cfg, SystemClock::new())
}

/// Like [`build_pipeline`] with an injected clock for the rate limiter.
///
/// # Errors
/// Fails if the directory cannot be created or the thread cannot spawn.
pub fn build_pipeline_with_clock(
    cfg: &LogConfig,
    clock: impl Clock,
) -> Result<Pipeline, io::Error> {
    let stats = Arc::new(LogStats::default());
    let file = RotatingFileWriter::new(cfg.rotation(), stats.clone())?;
    // The limiter needs the producer handle (for summaries) and the writer
    // thread needs the limiter (for idle ticks); break the cycle with a slot.
    let slot: Arc<std::sync::OnceLock<RateLimitLayer>> = Arc::default();
    let tick_slot = slot.clone();
    let (writer, thread) = WriterThread::spawn(
        file,
        cfg.queue_lines,
        stats.clone(),
        Duration::from_secs(1),
        Box::new(move || {
            if let Some(l) = tick_slot.get() {
                l.flush_idle();
            }
        }),
    )?;
    let w2 = writer.clone();
    let sink: SummarySink = Arc::new(move |line| w2.send(line.as_bytes()));
    let limiter = RateLimitLayer::with_clock(cfg.rate_limit.clone(), clock, sink);
    let _ = slot.set(limiter.clone());
    let guard = LogGuard {
        limiter: limiter.clone(),
        thread: Some(thread),
        stats,
    };
    Ok(Pipeline {
        writer,
        limiter,
        guard,
    })
}

/// Installs the global `tracing` subscriber: rate limiter, then a plain-text
/// formatter writing through a bounded queue to a size-capped rotating file
/// (and to stderr if configured).
///
/// Keep the returned guard alive for the life of the program.
///
/// # Errors
/// [`InitError::Io`] if the log directory cannot be created;
/// [`InitError::AlreadyInitialized`] if a global subscriber exists.
pub fn init(cfg: &LogConfig) -> Result<LogGuard, InitError> {
    let Pipeline {
        writer,
        limiter,
        guard,
    } = build_pipeline(cfg)?;
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(writer)
        .with_filter(cfg.level);
    let stderr_layer = cfg.stderr.then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_filter(cfg.level)
    });
    tracing_subscriber::registry()
        .with(limiter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(|_| InitError::AlreadyInitialized)?;
    Ok(guard)
}
