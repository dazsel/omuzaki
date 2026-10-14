//! The resource limits of the HTTP port, and the code that enforces them.
//!
//! A token holder is trusted with the studio's ops, not with its memory, sockets or time. Everything a client can make
//! the port hold on to is therefore bounded, and every bound that refuses a request names itself and its value so the
//! client (and whoever reads the log) can see which knob to turn:
//!
//! | what | bound | where |
//! |---|---|---|
//! | open sessions per token | `max_sessions_per_token` | [`SessionSlots`] |
//! | a quiet session | `idle_timeout` | rmcp's session worker (see [`crate::access`]) |
//! | one request body's size | `max_body_bytes` | [`read_body`] |
//! | one request body's time | `body_timeout` | [`read_body`] |
//! | request headers' time, open connections | `header_timeout`, `max_connections` | [`crate::http`] |
//!
//! Time is tokio's, so tests pause and advance the clock (`tokio::time::pause`) instead of sleeping.

use std::borrow::Cow;
use std::fmt::Display;
use std::pin::pin;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use http_body_util::BodyExt;
use hyper::HeaderMap;
use hyper::body::Body;
use rmcp::transport::common::http_header::HEADER_SESSION_ID;
use serde::Deserialize;
use tokio::sync::{Mutex, MutexGuard};
use tokio::time::Instant;

use crate::HttpOptions;

/// The limits of one HTTP port, copied out of [`HttpOptions`](crate::HttpOptions) once it has been checked.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Limits {
    /// See [`HttpOptions::idle_timeout`](crate::HttpOptions::idle_timeout).
    pub(crate) idle_timeout: Duration,
    /// See [`HttpOptions::max_sessions_per_token`](crate::HttpOptions::max_sessions_per_token).
    pub(crate) max_sessions_per_token: usize,
    /// See [`HttpOptions::max_body_bytes`](crate::HttpOptions::max_body_bytes).
    pub(crate) max_body_bytes: usize,
    /// See [`HttpOptions::body_timeout`](crate::HttpOptions::body_timeout).
    pub(crate) body_timeout: Duration,
}

// ---- request bodies -----------------------------------------------------------------------------------------------

/// Why a request body was not accepted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BodyRefusal {
    /// More than `max_body_bytes`, declared up front or counted while it arrived.
    TooLarge,
    /// Not all of it arrived within `body_timeout`.
    TimedOut,
    /// The connection failed while the body was arriving.
    Unreadable(String),
}

/// Reads a whole request body, refusing it as soon as it is over `max_bytes` or `timeout` has passed.
///
/// The timeout is a deadline for the *whole* body, started when this is called, rather than a gap between chunks: a
/// client that drips one byte just inside a per-chunk limit would otherwise hold its connection (and one of
/// `max_connections`) for as long as the size limit allows, which is the slow-loris attack the limit is there to stop.
/// A body declared larger than `max_bytes` is refused before a byte of it is read. Nothing is buffered beyond
/// `max_bytes`. A `timeout` over [`HttpOptions::MAX_TIMEOUT`] counts as that maximum, so no value can overflow the
/// deadline; if the deadline could not be built at all the body is timed out at once rather than left unbounded.
pub(crate) async fn read_body<B>(body: B, max_bytes: usize, timeout: Duration) -> Result<Bytes, BodyRefusal>
where
    B: Body,
    B::Error: Display,
{
    let limit = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if body.size_hint().lower() > limit {
        return Err(BodyRefusal::TooLarge);
    }
    let now = Instant::now();
    let deadline = now.checked_add(timeout.min(HttpOptions::MAX_TIMEOUT)).unwrap_or(now);
    let mut body = pin!(body);
    let mut collected = BytesMut::new();
    loop {
        let frame = match tokio::time::timeout_at(deadline, body.frame()).await {
            Err(_elapsed) => return Err(BodyRefusal::TimedOut),
            Ok(None) => return Ok(collected.freeze()),
            Ok(Some(Err(error))) => return Err(BodyRefusal::Unreadable(error.to_string())),
            Ok(Some(Ok(frame))) => frame,
        };
        // Trailers carry no JSON-RPC, so they are skipped rather than counted.
        if let Ok(data) = frame.into_data() {
            if data.remaining() > max_bytes - collected.len() {
                return Err(BodyRefusal::TooLarge);
            }
            collected.put(data);
        }
    }
}

