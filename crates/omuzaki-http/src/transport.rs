//! [`ConnectHttp`]: the app's [`HttpTransport`], the one HTTP client the connector adapters, `net.http`, the LLM
//! providers and the platform adapters share (connectors design D9 and D13). Same stack as [`HttpFetcher`]
//! (`reqwest` with rustls and the `ring` provider), different rules:
//!
//! - **Addresses.** `https://` to any host; `http://` only to this computer (`localhost`, `127.0.0.1`, `::1`), which
//!   local services such as `ComfyUI` speak. Anything else, and any URL with user-info, is refused before a lookup.
//!   Loopback never goes through a proxy.
//! - **No redirects.** A 3xx is an answer like any other: an adapter decides, so an auth header never follows a
//!   redirect to another host. No retries either: a request may not be safe to send twice.
//! - **Limits.** `timeout_s` (at most 600) bounds the whole call; the body stops at `max_response`; at most
//!   [`MAX_IN_FLIGHT`] requests run in the process at once.
//! - **Headers.** `User-Agent: Omuzaki/<version>` and nothing about the machine; a caller's `Host`, `Content-Length`
//!   and `Transfer-Encoding` are refused. Multipart bodies are encoded here, with a random 128-bit boundary.
//!
//! **Cancel and timeout.** reqwest's blocking client can't be interrupted, so each request runs on a thread of its own
//! that hands the head and the body over a short queue. The caller's thread polls that queue every [`POLL`], checking
//! the cancel handle and the deadline between polls, so a cancel is honoured within 50 ms even while the server is
//! silent; dropping the queue closes the connection when the request thread next wakes, which the timeout bounds.
//!
//! [`HttpFetcher`]: crate::HttpFetcher

use std::fmt::Write as _;
use std::io::{self, Read};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use omuzaki_http_api::{
    Cancel, HttpBody, HttpError, HttpRequest, HttpResponse, HttpStream, HttpTransport, Method, Part,
};
use reqwest::Url;
use reqwest::blocking::{Client, Request, Response};
use reqwest::header::CONTENT_TYPE;
use reqwest::redirect::Policy;

/// How often the caller's thread looks at its cancel handle and deadline while it waits.
const POLL: Duration = Duration::from_millis(20);
/// The most bytes one body read hands over.
const CHUNK: usize = 16 * 1024;
/// Chunks the request thread may read ahead of the caller.
const AHEAD: usize = 8;
/// The most requests in flight in the process; one more is refused.
pub const MAX_IN_FLIGHT: usize = 8;
/// The longest `timeout_s` honoured.
pub const MAX_TIMEOUT_S: u32 = 600;
/// How long a connection may take to open, inside the call's own timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Headers the transport sets itself.
const OWN_HEADERS: [&str; 3] = ["host", "content-length", "transfer-encoding"];

/// Requests in flight in this process, across every [`ConnectHttp`].
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// What the request thread hands the caller, in order: the head, then body chunks, then the end or a failure.
enum Piece {
    Head { status: u16, headers: Vec<(String, String)> },
    Bytes(Vec<u8>),
    End,
    Failed(HttpError),
}

/// The app's [`HttpTransport`]: blocking, safe to share between threads. See the module docs for its rules.
#[derive(Debug)]
pub struct ConnectHttp {
    /// For https to other hosts, through the system proxy when there is one.
    remote: Result<Client, String>,
    /// For this computer, never through a proxy.
    local: Result<Client, String>,
}

