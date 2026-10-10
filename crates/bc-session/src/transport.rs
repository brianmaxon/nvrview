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
