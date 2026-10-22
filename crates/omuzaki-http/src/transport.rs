//! [`HttpTransport`] on the same client as [`Fetcher`](omuzaki_ai_api::Fetcher): the one HTTP client the connector
//! adapters, `net.http`, the LLM providers and the platform adapters share (connectors design D13).
//!
//! The same rules as the fetcher: https only (plain http to loopback with the test aid), at most five redirects, each
//! to https. Unlike the fetcher it never retries: a request may not be safe to send twice, so retrying is the caller's
//! call.
//!
//! **Cancel and timeout.** reqwest's blocking client can't be interrupted, so each request runs on a thread of its own
//! that hands the head and the body over a short queue. The caller's thread polls that queue every [`POLL`], checking
//! the cancel handle and the `timeout_s` deadline between polls, so a cancel is honoured within 50 ms even while the
//! server is silent; the request thread finishes on its own when its next read ends (bounded by the same timeout).

use std::fmt::Write as _;
use std::io::{self, Read};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use omuzaki_http_api::{
    Cancel, HttpBody, HttpError, HttpRequest, HttpResponse, HttpStream, HttpTransport, Method, Part,
};
use reqwest::Url;
use reqwest::blocking::{Client, Request, Response};
use reqwest::header::CONTENT_TYPE;

use crate::{HttpFetcher, scheme_allowed};

/// How often the caller's thread looks at its cancel handle and deadline while it waits.
const POLL: Duration = Duration::from_millis(20);
/// The most bytes one body read hands over.
const CHUNK: usize = 16 * 1024;
/// Chunks the request thread may read ahead of the caller.
const AHEAD: usize = 8;

/// What the request thread hands the caller, in order: the head, then body chunks, then the end or a failure.
enum Piece {
    Head { status: u16, headers: Vec<(String, String)> },
    Bytes(Vec<u8>),
    End,
    Failed(HttpError),
}

impl HttpTransport for HttpFetcher {
    fn send(&self, req: &HttpRequest, cancel: &dyn Cancel) -> Result<HttpResponse, HttpError> {
        let incoming = self.begin(req, cancel)?;
        let (status, headers) = incoming.head(cancel)?;
        let mut body = Vec::new();
        loop {
            match incoming.next(cancel)? {
                Piece::Bytes(bytes) => {
                    let total = u64::try_from(body.len().saturating_add(bytes.len())).unwrap_or(u64::MAX);
                    if total > req.max_response {
                        return Err(HttpError::TooLarge { limit: req.max_response });
                    }
                    body.extend_from_slice(&bytes);
                }
                Piece::End => return Ok(HttpResponse { status, headers, body }),
                Piece::Failed(e) => return Err(e),
                Piece::Head { .. } => {}
            }
        }
    }

    fn open(&self, req: &HttpRequest, cancel: Arc<dyn Cancel>) -> Result<HttpStream, HttpError> {
        let incoming = self.begin(req, cancel.as_ref())?;
        let (status, headers) = incoming.head(cancel.as_ref())?;
        let body = Body { incoming, cancel, left: req.max_response, limit: req.max_response, held: Vec::new(), at: 0, over: false };
        Ok(HttpStream { status, headers, body: Box::new(body) })
    }
}

impl HttpFetcher {
    /// Builds the request and starts its thread.
    fn begin(&self, req: &HttpRequest, cancel: &dyn Cancel) -> Result<Incoming, HttpError> {
        let client = self.client.as_ref().map_err(|e| HttpError::Network(format!("no HTTP client: {e}")))?;
        if cancel.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        let timeout = Duration::from_secs(u64::from(req.timeout_s.max(1)));
        let request = build(client, req, timeout, self.allow_loopback_http)?;
        let (tx, rx) = sync_channel(AHEAD);
        let client = client.clone();
        std::thread::Builder::new()
            .name("omuzaki-http".to_owned())
            .spawn(move || run(&client, request, &tx))
            .map_err(|e| HttpError::Network(format!("the request thread didn't start: {e}")))?;
        Ok(Incoming { rx, deadline: Instant::now() + timeout })
    }
}

