//! The turn driver both providers share (llm-api D1, D5): one task per turn that sends the request, retries before
//! the first event, decodes the SSE body into [`LlmEvent`]s and pushes them into the bounded channel.
//!
//! A provider supplies a [`Protocol`] (its URL, headers, error mapping and event decoder); nothing else differs.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::provider::{SecretLabel, SecretStore, SecretValue};
use omuzaki_llm_api::stream::{Cancel, LlmEvent, TurnStream};
use tokio::runtime::Handle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::chan::{self, Closed, Sender};
use crate::http::{HeaderValue, Http, HttpFail, MAX_ERROR_BODY, Request};
use crate::retry::Backoff;
use crate::sse::{self, SseEvent, SseFail};

/// How often a running turn looks at its [`Cancel`] flag (llm-api D5: the connection closes within 50 ms).
const CANCEL_TICK: Duration = Duration::from_millis(10);

/// The waits of llm-api D5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// How long to wait for the response headers, and then for the first event.
    pub first_byte: Duration,
    /// The longest silence between two chunks of the body. Keep-alive pings count.
    pub idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self { first_byte: Duration::from_secs(120), idle: Duration::from_secs(60) }
    }
}

/// A failed attempt.
#[derive(Debug)]
pub(crate) enum Fail {
    /// Surface it at once.
    Fatal(LlmError),
    /// Worth another try if nothing has been sent to the caller yet.
    Retry {
        /// What the caller sees if the retries run out (or if an event was already sent).
        error: LlmError,
        /// The server's `retry-after`.
        retry_after: Option<Duration>,
        /// Why, for the `Retrying` event; never holds a key.
        cause: String,
    },
}

impl Fail {
    /// A retryable network failure.
    pub(crate) fn network(cause: impl Into<String>) -> Self {
        let cause = cause.into();
        Self::Retry { error: LlmError::Network(cause.clone()), retry_after: None, cause }
    }

    /// A retryable overload.
    pub(crate) fn overloaded() -> Self {
        Self::Retry { error: LlmError::Overloaded, retry_after: None, cause: "the provider is overloaded".into() }
    }
}

/// What differs between providers.
pub(crate) trait Protocol: Send + Sync + 'static {
    /// The request URL.
    fn url(&self) -> String;
    /// The headers; `key` is the resolved secret, if the provider has one.
    fn headers(&self, key: Option<&SecretValue>) -> Result<Vec<(&'static str, HeaderValue)>, LlmError>;
    /// Whether a missing key is an `Auth` failure (Anthropic) or just no header (a local server).
    fn key_required(&self) -> bool;
    /// Maps a non-2xx answer: `status`, the `retry-after`, and the (possibly empty) body.
    fn classify(&self, status: u16, retry_after: Option<Duration>, body: &[u8]) -> Fail;
    /// A fresh decoder for one attempt's body.
    fn decoder(&self) -> Box<dyn Decoder>;
}

/// Turns the SSE events of one attempt into [`LlmEvent`]s.
pub(crate) trait Decoder: Send {
    /// The events one SSE event produces. The last event of a turn is `Stop`.
    fn event(&mut self, event: SseEvent) -> Result<Vec<LlmEvent>, Fail>;
    /// The body ended without a `Stop`: the stream was cut.
    fn cut(&mut self) -> Fail;
}

/// Everything one turn needs once it runs.
pub(crate) struct Job {
    pub http: Http,
    pub protocol: Arc<dyn Protocol>,
    pub body: Vec<u8>,
    pub key: Option<(SecretLabel, Arc<dyn SecretStore>)>,
    pub max_retries: u8,
    pub timeouts: Timeouts,
    pub seed: u64,
}

/// Starts `job` on `runtime` and returns the stream that reads it. Does no I/O on the calling thread.
pub(crate) fn spawn(runtime: &Handle, job: Job, cancel: &Cancel) -> TurnStream {
    let (tx, rx) = chan::channel();
    let token = CancellationToken::new();
    runtime.spawn(run(job, cancel.clone(), token.clone(), tx));
    TurnStream::new(Box::new(Guarded { rx, token }), cancel)
}

/// The consumer's end; dropping it stops the task and so closes the connection.
struct Guarded {
    rx: chan::Receiver,
    token: CancellationToken,
}

