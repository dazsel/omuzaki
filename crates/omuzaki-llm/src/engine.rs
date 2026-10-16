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
use crate::retry::{Backoff, DEFAULT_UNIT};
use crate::sse::{self, SseEvent, SseFail};

/// How often a running turn looks at its [`Cancel`] flag (llm-api D5: the connection closes within 50 ms).
const CANCEL_TICK: Duration = Duration::from_millis(10);

/// The waits of llm-api D5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// How long to wait for the response headers, and then for the first event.
    pub first_byte: Duration,
    /// The longest silence between two chunks of the body. Any bytes count, a keep-alive comment included.
    pub idle: Duration,
    /// How long one metadata request (model list, one capability probe request) may take, headers and body together.
    pub metadata: Duration,
    /// How long the whole capability probe of `OpenAiClient::new` may take, all its requests together.
    pub probe: Duration,
    /// The unit of the retry jitter: the wait before retry `n` is uniform in `[0, min(16, 2^(n-1)) units]`. One second
    /// outside tests.
    pub backoff_unit: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            first_byte: Duration::from_secs(120),
            idle: Duration::from_secs(60),
            metadata: Duration::from_secs(30),
            probe: Duration::from_secs(60),
            backoff_unit: DEFAULT_UNIT,
        }
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
        Self::network_after(cause, None)
    }

    /// A retryable network failure, with the `retry-after` the server sent.
    pub(crate) fn network_after(cause: impl Into<String>, retry_after: Option<Duration>) -> Self {
        let cause = cause.into();
        Self::Retry { error: LlmError::Network(cause.clone()), retry_after, cause }
    }

    /// A retryable overload, with the `retry-after` the server sent.
    pub(crate) fn overloaded(retry_after: Option<Duration>) -> Self {
        Self::Retry { error: LlmError::Overloaded, retry_after, cause: "the provider is overloaded".into() }
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
    /// Maps a non-2xx answer: `status`, the `retry-after`, and the (possibly empty) body. `secret` is redacted from
    /// every message that is read out of the body.
    fn classify(&self, status: u16, retry_after: Option<Duration>, body: &[u8], secret: Option<&SecretValue>) -> Fail;
    /// A fresh decoder for one attempt's body; it redacts `secret` from the messages it reads.
    fn decoder(&self, secret: Option<SecretValue>) -> Box<dyn Decoder>;
}

