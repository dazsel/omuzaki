//! Keys (D8, CN-05): the host attaches a connector's key, never the adapter, and scrubs it from everything that comes
//! back. [`AuthedHttp`] is the transport a run hands its adapter; [`scrub_str`] is the same scrubbing for any text.
//!
//! The rules, the prior art being the GitHub Actions runner's secret masker (a value and its encoded forms):
//! - requests go only to the target's hosts (its effective endpoint and the manifest's destinations);
//! - the key goes in the manifest's header and nowhere else, never over plain `http://` to another computer;
//! - a request that sets an auth header itself is refused: only this wrapper sets one;
//! - the key, its percent-encoded form and its base64 form become `[key]` in every response header, body (streamed
//!   ones too, across chunk boundaries) and transport error.

use std::fmt;
use std::io::{self, Read};
use std::sync::Arc;

use base64::Engine as _;
use omuzaki_connect_api::net::{Endpoint, STDIO, Scheme};
use omuzaki_connect_api::{AuthSpec, ConnectorManifest, Deliver, Target, Transport};
use omuzaki_http_api::{Cancel, HttpError, HttpRequest, HttpResponse, HttpStream, HttpTransport};
use omuzaki_secrets_api::prelude::SecretValue;
use zeroize::Zeroizing;

/// What a scrubbed key reads as.
pub const MASK: &str = "[key]";

/// The shortest key the host accepts: scrubbing a shorter string would corrupt ordinary text.
pub const MIN_KEY_BYTES: usize = 8;

/// Headers only the host sets.
const AUTH_HEADERS: [&str; 4] = ["authorization", "proxy-authorization", "x-api-key", "cookie"];

/// Bytes read from a streamed body per pull.
const STREAM_CHUNK: usize = 8 * 1024;

/// The forms of one key to hide: raw, percent-encoded and base64. Each is zeroed on drop.
#[derive(Clone, Default)]
pub(crate) struct Scrubber {
    /// Longest first, so a longer form wins at the same position.
    patterns: Vec<Zeroizing<Vec<u8>>>,
}

impl fmt::Debug for Scrubber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Scrubber({} forms)", self.patterns.len())
    }
}

impl Scrubber {
    /// The scrubber of `key`; one that hides nothing for a key shorter than [`MIN_KEY_BYTES`].
    pub(crate) fn of(key: &str) -> Self {
        if key.len() < MIN_KEY_BYTES {
            return Self::default();
        }
        let raw = Zeroizing::new(key.as_bytes().to_vec());
        let percent = Zeroizing::new(percent_encode(key.as_bytes()));
        let base64 = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(key.as_bytes()).into_bytes());
        let mut patterns = vec![raw, percent, base64];
        patterns.sort_by_key(|p| std::cmp::Reverse(p.len()));
        patterns.dedup();
        Self { patterns }
    }

    /// `text` with every form of the key replaced by [`MASK`].
    pub(crate) fn text(&self, text: &str) -> String {
        if self.patterns.is_empty() {
            return text.to_owned();
        }
        let bytes = self.bytes(text.as_bytes());
        // A match of whole UTF-8 sequences replaced by ASCII stays UTF-8; lossy only guards the impossible.
        String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }

    /// `data` with every form of the key replaced by [`MASK`].
    pub(crate) fn bytes(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        self.scrub_into(data, &mut out, true);
        out
    }

    /// Scrubs `data` into `out` and returns how many bytes of `data` it used. Unless `at_end`, it stops before a tail
    /// that could be the start of a form, which the caller keeps for the next chunk.
    fn scrub_into(&self, data: &[u8], out: &mut Vec<u8>, at_end: bool) -> usize {
        let mut i = 0;
        'scan: while i < data.len() {
            let rest = &data[i..];
            for p in &self.patterns {
                if rest.starts_with(p) {
                    out.extend_from_slice(MASK.as_bytes());
                    i += p.len();
                    continue 'scan;
                }
            }
            if !at_end && self.patterns.iter().any(|p| rest.len() < p.len() && p.starts_with(rest)) {
                return i;
            }
            out.push(data[i]);
            i += 1;
        }
        i
    }

    /// Whether any form of the key occurs in `data`.
    pub(crate) fn finds(&self, data: &[u8]) -> bool {
        self.patterns.iter().any(|p| data.windows(p.len()).any(|w| w == p.as_slice()))
    }

    /// The same error, its text scrubbed.
    pub(crate) fn http_error(&self, e: HttpError) -> HttpError {
        match e {
            HttpError::Network(why) => HttpError::Network(self.text(&why)),
            HttpError::Refused(why) => HttpError::Refused(self.text(&why)),
            other => other,
        }
    }

    fn headers(&self, headers: Vec<(String, String)>) -> Vec<(String, String)> {
        headers.into_iter().map(|(name, value)| (name, self.text(&value))).collect()
    }
}

