//! Scripted bodies: [`StreamScript`] and its [`Chunk`]s, and the bounded, cancellable reader that plays them back.

use std::io::{self, Read};
use std::sync::Arc;

use omuzaki_http_api::{Cancel, HttpError, HttpResponse, HttpStream};

/// How many times a waiting call polls its cancel handle before it gives up with [`HttpError::Timeout`]. Counting
/// polls instead of reading the clock keeps the fake deterministic, and it never sleeps.
pub(crate) const POLL_LIMIT: u32 = 1_000;

/// A streamed answer: a status, headers and chunks that arrive in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamScript {
    /// The status code.
    pub status: u16,
    /// The headers, in order; a name may repeat.
    pub headers: Vec<(String, String)>,
    /// The body, one entry per arrival. A read never returns bytes from two chunks, so a test sees the boundaries.
    pub chunks: Vec<Chunk>,
}

/// One arrival in a streamed body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chunk {
    /// Bytes that arrive together.
    Data(Vec<u8>),
    /// The connection goes quiet: a read waits as a hang does, then gives up with a cancel or a timeout.
    Stall,
}

/// Polls `cancel` up to [`POLL_LIMIT`] times: [`HttpError::Cancelled`] as soon as it reads true, otherwise
/// [`HttpError::Timeout`].
pub(crate) fn poll_until_cancelled(cancel: &dyn Cancel) -> HttpError {
    for _ in 0..POLL_LIMIT {
        if cancel.is_cancelled() {
            return HttpError::Cancelled;
        }
    }
    HttpError::Timeout
}

/// True when `len` bytes would be more than `limit`.
pub(crate) fn over_limit(len: usize, limit: u64) -> bool {
    len as u64 > limit
}

/// `send` over a stream: joins the data chunks into one body. A stall waits as a hang does, and a body that would pass
/// `limit` is [`HttpError::TooLarge`], the same rule a streamed read follows.
pub(crate) fn join_chunks(script: StreamScript, limit: u64, cancel: &dyn Cancel) -> Result<HttpResponse, HttpError> {
    let mut body = Vec::new();
    for chunk in script.chunks {
        match chunk {
            Chunk::Data(bytes) => {
                if over_limit(body.len() + bytes.len(), limit) {
                    return Err(HttpError::TooLarge { limit });
                }
                body.extend_from_slice(&bytes);
            }
            Chunk::Stall => return Err(poll_until_cancelled(cancel)),
        }
    }
    Ok(HttpResponse { status: script.status, headers: script.headers, body })
}

/// The stream `open` returns: `chunks` read in order, bounded by `limit` and watching `cancel`.
pub(crate) fn open_body(
    status: u16,
    headers: Vec<(String, String)>,
    chunks: Vec<Chunk>,
    limit: u64,
    cancel: Arc<dyn Cancel>,
) -> HttpStream {
    let body = ScriptedBody { chunks, next: 0, offset: 0, delivered: 0, limit, cancel };
    HttpStream { status, headers, body: Box::new(body) }
}

/// The reader behind an [`HttpStream`]. `next` and `offset` are the read position: the chunk being served and how
/// many of its bytes went out.
struct ScriptedBody {
    chunks: Vec<Chunk>,
    next: usize,
    offset: usize,
    delivered: u64,
    limit: u64,
    cancel: Arc<dyn Cancel>,
}

impl Read for ScriptedBody {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Checked before anything else, as the real transport does: a cancelled body stops even mid-stream.
        if self.cancel.is_cancelled() {
            return Err(HttpError::Cancelled.into());
        }
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.chunks.get(self.next) {
                None => return Ok(0),
                Some(Chunk::Stall) => return Err(poll_until_cancelled(self.cancel.as_ref()).into()),
                // A chunk that is fully read (or empty) moves the position on to the next arrival.
                Some(Chunk::Data(bytes)) if self.offset == bytes.len() => {
                    self.next += 1;
                    self.offset = 0;
                }
                Some(Chunk::Data(bytes)) => {
                    // The read that would pass `limit` fails only when there is more data, so a body of exactly
                    // `limit` bytes still ends cleanly.
                    if self.delivered >= self.limit {
                        return Err(HttpError::TooLarge { limit: self.limit }.into());
                    }
                    let allowance = usize::try_from(self.limit - self.delivered).unwrap_or(usize::MAX);
                    let n = buf.len().min(bytes.len() - self.offset).min(allowance);
                    buf[..n].copy_from_slice(&bytes[self.offset..self.offset + n]);
                    self.offset += n;
                    self.delivered += n as u64;
                    return Ok(n);
                }
            }
        }
    }
}
