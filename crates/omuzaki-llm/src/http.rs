//! The one place a request leaves the process (llm-api D11). Everything above it (retries, SSE, the providers) sees
//! only the small types here, never a `reqwest` type, so a later task can move this module onto `omuzaki-http-api`'s
//! `HttpTransport` and change nothing else.
//!
//! The client matches `omuzaki-http`: the user agent `Omuzaki/<version>`, a 10 s connect timeout, the OS proxy for
//! hosted endpoints and https only there. Loopback endpoints differ on purpose: plain http is allowed and the proxy is
//! always bypassed, so a local model's traffic never leaves the machine.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::provider::{SecretValue, is_loopback_endpoint};

/// The user agent every request sends.
pub(crate) const USER_AGENT: &str = concat!("Omuzaki/", env!("CARGO_PKG_VERSION"));
/// How long a connection may take to open.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of an error body that is read; the provider's message is short.
pub(crate) const MAX_ERROR_BODY: usize = 64 * 1024;

/// A checked endpoint: where requests go, and whether that is this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// The URL without a trailing slash.
    pub base: String,
    /// The host is `localhost` or a loopback address.
    pub loopback: bool,
}

impl Endpoint {
    /// Checks `endpoint`: an `http` or `https` URL without credentials, and plain `http` only on loopback.
    ///
    /// # Errors
    /// `Unsupported`, saying why and never repeating the URL (it could carry a secret).
    pub(crate) fn parse(endpoint: &str) -> Result<Self, LlmError> {
        let endpoint = endpoint.trim();
        let loopback = is_loopback_endpoint(endpoint);
        let rest = endpoint
            .strip_prefix("https://")
            .or_else(|| endpoint.strip_prefix("http://"))
            .filter(|rest| !rest.is_empty())
            .ok_or_else(|| LlmError::Unsupported("the endpoint is not an http or https URL".into()))?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            return Err(LlmError::Unsupported("the endpoint carries credentials; use a key reference instead".into()));
        }
        if omuzaki_llm_api::provider::endpoint_host(endpoint).is_none() {
            return Err(LlmError::Unsupported("the endpoint has no host".into()));
        }
        if !loopback && endpoint.starts_with("http://") {
            return Err(LlmError::Unsupported("plain http is refused off loopback; use https".into()));
        }
        Ok(Self { base: endpoint.trim_end_matches('/').to_owned(), loopback })
    }

    /// `path` below the endpoint. A base that already ends in `/v1` and a `path` that starts with `/v1/` share the
    /// segment.
    pub(crate) fn url(&self, path: &str) -> String {
        match (self.base.ends_with("/v1"), path.strip_prefix("/v1")) {
            (true, Some(rest)) => format!("{}{rest}", self.base),
            _ => format!("{}{path}", self.base),
        }
    }
}

/// How a request failed before an HTTP status existed, or while its body was read. The text never holds a URL.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HttpFail {
    /// The connection could not be made or was refused or reset.
    #[error("connection failed: {0}")]
    Connect(String),
    /// A body read failed after the response began.
    #[error("connection dropped: {0}")]
    Body(String),
    /// The request could not be built.
    #[error("request refused: {0}")]
    Request(String),
}

impl From<HttpFail> for LlmError {
    fn from(fail: HttpFail) -> Self {
        match fail {
            HttpFail::Connect(_) | HttpFail::Body(_) => Self::Network(fail.to_string()),
            HttpFail::Request(_) => Self::Malformed(fail.to_string()),
        }
    }
}

/// The verbs the providers use: GET for listing and probing, POST for turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Post,
}

/// A header value. A secret never shows in `Debug`.
#[derive(Clone)]
pub(crate) enum HeaderValue {
    Plain(String),
    Secret(SecretValue),
}

impl std::fmt::Debug for HeaderValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plain(v) => write!(f, "{v:?}"),
            Self::Secret(_) => f.write_str("<redacted>"),
        }
    }
}

/// One request.
#[derive(Debug)]
pub(crate) struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, HeaderValue)>,
    pub body: Option<Vec<u8>>,
}

