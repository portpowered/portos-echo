use std::{
    fs,
    future::Future,
    io::{self, ErrorKind},
    net::SocketAddr,
    os::unix::{fs::FileTypeExt, net::SocketAddr as UnixAddress},
    path::{Path, PathBuf},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpStream, UnixStream},
};

/// The Wyoming transport to try each time the reconnect supervisor dials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    tcp: SocketAddr,
    unix: Option<PathBuf>,
}

impl From<SocketAddr> for Endpoint {
    fn from(tcp: SocketAddr) -> Self {
        Self { tcp, unix: None }
    }
}

impl Endpoint {
    /// Prefer a filesystem socket, with rollback to the given loopback address.
    ///
    /// # Errors
    /// Rejects invalid socket paths and addresses outside IPv4 loopback.
    pub fn unix_first(path: PathBuf, tcp: SocketAddr) -> Result<Self, crate::Error> {
        // Path::file_name normalizes a terminal dot. The kernel sees the raw path.
        let name = path.as_os_str().as_encoded_bytes().rsplit(|byte| *byte == b'/').next();
        if tcp.ip() != std::net::Ipv4Addr::LOCALHOST
            || tcp.port() == 0
            || !path.is_absolute()
            || matches!(name, Some(b"" | b"." | b".."))
            || UnixAddress::from_pathname(&path).is_err()
        {
            return Err(crate::Error::InvalidOptions);
        }
        Ok(Self { tcp, unix: Some(path) })
    }

    /// The configured TCP address, also used as the rollback peer in local mode.
    #[must_use]
    pub fn tcp_addr(&self) -> SocketAddr {
        self.tcp
    }

    pub(crate) fn prefers_unix(&self) -> bool {
        self.unix.is_some()
    }

    pub(crate) async fn dial(self) -> io::Result<Box<dyn Stream>> {
        let tcp = || async { TcpStream::connect(self.tcp).await.map(stream) };
        if let Some(path) = self.unix {
            prefer_unix(async { connect_unix(&path).await.map(stream) }, tcp).await
        } else {
            tcp().await
        }
    }
}

pub(crate) trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

fn stream(socket: impl Stream + 'static) -> Box<dyn Stream> {
    Box::new(socket)
}

async fn connect_unix(path: &Path) -> io::Result<UnixStream> {
    // A rejected final entry or ancestor must not turn into a rollback dial.
    for parent in path.ancestors().skip(1) {
        match fs::symlink_metadata(parent) {
            Ok(metadata) if !metadata.is_dir() => return Err(unsafe_path()),
            Ok(_) => (),
            Err(error) if error.kind() == ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    if !fs::symlink_metadata(path)?.file_type().is_socket() {
        return Err(unsafe_path());
    }
    UnixStream::connect(path).await
}

fn unsafe_path() -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, "unsafe Wyoming socket path")
}

async fn prefer_unix<T, F, Fut>(unix: impl Future<Output = io::Result<T>>, tcp: F) -> io::Result<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = io::Result<T>>,
{
    match unix.await {
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => tcp().await,
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::ready;

    #[tokio::test]
    async fn socket_policy_rolls_back_only_on_missing_or_refused() {
        let tcp = || ready(Ok("tcp"));
        assert_eq!(prefer_unix(ready(Ok("unix")), tcp).await.unwrap(), "unix");
        for kind in [ErrorKind::NotFound, ErrorKind::ConnectionRefused] {
            assert_eq!(prefer_unix(ready(Err(kind.into())), tcp).await.unwrap(), "tcp");
        }
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidInput,
            ErrorKind::WouldBlock,
            ErrorKind::TimedOut,
        ] {
            assert_eq!(
                prefer_unix(ready(Err(kind.into())), tcp).await.unwrap_err().kind(),
                kind
            );
        }
    }
}
