//! A bounded, non-blocking hand-off to a writer thread.
//!
//! Producers never wait on disk I/O: a line is queued with `try_send`, and if
//! the queue is full it is dropped and counted.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tracing_subscriber::fmt::MakeWriter;

use crate::stats::LogStats;

enum Msg {
    Line(Vec<u8>),
    Flush(SyncSender<()>),
    Shutdown,
}

/// Cheap-to-clone producer handle; implements [`MakeWriter`] and [`Write`].
#[derive(Clone)]
pub struct NonBlockingWriter {
    tx: SyncSender<Msg>,
    stats: Arc<LogStats>,
}

impl NonBlockingWriter {
    /// Queues a line without blocking; drops and counts it if the queue is full.
    pub fn send(&self, bytes: &[u8]) {
        match self.tx.try_send(Msg::Line(bytes.to_vec())) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => self.stats.dropped_line(),
        }
    }
}

impl Write for NonBlockingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.send(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for NonBlockingWriter {
    type Writer = NonBlockingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Owns the writer thread; flushes and joins it on drop.
pub struct WriterThread {
    tx: SyncSender<Msg>,
    handle: Option<JoinHandle<()>>,
}

impl WriterThread {
    /// Spawns the writer thread. `tick` runs roughly every `tick_every` while
    /// the queue is idle (used to emit rate-limit summaries once a flood ends).
    ///
    /// # Errors
    /// Fails if the thread cannot be spawned.
    pub fn spawn<W: Write + Send + 'static>(
        mut sink: W,
        capacity: usize,
        stats: Arc<LogStats>,
        tick_every: Duration,
        tick: Box<dyn Fn() + Send>,
    ) -> io::Result<(NonBlockingWriter, WriterThread)> {
        let (tx, rx) = sync_channel::<Msg>(capacity.max(1));
        let handle = thread::Builder::new()
            .name("viewer-log-writer".into())
            .spawn(move || {
                run(&mut sink, &rx, tick_every, &*tick);
            })?;
        let producer = NonBlockingWriter {
            tx: tx.clone(),
            stats,
        };
        Ok((
            producer,
            WriterThread {
                tx,
                handle: Some(handle),
            },
        ))
    }

    /// Blocks until everything queued so far has been written and flushed.
    pub fn flush(&self) {
        let (ack_tx, ack_rx) = sync_channel(1);
        if self.tx.send(Msg::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv();
        }
    }
}

impl Drop for WriterThread {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run<W: Write>(sink: &mut W, rx: &Receiver<Msg>, tick_every: Duration, tick: &dyn Fn()) {
    loop {
        match rx.recv_timeout(tick_every) {
            Ok(Msg::Line(bytes)) => {
                let _ = sink.write_all(&bytes);
            }
            Ok(Msg::Flush(ack)) => {
                let _ = sink.flush();
                let _ = ack.send(());
            }
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => tick(),
        }
    }
    let _ = sink.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn delivers_in_order_and_flushes() {
        let buf = Buf::default();
        let stats = Arc::new(LogStats::default());
        let (w, t) = WriterThread::spawn(
            buf.clone(),
            16,
            stats,
            Duration::from_secs(60),
            Box::new(|| {}),
        )
        .unwrap();
        w.send(b"a");
        w.send(b"b");
        t.flush();
        assert_eq!(&*buf.0.lock().unwrap(), b"ab");
    }

    struct Blocked(Arc<Mutex<()>>);
    impl Write for Blocked {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            let _g = self.0.lock().unwrap();
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn full_queue_drops_and_counts_without_blocking() {
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap(); // writer thread will stall on its first write
        let stats = Arc::new(LogStats::default());
        let (w, t) = WriterThread::spawn(
            Blocked(gate.clone()),
            2,
            stats.clone(),
            Duration::from_secs(60),
            Box::new(|| {}),
        )
        .unwrap();
        for _ in 0..100 {
            w.send(b"x");
        }
        assert!(stats.snapshot().dropped_lines >= 97);
        drop(held);
        drop(t);
    }
}