/// The request as reqwest's, after the scheme check.
fn build(client: &Client, req: &HttpRequest, timeout: Duration, allow_loopback: bool) -> Result<Request, HttpError> {
    let url = Url::parse(&req.url).map_err(|e| HttpError::Refused(format!("not a valid address: {e}")))?;
    if !scheme_allowed(&url, allow_loopback) {
        return Err(HttpError::Refused("only https addresses are allowed".to_owned()));
    }
    let method = match req.method {
        Method::Get => reqwest::Method::GET,
        Method::Post => reqwest::Method::POST,
        Method::Put => reqwest::Method::PUT,
        Method::Patch => reqwest::Method::PATCH,
        Method::Delete => reqwest::Method::DELETE,
    };
    let mut builder = client.request(method, url).timeout(timeout);
    for (name, value) in &req.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder = match &req.body {
        HttpBody::Empty => builder,
        HttpBody::Bytes { content_type, data } => builder.header(CONTENT_TYPE, content_type.as_str()).body(data.clone()),
        HttpBody::Json(value) => {
            let data = serde_json::to_vec(value).map_err(|e| HttpError::Refused(format!("the JSON body: {e}")))?;
            builder.header(CONTENT_TYPE, "application/json").body(data)
        }
        HttpBody::Multipart(parts) => {
            let (content_type, data) = multipart(parts);
            builder.header(CONTENT_TYPE, content_type).body(data)
        }
    };
    builder.build().map_err(|e| HttpError::Refused(e.without_url().to_string()))
}

/// The request thread: sends, then hands over the head and the body until the end, a failure or a caller that left.
fn run(client: &Client, request: Request, out: &SyncSender<Piece>) {
    let mut response: Response = match client.execute(request) {
        Ok(response) => response,
        Err(e) => {
            let _ = out.send(Piece::Failed(from_reqwest(e)));
            return;
        }
    };
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().to_owned(), String::from_utf8_lossy(value.as_bytes()).into_owned()))
        .collect();
    if out.send(Piece::Head { status: response.status().as_u16(), headers }).is_err() {
        return;
    }
    loop {
        let mut chunk = vec![0; CHUNK];
        let piece = match response.read(&mut chunk) {
            Ok(0) => Piece::End,
            Ok(n) => {
                chunk.truncate(n);
                Piece::Bytes(chunk)
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => Piece::Failed(from_io(&e)),
        };
        let last = !matches!(piece, Piece::Bytes(_));
        if out.send(piece).is_err() || last {
            return;
        }
    }
}

fn from_reqwest(e: reqwest::Error) -> HttpError {
    if e.is_timeout() { HttpError::Timeout } else { HttpError::Network(e.without_url().to_string()) }
}

fn from_io(e: &io::Error) -> HttpError {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => HttpError::Timeout,
        _ => HttpError::Network(e.to_string()),
    }
}

/// The caller's end of a running request.
struct Incoming {
    rx: Receiver<Piece>,
    deadline: Instant,
}

impl Incoming {
    /// The next piece, waiting no longer than the deadline and no more than [`POLL`] between looks at `cancel`.
    fn next(&self, cancel: &dyn Cancel) -> Result<Piece, HttpError> {
        loop {
            if cancel.is_cancelled() {
                return Err(HttpError::Cancelled);
            }
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(HttpError::Timeout);
            }
            match self.rx.recv_timeout(POLL.min(left)) {
                Ok(piece) => return Ok(piece),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(HttpError::Network("the connection ended without an answer".to_owned()));
                }
            }
        }
    }

    /// The status and the headers.
    fn head(&self, cancel: &dyn Cancel) -> Result<(u16, Vec<(String, String)>), HttpError> {
        match self.next(cancel)? {
            Piece::Head { status, headers } => Ok((status, headers)),
            Piece::Failed(e) => Err(e),
            Piece::Bytes(_) | Piece::End => Err(HttpError::Network("the answer had no head".to_owned())),
        }
    }
}