/// Every byte outside RFC 3986's unreserved set as `%XX`, upper-case.
fn percent_encode(bytes: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = Vec::with_capacity(bytes.len() * 3);
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b);
        } else {
            out.extend_from_slice(&[b'%', HEX[usize::from(b >> 4)], HEX[usize::from(b & 15)]]);
        }
    }
    out
}

/// `text` with `key`, its percent-encoded form and its base64 form replaced by `[key]`. The host runs every string a
/// connector returns through it (D8). A key shorter than [`MIN_KEY_BYTES`] leaves the text as it is: such a key is
/// refused before any request.
pub fn scrub_str(key: &SecretValue, text: &str) -> String {
    Scrubber::of(key.expose()).text(text)
}

/// The transport one run of one connector uses: it attaches the key as the manifest says, refuses any host the target
/// does not name, and scrubs the key from responses and errors. It borrows the key for the run and keeps no copy in
/// a string of its own.
pub struct AuthedHttp<'a> {
    inner: &'a dyn HttpTransport,
    allowed: Vec<String>,
    /// The header name and the scheme before the key.
    header: Option<(String, String)>,
    key: Option<&'a SecretValue>,
    scrub: Scrubber,
    /// Why every request is refused, when one is.
    refusal: Option<String>,
}

impl fmt::Debug for AuthedHttp<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthedHttp")
            .field("allowed", &self.allowed)
            .field("header", &self.header.as_ref().map(|(name, _)| name))
            .field("refusal", &self.refusal)
            .finish_non_exhaustive()
    }
}

impl<'a> AuthedHttp<'a> {
    /// The transport for one run of the connector `manifest` describes, going where `target` says, with `key` (the
    /// key behind `target.secret`; `None` without one).
    ///
    /// Every request is refused (`HttpError::Refused`, nothing sent) when the connector's key goes elsewhere (to a
    /// program's environment, or through the LLM seam), when its header key is missing, or when the key is shorter
    /// than [`MIN_KEY_BYTES`] ("that does not look like a key").
    pub fn new(
        inner: &'a dyn HttpTransport,
        manifest: &ConnectorManifest,
        target: &Target,
        key: Option<&'a SecretValue>,
    ) -> Self {
        let mut allowed: Vec<String> =
            target.destinations.iter().filter(|d| d.as_str() != STDIO).map(|d| d.to_ascii_lowercase()).collect();
        if let Some(e) = target.endpoint.as_deref().and_then(Endpoint::parse)
            && !allowed.contains(&e.host)
        {
            allowed.push(e.host);
        }
        let mut header = None;
        let mut refusal = None;
        let key = key.filter(|_| target.secret.is_some());
        if manifest.transport == Transport::Llm {
            refusal = Some("the LLM seam sends this connector's requests".to_owned());
        }
        match &manifest.auth {
            AuthSpec::None => {}
            AuthSpec::ApiKey { deliver, .. } => match deliver {
                Deliver::Header { name, scheme, .. } => header = Some((name.clone(), scheme.clone())),
                Deliver::Env { .. } => {
                    refusal = Some("this connector's key goes to its program's environment, never over HTTP".into());
                }
                Deliver::Provider => refusal = Some("the LLM seam sends this connector's requests".to_owned()),
            },
        }
        if header.is_some() && target.secret.is_some() {
            match key {
                None => refusal = Some("the key is not in the keychain; add it on the Connectors page".to_owned()),
                Some(k) if k.expose().len() < MIN_KEY_BYTES => {
                    refusal = Some("that does not look like a key".to_owned());
                }
                Some(_) => {}
            }
        }
        let scrub = key.map(|k| Scrubber::of(k.expose())).unwrap_or_default();
        let header = header.filter(|_| key.is_some());
        Self { inner, allowed, header, key, scrub, refusal }
    }

