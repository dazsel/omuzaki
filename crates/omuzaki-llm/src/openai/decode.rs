//! The OpenAI-compatible chat stream, decoded into [`LlmEvent`]s (llm-api D3, D4, D5).

use std::time::Duration;

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::stream::{LlmEvent, StopReason};
use omuzaki_llm_api::usage::Usage;
use serde_json::Value;

use omuzaki_llm_api::provider::SecretValue;

use crate::engine::{Decoder, Fail, MAX_BUFFERED, short, status_fail};
use crate::sse::SseEvent;

/// A call being streamed: its position in the `tool_calls` array, its id, name and the arguments so far. Servers may
/// interleave deltas of parallel calls, so calls are held whole and handed on one after the other when they close.
struct OpenCall {
    index: u64,
    id: String,
    name: String,
    arguments: String,
}

/// Decodes one attempt's body.
pub(crate) struct OpenAiDecoder {
    /// The model to report when a chunk names none.
    model: String,
    context_window: u64,
    started: bool,
    calls: Vec<OpenCall>,
    called: bool,
    refused: bool,
    finish: Option<String>,
    usage: Option<Usage>,
    /// Redacted from the messages of an error chunk.
    secret: Option<SecretValue>,
    /// The server is on this machine (see [`on_loopback`](Self::on_loopback)).
    loopback: bool,
    /// Characters of text and tool input streamed, for the estimate a loopback server without usage gets.
    streamed: u64,
}

impl OpenAiDecoder {
    pub(crate) fn new(model: String, context_window: u64, secret: Option<SecretValue>) -> Self {
        Self {
            model,
            context_window,
            started: false,
            calls: Vec::new(),
            called: false,
            refused: false,
            finish: None,
            usage: None,
            secret,
            loopback: false,
            streamed: 0,
        }
    }

    /// Marks the server as one on this machine: a turn it reports no usage for (llama-cpp-python ignores
    /// `include_usage`) costs nothing, so it ends with an estimate instead of an error.
    pub(crate) fn on_loopback(mut self, loopback: bool) -> Self {
        self.loopback = loopback;
        self
    }

    /// Hands on every held call as a complete start, input, end sequence.
    fn close_call(&mut self, events: &mut Vec<LlmEvent>) {
        for call in self.calls.drain(..) {
            events.push(LlmEvent::tool_use_start(call.id.clone(), call.name));
            if !call.arguments.is_empty() {
                events.push(LlmEvent::ToolUseInput(call.arguments));
            }
            events.push(LlmEvent::tool_use_end(call.id));
        }
    }

