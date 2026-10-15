//! A deadline on writes to a client that has stopped reading.
//!
//! A response the client never reads fills the kernel's send buffer, and then the write stays pending for as long as
//! the client keeps the socket open, holding its connection and its place under `max_connections`. This wrapper gives
//! each blocked write `write_timeout` to finish. Reads pass straight through and never touch the deadline, so a quiet
//! event stream is never cut off: only a write the client refuses to take counts.

use std::future::Future;
use std::io::{self, IoSlice};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

/// A connection whose blocked writes fail with [`io::ErrorKind::TimedOut`] once they have been pending for `timeout`.
pub(super) struct WriteDeadline<S> {
    inner: S,
    timeout: Duration,
    /// One timer per connection. It is re-armed with [`Sleep::reset`] each time a write blocks, so a blocked write
    /// costs no allocation.
    deadline: Pin<Box<Sleep>>,
    /// Whether `deadline` is counting down for a write that is still pending.
    armed: bool,
}

impl<S> WriteDeadline<S> {
    /// Wraps `inner`; no deadline runs until a write blocks.
    pub(super) fn new(inner: S, timeout: Duration) -> Self {
        Self { inner, timeout, deadline: Box::pin(tokio::time::sleep(timeout)), armed: false }
    }
}

impl<S: AsyncWrite + Unpin> WriteDeadline<S> {
    /// Runs one write-side operation. A pending result arms the deadline, unless it is already running, and a deadline
    /// that has passed fails the operation. Any other result disarms it, so the next block gets a full `timeout`.
    fn guard<T>(
        &mut self,
        cx: &mut Context<'_>,
        operation: impl FnOnce(Pin<&mut S>, &mut Context<'_>) -> Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        match operation(Pin::new(&mut self.inner), cx) {
            Poll::Pending => {
                if !self.armed {
                    self.deadline.as_mut().reset(Instant::now() + self.timeout);
                    self.armed = true;
                }
                if self.deadline.as_mut().poll(cx).is_pending() {
                    return Poll::Pending;
                }
                tracing::warn!(
                    timeout = ?self.timeout,
                    "closing an MCP HTTP connection: a response write was still blocked after write_timeout",
                );
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("a response write was still blocked after write_timeout = {:?}", self.timeout),
                )))
            }
            ready @ Poll::Ready(_) => {
                self.armed = false;
                ready
            }
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for WriteDeadline<S> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for WriteDeadline<S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        self.get_mut().guard(cx, |stream, cx| stream.poll_write(cx, buf))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().guard(cx, AsyncWrite::poll_flush)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.get_mut().guard(cx, |stream, cx| stream.poll_write_vectored(cx, bufs))
    }

    /// Forwarded, so hyper still writes a response's pieces in one vectored call when the socket allows it. Without
    /// this, hyper flattens each response into one buffer that then stays large for the connection's lifetime.
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().guard(cx, AsyncWrite::poll_shutdown)
    }
}

#[cfg(test)]
mod tests {
    // Test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::io::IoSlice;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::WriteDeadline;

    /// A 16-byte write into an 8-byte pipe with nobody reading: the second half blocks until the deadline.
    #[tokio::test(start_paused = true)]
    async fn a_write_still_blocked_at_the_deadline_fails_with_timed_out_naming_the_limit() {
        let (near, _far) = tokio::io::duplex(8);
        let mut stream = WriteDeadline::new(near, Duration::from_secs(5));
        let error = stream.write_all(&[7; 16]).await.expect_err("the write times out");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("write_timeout = 5s"), "{error}");
    }

    /// A write that unblocks in time succeeds, and disarms the deadline: a later block gets a full timeout again,
    /// rather than failing at once on the old deadline (which has long passed by then).
    #[tokio::test(start_paused = true)]
    async fn a_write_that_unblocks_in_time_succeeds_and_disarms_the_deadline() {
        let (near, mut far) = tokio::io::duplex(8);
        let mut stream = WriteDeadline::new(near, Duration::from_secs(5));
        let reader = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let mut first = [0; 16];
            far.read_exact(&mut first).await.unwrap();
            tokio::time::sleep(Duration::from_secs(12)).await;
            let mut second = [0; 16];
            far.read_exact(&mut second).await.unwrap();
        });

        // Blocked from t = 0 to t = 2: well inside the 5 s deadline.
        stream.write_all(&[1; 16]).await.expect("unblocked at 2 s, before the deadline");
        // Idle until t = 12, long after the first deadline would have passed.
        tokio::time::sleep(Duration::from_secs(10)).await;
        // Blocked from t = 12 to t = 14: a fresh deadline, still inside its 5 s.
        stream.write_all(&[2; 16]).await.expect("a new block gets a full timeout");
        reader.await.unwrap();
    }

    /// Hyper writes a response's pieces with one vectored call when the connection allows it. The wrapper must keep
    /// that capability from the socket, and the deadline must still apply to a vectored write that blocks.
    #[tokio::test(start_paused = true)]
    async fn a_blocked_vectored_write_fails_at_the_deadline_too() {
        let (near, _far) = tokio::io::duplex(8);
        let mut stream = WriteDeadline::new(near, Duration::from_secs(5));
        let pieces = [IoSlice::new(&[1; 8]), IoSlice::new(&[2; 8])];
        assert_eq!(stream.write_vectored(&pieces).await.unwrap(), 8, "the pipe takes the first piece");
        let error = stream.write_vectored(&pieces).await.expect_err("the second write times out");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("write_timeout = 5s"), "{error}");
    }

    #[tokio::test]
    async fn the_wrapper_passes_on_the_sockets_vectored_writes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        assert!(client.is_write_vectored(), "the socket itself is vectored");
        assert!(WriteDeadline::new(client, Duration::from_secs(5)).is_write_vectored());
    }
}
