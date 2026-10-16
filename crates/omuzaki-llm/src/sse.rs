//! Server-Sent Events over a body stream (llm-api D11), framed by `sse-stream`.

use std::sync::{Arc, Mutex, PoisonError};

use tokio::time::Instant;

use futures_util::Stream;
use sse_stream::{Error as SseError, Sse, SseStream};

use crate::http::{ByteStream, HttpFail};

/// One event of the stream: the `event:` name, if any, and the joined `data:` lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Why the stream cannot go on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SseFail {
    /// The connection broke while reading.
    Transport(String),
    /// The bytes are not Server-Sent Events.
    Malformed(String),
}

fn convert(item: Result<Sse, SseError>) -> Result<Option<SseEvent>, SseFail> {
    match item {
        // A comment or a bare `retry:` line carries no data and is nothing to a provider.
        Ok(Sse { event: None, data: None, .. }) => Ok(None),
        Ok(sse) => Ok(Some(SseEvent { event: sse.event, data: sse.data.unwrap_or_default() })),
        Err(SseError::Body(e)) => Err(SseFail::Transport(e.to_string())),
        Err(e) => Err(SseFail::Malformed(e.to_string())),
    }
}

/// When bytes last arrived, whether or not they completed an event. A server that keeps a long generation alive with
/// comment frames sends no event, but the connection is alive, so the engine's idle deadline runs from the last bytes,
/// not from the last event.
///
/// Caveat: a server that sends only comments keeps a turn alive for as long as it likes; the idle limit bounds silence,
/// not total time, and the caller's cancel is what ends such a turn.
#[derive(Clone, Debug)]
pub(crate) struct Activity(Arc<Mutex<Instant>>);

impl Activity {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    fn touch(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    /// When the latest bytes arrived (or the stream was opened).
    pub(crate) fn last(&self) -> Instant {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The events in `body`, with comment-only frames (keep-alives) left out, and the flag that bytes have arrived.
pub(crate) fn events(body: ByteStream) -> (impl Stream<Item = Result<SseEvent, SseFail>> + Send, Activity) {
    use futures_util::StreamExt;
    let activity = Activity::new();
    let seen = activity.clone();
    let counted = body.map(move |chunk| {
        seen.touch();
        chunk.map_err(|e: HttpFail| Boxed(e.to_string()))
    });
    let stream = SseStream::from_bytes_stream(counted).filter_map(|item| std::future::ready(convert(item).transpose()));
    (stream, activity)
}

/// A body error as `std::error::Error`, which `sse-stream` wants.
#[derive(Debug)]
struct Boxed(String);

impl std::fmt::Display for Boxed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Boxed {}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::StreamExt;

    use super::*;

    const WIRE: &str = ": keep-alive\n\nevent: message_start\ndata: {\"a\":1}\n\nevent: ping\ndata: {\"type\": \"ping\"}\n\n\
                        data: {\"x\":\"é\"}\n\ndata: line one\ndata: line two\n\n";

    fn run(chunks: Vec<Vec<u8>>) -> Vec<Result<SseEvent, SseFail>> {
        let body: ByteStream = Box::pin(futures_util::stream::iter(chunks.into_iter().map(|c| Ok(Bytes::from(c)))));
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap_or_else(|e| panic!("{e}"));
        rt.block_on(events(body).0.collect())
    }

    #[test]
    fn a_comment_only_stream_still_counts_as_activity() {
        let body: ByteStream = Box::pin(futures_util::stream::iter(vec![Ok(Bytes::from_static(b": keep-alive\n\n"))]));
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap_or_else(|e| panic!("{e}"));
        let (stream, activity) = events(body);
        let opened = activity.last();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let got: Vec<_> = rt.block_on(stream.collect());
        assert_eq!(got, Vec::new());
        assert!(activity.last() > opened, "the comment bytes moved the clock");
    }

    #[allow(clippy::unnecessary_wraps)] // the events are compared with `Result`s
    fn event(name: Option<&str>, data: &str) -> Result<SseEvent, SseFail> {
        Ok(SseEvent { event: name.map(str::to_owned), data: data.to_owned() })
    }

    #[test]
    fn frames_become_events_and_comments_are_dropped() {
        let got = run(vec![WIRE.as_bytes().to_vec()]);
        let want = vec![
            event(Some("message_start"), "{\"a\":1}"),
            event(Some("ping"), "{\"type\": \"ping\"}"),
            event(None, "{\"x\":\"é\"}"),
            event(None, "line one\nline two"),
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn splitting_the_stream_at_every_byte_gives_the_same_events() {
        let whole = run(vec![WIRE.as_bytes().to_vec()]);
        let bytes = WIRE.as_bytes();
        for at in 0..=bytes.len() {
            assert_eq!(run(vec![bytes[..at].to_vec(), bytes[at..].to_vec()]), whole, "split at {at}");
        }
        let singles = bytes.iter().map(|b| vec![*b]).collect();
        assert_eq!(run(singles), whole, "one byte at a time");
    }

    #[test]
    fn a_body_error_is_a_transport_failure_and_bad_framing_is_malformed() {
        let body: ByteStream = Box::pin(futures_util::stream::iter(vec![
            Ok(Bytes::from_static(b"data: a\n\n")),
            Err(HttpFail::Body("reset".into())),
        ]));
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap_or_else(|e| panic!("{e}"));
        let got: Vec<_> = rt.block_on(events(body).0.collect());
        assert_eq!(got[0], event(None, "a"));
        assert!(matches!(&got[1], Err(SseFail::Transport(m)) if m.contains("reset")), "{got:?}");
        let bad = run(vec![b"not an sse line without a colon\n\n".to_vec()]);
        assert!(bad.iter().any(|r| matches!(r, Err(SseFail::Malformed(_)))) || bad.is_empty(), "{bad:?}");
    }
}
