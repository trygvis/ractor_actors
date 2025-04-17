use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, ReadHalf, WriteHalf};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

/// A network port
pub type NetworkPort = u16;

/// Incoming encryption mode
#[derive(Clone)]
pub enum IncomingEncryptionMode {
    /// Accept sockets raw, with no encryption
    Raw,
    /// Accept sockets and establish a secure connection
    Tls(tokio_rustls::TlsAcceptor),
}

/// A network data stream which can either be
/// 1. unencrypted
/// 2. encrypted and the server-side of the session
/// 3. encrypted and the client-side of the session
pub enum NetworkStream {
    /// Unencrypted session
    Raw {
        /// The peer's address
        peer_addr: SocketAddr,
        /// The local address
        local_addr: SocketAddr,
        /// The stream
        stream: TcpStream,
    },
    /// Encrypted as the server-side of the session
    TlsServer {
        /// The peer's address
        peer_addr: SocketAddr,
        /// The local address
        local_addr: SocketAddr,
        /// The stream
        stream: tokio_rustls::server::TlsStream<TcpStream>,
    },
    /// Encrypted as the client-side of the session
    TlsClient {
        /// The peer's address
        peer_addr: SocketAddr,
        /// The local address
        local_addr: SocketAddr,
        /// The stream
        stream: tokio_rustls::client::TlsStream<TcpStream>,
    },
}

pub struct NetworkStreamInfo {
    pub peer_addr: SocketAddr,
    pub local_addr: SocketAddr,
}

impl NetworkStream {
    pub fn info(&self) -> NetworkStreamInfo {
        NetworkStreamInfo {
            peer_addr: self.peer_addr(),
            local_addr: self.local_addr(),
        }
    }

    /// Retrieve the peer (other) socket address
    pub fn peer_addr(&self) -> SocketAddr {
        match self {
            Self::Raw { peer_addr, .. } => *peer_addr,
            Self::TlsServer { peer_addr, .. } => *peer_addr,
            Self::TlsClient { peer_addr, .. } => *peer_addr,
        }
    }

    /// Retrieve the local socket address
    pub fn local_addr(&self) -> SocketAddr {
        match self {
            Self::Raw { local_addr, .. } => *local_addr,
            Self::TlsServer { local_addr, .. } => *local_addr,
            Self::TlsClient { local_addr, .. } => *local_addr,
        }
    }

    pub fn into_split(self) -> (ReaderHalf, WriterHalf) {
        match self {
            NetworkStream::Raw { stream, .. } => {
                let (read, write) = stream.into_split();
                (ReaderHalf::Regular(read), WriterHalf::Regular(write))
            }
            NetworkStream::TlsClient { stream, .. } => {
                let (read_half, write_half) = tokio::io::split(stream);
                (
                    ReaderHalf::ClientTls(read_half),
                    WriterHalf::ClientTls(write_half),
                )
            }
            NetworkStream::TlsServer { stream, .. } => {
                let (read_half, write_half) = tokio::io::split(stream);
                (
                    ReaderHalf::ServerTls(read_half),
                    WriterHalf::ServerTls(write_half),
                )
            }
        }
    }
}

pub enum ReaderHalf {
    ServerTls(ReadHalf<tokio_rustls::server::TlsStream<TcpStream>>),
    ClientTls(ReadHalf<tokio_rustls::client::TlsStream<TcpStream>>),
    Regular(OwnedReadHalf),
}

impl ReaderHalf {
    /// Helper method to read exactly `len` bytes from the stream into a pre-allocated buffer
    /// of bytes
    pub async fn read_n_bytes(&mut self, len: usize) -> Result<Vec<u8>, tokio::io::Error> {
        let mut buf = vec![0u8; len];
        let mut c_len = 0;
        if let ReaderHalf::Regular(stream) = self {
            stream.readable().await?;
        }

        while c_len < len {
            let n = match self {
                Self::ServerTls(t) => t.read(&mut buf[c_len..]).await?,
                Self::ClientTls(t) => t.read(&mut buf[c_len..]).await?,
                Self::Regular(t) => t.read(&mut buf[c_len..]).await?,
            };
            if n == 0 {
                // EOF
                return Err(tokio::io::Error::new(
                    tokio::io::ErrorKind::UnexpectedEof,
                    "EOF",
                ));
            }
            c_len += n;
        }
        Ok(buf)
    }

    pub async fn read_u64(&mut self) -> tokio::io::Result<u64> {
        match self {
            Self::ServerTls(t) => t.read_u64().await,
            Self::ClientTls(t) => t.read_u64().await,
            Self::Regular(t) => t.read_u64().await,
        }
    }
}

// =========================== WriterHalf =========================== //

pub enum WriterHalf {
    ServerTls(WriteHalf<tokio_rustls::server::TlsStream<TcpStream>>),
    ClientTls(WriteHalf<tokio_rustls::client::TlsStream<TcpStream>>),
    Regular(OwnedWriteHalf),
}

impl WriterHalf {
    pub async fn write_u64(&mut self, n: u64) -> tokio::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        match self {
            Self::ServerTls(t) => t.write_u64(n).await,
            Self::ClientTls(t) => t.write_u64(n).await,
            Self::Regular(t) => t.write_u64(n).await,
        }
    }

    pub async fn write_all(&mut self, data: &[u8]) -> tokio::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        match self {
            Self::ServerTls(t) => t.write_all(data).await,
            Self::ClientTls(t) => t.write_all(data).await,
            Self::Regular(t) => t.write_all(data).await,
        }
    }

    pub async fn flush(&mut self) -> tokio::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        match self {
            Self::ServerTls(t) => t.flush().await,
            Self::ClientTls(t) => t.flush().await,
            Self::Regular(t) => t.flush().await,
        }
    }
}
