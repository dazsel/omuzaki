//! Contract for outgoing HTTP (connectors design D13, ADR 0030 item 13).
//!
//! Implementation crates may not depend on each other, so a trait in an API crate is the only way the connector
//! adapters, `net.http` and the LLM providers share one HTTP client. This crate is that trait and its data:
//!
//! - **[`HttpTransport`]**: [`send`](HttpTransport::send) returns the whole answer, [`open`](HttpTransport::open)
//!   returns it as an [`HttpStream`] (server-sent events). Both are required, so no transport can forget streaming.
//! - **[`HttpRequest`], [`HttpResponse`], [`HttpBody`], [`Part`], [`Method`]**: plain data.
//! - **[`HttpError`]**: the five ways a call fails, with `Display` text that says what to do next.
//! - **[`Cancel`], [`NeverCancel`], [`CancelFlag`]**: the cooperative cancel handle and the two copies every mock
//!   and adapter shares.
//!
//! Which hosts a caller may reach is the caller's rule, never the transport's. The real client is `omuzaki-http`, the
//! fake is `omuzaki-http-mock`; this crate has no network stack.
#![deny(missing_docs)]

mod cancel;
mod error;

use std::io::Read;
use std::sync::Arc;

pub use cancel::{Cancel, CancelFlag, NeverCancel};
pub use error::HttpError;
use serde_json::Value;

/// An HTTP method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `PUT`.
    Put,
    /// `PATCH`.
    Patch,
    /// `DELETE`.
    Delete,
}

/// A request body.
#[derive(Clone, Debug, PartialEq)]
pub enum HttpBody {
    /// No body.
    Empty,
    /// Raw bytes sent with the given `Content-Type`.
    Bytes {
        /// The `Content-Type` header value.
        content_type: String,
        /// The body bytes.
        data: Vec<u8>,
    },
    /// A JSON document, sent as `application/json`.
    Json(Value),
    /// A `multipart/form-data` body, one entry per part, in order.
    Multipart(Vec<Part>),
}

/// One part of a [`HttpBody::Multipart`] body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// The form field name.
    pub name: String,
    /// The file name, for a file part.
    pub file_name: Option<String>,
    /// The part's `Content-Type`, if it has one.
    pub content_type: Option<String>,
    /// The part's bytes.
    pub data: Vec<u8>,
}

/// One outgoing request.
#[derive(Clone, Debug, PartialEq)]
pub struct HttpRequest {
    /// The method.
    pub method: Method,
    /// The absolute URL.
    pub url: String,
    /// Headers, in order; a name may repeat.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: HttpBody,
    /// Seconds the server may take, end to end, before [`HttpError::Timeout`].
    pub timeout_s: u32,
    /// The most response-body bytes to accept before [`HttpError::TooLarge`].
    pub max_response: u64,
}

/// A whole answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// Headers, in order; a name may repeat.
    pub headers: Vec<(String, String)>,
    /// The body, at most `max_response` bytes.
    pub body: Vec<u8>,
}

/// A streaming answer (server-sent events): the body is read as it arrives.
///
/// It owns the cancel handle it was opened with, so a stalled body still stops. A read of [`body`](Self::body) fails
/// in three required ways, each an [`std::io::Error`] of kind `Other` built with `From<HttpError>` (read the
/// [`HttpError`] back with [`HttpError::of_io`]); never `ErrorKind::Interrupted`, which std's `read_to_end` retries
/// forever:
///
/// - [`HttpError::Cancelled`] within 50 ms of the handle's `is_cancelled()` turning true; the connection is dropped.
/// - [`HttpError::Timeout`] after `timeout_s`.
/// - [`HttpError::TooLarge`] `{ limit }` on the read that would pass `max_response`, after the bytes up to the limit
///   were delivered.
///
/// A connection or protocol failure while the body is being read surfaces the same way, as an `io::Error` of kind
/// `Other` whose inner error is [`HttpError::Network`], and [`HttpError::of_io`] recovers it.
pub struct HttpStream {
    /// The status code.
    pub status: u16,
    /// Headers, in order; a name may repeat.
    pub headers: Vec<(String, String)>,
    /// The body, read as it arrives.
    pub body: Box<dyn Read + Send>,
}

impl std::fmt::Debug for HttpStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpStream")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

/// The one HTTP client every caller shares. Which hosts a caller may reach is the caller's rule, not the transport's.
pub trait HttpTransport: Send + Sync {
    /// The whole answer. A non-2xx status is a response, never an error.
    ///
    /// # Errors
    /// [`HttpError::Cancelled`] when `cancel` turns true, [`HttpError::Timeout`] after `req.timeout_s`,
    /// [`HttpError::TooLarge`] past `req.max_response`, [`HttpError::Network`] on a connection failure and
    /// [`HttpError::Refused`] when the transport will not send the request.
    fn send(&self, req: &HttpRequest, cancel: &dyn Cancel) -> Result<HttpResponse, HttpError>;

    /// The answer as a stream; `max_response` and `timeout_s` still bound it. Takes the handle owned (`Arc`) because
    /// the returned stream outlives this call and keeps checking it.
    ///
    /// # Errors
    /// The same as [`send`](Self::send), for everything before the first body byte; later failures surface as
    /// described on [`HttpStream`].
    fn open(&self, req: &HttpRequest, cancel: Arc<dyn Cancel>) -> Result<HttpStream, HttpError>;
}