/// A streamed body: the chunks as they arrive, cut at `max_response`, with the failures [`HttpStream`] requires.
struct Body {
    incoming: Incoming,
    cancel: Arc<dyn Cancel>,
    /// Bytes still allowed.
    left: u64,
    limit: u64,
    /// The chunk being handed out, from `at`.
    held: Vec<u8>,
    at: usize,
    /// The held chunk was cut at the limit: once it is handed out, the next read fails.
    over: bool,
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(HttpError::Cancelled.into());
        }
        if buf.is_empty() {
            return Ok(0);
        }
        while self.at >= self.held.len() {
            if self.over {
                return Err(HttpError::TooLarge { limit: self.limit }.into());
            }
            match self.incoming.next(self.cancel.as_ref())? {
                Piece::Bytes(bytes) => {
                    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                    if len > self.left {
                        // Hand out what the limit allows; the read after it fails.
                        let allowed = usize::try_from(self.left).unwrap_or(usize::MAX);
                        if allowed == 0 {
                            return Err(HttpError::TooLarge { limit: self.limit }.into());
                        }
                        self.held = bytes;
                        self.held.truncate(allowed);
                        self.over = true;
                    } else {
                        self.held = bytes;
                    }
                    self.left = self.left.saturating_sub(u64::try_from(self.held.len()).unwrap_or(u64::MAX));
                    self.at = 0;
                }
                Piece::End => return Ok(0),
                Piece::Failed(e) => return Err(e.into()),
                Piece::Head { .. } => {}
            }
        }
        let rest = self.held.get(self.at..).unwrap_or_default();
        let n = rest.len().min(buf.len());
        if let (Some(to), Some(from)) = (buf.get_mut(..n), rest.get(..n)) {
            to.copy_from_slice(from);
        }
        self.at += n;
        Ok(n)
    }
}

/// A `multipart/form-data` body: its `Content-Type` (with the boundary) and its bytes. The boundary is one no part
/// contains.
fn multipart(parts: &[Part]) -> (String, Vec<u8>) {
    // Each part rules out at most one candidate, so one more candidate than there are parts always finds one.
    let boundary = (0..=parts.len())
        .map(|n| format!("omuzaki-form-boundary-{n:08x}"))
        .find(|b| !parts.iter().any(|p| contains(&p.data, b.as_bytes())))
        .unwrap_or_default();
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let mut disposition = format!("Content-Disposition: form-data; name=\"{}\"", quoted(&part.name));
        if let Some(file) = &part.file_name {
            let _ = write!(disposition, "; filename=\"{}\"", quoted(file));
        }
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(b"\r\n");
        if let Some(content_type) = &part.content_type {
            body.extend_from_slice(format!("Content-Type: {}\r\n", quoted(content_type)).as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(&part.data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// A header parameter value with `"`, CR and LF escaped as the HTML form encoding does.
fn quoted(value: &str) -> String {
    value.replace('"', "%22").replace('\r', "%0D").replace('\n', "%0A")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multipart_body_has_its_parts_and_a_boundary_none_contains() {
        let parts = vec![
            Part { name: "a\"b".into(), file_name: None, content_type: None, data: b"omuzaki-form-boundary-00000000".to_vec() },
            Part {
                name: "file".into(),
                file_name: Some("x.png".into()),
                content_type: Some("image/png".into()),
                data: vec![1, 2, 3],
            },
        ];
        let (content_type, body) = multipart(&parts);
        assert_eq!(content_type, "multipart/form-data; boundary=omuzaki-form-boundary-00000001");
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with("--omuzaki-form-boundary-00000001\r\nContent-Disposition: form-data; name=\"a%22b\"\r\n\r\n"));
        assert!(text.contains("name=\"file\"; filename=\"x.png\"\r\nContent-Type: image/png\r\n\r\n\u{1}\u{2}\u{3}\r\n"));
        assert!(text.ends_with("--omuzaki-form-boundary-00000001--\r\n"));
    }
}