impl omuzaki_llm_api::stream::EventSource for Guarded {
    fn poll(&mut self, wait: Duration) -> Result<omuzaki_llm_api::stream::Next, LlmError> {
        self.rx.poll(wait)
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

/// Resolves when the caller cancelled or dropped the stream.
async fn stopped(cancel: &Cancel, token: &CancellationToken) {
    let mut tick = tokio::time::interval(CANCEL_TICK);
    loop {
        tokio::select! {
            () = token.cancelled() => return,
            _ = tick.tick() => if cancel.is_cancelled() { return },
        }
    }
}

async fn run(job: Job, cancel: Cancel, token: CancellationToken, mut tx: Sender) {
    // `biased`: a stop request wins over a ready body chunk, so cancellation never waits behind the network.
    let result = tokio::select! {
        biased;
        () = stopped(&cancel, &token) => None,
        r = drive(&job, &tx) => Some(r),
    };
    match result {
        Some(Ok(())) => tx.finish(),
        Some(Err(error)) => tx.fail(error).await,
        // Cancelled: `TurnStream` already answers `Cancelled` from the flag; the consumer needs nothing queued.
        None => tx.finish(),
    }
}

/// Redacts `secret` from `text`, in case a server echoes it.
fn redact(text: String, secret: Option<&SecretValue>) -> String {
    match secret {
        Some(s) if !s.expose().is_empty() => text.replace(s.expose(), "<redacted>"),
        _ => text,
    }
}

async fn drive(job: &Job, tx: &Sender) -> Result<(), LlmError> {
    let mut backoff = Backoff::new(job.seed);
    let mut attempt = 0u8;
    loop {
        let mut started = false;
        let failure = match attempt_once(job, tx, &mut started).await {
            Ok(()) => return Ok(()),
            Err(failure) => failure,
        };
        let (error, retry_after, cause) = match failure {
            Fail::Fatal(error) => return Err(error),
            Fail::Retry { error, retry_after, cause } => (error, retry_after, cause),
        };
        // Once an event has gone out, a retry would repeat it: surface the failure instead.
        if started || attempt >= job.max_retries {
            return Err(error);
        }
        attempt += 1;
        let wait = backoff.wait(attempt, retry_after);
        tx.event(LlmEvent::retrying(attempt, wait, cause)).await.map_err(|Closed| LlmError::Cancelled)?;
        tokio::time::sleep(wait).await;
    }
}

async fn attempt_once(job: &Job, tx: &Sender, started: &mut bool) -> Result<(), Fail> {
    let secret = match &job.key {
        Some((label, store)) => store.get(label),
        None => None,
    };
    if secret.is_none() && job.protocol.key_required() {
        return Err(Fail::Fatal(LlmError::Auth));
    }
    let headers = job.protocol.headers(secret.as_ref()).map_err(Fail::Fatal)?;
    let request = Request::post(job.protocol.url(), headers, job.body.clone());
    let response = match timeout(job.timeouts.first_byte, job.http.send(request)).await {
        Err(_) => return Err(Fail::network("no response before the first-byte timeout")),
        Ok(Err(fail)) => return Err(http_fail(fail, secret.as_ref())),
        Ok(Ok(response)) => response,
    };
    if !(200..300).contains(&response.status) {
        let (status, retry_after) = (response.status, response.retry_after);
        let body = match timeout(job.timeouts.idle, response.read_limited(MAX_ERROR_BODY)).await {
            Ok(Ok(body)) => body,
            // The status alone decides how to treat the failure; a body that cannot be read adds nothing.
            Ok(Err(_)) | Err(_) => Vec::new(),
        };
        return Err(job.protocol.classify(status, retry_after, &body));
    }
    let mut events = Box::pin(sse::events(response.stream()));
    let mut decoder = job.protocol.decoder();
    let mut wait = job.timeouts.first_byte;
    loop {
        let item = match timeout(wait, events.next()).await {
            Err(_) => return Err(Fail::network("no data before the timeout")),
            Ok(item) => item,
        };
        wait = job.timeouts.idle;
        let event = match item {
            None => return Err(decoder.cut()),
            Some(Err(SseFail::Transport(why))) => return Err(Fail::network(redact(why, secret.as_ref()))),
            Some(Err(SseFail::Malformed(why))) => {
                return Err(Fail::Fatal(LlmError::Malformed(format!("malformed event stream: {why}"))));
            }
            Some(Ok(event)) => event,
        };
        for out in decoder.event(event)? {
            let done = matches!(out, LlmEvent::Stop(_));
            *started |= matches!(out, LlmEvent::Start { .. });
            tx.event(out).await.map_err(|Closed| Fail::Fatal(LlmError::Cancelled))?;
            if done {
                return Ok(());
            }
        }
    }
}

fn http_fail(fail: HttpFail, secret: Option<&SecretValue>) -> Fail {
    match fail {
        HttpFail::Connect(why) | HttpFail::Body(why) => Fail::network(redact(why, secret)),
        HttpFail::Request(why) => Fail::Fatal(LlmError::Malformed(redact(why, secret))),
    }
}

/// The error text a server sent, cut to a line a log can hold.
pub(crate) fn short(message: &str) -> String {
    const MAX: usize = 300;
    let line = message.trim();
    match line.char_indices().nth(MAX) {
        Some((at, _)) => format!("{}...", &line[..at]),
        None => line.to_owned(),
    }
}

/// The common mapping of an HTTP status (llm-api D5), once a provider has read the message out of the body.
pub(crate) fn status_fail(status: u16, retry_after: Option<Duration>, message: &str, context_window: u64) -> Fail {
    let context = {
        let m = message.to_ascii_lowercase();
        ["prompt is too long", "context_length_exceeded", "context length", "context window", "too many tokens"]
            .iter()
            .any(|needle| m.contains(needle))
    };
    match status {
        401 | 403 => Fail::Fatal(LlmError::Auth),
        413 => Fail::Fatal(LlmError::context_too_long(context_window)),
        400 if context => Fail::Fatal(LlmError::context_too_long(context_window)),
        429 => match retry_after {
            Some(wait) => Fail::Retry {
                error: LlmError::rate_limited(Some(wait)),
                retry_after: Some(wait),
                cause: "rate limited".into(),
            },
            // No `retry-after`: Anthropic sends that for a spend cap, which keeps failing until access resumes.
            None => Fail::Fatal(LlmError::rate_limited(None)),
        },
        503 | 529 => Fail::overloaded(),
        500 | 502 | 504 => Fail::network(format!("the server answered {status}")),
        _ => Fail::Fatal(LlmError::Malformed(format!("the server answered {status}: {}", short(message)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(f: &Fail) -> String {
        match f {
            Fail::Fatal(e) => format!("fatal {e}"),
            Fail::Retry { error, retry_after, .. } => format!("retry {error} after {retry_after:?}"),
        }
    }

    #[test]
    fn statuses_map_as_the_d5_table_says() {
        let at = |status, after| kind(&status_fail(status, after, "", 1000));
        assert!(at(401, None).starts_with("fatal the provider refused the key"));
        assert!(at(403, None).starts_with("fatal the provider refused the key"));
        assert!(at(413, None).contains("1000-token"));
        assert!(at(429, Some(Duration::from_secs(2))).starts_with("retry rate limited, retry after 2 s after Some(2s)"));
        assert_eq!(at(429, None), "fatal rate limited");
        for status in [500, 502, 504] {
            assert!(at(status, None).starts_with("retry network error"), "{status}");
        }
        for status in [503, 529] {
            assert!(at(status, None).starts_with("retry the provider is overloaded"), "{status}");
        }
        assert!(at(400, None).starts_with("fatal malformed"));
        assert!(at(404, None).starts_with("fatal malformed"));
    }

    #[test]
    fn a_400_that_names_the_context_is_context_too_long() {
        for message in ["prompt is too long: 250000 tokens > 200000", "context_length_exceeded", "maximum context length is 8192"] {
            assert!(kind(&status_fail(400, None, message, 8192)).contains("8192-token"), "{message}");
        }
    }

    #[test]
    fn a_server_message_is_cut_and_a_secret_is_redacted() {
        assert!(short(&"x".repeat(1000)).len() < 310);
        let key = SecretValue::new("sk-abc");
        assert_eq!(redact("bad key sk-abc!".into(), Some(&key)), "bad key <redacted>!");
        assert_eq!(redact("same".into(), None), "same");
    }
}