    fn tool_call(&mut self, tc: &Value) -> Result<(), Fail> {
        let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0);
        let id = tc.get("id").and_then(Value::as_str).filter(|i| !i.is_empty());
        let function = tc.get("function").unwrap_or(&Value::Null);
        let name = function.get("name").and_then(Value::as_str).filter(|n| !n.is_empty());
        let arguments = function.get("arguments").and_then(Value::as_str).unwrap_or_default();
        // A delta continues the latest call at its index, unless it names a different id (servers that reuse index 0).
        let open = self.calls.iter().rposition(|c| c.index == index && id.is_none_or(|i| i == c.id));
        let at = if let Some(at) = open {
            at
        } else {
            let name = name.ok_or_else(|| Fail::Fatal(LlmError::Malformed("a tool call without a name".into())))?;
            let id = id.map_or_else(|| format!("call_{index}"), str::to_owned);
            self.calls.push(OpenCall { index, id, name: name.to_owned(), arguments: String::new() });
            self.called = true;
            self.calls.len() - 1
        };
        self.streamed += arguments.chars().count() as u64;
        self.calls[at].arguments.push_str(arguments);
        // Calls are held whole until they close, so what a server sends is bounded here.
        let held: usize = self.calls.iter().map(|c| c.arguments.len()).sum();
        if held > MAX_BUFFERED {
            return Err(Fail::Fatal(LlmError::Malformed(format!("tool arguments over {MAX_BUFFERED} bytes"))));
        }
        Ok(())
    }

    fn chunk(&mut self, v: &Value) -> Result<Vec<LlmEvent>, Fail> {
        let mut events = Vec::new();
        if !self.started {
            self.started = true;
            let model = v.get("model").and_then(Value::as_str).filter(|m| !m.is_empty()).unwrap_or(&self.model);
            events.push(LlmEvent::start(model));
        }
        if let Some(usage) = v.get("usage").filter(|u| u.is_object()) {
            let count = |value: Option<&Value>| value.and_then(Value::as_u64);
            // Both counters are mandatory: a report without them would be priced as zero.
            let (Some(prompt), Some(completion)) =
                (count(usage.get("prompt_tokens")), count(usage.get("completion_tokens")))
            else {
                return Err(Fail::Fatal(LlmError::Malformed(
                    "the usage report lacks prompt_tokens or completion_tokens".into(),
                )));
            };
            let cached = count(usage.get("prompt_tokens_details").and_then(|d| d.get("cached_tokens"))).unwrap_or(0);
            // llm-api D4: `input` excludes the cached tokens, which are reported as `cache_read`.
            let total = Usage::new(prompt.saturating_sub(cached), completion).with_cache_read(cached);
            self.usage = Some(total);
            events.push(LlmEvent::Usage(total));
        }
        let Some(choice) = v.get("choices").and_then(Value::as_array).and_then(|c| c.first()) else {
            return Ok(events);
        };
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        if let Some(refusal) = delta.get("refusal").and_then(Value::as_str).filter(|r| !r.is_empty()) {
            self.refused = true;
            self.close_call(&mut events);
            self.streamed += refusal.chars().count() as u64;
            events.push(LlmEvent::TextDelta(refusal.to_owned()));
        }
        // `reasoning_content` and `reasoning` are dropped: there are no opaque blocks on this provider.
        if let Some(text) = delta.get("content").and_then(Value::as_str).filter(|t| !t.is_empty()) {
            self.close_call(&mut events);
            self.streamed += text.chars().count() as u64;
            events.push(LlmEvent::TextDelta(text.to_owned()));
        }
        for tc in delta.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
            self.tool_call(tc)?;
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.close_call(&mut events);
            self.finish = Some(reason.to_owned());
        }
        Ok(events)
    }

    /// The final events: the usage if the server never reported one, then the stop.
    fn finish(&mut self) -> Result<Vec<LlmEvent>, Fail> {
        let mut events = Vec::new();
        self.close_call(&mut events);
        let reason =
            self.finish.take().ok_or_else(|| Fail::network("the event stream ended before the turn completed"))?;
        let stop = if self.refused {
            StopReason::Refused
        } else {
            match reason.as_str() {
                "content_filter" => StopReason::Refused,
                "length" => StopReason::MaxOutput,
                "tool_calls" | "function_call" => StopReason::ToolUse,
                // Some servers say `stop` after a tool call.
                "stop" if self.called => StopReason::ToolUse,
                "stop" => StopReason::EndTurn,
                other => return Err(Fail::Fatal(LlmError::Malformed(format!("unknown finish reason {other:?}")))),
            }
        };
        // A server that ignores `include_usage` reports nothing, and a turn with no usage would bypass the token and
        // spending accounting, so it is an error and never a free turn; except on this machine, where a turn costs
        // nothing and the token caps count an estimate of what was produced (four characters a token).
        if self.usage.is_none() && self.loopback {
            let estimate = Usage::new(0, self.streamed.div_ceil(4));
            self.usage = Some(estimate);
            events.push(LlmEvent::Usage(estimate));
        }
        if self.usage.is_none() {
            return Err(Fail::Fatal(LlmError::Malformed(
                "the server reported no token usage; it must honour stream_options.include_usage".into(),
            )));
        }
        events.push(LlmEvent::Stop(stop));
        Ok(events)
    }
}