// ---- sessions -----------------------------------------------------------------------------------------------------

/// What `Mcp-Session-Id`-less requests are checked for: the JSON-RPC `method`, and nothing else.
#[derive(Deserialize)]
struct MethodOnly<'a> {
    #[serde(borrow)]
    method: Option<Cow<'a, str>>,
}

/// Whether rmcp may open a new session to serve this POST.
///
/// Only an `initialize` request without a session id does (anything else either belongs to an existing session or is
/// answered statelessly), so counting just those keeps a client at its cap able to make stateless calls. The check
/// fails closed: a body that does not parse as a JSON object with at most one `method` counts as opening a session,
/// because rmcp parses the same bytes with its own rules and the cap must not depend on the two agreeing about
/// malformed or duplicated keys. (An object that parses and has no `method` is a response, which rmcp refuses.) A
/// session id header that is not visible ASCII is ignored by rmcp, so it is ignored here too.
pub(crate) fn may_open_session(headers: &HeaderMap, body: &[u8]) -> bool {
    let names_a_session = headers.get(HEADER_SESSION_ID).and_then(|value| value.to_str().ok()).is_some();
    if names_a_session {
        return false;
    }
    match serde_json::from_slice::<MethodOnly<'_>>(body) {
        Ok(MethodOnly { method }) => method.is_some_and(|method| method == "initialize"),
        Err(_) => true,
    }
}

/// Why a token may not open another session.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SessionsFull {
    /// Sessions the token has open.
    pub(crate) open: usize,
    /// `max_sessions_per_token`.
    pub(crate) max: usize,
}

/// Admission control for one token's sessions: at most `max` open at a time.
///
/// Checking `sessions.len()` alone would let a burst of concurrent `initialize`s all see room and all create a session.
/// So admissions take turns: one request at a time per token is checked *and* served (the [`Admission`] is held until
/// rmcp has answered, by which time the new session is in its table), and the next sees the true count. A session can
/// only be added through this path, and others only leave, so the count never goes above `max`. `initialize` is rare
/// and quick, so the turn is short, and a client that hangs up mid-request drops its turn with the request.
pub(crate) struct SessionSlots {
    max: usize,
    /// Whose turn it is to open a session.
    turn: Mutex<()>,
    /// Whether the last refusal was already logged at `warn`; reset by the next admission. A client that loops on
    /// reconnect at its cap would otherwise write a warning per attempt.
    full_logged: AtomicBool,
}

/// The turn to open a session, held until the request that opens it has been served.
pub(crate) struct Admission<'a> {
    /// Released when the admission is dropped.
    _turn: MutexGuard<'a, ()>,
}

impl SessionSlots {
    pub(crate) fn new(max: usize) -> Self {
        Self { max, turn: Mutex::new(()), full_logged: AtomicBool::new(false) }
    }

    /// Waits for a turn, then admits the request if the token has fewer than `max` sessions. `count_open` is only
    /// called once the turn is held (which is why it is a function and not a count): a number read earlier could be
    /// out of date by the time the previous request's session has appeared.
    pub(crate) async fn admit(&self, count_open: impl AsyncFnOnce() -> usize) -> Result<Admission<'_>, SessionsFull> {
        let turn = self.turn.lock().await;
        let open = count_open().await;
        if open >= self.max {
            return Err(SessionsFull { open, max: self.max });
        }
        self.full_logged.store(false, Ordering::Relaxed);
        Ok(Admission { _turn: turn })
    }

    /// True the first time it is called after a successful admission: the caller logs that refusal loudly and the
    /// repeats quietly.
    pub(crate) fn first_refusal(&self) -> bool {
        !self.full_logged.swap(true, Ordering::Relaxed)
    }
}

// ---- refusal logging ----------------------------------------------------------------------------------------------

/// How often one kind of refusal may be written at `warn`. A client looping on bad requests would otherwise write a
/// line per request, which buries the events that matter; the refusals in between are still logged, at `debug`.
pub(crate) const REFUSAL_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Decides which refusals of one kind are written at `warn` and which at `debug`: at most one `warn` per interval.
pub(crate) struct RefusalLog {
    interval: Duration,
    state: std::sync::Mutex<RefusalState>,
}

#[derive(Default)]
struct RefusalState {
    /// When the last `warn` was written; `None` before the first refusal.
    last_warn: Option<Instant>,
    /// Refusals since then that were only logged at `debug`.
    suppressed: u64,
}

