//! The Anthropic Messages event stream, decoded into [`LlmEvent`]s (llm-api D3, D5), and the error mapping.

use std::collections::HashMap;
use std::time::Duration;

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::stream::{LlmEvent, StopReason};
use omuzaki_llm_api::usage::Usage;
use serde_json::{Value, json};

use super::wire::PROVIDER;
use omuzaki_llm_api::provider::SecretValue;

use crate::engine::{Decoder, Fail, MAX_BUFFERED, short, status_fail};
use crate::sse::SseEvent;

/// What an open content block needs remembered until it closes.
enum Open {
    Text,
    Tool {
        id: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    /// `redacted_thinking`, or a block type this client does not know: returned whole at its end, with its size.
    Whole(Value, usize),
}

/// Decodes one attempt's body.
#[derive(Default)]
pub(crate) struct AnthropicDecoder {
    started: bool,
    stopped: bool,
    open: HashMap<u64, Open>,
    usage: Usage,
    /// Redacted from the messages of an error event.
    secret: Option<SecretValue>,
}

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn counter(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

fn malformed(why: impl Into<String>) -> Fail {
    Fail::Fatal(LlmError::Malformed(why.into()))
}

fn text_event(delta: &Value, key: &str, make: fn(String) -> LlmEvent) -> Vec<LlmEvent> {
    field(delta, key).filter(|t| !t.is_empty()).map(|t| make(t.to_owned())).into_iter().collect()
}

/// `stop_reason` as the contract's [`StopReason`].
fn stop_reason(reason: &str) -> Result<StopReason, Fail> {
    match reason {
        "end_turn" | "stop_sequence" => Ok(StopReason::EndTurn),
        "tool_use" => Ok(StopReason::ToolUse),
        "max_tokens" => Ok(StopReason::MaxOutput),
        "pause_turn" => Ok(StopReason::Paused),
        "refusal" => Ok(StopReason::Refused),
        other => Err(malformed(format!("unknown stop reason {other:?}"))),
    }
}

impl AnthropicDecoder {
    pub(crate) fn new(secret: Option<SecretValue>) -> Self {
        Self { secret, ..Self::default() }
    }

    /// Thinking and unknown blocks are held whole until they close. Everything the open blocks hold, their first
    /// payloads included, counts against one cap, so many blocks cannot add up past it.
    fn check_held(&self) -> Result<(), Fail> {
        let held: usize = self
            .open
            .values()
            .map(|open| match open {
                Open::Thinking { thinking, signature } => thinking.len() + signature.len(),
                Open::Whole(_, size) => *size,
                Open::Text | Open::Tool { .. } => 0,
            })
            .sum();
        if held > MAX_BUFFERED {
            return Err(malformed(format!("more than {MAX_BUFFERED} bytes of blocks held open")));
        }
        Ok(())
    }

    /// Folds the counters present in `usage` into the running (cumulative) totals.
    fn merge(&mut self, usage: &Value) -> LlmEvent {
        let take = |slot: &mut u64, found: Option<u64>| *slot = found.unwrap_or(*slot);
        take(&mut self.usage.input, counter(usage, "input_tokens"));
        take(&mut self.usage.output, counter(usage, "output_tokens"));
        take(&mut self.usage.cache_read, counter(usage, "cache_read_input_tokens"));
        take(&mut self.usage.cache_write, counter(usage, "cache_creation_input_tokens"));
        LlmEvent::Usage(self.usage)
    }

    fn block_start(&mut self, v: &Value) -> Result<Vec<LlmEvent>, Fail> {
        let index = counter(v, "index").ok_or_else(|| malformed("a content block without an index"))?;
        let block = v.get("content_block").ok_or_else(|| malformed("content_block_start without a block"))?;
        let (open, events) = match field(block, "type") {
            Some("text") => (Open::Text, text_event(block, "text", LlmEvent::TextDelta)),
            Some("tool_use") => {
                let id = field(block, "id").ok_or_else(|| malformed("a tool call without an id"))?;
                let name = field(block, "name").ok_or_else(|| malformed("a tool call without a name"))?;
                (Open::Tool { id: id.to_owned() }, vec![LlmEvent::tool_use_start(id, name)])
            }
            Some("thinking") => (
                Open::Thinking {
                    thinking: field(block, "thinking").unwrap_or_default().to_owned(),
                    signature: field(block, "signature").unwrap_or_default().to_owned(),
                },
                Vec::new(),
            ),
            _ => (Open::Whole(block.clone(), block.to_string().len()), Vec::new()),
        };
        self.open.insert(index, open);
        self.check_held()?;
        Ok(events)
    }

    fn block_delta(&mut self, v: &Value) -> Result<Vec<LlmEvent>, Fail> {
        let index = counter(v, "index").ok_or_else(|| malformed("a delta without an index"))?;
        let delta = v.get("delta").ok_or_else(|| malformed("content_block_delta without a delta"))?;
        let open =
            self.open.get_mut(&index).ok_or_else(|| malformed(format!("a delta for the unopened block {index}")))?;
        let events = match (field(delta, "type"), open) {
            (Some("text_delta"), _) => text_event(delta, "text", LlmEvent::TextDelta),
            (Some("input_json_delta"), _) => text_event(delta, "partial_json", LlmEvent::ToolUseInput),
            (Some("thinking_delta"), Open::Thinking { thinking, .. }) => {
                thinking.push_str(field(delta, "thinking").unwrap_or_default());
                Vec::new()
            }
            (Some("signature_delta"), Open::Thinking { signature, .. }) => {
                signature.push_str(field(delta, "signature").unwrap_or_default());
                Vec::new()
            }
            // Citations and the deltas of blocks this client does not read.
            _ => Vec::new(),
        };
        self.check_held()?;
        Ok(events)
    }

    fn block_stop(&mut self, v: &Value) -> Result<Vec<LlmEvent>, Fail> {
        let index = counter(v, "index").ok_or_else(|| malformed("a block stop without an index"))?;
        let open =
            self.open.remove(&index).ok_or_else(|| malformed(format!("a stop for the unopened block {index}")))?;
        Ok(match open {
            Open::Text => Vec::new(),
            Open::Tool { id } => vec![LlmEvent::tool_use_end(id)],
            Open::Thinking { thinking, signature } => {
                let block = json!({"type": "thinking", "thinking": thinking, "signature": signature});
                vec![LlmEvent::opaque(PROVIDER, block)]
            }
            Open::Whole(block, _) => vec![LlmEvent::opaque(PROVIDER, block)],
        })
    }

    fn message_delta(&mut self, v: &Value) -> Result<Vec<LlmEvent>, Fail> {
        let mut events = Vec::new();
        if let Some(usage) = v.get("usage") {
            events.push(self.merge(usage));
        }
        if let Some(reason) = v.get("delta").and_then(|d| field(d, "stop_reason")) {
            let stop = stop_reason(reason)?;
            // A turn always reports its usage before it stops.
            if events.is_empty() {
                events.push(LlmEvent::Usage(self.usage));
            }
            events.push(LlmEvent::Stop(stop));
            self.stopped = true;
        }
        Ok(events)
    }
}

/// An `error` object's `type` and `message`, from an error event or an error body.
fn error_parts(v: &Value) -> (&str, &str) {
    let error = v.get("error").unwrap_or(v);
    (field(error, "type").unwrap_or("api_error"), field(error, "message").unwrap_or_default())
}

/// Maps an `error` event: the table of llm-api D5 retries `overloaded_error` and nothing else.
fn event_fail(kind: &str, message: &str, secret: Option<&SecretValue>) -> Fail {
    match kind {
        "overloaded_error" => Fail::overloaded(None),
        "rate_limit_error" => Fail::Fatal(LlmError::rate_limited(None)),
        "authentication_error" | "permission_error" => Fail::Fatal(LlmError::Auth),
        "invalid_request_error" => Fail::Fatal(LlmError::Malformed(short(message, secret))),
        // Only `overloaded_error` is retried (D5): any other kind surfaces at once.
        _ => Fail::Fatal(LlmError::Network(format!("the provider reported {kind}: {}", short(message, secret)))),
    }
}

impl Decoder for AnthropicDecoder {
    fn event(&mut self, event: SseEvent) -> Result<Vec<LlmEvent>, Fail> {
        if event.data.trim().is_empty() {
            return Ok(Vec::new());
        }
        let v: Value =
            serde_json::from_str(&event.data).map_err(|e| malformed(format!("an event that is not JSON: {e}")))?;
        let kind = field(&v, "type").or(event.event.as_deref()).unwrap_or_default();
        if !self.started && !matches!(kind, "message_start" | "error" | "ping") {
            return Err(malformed(format!("{kind} arrived before message_start")));
        }
        match kind {
            "message_start" => {
                if self.started {
                    return Err(malformed("a second message_start"));
                }
                self.started = true;
                let message = v.get("message").unwrap_or(&Value::Null);
                let model = field(message, "model").ok_or_else(|| malformed("message_start without a model"))?;
                let mut events = vec![LlmEvent::start(model)];
                if let Some(usage) = message.get("usage") {
                    events.push(self.merge(usage));
                }
                Ok(events)
            }
            "content_block_start" => self.block_start(&v),
            "content_block_delta" => self.block_delta(&v),
            "content_block_stop" => self.block_stop(&v),
            "message_delta" => self.message_delta(&v),
            "message_stop" if !self.stopped => Err(malformed("message_stop before a stop reason")),
            "error" => {
                let (kind, message) = error_parts(&v);
                Err(event_fail(kind, message, self.secret.as_ref()))
            }
            // `ping`, `message_stop` after a stop, and event types added later.
            _ => Ok(Vec::new()),
        }
    }

    fn end(&mut self) -> Result<Vec<LlmEvent>, Fail> {
        Err(Fail::network("the event stream ended before the turn completed"))
    }
}

/// Maps a non-2xx answer: the JSON error body names the type and message.
pub(crate) fn classify(
    status: u16,
    retry_after: Option<Duration>,
    body: &[u8],
    context_window: u64,
    secret: Option<&SecretValue>,
) -> Fail {
    let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let (kind, message) = error_parts(&parsed);
    if kind == "overloaded_error" {
        return Fail::overloaded(retry_after);
    }
    status_fail(status, retry_after, message, context_window, secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(d: &mut AnthropicDecoder, data: &str) -> Result<Vec<LlmEvent>, Fail> {
        d.event(SseEvent { event: None, data: data.to_owned() })
    }

    fn ok(r: Result<Vec<LlmEvent>, Fail>) -> Vec<LlmEvent> {
        r.unwrap_or_else(|e| panic!("{e:?}"))
    }

    const START: &str = r#"{"type":"message_start","message":{"id":"m","model":"claude-haiku-5-5","usage":{"input_tokens":25,"cache_creation_input_tokens":4800,"cache_read_input_tokens":0,"output_tokens":1}}}"#;

    fn started() -> AnthropicDecoder {
        let mut d = AnthropicDecoder::default();
        ok(feed(&mut d, START));
        d
    }

    fn all(d: &mut AnthropicDecoder, steps: &[&str]) -> Vec<LlmEvent> {
        steps.iter().flat_map(|s| ok(feed(d, s))).collect()
    }

    #[test]
    fn message_start_gives_start_and_the_first_usage() {
        let mut d = AnthropicDecoder::default();
        let want = vec![LlmEvent::start("claude-haiku-5-5"), LlmEvent::Usage(Usage::new(25, 1).with_cache_write(4800))];
        assert_eq!(ok(feed(&mut d, START)), want);
    }

    #[test]
    fn text_tools_and_the_final_usage_and_stop_decode_in_order() {
        let mut d = started();
        let got = all(
            &mut d,
            &[
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
                r#"{"type":"content_block_stop","index":0}"#,
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_1","name":"t","input":{}}}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"1}"}}"#,
                r#"{"type":"content_block_stop","index":1}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":58}}"#,
            ],
        );
        let want = vec![
            LlmEvent::TextDelta("Hi".into()),
            LlmEvent::tool_use_start("tu_1", "t"),
            LlmEvent::ToolUseInput("{\"a\":".into()),
            LlmEvent::ToolUseInput("1}".into()),
            LlmEvent::tool_use_end("tu_1"),
            LlmEvent::Usage(Usage::new(25, 58).with_cache_write(4800)),
            LlmEvent::Stop(StopReason::ToolUse),
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn thinking_is_assembled_into_one_opaque_block_and_redacted_thinking_passes_whole() {
        let mut d = started();
        let got = all(
            &mut d,
            &[
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"a"}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"b"}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
                r#"{"type":"content_block_stop","index":0}"#,
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"xyz"}}"#,
                r#"{"type":"content_block_stop","index":1}"#,
            ],
        );
        let want = vec![
            LlmEvent::opaque("anthropic", json!({"type": "thinking", "thinking": "ab", "signature": "sig"})),
            LlmEvent::opaque("anthropic", json!({"type": "redacted_thinking", "data": "xyz"})),
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn stop_reasons_map_and_an_unknown_one_is_malformed() {
        for (wire, want) in [
            ("end_turn", StopReason::EndTurn),
            ("max_tokens", StopReason::MaxOutput),
            ("pause_turn", StopReason::Paused),
            ("refusal", StopReason::Refused),
            ("tool_use", StopReason::ToolUse),
        ] {
            let mut d = started();
            let delta = format!(
                r#"{{"type":"message_delta","delta":{{"stop_reason":"{wire}"}},"usage":{{"output_tokens":3}}}}"#
            );
            assert_eq!(ok(feed(&mut d, &delta)).last(), Some(&LlmEvent::Stop(want)), "{wire}");
        }
        assert!(feed(&mut started(), r#"{"type":"message_delta","delta":{"stop_reason":"nope"}}"#).is_err());
    }

    #[test]
    fn events_out_of_order_or_not_json_are_malformed() {
        let mut d = AnthropicDecoder::default();
        let early = feed(&mut d, r#"{"type":"content_block_stop","index":0}"#);
        assert!(matches!(early, Err(Fail::Fatal(LlmError::Malformed(_)))));
        assert!(matches!(feed(&mut d, "{oops"), Err(Fail::Fatal(LlmError::Malformed(_)))));
        let mut d = started();
        assert!(feed(&mut d, START).is_err(), "a second message_start");
        assert!(
            feed(&mut d, r#"{"type":"content_block_delta","index":9,"delta":{"type":"text_delta","text":"x"}}"#)
                .is_err()
        );
        assert!(feed(&mut d, r#"{"type":"message_stop"}"#).is_err(), "stop without a reason");
        assert_eq!(ok(feed(&mut d, r#"{"type":"ping"}"#)), Vec::new());
        assert_eq!(ok(feed(&mut d, r#"{"type":"something_new"}"#)), Vec::new());
    }

    #[test]
    fn error_events_map_by_type() {
        let mut d = AnthropicDecoder::default();
        let mut error =
            |kind: &str| feed(&mut d, &format!(r#"{{"type":"error","error":{{"type":"{kind}","message":"x"}}}}"#));
        assert!(matches!(error("overloaded_error"), Err(Fail::Retry { error: LlmError::Overloaded, .. })));
        assert!(matches!(error("authentication_error"), Err(Fail::Fatal(LlmError::Auth))));
        assert!(matches!(error("rate_limit_error"), Err(Fail::Fatal(LlmError::RateLimited { retry_after: None, .. }))));
        assert!(matches!(error("api_error"), Err(Fail::Fatal(LlmError::Network(_)))));
    }

    fn thinking_start(index: u64, thinking: &str) -> String {
        json!({"type": "content_block_start", "index": index,
               "content_block": {"type": "thinking", "thinking": thinking, "signature": ""}})
        .to_string()
    }

    #[test]
    fn the_held_blocks_share_one_cap_that_counts_their_first_payloads() {
        let third = "x".repeat(MAX_BUFFERED / 3 + 1);
        // One block whose first payload is over the cap.
        let mut d = started();
        assert!(feed(&mut d, &thinking_start(0, &"x".repeat(MAX_BUFFERED + 1))).is_err());
        // Several open blocks, each under the cap, that add up past it.
        let mut d = started();
        assert!(feed(&mut d, &thinking_start(0, &third)).is_ok());
        assert!(feed(&mut d, &thinking_start(1, &third)).is_ok());
        assert!(feed(&mut d, &thinking_start(2, &third)).is_err());
        // The same through deltas.
        let mut d = started();
        assert!(feed(&mut d, &thinking_start(0, "")).is_ok());
        assert!(feed(&mut d, &thinking_start(1, "")).is_ok());
        let delta = |index: u64| {
            json!({"type": "content_block_delta", "index": index,
                   "delta": {"type": "thinking_delta", "thinking": third}})
            .to_string()
        };
        assert!(feed(&mut d, &delta(0)).is_ok());
        assert!(feed(&mut d, &delta(1)).is_ok());
        assert!(feed(&mut d, &delta(0)).is_err());
        // A block of a type this client does not know counts too.
        let mut d = started();
        let unknown = json!({"type": "content_block_start", "index": 0,
                             "content_block": {"type": "redacted_thinking", "data": "x".repeat(MAX_BUFFERED + 1)}});
        assert!(feed(&mut d, &unknown.to_string()).is_err());
    }

    #[test]
    fn a_closed_block_releases_what_it_held() {
        let mut d = started();
        let big = "x".repeat(MAX_BUFFERED / 2 + 1);
        for index in 0..4 {
            assert!(feed(&mut d, &thinking_start(index, &big)).is_ok(), "block {index}");
            assert!(feed(&mut d, &json!({"type": "content_block_stop", "index": index}).to_string()).is_ok());
        }
    }

    #[test]
    fn error_bodies_map_by_status_and_message() {
        let long = br#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 300000 tokens > 200000 maximum"}}"#;
        assert!(matches!(
            classify(400, None, long, 200_000, None),
            Fail::Fatal(LlmError::ContextTooLong { max: 200_000, .. })
        ));
        let over = br#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert!(matches!(classify(529, None, over, 1, None), Fail::Retry { error: LlmError::Overloaded, .. }));
        assert!(matches!(classify(401, None, b"", 1, None), Fail::Fatal(LlmError::Auth)));
        assert!(matches!(classify(429, Some(Duration::from_secs(3)), b"{}", 1, None), Fail::Retry { .. }));
        let bad = classify(400, None, br#"{"error":{"message":"bad field"}}"#, 1, None);
        assert!(matches!(bad, Fail::Fatal(LlmError::Malformed(m)) if m.contains("bad field")));
    }
}