/// An `error` object streamed as data, or returned as a body: `{"error": {"message", "type", "code"}}`.
fn error_fail(
    v: &Value,
    status: u16,
    retry_after: Option<Duration>,
    context_window: u64,
    secret: Option<&SecretValue>,
) -> Fail {
    let error = v.get("error").unwrap_or(v);
    let message = error.get("message").and_then(Value::as_str).unwrap_or_default();
    let code = error.get("code");
    let kind = error.get("type").and_then(Value::as_str).unwrap_or_default();
    let named = code.and_then(Value::as_str).unwrap_or_default();
    let context = format!("{named} {kind} {message}");
    // `context_length_exceeded` arrives as a `code` string, in a body or in a chunk of a stream that answered 200.
    if let overflow @ Fail::Fatal(LlmError::ContextTooLong { .. }) =
        status_fail(400, None, &context, context_window, secret)
    {
        return overflow;
    }
    let numeric = code.and_then(Value::as_u64).and_then(|c| u16::try_from(c).ok());
    // A chunk carries no status of its own: an error the server cannot name is a server failure.
    let status = numeric.unwrap_or(if status == 200 { 500 } else { status });
    status_fail(status, retry_after, message, context_window, secret)
}

impl Decoder for OpenAiDecoder {
    fn event(&mut self, event: SseEvent) -> Result<Vec<LlmEvent>, Fail> {
        let data = event.data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            if !self.started {
                return Err(Fail::Fatal(LlmError::Malformed("the stream ended before any chunk".into())));
            }
            return self.finish();
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| Fail::Fatal(LlmError::Malformed(format!("a chunk that is not JSON: {e}"))))?;
        if v.get("error").is_some_and(|e| !e.is_null()) {
            return Err(error_fail(&v, 200, None, self.context_window, self.secret.as_ref()));
        }
        self.chunk(&v)
    }

    fn end(&mut self) -> Result<Vec<LlmEvent>, Fail> {
        // Some servers close after the finish chunk without `[DONE]`; the turn is whole then.
        if self.started && self.finish.is_some() {
            return self.finish();
        }
        Err(Fail::network("the event stream ended before the turn completed"))
    }
}