impl RefusalLog {
    pub(crate) fn new(interval: Duration) -> Self {
        Self { interval, state: std::sync::Mutex::new(RefusalState::default()) }
    }

    /// Called once per refusal. `Some(n)` means write it at `warn`, with `n` refusals suppressed since the last `warn`
    /// (zero for the first); `None` means write it at `debug`. Time is tokio's, so tests move it with the paused clock.
    pub(crate) fn warn_now(&self) -> Option<u64> {
        let now = Instant::now();
        // The lock is held for a few integer operations only, and a poisoned one still holds valid counts.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let due = state.last_warn.is_none_or(|last| now.saturating_duration_since(last) >= self.interval);
        if !due {
            state.suppressed += 1;
            return None;
        }
        state.last_warn = Some(now);
        Some(std::mem::take(&mut state.suppressed))
    }

    /// Refusals suppressed since the last `warn`.
    #[cfg(test)]
    pub(crate) fn suppressed(&self) -> u64 {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).suppressed
    }
}

/// Bodies for tests to feed by hand, here and in the gate's tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::convert::Infallible;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use bytes::Bytes;
    use hyper::body::{Body, Frame, SizeHint};
    use tokio::sync::mpsc;

    /// A body fed by hand: every `send` is one chunk, and while nothing is sent it is stalled, as a slow client's is.
    pub(crate) struct Fed {
        chunks: mpsc::UnboundedReceiver<Bytes>,
        /// What it claims to be (its `Content-Length`), if anything.
        pub(crate) declared: Option<u64>,
    }

    /// A stalled body and the sender that feeds it. The sender reports closed once the body has been dropped, which
    /// is how a test sees that a refused request's connection was let go.
    pub(crate) fn fed() -> (mpsc::UnboundedSender<Bytes>, Fed) {
        let (tx, chunks) = mpsc::unbounded_channel();
        (tx, Fed { chunks, declared: None })
    }

    impl Body for Fed {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
            self.chunks.poll_recv(cx).map(|chunk| chunk.map(|chunk| Ok(Frame::data(chunk))))
        }

        fn size_hint(&self) -> SizeHint {
            self.declared.map_or_else(SizeHint::default, SizeHint::with_exact)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Context, Poll};

    use http_body_util::Full;
    use hyper::body::Frame;
    use hyper::header::HeaderValue;

    use super::test_support::fed;
    use super::*;

    const MAX: usize = 16;
    const TIMEOUT: Duration = Duration::from_secs(30);

    /// A body whose connection fails after its first chunk.
    struct Broken(bool);

    impl Body for Broken {
        type Data = Bytes;
        type Error = String;

        fn poll_frame(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, String>>> {
            let first = !std::mem::replace(&mut self.0, true);
            Poll::Ready(Some(if first {
                Ok(Frame::data(Bytes::from_static(b"{")))
            } else {
                Err("reset by peer".to_owned())
            }))
        }
    }

    // ---- bodies ----

    #[tokio::test(start_paused = true)]
    async fn a_body_within_both_limits_is_read_whole() {
        let whole = read_body(Full::new(Bytes::from_static(b"{}")), MAX, TIMEOUT).await;
        assert_eq!(whole.as_deref(), Ok(&b"{}"[..]));

        let (tx, body) = fed();
        tx.send(Bytes::from_static(b"01234567")).unwrap();
        tx.send(Bytes::from_static(b"89abcdef")).unwrap(); // exactly MAX bytes in all
        drop(tx);
        assert_eq!(read_body(body, MAX, TIMEOUT).await.as_deref(), Ok(&b"0123456789abcdef"[..]));
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_declared_over_the_limit_is_refused_before_a_byte_is_read() {
        // If it were read, this stalled body would run into the timeout instead.
        let (_tx, mut body) = fed();
        body.declared = Some(MAX as u64 + 1);
        assert_eq!(read_body(body, MAX, TIMEOUT).await, Err(BodyRefusal::TooLarge));
        assert_eq!(read_body(Full::new(Bytes::from(vec![0; MAX + 1])), MAX, TIMEOUT).await, Err(BodyRefusal::TooLarge));
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_that_outgrows_its_declaration_is_refused_as_it_arrives() {
        // No declared size at all (chunked), or a false one: the count of bytes received is what is limited.
        let (tx, body) = fed();
        tx.send(Bytes::from(vec![0; MAX])).unwrap();
        tx.send(Bytes::from_static(b"!")).unwrap(); // one byte over
        assert_eq!(read_body(body, MAX, TIMEOUT).await, Err(BodyRefusal::TooLarge));
        assert!(tx.is_closed(), "the refused body was dropped, so its connection is released");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_body_times_out_exactly_at_the_limit() {
        let (tx, body) = fed();
        tx.send(Bytes::from_static(b"{\"partial\":")).unwrap();
        let started = Instant::now();
        assert_eq!(read_body(body, MAX, TIMEOUT).await, Err(BodyRefusal::TimedOut));
        assert_eq!(started.elapsed(), TIMEOUT);
        assert!(tx.is_closed(), "the stalled body was dropped, so its connection is released");
    }

    #[tokio::test(start_paused = true)]
    async fn a_trickle_cannot_outlast_the_deadline() {
        // One byte every 10 s never goes 30 s without progress, but the whole body is still due after 30 s.
        let (tx, body) = fed();
        let dripper = tokio::spawn(async move {
            while tx.send(Bytes::from_static(b" ")).is_ok() {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
        let started = Instant::now();
        assert_eq!(read_body(body, 1_000, TIMEOUT).await, Err(BodyRefusal::TimedOut));
        assert_eq!(started.elapsed(), TIMEOUT);
        dripper.await.unwrap(); // it stops on its own once the body is dropped
    }

    #[tokio::test(start_paused = true)]
    async fn a_timeout_beyond_the_maximum_counts_as_the_maximum_and_never_overflows() {
        for huge in [Duration::MAX, HttpOptions::MAX_TIMEOUT + Duration::from_secs(1)] {
            let (_tx, body) = fed();
            let started = Instant::now();
            assert_eq!(read_body(body, MAX, huge).await, Err(BodyRefusal::TimedOut), "{huge:?}");
            assert_eq!(started.elapsed(), HttpOptions::MAX_TIMEOUT, "{huge:?} was cut to the maximum");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_that_fails_mid_body_is_reported_with_its_cause() {
        assert_eq!(
            read_body(Broken(false), MAX, TIMEOUT).await,
            Err(BodyRefusal::Unreadable("reset by peer".to_owned()))
        );
    }

    // ---- which requests may open a session ----

    fn session_header(value: &[u8]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_SESSION_ID, HeaderValue::from_bytes(value).unwrap());
        headers
    }

    #[test]
    fn only_an_initialize_without_a_session_id_can_open_a_session() {
        let initialize = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let call = br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"initialize"}}"#;
        let notification = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let response = br#"{"jsonrpc":"2.0","id":3,"result":{}}"#;
        let none = HeaderMap::new();

        assert!(may_open_session(&none, initialize));
        assert!(!may_open_session(&session_header(b"abc"), initialize), "it belongs to a session already");
        for other in [&call[..], notification, response] {
            assert!(!may_open_session(&none, other), "{}", String::from_utf8_lossy(other));
        }
    }

    #[test]
    fn what_the_check_cannot_read_counts_as_opening_a_session() {
        let none = HeaderMap::new();
        for unreadable in [
            &b""[..],
            b"not json",
            b"[{\"method\":\"initialize\"}]",
            b"{\"method\":5}",
            // rmcp and this check could disagree about which of two `method`s wins, so neither is trusted as benign.
            br#"{"method":"tools/call","method":"initialize"}"#,
            &[0xff, 0xfe],
        ] {
            assert!(may_open_session(&none, unreadable), "{}", String::from_utf8_lossy(unreadable));
        }
        // rmcp reads a session id only if it is visible ASCII, and otherwise opens a session; so must this check.
        let initialize = br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        assert!(may_open_session(&session_header(&[0xe9]), initialize));
        assert!(!may_open_session(&session_header(b""), initialize), "an empty id is still an id (rmcp answers 404)");
    }

    // ---- session slots ----

    #[tokio::test]
    async fn a_token_is_admitted_while_it_has_room_and_not_once_it_is_full() {
        let slots = SessionSlots::new(2);
        let sessions = AtomicUsize::new(1);
        assert!(slots.admit(async || sessions.load(Ordering::SeqCst)).await.is_ok());
        sessions.store(2, Ordering::Relaxed);
        assert_eq!(
            slots.admit(async || sessions.load(Ordering::SeqCst)).await.err(),
            Some(SessionsFull { open: 2, max: 2 })
        );
        sessions.store(1, Ordering::Relaxed); // one ended
        assert!(slots.admit(async || sessions.load(Ordering::SeqCst)).await.is_ok());
    }

    #[tokio::test]
    async fn admissions_take_turns_so_the_count_each_one_sees_is_the_true_one() {
        let slots = SessionSlots::new(1);
        let sessions = AtomicUsize::new(0);
        let first = slots.admit(async || sessions.load(Ordering::SeqCst)).await.unwrap();
        // While the first request is being served the next waits; it does not count a session that isn't there yet.
        let waiting = tokio::time::timeout(Duration::ZERO, slots.admit(async || sessions.load(Ordering::SeqCst))).await;
        assert!(waiting.is_err(), "the second admission waits for its turn");
        sessions.store(1, Ordering::Relaxed); // the first request created its session...
        drop(first); // ...and has been answered
        assert_eq!(
            slots.admit(async || sessions.load(Ordering::SeqCst)).await.err(),
            Some(SessionsFull { open: 1, max: 1 })
        );
    }

    #[tokio::test]
    async fn only_the_first_refusal_after_an_admission_is_logged_loudly() {
        let slots = SessionSlots::new(1);
        let full = AtomicUsize::new(1);
        assert!(slots.admit(async || full.load(Ordering::SeqCst)).await.is_err());
        assert!(slots.first_refusal());
        assert!(slots.admit(async || full.load(Ordering::SeqCst)).await.is_err());
        assert!(!slots.first_refusal(), "a client looping at its cap logs once, not once per attempt");
        full.store(0, Ordering::Relaxed);
        drop(slots.admit(async || full.load(Ordering::SeqCst)).await.unwrap());
        full.store(1, Ordering::Relaxed);
        assert!(slots.admit(async || full.load(Ordering::SeqCst)).await.is_err());
        assert!(slots.first_refusal(), "a new episode is logged again");
    }

    #[tokio::test(start_paused = true)]
    async fn a_loop_of_refusals_writes_one_warning_per_interval_and_counts_the_rest() {
        let log = RefusalLog::new(REFUSAL_WARN_INTERVAL);
        assert_eq!(log.warn_now(), Some(0), "the first refusal is written at warn");
        for _ in 0..99 {
            assert_eq!(log.warn_now(), None, "inside the interval the rest are only logged at debug");
        }
        assert_eq!(log.suppressed(), 99);
        tokio::time::advance(Duration::from_secs(59) + Duration::from_millis(999)).await;
        assert_eq!(log.warn_now(), None, "one millisecond short of the interval is still inside it");
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(log.warn_now(), Some(100), "the next warning says how many were suppressed, this one included");
        assert_eq!(log.suppressed(), 0, "the count starts again after each warning");
        assert_eq!(log.warn_now(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn refusal_kinds_are_counted_separately() {
        let tokens = RefusalLog::new(REFUSAL_WARN_INTERVAL);
        let bodies = RefusalLog::new(REFUSAL_WARN_INTERVAL);
        assert_eq!(tokens.warn_now(), Some(0));
        assert_eq!(tokens.warn_now(), None);
        assert_eq!(bodies.warn_now(), Some(0), "a flood of one kind does not silence the other");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_admissions_fill_the_cap_exactly_and_never_pass_it() {
        const CAP: usize = 8;
        const RACERS: usize = 64;
        let slots = Arc::new(SessionSlots::new(CAP));
        let sessions = Arc::new(AtomicUsize::new(0));
        let racers: Vec<_> = (0..RACERS)
            .map(|_| {
                let (slots, sessions) = (Arc::clone(&slots), Arc::clone(&sessions));
                tokio::spawn(async move {
                    let Ok(admission) = slots.admit(async || sessions.load(Ordering::SeqCst)).await else {
                        return false;
                    };
                    // Serving the request: the session appears in the table only after the check, as rmcp's does.
                    tokio::task::yield_now().await;
                    sessions.fetch_add(1, Ordering::SeqCst);
                    drop(admission);
                    true
                })
            })
            .collect();
        let mut admitted = 0;
        for racer in racers {
            admitted += usize::from(racer.await.unwrap());
        }
        assert_eq!(admitted, CAP, "exactly the cap is admitted, however the racers interleave");
        assert_eq!(sessions.load(Ordering::SeqCst), CAP);
    }
}