/// Turns the SSE events of one attempt into [`LlmEvent`]s.
pub(crate) trait Decoder: Send {
    /// The events one SSE event produces. The last event of a turn is `Stop`.
    fn event(&mut self, event: SseEvent) -> Result<Vec<LlmEvent>, Fail>;
    /// The body ended. A decoder whose protocol has no closing event finishes the turn here (the events end in `Stop`);
    /// otherwise the stream was cut.
    fn end(&mut self) -> Result<Vec<LlmEvent>, Fail>;
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

/// Starts the turn on `runtime` and returns the stream that reads it. `make` builds the job, so the encoding of the
/// history and the images runs on the task and never on the calling thread (llm-api D1).
pub(crate) fn spawn(runtime: &Handle, make: impl FnOnce() -> Job + Send + 'static, cancel: &Cancel) -> TurnStream {
    let (tx, rx) = chan::channel();
    let token = CancellationToken::new();
    runtime.spawn(run(make, cancel.clone(), token.clone(), tx));
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

async fn run(make: impl FnOnce() -> Job + Send + 'static, cancel: Cancel, token: CancellationToken, mut tx: Sender) {
    // The encoding of a large history (base64 of many images) is CPU work: it runs on the blocking pool, so it never
    // holds the async worker that polls other turns' sockets and cancel ticks, and a cancel does not wait for it.
    let built = tokio::select! {
        biased;
        () = stopped(&cancel, &token) => None,
        built = tokio::task::spawn_blocking(make) => Some(built),
    };
    let job = match built {
        None => return tx.finish(),
        Some(Ok(job)) => job,
        // The encoder panicked: a turn that never started is a failure, not an empty success.
        Some(Err(_)) => return tx.fail(LlmError::Malformed("the request could not be built".into())).await,
    };
    // `biased`: a stop request wins over a ready body chunk, so cancellation never waits behind the network.
    let result = tokio::select! {
        biased;
        () = stopped(&cancel, &token) => None,
        r = drive(&job, &tx) => Some(r),
    };
    match result {
        // Cancelled (`None`): `TurnStream` already answers `Cancelled` from the flag, so the consumer needs nothing queued.
        Some(Ok(())) | None => tx.finish(),
        Some(Err(error)) => tx.fail(error).await,
    }
}

/// The most a decoder holds for one turn's buffered pieces (tool arguments held whole, thinking text): a server cannot
/// make the client grow without bound.
pub(crate) const MAX_BUFFERED: usize = 4 * 1024 * 1024;

/// Redacts `secret` from the bytes of an answer. Callers that cut a body to a limit redact first (`read_limited`): a key
/// that sits across a truncation point would otherwise survive in part.
pub(crate) fn redact_bytes(body: Vec<u8>, secret: Option<&SecretValue>) -> Vec<u8> {
    match secret {
        Some(s) if !s.expose().is_empty() => redact(String::from_utf8_lossy(&body).into_owned(), Some(s)).into_bytes(),
        _ => body,
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
    let mut backoff = Backoff::new(job.seed, job.timeouts.backoff_unit);
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

/// Redacts `secret` from the text of `error`.
pub(crate) fn redact_error(error: LlmError, secret: Option<&SecretValue>) -> LlmError {
    match error {
        LlmError::Unsupported(m) => LlmError::Unsupported(redact(m, secret)),
        LlmError::Network(m) => LlmError::Network(redact(m, secret)),
        LlmError::Malformed(m) => LlmError::Malformed(redact(m, secret)),
        other => other,
    }
}

/// Runs one attempt. Whatever a server echoes in an error, the key never leaves in it (llm-api D5).
async fn attempt_once(job: &Job, tx: &Sender, started: &mut bool) -> Result<(), Fail> {
    let secret = match &job.key {
        Some((label, store)) => store.get(label),
        None => None,
    };
    attempt_with(job, tx, started, secret.as_ref()).await.map_err(|fail| match fail {
        Fail::Fatal(error) => Fail::Fatal(redact_error(error, secret.as_ref())),
        Fail::Retry { error, retry_after, cause } => Fail::Retry {
            error: redact_error(error, secret.as_ref()),
            retry_after,
            cause: redact(cause, secret.as_ref()),
        },
    })
}

async fn attempt_with(job: &Job, tx: &Sender, started: &mut bool, secret: Option<&SecretValue>) -> Result<(), Fail> {
    if secret.is_none() && job.protocol.key_required() {
        return Err(Fail::Fatal(LlmError::Auth));
    }
    let headers = job.protocol.headers(secret).map_err(Fail::Fatal)?;
    let request = Request::post(job.protocol.url(), headers, job.body.clone());
    let response = match timeout(job.timeouts.first_byte, job.http.send(request)).await {
        Err(_) => return Err(Fail::network("no response before the first-byte timeout")),
        Ok(Err(fail)) => return Err(http_fail(fail, secret)),
        Ok(Ok(response)) => response,
    };
    if !(200..300).contains(&response.status) {
        let (status, retry_after) = (response.status, response.retry_after);
        let body = match timeout(job.timeouts.idle, response.read_limited(MAX_ERROR_BODY, secret)).await {
            Ok(Ok(body)) => body,
            // The status alone decides how to treat the failure; a body that cannot be read adds nothing.
            Ok(Err(_)) | Err(_) => Vec::new(),
        };
        return Err(job.protocol.classify(status, retry_after, &body, secret));
    }
    let (events, activity) = sse::events(response.stream());
    let mut events = Box::pin(events);
    let mut decoder = job.protocol.decoder(secret.cloned());
    let mut wait = job.timeouts.first_byte;
    loop {
        // The deadline runs from the latest bytes, so a keep-alive comment (bytes, no event) restarts it at once and an
        // event never leaves a stale mark behind.
        let item = loop {
            let deadline = activity.last() + wait;
            match tokio::time::timeout_at(deadline, events.next()).await {
                Ok(item) => break item,
                Err(_) if activity.last() + wait > tokio::time::Instant::now() => {}
                Err(_) => return Err(Fail::network("no data before the timeout")),
            }
        };
        wait = job.timeouts.idle;
        let event = match item {
            None => {
                if send_all(tx, decoder.end()?, started).await? {
                    return Ok(());
                }
                return Err(Fail::network("the event stream ended before the turn completed"));
            }
            Some(Err(SseFail::Transport(why))) => return Err(Fail::network(redact(why, secret))),
            Some(Err(SseFail::Malformed(why))) => {
                return Err(Fail::Fatal(LlmError::Malformed(format!("malformed event stream: {why}"))));
            }
            Some(Ok(event)) => event,
        };
        let event = SseEvent { data: redact(event.data, secret), ..event };
        if send_all(tx, decoder.event(event)?, started).await? {
            return Ok(());
        }
    }
}

/// Queues `events` for the consumer, noting whether `Start` went out. True once `Stop` has: the turn is over.
async fn send_all(tx: &Sender, events: Vec<LlmEvent>, started: &mut bool) -> Result<bool, Fail> {
    for out in events {
        let done = matches!(out, LlmEvent::Stop(_));
        *started |= matches!(out, LlmEvent::Start { .. });
        tx.event(out).await.map_err(|Closed| Fail::Fatal(LlmError::Cancelled))?;
        if done {
            return Ok(true);
        }
    }
    Ok(false)
}

fn http_fail(fail: HttpFail, secret: Option<&SecretValue>) -> Fail {
    match fail {
        HttpFail::Connect(why) | HttpFail::Body(why) => Fail::network(redact(why, secret)),
        HttpFail::Request(why) => Fail::Fatal(LlmError::Malformed(redact(why, secret))),
    }
}

/// The error text a server sent, redacted and then cut to a line a log can hold.
pub(crate) fn short(message: &str, secret: Option<&SecretValue>) -> String {
    const MAX: usize = 300;
    let redacted = redact(message.to_owned(), secret);
    let line = redacted.trim();
    match line.char_indices().nth(MAX) {
        Some((at, _)) => format!("{}...", &line[..at]),
        None => line.to_owned(),
    }
}

/// The common mapping of an HTTP status (llm-api D5), once a provider has read the message out of the body.
pub(crate) fn status_fail(
    status: u16,
    retry_after: Option<Duration>,
    message: &str,
    context_window: u64,
    secret: Option<&SecretValue>,
) -> Fail {
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
        503 | 529 => Fail::overloaded(retry_after),
        500 | 502 | 504 => Fail::network_after(format!("the server answered {status}"), retry_after),
        _ => Fail::Fatal(LlmError::Malformed(format!("the server answered {status}: {}", short(message, secret)))),
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

    /// A protocol for a loopback server that sends no events.
    struct Quiet(String);

    struct NoEvents;

    impl Decoder for NoEvents {
        fn event(&mut self, _: SseEvent) -> Result<Vec<LlmEvent>, Fail> {
            Ok(Vec::new())
        }
        fn end(&mut self) -> Result<Vec<LlmEvent>, Fail> {
            Err(Fail::network("the quiet server hung up"))
        }
    }

    impl Protocol for Quiet {
        fn url(&self) -> String {
            self.0.clone()
        }
        fn headers(&self, _: Option<&SecretValue>) -> Result<Vec<(&'static str, HeaderValue)>, LlmError> {
            Ok(Vec::new())
        }
        fn key_required(&self) -> bool {
            false
        }
        fn classify(&self, _: u16, _: Option<Duration>, _: &[u8], _: Option<&SecretValue>) -> Fail {
            Fail::network("unused")
        }
        fn decoder(&self, _: Option<SecretValue>) -> Box<dyn Decoder> {
            Box::new(NoEvents)
        }
    }

    fn job(url: &str) -> Job {
        let endpoint = crate::http::Endpoint::parse(url).unwrap_or_else(|e| panic!("{e}"));
        Job {
            http: Http::new(&endpoint).unwrap_or_else(|e| panic!("{e}")),
            protocol: Arc::new(Quiet(format!("{url}/v1/x"))),
            body: Vec::new(),
            key: None,
            max_retries: 0,
            timeouts: Timeouts::default(),
            seed: 1,
        }
    }

    /// A server that answers every connection with an open event stream and reports, on `closed`, the instant the
    /// client hung up. `connected` gets one message per accepted connection.
    fn quiet_server() -> (String, std::sync::mpsc::Receiver<()>, std::sync::mpsc::Receiver<std::time::Instant>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
        let url = format!("http://{}", listener.local_addr().unwrap_or_else(|e| panic!("{e}")));
        let (connected_tx, connected) = std::sync::mpsc::channel();
        let (closed_tx, closed) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let (connected_tx, closed_tx) = (connected_tx.clone(), closed_tx.clone());
                std::thread::spawn(move || {
                    let mut seen = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => seen.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n:open\n\n";
                    if stream.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    let _ = connected_tx.send(());
                    while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
                    let _ = closed_tx.send(std::time::Instant::now());
                });
            }
        });
        (url, connected, closed)
    }

    #[test]
    fn a_cancel_closes_the_connection_within_50_ms_while_another_request_is_being_built() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        let (url, connected, closed) = quiet_server();
        let cancel_a = Cancel::new();
        let url_a = url.clone();
        let _a = spawn(rt.handle(), move || job(&url_a), &cancel_a);
        connected.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|e| panic!("turn A never connected: {e}"));
        // Turn B builds its request for 400 ms (standing for the base64 of many images) on the same one-worker runtime.
        let (building_tx, building) = std::sync::mpsc::channel();
        let cancel_b = Cancel::new();
        let _b = spawn(
            rt.handle(),
            move || {
                let _ = building_tx.send(());
                std::thread::sleep(Duration::from_millis(400));
                job(&url)
            },
            &cancel_b,
        );
        building.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|e| panic!("turn B never began: {e}"));
        let cancelled_at = std::time::Instant::now();
        cancel_a.cancel();
        let hung_up = closed.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|e| panic!("never closed: {e}"));
        let took = hung_up.saturating_duration_since(cancelled_at);
        assert!(took <= Duration::from_millis(50), "the connection closed {took:?} after cancel()");
        cancel_b.cancel();
    }

    #[test]
    fn statuses_map_as_the_d5_table_says() {
        let at = |status, after| kind(&status_fail(status, after, "", 1000, None));
        assert!(at(401, None).starts_with("fatal the provider refused the key"));
        assert!(at(403, None).starts_with("fatal the provider refused the key"));
        assert!(at(413, None).contains("1000-token"));
        assert!(
            at(429, Some(Duration::from_secs(2))).starts_with("retry rate limited, retry after 2 s after Some(2s)")
        );
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
        for message in
            ["prompt is too long: 250000 tokens > 200000", "context_length_exceeded", "maximum context length is 8192"]
        {
            assert!(kind(&status_fail(400, None, message, 8192, None)).contains("8192-token"), "{message}");
        }
    }

    #[test]
    fn a_server_message_is_cut_and_a_secret_is_redacted() {
        assert!(short(&"x".repeat(1000), None).len() < 310);
        let key = SecretValue::new("sk-abc");
        assert_eq!(redact("bad key sk-abc!".into(), Some(&key)), "bad key <redacted>!");
        assert_eq!(redact("same".into(), None), "same");
    }

    #[test]
    fn a_server_message_is_redacted_before_it_is_shortened() {
        let key = SecretValue::new("sk-0123456789-secret");
        for pad in [0, 280, 290, 299, 300, 305] {
            let message = format!("{}{}tail", "x".repeat(pad), key.expose());
            let shown = short(&message, Some(&key));
            assert!(!shown.contains("sk-0"), "{pad}: {shown}");
            assert!(!shown.contains("secret"), "{pad}: {shown}");
        }
    }

    #[test]
    fn a_retry_after_survives_on_server_failures_and_overloads() {
        let after = Some(Duration::from_secs(7));
        for status in [500, 502, 503, 504, 529] {
            let Fail::Retry { retry_after, .. } = status_fail(status, after, "", 1000, None) else {
                panic!("{status} must retry");
            };
            assert_eq!(retry_after, after, "{status}");
        }
    }
}
