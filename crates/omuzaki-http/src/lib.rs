//! The HTTPS client behind [`Fetcher`]: [`HttpFetcher`] over reqwest with rustls.
//!
//! It is https only, follows at most five redirects (each to an https target), retries transient failures inside
//! [`Fetcher::open`], and returns the response as a stream, never buffering the body. Nothing below the app links this
//! crate (`omuzaki-ai-api` says so); the app hands it to the model cache.
//!
//! TLS uses rustls with the `ring` crypto provider, not `aws-lc-rs`: `ring` builds with a plain C toolchain on every
//! platform, with no NASM and no prebuilt objects. What that gives up is rustls's post-quantum hybrid key exchange,
//! which only the aws-lc provider offers; revisit it if a feature needs post-quantum TLS.
//!
//! reqwest is built with `rustls-no-provider`, so any other code that builds a reqwest client in the same build, tests
//! included, must install the `ring` provider first.

use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;

use omuzaki_ai_api::{Body, FetchError, Fetcher};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT_ENCODING, CONTENT_RANGE, RANGE};
use reqwest::redirect::Policy;
use reqwest::{StatusCode, Url};

/// The settings behind [`HttpFetcher`]; see [`HttpFetcher::builder`].
#[derive(Clone, Debug)]
pub struct HttpFetcherBuilder {
    user_agent: String,
    connect_timeout: Duration,
    read_timeout: Duration,
    max_redirects: usize,
    retries: u32,
    backoff: Duration,
    allow_loopback_http: bool,
}

impl Default for HttpFetcherBuilder {
    fn default() -> Self {
        Self {
            user_agent: concat!("Omuzaki/", env!("CARGO_PKG_VERSION")).to_owned(),
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(30),
            max_redirects: 5,
            retries: 3,
            backoff: Duration::from_millis(500),
            allow_loopback_http: false,
        }
    }
}

impl HttpFetcherBuilder {
    /// The `User-Agent` header (default `Omuzaki/<version>`).
    #[must_use]
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// How long a connection may take to open (default 10 s).
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// How long the server may stay silent while a body is read (default 30 s). It is an idle timeout, not a total
    /// one, so a large model downloads fine and a stalled server does not hang the caller.
    #[must_use]
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = timeout;
        self
    }

    /// The most redirects followed (default 5); each must lead to an https URL.
    #[must_use]
    pub fn max_redirects(mut self, max: usize) -> Self {
        self.max_redirects = max;
        self
    }

    /// The most tries per `open` (default 3, at least 1).
    #[must_use]
    pub fn retries(mut self, tries: u32) -> Self {
        self.retries = tries.max(1);
        self
    }

    /// The wait before the second try; it doubles for each later one (default 0.5 s, so 0.5, 1, 2 s). Tests set zero.
    #[must_use]
    pub fn backoff(mut self, base: Duration) -> Self {
        self.backoff = base;
        self
    }

    /// A test aid: lets plain `http://` reach loopback hosts (`127.0.0.1`, `::1`, `localhost`) so tests can use a local
    /// server. Any other plain-http URL is still refused. It also makes the client ignore proxy settings (environment
    /// and system), so loopback is never sent through a proxy. Never set it in the app.
    #[must_use]
    pub fn allow_loopback_http(mut self, allow: bool) -> Self {
        self.allow_loopback_http = allow;
        self
    }

    /// Builds the fetcher. A client that fails to build is not an error here: every `open` then returns it.
    #[must_use]
    pub fn build(self) -> HttpFetcher {
        let allow = self.allow_loopback_http;
        let max = self.max_redirects;
        let policy = Policy::custom(move |attempt| {
            if attempt.previous().len() > max {
                attempt.error(format!("more than {max} redirects"))
            } else if !scheme_allowed(attempt.url(), allow) {
                attempt.error("redirect to a non-https address")
            } else {
                attempt.follow()
            }
        });
        install_crypto_provider();
        let mut builder = Client::builder()
            .user_agent(self.user_agent.clone())
            .connect_timeout(self.connect_timeout)
            // The blocking client applies this to the wait for the headers and again to each body read, which is
            // an idle timeout for the body.
            .timeout(self.read_timeout)
            .redirect(policy);
        if allow {
            // Loopback bypasses the proxy (llm-api D11); the test aid is the only way to reach it.
            builder = builder.no_proxy();
        }
        let client = builder.build().map_err(|e| e.to_string());
        HttpFetcher { client, tries: self.retries, backoff: self.backoff, allow_loopback_http: allow }
    }
}

/// Installs `ring` as the process-wide rustls provider, once. If another provider is already installed the call is a
/// no-op: the first one wins, and reqwest uses whichever is installed.
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Err means a provider is already installed, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The HTTPS [`Fetcher`]. Build one with [`HttpFetcher::new`] or [`HttpFetcher::builder`].
///
/// # Blocking
///
/// [`Fetcher::open`] blocks the calling thread and cannot be cancelled. With the defaults it can take about 90 s
/// before it returns an error: three tries, each waiting up to the 30 s read timeout for the response headers, plus
/// the backoff between them. Call it from a worker thread, never from a UI or frame loop.
///
/// It also must not be called from inside an async runtime: reqwest's blocking client panics there. Use
/// `spawn_blocking` (or a plain thread) when the caller is async.
#[derive(Debug)]
pub struct HttpFetcher {
    client: Result<Client, String>,
    tries: u32,
    backoff: Duration,
    allow_loopback_http: bool,
}