/// Maps a non-2xx answer: `{"error": {"message", "type", "code"}}`, or plain text.
pub(crate) fn classify(
    status: u16,
    retry_after: Option<Duration>,
    body: &[u8],
    context_window: u64,
    secret: Option<&SecretValue>,
) -> Fail {
    match serde_json::from_slice::<Value>(body) {
        Ok(v) if v.is_object() => error_fail(&v, status, retry_after, context_window, secret),
        _ => status_fail(status, retry_after, &short(&String::from_utf8_lossy(body), secret), context_window, secret),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The usage report every turn must carry.
    const USAGE: &str = r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#;

    fn decoder() -> OpenAiDecoder {
        OpenAiDecoder::new("fallback".into(), 8192, None)
    }

    fn feed(d: &mut OpenAiDecoder, data: &str) -> Vec<LlmEvent> {
        d.event(SseEvent { event: None, data: data.to_owned() }).unwrap_or_else(|e| panic!("{e:?}"))
    }

    fn all(d: &mut OpenAiDecoder, steps: &[&str]) -> Vec<LlmEvent> {
        steps.iter().flat_map(|s| feed(d, s)).collect()
    }

    #[test]
    fn text_then_usage_then_done_ends_with_usage_before_stop() {
        let mut d = decoder();
        let got = all(
            &mut d,
            &[
                r#"{"model":"llama3.2","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}"#,
                r#"{"choices":[{"index":0,"delta":{"content":"lo"}}]}"#,
                r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
                r#"{"choices":[],"usage":{"prompt_tokens":30,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":10}}}"#,
                "[DONE]",
            ],
        );
        let want = vec![
            LlmEvent::start("llama3.2"),
            LlmEvent::TextDelta("Hel".into()),
            LlmEvent::TextDelta("lo".into()),
            LlmEvent::Usage(Usage::new(20, 4).with_cache_read(10)),
            LlmEvent::Stop(StopReason::EndTurn),
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn streamed_tool_calls_open_and_close_in_order_and_parallel_calls_stay_apart() {
        let mut d = decoder();
        let got = all(
            &mut d,
            &[
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"render","arguments":""}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"cam"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"era\":1}"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"list","arguments":"{}"}}]}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                r#"{"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#,
                "[DONE]",
            ],
        );
        let want = vec![
            LlmEvent::start("fallback"),
            LlmEvent::tool_use_start("call_a", "render"),
            LlmEvent::ToolUseInput("{\"camera\":1}".into()),
            LlmEvent::tool_use_end("call_a"),
            LlmEvent::tool_use_start("call_b", "list"),
            LlmEvent::ToolUseInput("{}".into()),
            LlmEvent::tool_use_end("call_b"),
            LlmEvent::Usage(Usage::new(5, 2)),
            LlmEvent::Stop(StopReason::ToolUse),
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn interleaved_deltas_of_parallel_calls_are_handed_on_one_call_at_a_time() {
        let mut d = decoder();
        let got = all(
            &mut d,
            &[
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"one","arguments":"{\"x\""}},{"index":1,"id":"b","function":{"name":"two","arguments":"{\"y\""}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":":2}"}}]}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                USAGE,
                "[DONE]",
            ],
        );
        assert_eq!(
            &got[1..7],
            &[
                LlmEvent::tool_use_start("a", "one"),
                LlmEvent::ToolUseInput("{\"x\":1}".into()),
                LlmEvent::tool_use_end("a"),
                LlmEvent::tool_use_start("b", "two"),
                LlmEvent::ToolUseInput("{\"y\":2}".into()),
                LlmEvent::tool_use_end("b"),
            ]
        );
    }

    #[test]
    fn a_whole_call_in_one_chunk_without_an_id_gets_one_and_stop_after_a_call_is_tool_use() {
        let mut d = decoder();
        let got = all(
            &mut d,
            &[
                r#"{"choices":[{"delta":{"tool_calls":[{"function":{"name":"t","arguments":"{\"a\":1}"}}]},"finish_reason":"stop"}]}"#,
                USAGE,
                "[DONE]",
            ],
        );
        assert_eq!(got[1], LlmEvent::tool_use_start("call_0", "t"));
        assert_eq!(got.last(), Some(&LlmEvent::Stop(StopReason::ToolUse)));
        assert!(got.contains(&LlmEvent::Usage(Usage::new(3, 2))));
    }

    #[test]
    fn a_server_that_reuses_index_zero_for_every_call_is_split_by_id() {
        let mut d = decoder();
        let got = all(
            &mut d,
            &[
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"x","function":{"name":"a","arguments":"{}"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"y","function":{"name":"b","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
                USAGE,
                "[DONE]",
            ],
        );
        let starts: Vec<&LlmEvent> = got.iter().filter(|e| matches!(e, LlmEvent::ToolUseStart { .. })).collect();
        assert_eq!(starts.len(), 2);
    }

    #[test]
    fn finish_reasons_map_and_a_refusal_wins() {
        for (reason, want) in
            [("length", StopReason::MaxOutput), ("content_filter", StopReason::Refused), ("stop", StopReason::EndTurn)]
        {
            let mut d = decoder();
            let step = format!(r#"{{"choices":[{{"delta":{{"content":"x"}},"finish_reason":"{reason}"}}]}}"#);
            let got = all(&mut d, &[&step, USAGE, "[DONE]"]);
            assert_eq!(got.last(), Some(&LlmEvent::Stop(want)), "{reason}");
        }
        let mut d = decoder();
        let got = all(
            &mut d,
            &[r#"{"choices":[{"delta":{"refusal":"I can't help."},"finish_reason":"stop"}]}"#, USAGE, "[DONE]"],
        );
        assert_eq!(got[1], LlmEvent::TextDelta("I can't help.".into()));
        assert_eq!(got.last(), Some(&LlmEvent::Stop(StopReason::Refused)));
    }

    #[test]
    fn reasoning_content_is_dropped() {
        let mut d = decoder();
        let got = all(
            &mut d,
            &[
                r#"{"choices":[{"delta":{"reasoning_content":"thinking...","content":"ok"},"finish_reason":"stop"}]}"#,
                USAGE,
                "[DONE]",
            ],
        );
        assert!(got.iter().all(|e| !format!("{e:?}").contains("thinking")));
    }

    #[test]
    fn the_stream_closing_after_the_finish_chunk_still_completes_the_turn() {
        let mut d = decoder();
        feed(&mut d, r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"stop"}]}"#);
        feed(&mut d, USAGE);
        let end = d.end().unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(end.last(), Some(&LlmEvent::Stop(StopReason::EndTurn)));
        let mut cut = decoder();
        feed(&mut cut, r#"{"choices":[{"delta":{"content":"x"}}]}"#);
        assert!(matches!(cut.end(), Err(Fail::Retry { error: LlmError::Network(_), .. })));
    }

    #[test]
    fn a_usage_report_missing_a_counter_is_malformed() {
        for usage in [
            "{}",
            r#"{"prompt_tokens":3}"#,
            r#"{"completion_tokens":2}"#,
            r#"{"prompt_tokens":"3","completion_tokens":2}"#,
        ] {
            let mut d = decoder();
            let chunk = format!(r#"{{"choices":[],"usage":{usage}}}"#);
            let got = d.event(SseEvent { event: None, data: chunk });
            assert!(matches!(got, Err(Fail::Fatal(LlmError::Malformed(m))) if m.contains("usage")), "{usage}");
        }
    }

    #[test]
    fn a_finished_turn_without_a_usage_report_is_an_error() {
        let mut d = decoder();
        feed(&mut d, r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"stop"}]}"#);
        let end = d.event(SseEvent { event: None, data: "[DONE]".into() });
        assert!(matches!(end, Err(Fail::Fatal(LlmError::Malformed(m))) if m.contains("usage")));
    }

    #[test]
    fn a_loopback_server_without_a_usage_report_ends_with_an_estimate() {
        let mut d = decoder().on_loopback(true);
        feed(&mut d, r#"{"choices":[{"delta":{"content":"Hi chat!"},"finish_reason":"stop"}]}"#);
        let end = d.event(SseEvent { event: None, data: "[DONE]".into() }).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(end, [LlmEvent::Usage(Usage::new(0, 2)), LlmEvent::Stop(StopReason::EndTurn)]);
    }

    #[test]
    fn errors_in_a_chunk_or_body_map_like_http_statuses() {
        let mut d = decoder();
        let ctx = d.event(SseEvent {
            event: None,
            data: r#"{"error":{"message":"too long","type":"invalid_request_error","code":"context_length_exceeded"}}"#
                .into(),
        });
        assert!(matches!(ctx, Err(Fail::Fatal(LlmError::ContextTooLong { max: 8192, .. }))));
        let body = br#"{"error":{"message":"bad key","type":"auth","code":401}}"#;
        assert!(matches!(classify(401, None, body, 1, None), Fail::Fatal(LlmError::Auth)));
        assert!(matches!(classify(429, Some(Duration::from_secs(1)), b"{}", 1, None), Fail::Retry { .. }));
        assert!(matches!(
            classify(503, None, b"loading model", 1, None),
            Fail::Retry { error: LlmError::Overloaded, .. }
        ));
        assert!(
            matches!(classify(404, None, b"model not found", 1, None), Fail::Fatal(LlmError::Malformed(m)) if m.contains("not found"))
        );
    }

    #[test]
    fn malformed_chunks_and_unknown_finish_reasons_are_malformed() {
        let mut d = decoder();
        assert!(matches!(
            d.event(SseEvent { event: None, data: "{oops".into() }),
            Err(Fail::Fatal(LlmError::Malformed(_)))
        ));
        assert!(matches!(
            d.event(SseEvent { event: None, data: "[DONE]".into() }),
            Err(Fail::Fatal(LlmError::Malformed(_)))
        ));
        let mut d = decoder();
        feed(&mut d, r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"weird"}]}"#);
        assert!(matches!(
            d.event(SseEvent { event: None, data: "[DONE]".into() }),
            Err(Fail::Fatal(LlmError::Malformed(_)))
        ));
    }
}