impl ConnectHttp {
    /// The transport. Infallible: if a client can't be built, each call returns the reason.
    #[must_use]
    pub fn new() -> Self {
        crate::install_crypto_provider();
        let build = |local: bool| {
            let mut builder = Client::builder()
                .user_agent(concat!("Omuzaki/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(CONNECT_TIMEOUT)
                .redirect(Policy::none());
            if local {
                builder = builder.no_proxy();
            }
            // reqwest's blocking builder panics on a thread that runs async tasks: build on a plain one.
            crate::on_plain_thread(|| builder.build().map_err(|e| e.to_string()))
        };
        Self { remote: build(false), local: build(true) }
    }
}

impl Default for ConnectHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpTransport for ConnectHttp {
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
        let limit = req.max_response;
        let body = Body { incoming, cancel, left: limit, limit, held: Vec::new(), at: 0, over: false };
        Ok(HttpStream { status, headers, body: Box::new(body) })
    }
}

impl ConnectHttp {
    /// Checks and builds the request, takes an in-flight slot and starts the request thread.
    fn begin(&self, req: &HttpRequest, cancel: &dyn Cancel) -> Result<Incoming, HttpError> {
        let url = Url::parse(&req.url).map_err(|e| HttpError::Refused(format!("not a valid address: {e}")))?;
        let local = check_address(&url)?;
        if let Some((name, _)) =
            req.headers.iter().find(|(n, _)| OWN_HEADERS.contains(&n.to_ascii_lowercase().as_str()))
        {
            return Err(HttpError::Refused(format!("the {name} header is the transport's to set")));
        }
        let client = if local { &self.local } else { &self.remote };
        let client = client.as_ref().map_err(|e| HttpError::Network(format!("no HTTP client: {e}")))?;
        let slot = Slot::take().ok_or_else(|| HttpError::Refused("too many requests in flight".to_owned()))?;
        if cancel.is_cancelled() {
            return Err(HttpError::Cancelled);
        }
        let timeout = Duration::from_secs(u64::from(req.timeout_s.clamp(1, MAX_TIMEOUT_S)));
        let request = build(client, url, req, timeout)?;
        let (tx, rx) = sync_channel(AHEAD);
        let client = client.clone();
        std::thread::Builder::new()
            .name("omuzaki-http".to_owned())
            .spawn(move || {
                let _slot = slot;
                run(&client, request, &tx);
            })
            .map_err(|e| HttpError::Network(format!("the request thread didn't start: {e}")))?;
        Ok(Incoming { rx, deadline: Instant::now() + timeout })
    }
}

/// An in-flight slot, given back when the request thread ends.
struct Slot;

impl Slot {
    fn take() -> Option<Self> {
        let mut now = IN_FLIGHT.load(Ordering::Acquire);
        while now < MAX_IN_FLIGHT {
            match IN_FLIGHT.compare_exchange_weak(now, now + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(Self),
                Err(seen) => now = seen,
            }
        }
        None
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Whether `url` may be called, and whether it is this computer.
fn check_address(url: &Url) -> Result<bool, HttpError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(HttpError::Refused("an address with a user name or password in it".to_owned()));
    }
    let local = match url.host_str() {
        Some("localhost") => true,
        Some(host) => host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()),
        None => return Err(HttpError::Refused("the address has no host".to_owned())),
    };
    match url.scheme() {
        "https" => Ok(local),
        "http" if local => Ok(true),
        "http" => Err(HttpError::Refused("plain http is only allowed to this computer".to_owned())),
        other => Err(HttpError::Refused(format!("{other}:// addresses aren't spoken here"))),
    }
}

/// The request as reqwest's.
fn build(client: &Client, url: Url, req: &HttpRequest, timeout: Duration) -> Result<Request, HttpError> {
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
        HttpBody::Bytes { content_type, data } => {
            builder.header(CONTENT_TYPE, content_type.as_str()).body(data.clone())
        }
        HttpBody::Json(value) => {
            let data = serde_json::to_vec(value).map_err(|e| HttpError::Refused(format!("the JSON body: {e}")))?;
            builder.header(CONTENT_TYPE, "application/json").body(data)
        }
        HttpBody::Multipart(parts) => {
            let (content_type, data) = multipart(parts, &boundary()?);
            builder.header(CONTENT_TYPE, content_type).body(data)
        }
    };
    builder.build().map_err(|e| HttpError::Refused(e.without_url().to_string()))
}

/// A random 128-bit multipart boundary.
fn boundary() -> Result<String, HttpError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| HttpError::Network(format!("no randomness for the boundary: {e}")))?;
    let mut text = String::from("omuzaki-");
    for b in bytes {
        let _ = write!(text, "{b:02x}");
    }
    Ok(text)
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

/// A `multipart/form-data` body with `boundary`: its `Content-Type` and its bytes.
fn multipart(parts: &[Part], boundary: &str) -> (String, Vec<u8>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multipart_body_has_its_parts() {
        let parts = vec![
            Part { name: "a\"b".into(), file_name: None, content_type: None, data: b"one".to_vec() },
            Part {
                name: "file".into(),
                file_name: Some("x.png".into()),
                content_type: Some("image/png".into()),
                data: vec![1, 2, 3],
            },
        ];
        let (content_type, body) = multipart(&parts, "B");
        assert_eq!(content_type, "multipart/form-data; boundary=B");
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with("--B\r\nContent-Disposition: form-data; name=\"a%22b\"\r\n\r\none\r\n"));
        assert!(
            text.contains("name=\"file\"; filename=\"x.png\"\r\nContent-Type: image/png\r\n\r\n\u{1}\u{2}\u{3}\r\n")
        );
        assert!(text.ends_with("--B--\r\n"));
    }

    #[test]
    fn boundaries_are_random_and_128_bits() {
        let (a, b) = (boundary().unwrap_or_default(), boundary().unwrap_or_default());
        assert_eq!(a.len(), "omuzaki-".len() + 32);
        assert_ne!(a, b);
    }

    #[test]
    fn only_https_and_loopback_http_without_user_info_are_called() {
        let check = |s: &str| check_address(&Url::parse(s).unwrap_or_else(|_| unreachable!()));
        assert_eq!(check("https://api.example.com/x"), Ok(false));
        assert_eq!(check("https://127.0.0.1/x"), Ok(true));
        assert_eq!(check("http://localhost:8188/x"), Ok(true));
        assert_eq!(check("http://[::1]/x"), Ok(true));
        for refused in ["http://192.0.2.1/", "http://example.com/", "https://u:p@example.com/", "ftp://127.0.0.1/"] {
            assert!(matches!(check(refused), Err(HttpError::Refused(_))), "{refused}");
        }
    }
}