impl HttpFetcher {
    /// A fetcher with the default settings. Infallible: if the client can't be built, `open` returns the reason.
    #[must_use]
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// The settings, to change before [`HttpFetcherBuilder::build`].
    #[must_use]
    pub fn builder() -> HttpFetcherBuilder {
        HttpFetcherBuilder::default()
    }
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher for HttpFetcher {
    fn open(&self, url: &str, from: u64) -> Result<Body, FetchError> {
        let net = |reason: String| FetchError::Network { url: url.to_owned(), reason };
        let client = self.client.as_ref().map_err(|e| net(format!("the HTTP client could not be built: {e}")))?;
        let parsed = Url::parse(url).map_err(|e| net(format!("not a valid address: {e}")))?;
        if !scheme_allowed(&parsed, self.allow_loopback_http) {
            return Err(net("only https addresses are allowed".to_owned()));
        }
        let mut last = String::new();
        for attempt in 0..self.tries {
            if attempt > 0 {
                std::thread::sleep(backoff_for(self.backoff, attempt));
            }
            let mut request = client.get(parsed.clone()).header(ACCEPT_ENCODING, "identity");
            if from > 0 {
                request = request.header(RANGE, format!("bytes={from}-"));
            }
            match request.send() {
                Ok(response) if retryable(response.status()) => last = format!("HTTP {}", response.status()),
                Ok(response) => return into_body(response, from).map_err(net),
                Err(e) if e.is_connect() || e.is_timeout() => last = e.to_string(),
                Err(e) => return Err(net(e.to_string())),
            }
        }
        Err(net(format!("{last} (gave up after {} tries)", self.tries)))
    }
}

/// The wait before try number `attempt` (counting from 1 for the second try): `base` doubled each time, saturating.
fn backoff_for(base: Duration, attempt: u32) -> Duration {
    let factor = 1u32.checked_shl(attempt.saturating_sub(1)).unwrap_or(u32::MAX);
    base.saturating_mul(factor)
}

/// Whether a status is worth another try: a server error or "too many requests".
fn retryable(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

/// https always; plain http only to a loopback host when `allow_loopback` (the test aid).
fn scheme_allowed(url: &Url, allow_loopback: bool) -> bool {
    match url.scheme() {
        "https" => true,
        "http" if allow_loopback => match url.host_str() {
            Some("localhost") => true,
            Some(host) => host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()),
            None => false,
        },
        _ => false,
    }
}

/// Checks the status and the `Content-Range` against the request and wraps the still-unread body.
fn into_body(response: Response, from: u64) -> Result<Body, String> {
    let status = response.status();
    let start = match status {
        StatusCode::PARTIAL_CONTENT => {
            let given = response.headers().get(CONTENT_RANGE).and_then(|v| v.to_str().ok()).and_then(range_start);
            if given != Some(from) {
                return Err(format!("HTTP 206 with a Content-Range that does not start at {from}"));
            }
            from
        }
        s if s.is_success() => 0,
        s => return Err(format!("HTTP {s}")),
    };
    Ok(Body { start, reader: Box::new(response) as Box<dyn Read + Send> })
}

/// The first byte of a `Content-Range: bytes <start>-<end>/<total>` value.
fn range_start(value: &str) -> Option<u64> {
    value.strip_prefix("bytes ")?.split('-').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_start_is_parsed() {
        assert_eq!(range_start("bytes 5-9/10"), Some(5));
        assert_eq!(range_start("bytes */10"), None);
        assert_eq!(range_start("items 5-9/10"), None);
    }

    #[test]
    fn backoff_doubles_and_saturates_for_huge_retry_counts() {
        let base = Duration::from_millis(500);
        assert_eq!(backoff_for(base, 1), base);
        assert_eq!(backoff_for(base, 3), base * 4);
        assert_eq!(backoff_for(base, 32), base.saturating_mul(1 << 31));
        assert_eq!(backoff_for(base, 33), base.saturating_mul(u32::MAX));
        assert_eq!(backoff_for(base, u32::MAX), base.saturating_mul(u32::MAX));
        assert_eq!(backoff_for(Duration::ZERO, 100), Duration::ZERO);
    }

    #[test]
    fn plain_http_is_allowed_only_to_loopback_when_asked() {
        let ok = |s: &str, allow| scheme_allowed(&Url::parse(s).unwrap_or_else(|_| unreachable!()), allow);
        assert!(ok("https://example.com/a", false));
        assert!(!ok("http://127.0.0.1/a", false));
        assert!(ok("http://127.0.0.1:9/a", true));
        assert!(ok("http://[::1]/a", true));
        assert!(ok("http://localhost/a", true));
        assert!(!ok("http://example.com/a", true));
        assert!(!ok("ftp://127.0.0.1/a", true));
    }
}
