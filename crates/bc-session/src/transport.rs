//! How a session reaches its device.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

/// Opens transport connections to one device. The supervisor calls
/// [`Connector::connect`] once per (re)connect attempt, never concurrently.
pub trait Connector: Send + Sync + 'static {
    /// The byte stream type.
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Opens a new connection.
    fn connect(&self) -> Pin<Box<dyn Future<Output = io::Result<Self::Stream>> + Send + '_>>;
}

/// Plain TCP to a fixed address (the device's port is 9000 by default).
#[derive(Clone, Copy, Debug)]
pub struct TcpConnector {
    addr: SocketAddr,
}

impl TcpConnector {
    /// Connects to `addr`.
    pub fn new(addr: SocketAddr) -> TcpConnector {
        TcpConnector { addr }
    }
}

impl Connector for TcpConnector {
    type Stream = TcpStream;

    fn connect(&self) -> Pin<Box<dyn Future<Output = io::Result<TcpStream>> + Send + '_>> {
        Box::pin(async move {
            let s = TcpStream::connect(self.addr).await?;
            s.set_nodelay(true)?;
            Ok(s)
        })
    }
}

/// Opens each connection by calling a function: for example an in-memory
/// pipe to a test device (`mock_nvr::connect`), so a test can run a session
/// on a paused tokio clock without real sockets.
#[derive(Clone, Copy, Debug)]
pub struct FnConnector<F>(pub F);

impl<F, S> Connector for FnConnector<F>
where
    F: Fn() -> io::Result<S> + Send + Sync + 'static,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = S;

    fn connect(&self) -> Pin<Box<dyn Future<Output = io::Result<S>> + Send + '_>> {
        let r = (self.0)();
        Box::pin(async move { r })
    }
}