impl Request {
    pub(crate) fn get(url: String, headers: Vec<(&'static str, HeaderValue)>) -> Self {
        Self { method: Method::Get, url, headers, body: None }
    }

    pub(crate) fn post(url: String, headers: Vec<(&'static str, HeaderValue)>, body: Vec<u8>) -> Self {
        Self { method: Method::Post, url, headers, body: Some(body) }
    }
}

/// A boxed stream of body chunks.
pub(crate) type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, HttpFail>> + Send>>;

/// A response whose body has not been read yet.
pub(crate) struct Response {
    pub status: u16,
    /// `retry-after` in whole seconds, when the server sent that form.
    pub retry_after: Option<Duration>,
    inner: reqwest::Response,
}

impl Response {
    /// The body as chunks.
    pub(crate) fn stream(self) -> ByteStream {
        Box::pin(self.inner.bytes_stream().map(|chunk| chunk.map_err(|e| HttpFail::Body(e.without_url().to_string()))))
    }

    /// At most `limit` bytes of the body.
    pub(crate) async fn read_limited(self, limit: usize) -> Result<Vec<u8>, HttpFail> {
        let mut out = Vec::new();
        let mut body = self.stream();
        while let Some(chunk) = body.next().await {
            out.extend_from_slice(&chunk?);
            if out.len() >= limit {
                out.truncate(limit);
                break;
            }
        }
        Ok(out)
    }
}

/// The client of one provider: a connection pool for one endpoint.
#[derive(Clone)]
pub(crate) struct Http {
    client: reqwest::Client,
}

impl Http {
    /// A client for `endpoint`.
    ///
    /// # Errors
    /// `Unsupported` if the TLS stack or the platform verifier cannot start.
    pub(crate) fn new(endpoint: &Endpoint) -> Result<Self, LlmError> {
        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            // A redirect would carry the key to another host.
            .redirect(reqwest::redirect::Policy::none())
            .tcp_nodelay(true);
        if endpoint.loopback {
            // `localhost` always means this machine, whatever the resolver says; and no proxy sees local traffic.
            let local = [
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
            ];
            builder = builder.no_proxy().resolve_to_addrs("localhost", &local);
        }
        let client =
            builder.build().map_err(|e| LlmError::Unsupported(format!("cannot start the HTTP client: {e}")))?;
        Ok(Self { client })
    }

    /// Sends `request` and returns once the status and headers are in.
    pub(crate) async fn send(&self, request: Request) -> Result<Response, HttpFail> {
        let mut builder = match request.method {
            Method::Get => self.client.get(&request.url),
            Method::Post => self.client.post(&request.url),
        };
        for (name, value) in &request.headers {
            let text = match value {
                HeaderValue::Plain(v) => v.as_str(),
                HeaderValue::Secret(v) => v.expose(),
            };
            let mut value = reqwest::header::HeaderValue::from_str(text)
                .map_err(|_| HttpFail::Request(format!("the {name} header holds a character a header cannot")))?;
            value.set_sensitive(matches!(name, &"x-api-key" | &"authorization"));
            builder = builder.header(*name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let response = builder.send().await.map_err(|e| {
            let before_response = e.is_connect() || e.is_timeout() || e.is_request();
            let text = e.without_url().to_string();
            if before_response { HttpFail::Connect(text) } else { HttpFail::Body(text) }
        })?;
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        Ok(Response { status: response.status().as_u16(), retry_after, inner: response })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(endpoint: &str) -> Result<Endpoint, LlmError> {
        Endpoint::parse(endpoint)
    }

    #[test]
    fn loopback_may_use_plain_http_and_a_remote_host_may_not() {
        assert_eq!(
            parse("http://127.0.0.1:11434/").map(|e| (e.base, e.loopback)),
            Ok(("http://127.0.0.1:11434".into(), true))
        );
        assert_eq!(parse("https://api.example.com").map(|e| e.loopback), Ok(false));
        assert!(matches!(parse("http://api.example.com"), Err(LlmError::Unsupported(m)) if m.contains("plain http")));
    }

    #[test]
    fn credentials_and_junk_in_the_endpoint_are_refused_without_echoing_them() {
        for bad in
            ["https://user:secret-key@api.example.com", "http://127.0.0.1@evil.example", "ftp://x", "", "https://"]
        {
            match parse(bad) {
                Err(LlmError::Unsupported(m)) => assert!(!m.contains("secret-key"), "{m}"),
                other => panic!("{bad:?} gave {other:?}"),
            }
        }
    }

    #[test]
    fn paths_share_a_v1_segment() {
        let plain = parse("http://localhost:1").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(plain.url("/v1/models"), "http://localhost:1/v1/models");
        let v1 = parse("http://localhost:1/v1/").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(v1.url("/v1/models"), "http://localhost:1/v1/models");
        assert_eq!(v1.url("/props"), "http://localhost:1/v1/props");
    }

    #[test]
    fn a_secret_header_does_not_show_in_debug() {
        let request = Request::post(
            "http://x".into(),
            vec![("x-api-key", HeaderValue::Secret(SecretValue::new("sk-very-secret")))],
            Vec::new(),
        );
        assert!(!format!("{request:?}").contains("sk-very-secret"));
    }
}
