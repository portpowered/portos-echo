use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::Sleep,
};

/// Independently bound response writes after TLS establishment.
pub(super) struct ResponseDeadline<T> {
    inner: T,
    deadline: Option<Pin<Box<Sleep>>>,
}

impl<T> ResponseDeadline<T> {
    pub fn new(inner: T) -> Self {
        Self { inner, deadline: None }
    }
    fn expired(&mut self, cx: &mut Context<'_>) -> bool {
        self.deadline
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(Duration::from_secs(2))))
            .as_mut()
            .poll(cx)
            .is_ready()
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for ResponseDeadline<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buffer: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for ResponseDeadline<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
        if self.expired(cx) {
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }
        Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.deadline.is_some() && self.expired(cx) {
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.deadline.is_some() && self.expired(cx) {
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
