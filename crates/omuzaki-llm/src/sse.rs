//! Server-Sent Events over a body stream (llm-api D11), framed by `sse-stream`.

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

/// The events in `body`, with comment-only frames (keep-alives) left out.
pub(crate) fn events(body: ByteStream) -> impl Stream<Item = Result<SseEvent, SseFail>> + Send {
    use futures_util::StreamExt;
    SseStream::from_bytes_stream(body.map(|chunk| chunk.map_err(|e: HttpFail| Boxed(e.to_string()))))
        .filter_map(|item| std::future::ready(convert(item).transpose()))
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
        rt.block_on(events(body).collect())
    }

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
        let got: Vec<_> = rt.block_on(events(body).collect());
        assert_eq!(got[0], event(None, "a"));
        assert!(matches!(&got[1], Err(SseFail::Transport(m)) if m.contains("reset")), "{got:?}");
        let bad = run(vec![b"not an sse line without a colon\n\n".to_vec()]);
        assert!(bad.iter().any(|r| matches!(r, Err(SseFail::Malformed(_)))) || bad.is_empty(), "{bad:?}");
    }
}
