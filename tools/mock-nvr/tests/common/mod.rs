//! Test helpers: a mock on a simulated clock, and a blocking connection to
//! it that lets simulated time pass while it waits.
//!
//! The mock runs on a [`SimRuntime`] (`mock_nvr::spawn_in`), so its frame
//! pacing, replay schedules, and download rates take simulated time; a
//! [`Link`] reads like a socket, moving the simulated clock forward a
//! millisecond at a time until bytes arrive, so a test sees each message at
//! the simulated time the mock sent it. [`TestNvr::tcp`] starts a real TCP
//! mock in real time instead, for the tests of the TCP server itself.

#![allow(dead_code)]

use mock_nvr::{Config, MockNvr};
use simtime::SimRuntime;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::time::Instant;

/// A running mock, on a simulated clock or (for TCP tests) in real time.
pub struct TestNvr {
    nvr: MockNvr,
    sim: Option<Arc<SimRuntime>>,
}

impl TestNvr {
    /// The mock on a simulated clock.
    pub fn sim(cfg: Config) -> TestNvr {
        let sim = Arc::new(SimRuntime::new());
        let nvr = mock_nvr::spawn_in(cfg, sim.handle());
        TestNvr {
            nvr,
            sim: Some(sim),
        }
    }

    /// The mock serving TCP in real time.
    pub fn tcp(cfg: Config) -> TestNvr {
        TestNvr {
            nvr: mock_nvr::spawn(cfg).unwrap(),
            sim: None,
        }
    }

    /// A new connection.
    pub fn connect(&self) -> Link {
        match &self.sim {
            Some(sim) => Link::Sim {
                sim: Arc::clone(sim),
                io: self.nvr.connect().unwrap(),
                timeout: None,
            },
            None => {
                let tcp = TcpStream::connect(self.nvr.addr()).unwrap();
                tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                Link::Tcp(tcp)
            }
        }
    }

    /// Now on the mock's clock.
    pub fn now(&self) -> Instant {
        match &self.sim {
            Some(sim) => sim.now(),
            None => Instant::now(),
        }
    }

    /// Lets `d` pass on the mock's clock.
    pub fn sleep(&self, d: Duration) {
        match &self.sim {
            Some(sim) => sim.advance(d),
            None => std::thread::sleep(d),
        }
    }

    /// Stops the mock.
    pub fn shutdown(self) {
        let TestNvr { nvr, sim } = self;
        nvr.shutdown();
        if let Some(sim) = sim {
            // Let the connections close.
            sim.advance(Duration::from_millis(1));
        }
    }
}

impl Deref for TestNvr {
    type Target = MockNvr;
    fn deref(&self) -> &MockNvr {
        &self.nvr
    }
}

/// A blocking connection to a [`TestNvr`].
pub enum Link {
    Tcp(TcpStream),
    Sim {
        sim: Arc<SimRuntime>,
        io: DuplexStream,
        /// Longest simulated wait for bytes; `None` waits up to a minute.
        timeout: Option<Duration>,
    },
}

impl Link {
    /// Now on the mock's clock.
    pub fn now(&self) -> Instant {
        match self {
            Link::Tcp(_) => Instant::now(),
            Link::Sim { sim, .. } => sim.now(),
        }
    }

    /// Lets `d` pass on the mock's clock.
    pub fn sleep(&self, d: Duration) {
        match self {
            Link::Tcp(_) => std::thread::sleep(d),
            Link::Sim { sim, .. } => sim.advance(d),
        }
    }

    /// As `TcpStream::set_read_timeout`; on a simulated clock, the time is
    /// simulated.
    pub fn set_read_timeout(&mut self, t: Option<Duration>) -> io::Result<()> {
        match self {
            Link::Tcp(s) => s.set_read_timeout(t),
            Link::Sim { timeout, .. } => {
                *timeout = t;
                Ok(())
            }
        }
    }
}

impl Read for Link {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Link::Tcp(s) => s.read(buf),
            Link::Sim { sim, io, timeout } => {
                // Whatever the mock does now (answering what was just
                // written) happens before the clock moves.
                sim.settle();
                let end = sim.now() + timeout.unwrap_or(Duration::from_secs(60));
                loop {
                    let ready = sim.block_on(std::future::poll_fn(|cx| {
                        let mut rb = ReadBuf::new(buf);
                        match Pin::new(&mut *io).poll_read(cx, &mut rb) {
                            Poll::Ready(Ok(())) => Poll::Ready(Some(Ok(rb.filled().len()))),
                            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
                            Poll::Pending => Poll::Ready(None),
                        }
                    }));
                    if let Some(r) = ready {
                        return r;
                    }
                    if sim.now() >= end {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    sim.advance(Duration::from_millis(1));
                }
            }
        }
    }
}

impl Write for Link {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Link::Tcp(s) => s.write(buf),
            Link::Sim { sim, io, .. } => {
                sim.block_on(io.write_all(buf))?;
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Link::Tcp(s) => s.flush(),
            Link::Sim { .. } => Ok(()),
        }
    }
}