    /// The hosts it sends to.
    pub fn allowed_hosts(&self) -> &[String] {
        &self.allowed
    }

    /// `req` with the key attached, or why it may not be sent.
    fn prepare(&self, req: &HttpRequest) -> Result<HttpRequest, HttpError> {
        if let Some(why) = &self.refusal {
            return Err(HttpError::Refused(why.clone()));
        }
        let url = Endpoint::parse(&req.url)
            .ok_or_else(|| HttpError::Refused("the URL is not an http:// or https:// address".to_owned()))?;
        if !self.allowed.contains(&url.host) {
            return Err(HttpError::Refused(format!("host not allowed: {}", url.host)));
        }
        let ours = self.header.as_ref().map(|(name, _)| name.as_str());
        if req.headers.iter().any(|(name, _)| {
            AUTH_HEADERS.iter().any(|h| name.eq_ignore_ascii_case(h)) || ours.is_some_and(|o| name.eq_ignore_ascii_case(o))
        }) {
            return Err(HttpError::Refused("connectors never set auth headers".to_owned()));
        }
        let mut out = req.clone();
        if let (Some((name, scheme)), Some(key)) = (&self.header, self.key) {
            if url.scheme == Scheme::Http && !url.is_loopback() {
                return Err(HttpError::Refused(
                    "a key is never sent over plain http:// to another computer; use https://".to_owned(),
                ));
            }
            let value = if scheme.is_empty() { key.expose().to_owned() } else { format!("{scheme} {}", key.expose()) };
            out.headers.push((name.clone(), value));
        }
        Ok(out)
    }
}

impl HttpTransport for AuthedHttp<'_> {
    fn send(&self, req: &HttpRequest, cancel: &dyn Cancel) -> Result<HttpResponse, HttpError> {
        let req = self.prepare(req)?;
        match self.inner.send(&req, cancel) {
            Ok(r) => Ok(HttpResponse {
                status: r.status,
                headers: self.scrub.headers(r.headers),
                body: if self.scrub.finds(&r.body) { self.scrub.bytes(&r.body) } else { r.body },
            }),
            Err(e) => Err(self.scrub.http_error(e)),
        }
    }

    fn open(&self, req: &HttpRequest, cancel: Arc<dyn Cancel>) -> Result<HttpStream, HttpError> {
        let req = self.prepare(req)?;
        match self.inner.open(&req, cancel) {
            Ok(s) => Ok(HttpStream {
                status: s.status,
                headers: self.scrub.headers(s.headers),
                body: Box::new(ScrubReader::new(s.body, self.scrub.clone())),
            }),
            Err(e) => Err(self.scrub.http_error(e)),
        }
    }
}

/// A streamed body with the key scrubbed, including a form split across two reads.
struct ScrubReader {
    inner: Box<dyn Read + Send>,
    scrub: Scrubber,
    /// Read but not yet scrubbed: a tail that may start a form.
    held: Vec<u8>,
    /// Scrubbed, not yet handed out.
    ready: Vec<u8>,
    at: usize,
    ended: bool,
}

impl ScrubReader {
    fn new(inner: Box<dyn Read + Send>, scrub: Scrubber) -> Self {
        Self { inner, scrub, held: Vec::new(), ready: Vec::new(), at: 0, ended: false }
    }
}

impl Read for ScrubReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.at < self.ready.len() {
                let n = buf.len().min(self.ready.len() - self.at);
                buf[..n].copy_from_slice(&self.ready[self.at..self.at + n]);
                self.at += n;
                return Ok(n);
            }
            if self.ended || buf.is_empty() {
                return Ok(0);
            }
            let mut chunk = [0u8; STREAM_CHUNK];
            let n = self.inner.read(&mut chunk)?;
            self.ended = n == 0;
            self.held.extend_from_slice(&chunk[..n]);
            self.ready.clear();
            self.at = 0;
            let used = self.scrub.scrub_into(&self.held, &mut self.ready, self.ended);
            self.held.drain(..used);
        }
    }
}
