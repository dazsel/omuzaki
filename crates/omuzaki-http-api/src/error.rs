//! [`HttpError`] and its carriage through a stream read's [`std::io::Error`].

use std::io;

/// Why an HTTP call failed. A non-2xx status is never one of these: it is an [`HttpResponse`](crate::HttpResponse).
///
/// The `Display` text is written for the person who sees it, saying what to do next.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    /// The caller's [`Cancel`](crate::Cancel) handle turned true.
    #[error("the request was cancelled")]
    Cancelled,
    /// The server did not answer within `timeout_s`.
    #[error("the server did not answer in time; try again, or raise the timeout")]
    Timeout,
    /// The connection, TLS or protocol failed; the text says how.
    #[error("the network call failed ({0}); check the address and your connection")]
    Network(String),
    /// The answer is larger than `max_response`.
    #[error("the answer is larger than {limit} bytes; ask for less or raise the limit")]
    TooLarge {
        /// The `max_response` that was exceeded, in bytes.
        limit: u64,
    },
    /// The transport declined the request without sending it (for example a scheme it does not speak).
    #[error("refused: {0}")]
    Refused(String),
}

impl From<HttpError> for io::Error {
    /// Kind [`io::ErrorKind::Other`] with the `HttpError` as the inner error, never `Interrupted`, which std's
    /// `read_to_end` retries forever.
    fn from(e: HttpError) -> Self {
        Self::other(e)
    }
}

impl HttpError {
    /// The `HttpError` inside a stream read's `io::Error`, if it is one.
    #[must_use]
    pub fn of_io(e: &io::Error) -> Option<&Self> {
        e.get_ref()?.downcast_ref::<Self>()
    }
}
