//! The shared conformance suite (`omuzaki-llm-mock::conformance`, llm-api D10) run against both providers. Each case
//! is a `Script`; the harness here plays it on the wire of the provider under test, from a loopback fake server, by
//! the mapping that the suite's documentation fixes.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

mod common;

use std::fmt::Write as _;
use std::time::Duration;

use common::{End, Reply, Server, config, env, runtime_handle};
use omuzaki_llm::{AnthropicClient, OpenAiClient, Timeouts};
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::TurnRequest;
use omuzaki_llm_api::provider::RuntimeHandle;
use omuzaki_llm_api::stream::{Cancel, LlmEvent, StopReason, TurnStream};
use omuzaki_llm_api::usage::{ModelInfo, Usage};
use omuzaki_llm_mock::conformance::{Harness, run};
use omuzaki_llm_mock::{Script, ScriptTurn};
use serde_json::{Value, json};

fn quick() -> Timeouts {
    Timeouts {
        first_byte: Duration::from_secs(5),
        idle: Duration::from_secs(5),
        backoff_unit: Duration::from_millis(1),
    }
}

/// A client that owns the server and the runtime it talks to.
struct Held {
    client: Box<dyn LlmClient>,
    _server: Server,
    _runtime: RuntimeHandle,
}

impl LlmClient for Held {
    fn info(&self) -> &ModelInfo {
        self.client.info()
    }

    fn stream(&self, req: TurnRequest, cancel: &Cancel) -> Result<TurnStream, LlmError> {
        self.client.stream(req, cancel)
    }
}

// ---- what is common to both encoders ----

/// Splits `text` into pieces of at least `size` bytes ending on character boundaries, as `FakeLlm` does.
fn pieces(text: &str, size: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut piece = String::new();
    for c in text.chars() {
        piece.push(c);
        if piece.len() >= size.max(1) {
            out.push(std::mem::take(&mut piece));
        }
    }
    if !piece.is_empty() {
        out.push(piece);
    }
    out
}

/// The turn's events with `chunk` applied.
fn expanded(turn: &ScriptTurn) -> Vec<LlmEvent> {
    let Some(size) = turn.chunk else { return turn.events.clone() };
    turn.events
        .iter()
        .flat_map(|event| match event {
            LlmEvent::TextDelta(t) => pieces(t, size).into_iter().map(LlmEvent::TextDelta).collect(),
            LlmEvent::ToolUseInput(t) => pieces(t, size).into_iter().map(LlmEvent::ToolUseInput).collect(),
            other => vec![other.clone()],
        })
        .collect()
}

/// How the wire plays one turn.
struct Plan {
    /// Answers before the stream: each is a failing status.
    failures: Vec<Reply>,
    /// The events that reach the wire, and how the body ends.
    stream: Option<(Vec<LlmEvent>, Tail)>,
}

/// What follows the encoded events.
enum Tail {
    /// The script's last event is `Stop`, or the stream is cut by an error after it.
    Finished,
    /// There is no `Stop` and no error: the connection stays open.
    Open,
    /// The connection breaks.
    Drop,
    /// Bytes that are not an event stream follow.
    Garbage,
}

fn retry_after_header(ms: u64) -> String {
    ms.div_ceil(1000).to_string()
}

/// Splits a turn into its failing answers and its stream, by the suite's mapping.
fn plan(turn: &ScriptTurn, request_failure: &dyn Fn(&LlmError, u64) -> Reply) -> Plan {
    let events = expanded(turn);
    let throttled = || Reply::status_with(429, &[("retry-after", &retry_after_header(turn.retry_after_ms))], "{}");
    match &turn.error {
        Some(injected) if injected.before == 0 => {
            // The request itself fails, `retries + 1` times (a 429 without `retry-after` is never retried).
            let times = if matches!(injected.error, LlmError::RateLimited { retry_after: None, .. }) {
                1
            } else {
                usize::from(turn.retries) + 1
            };
            Plan {
                failures: (0..times).map(|_| request_failure(&injected.error, turn.retry_after_ms)).collect(),
                stream: None,
            }
        }
        Some(injected) => {
            let cut = injected.before.min(events.len());
            let tail = if matches!(injected.error, LlmError::Malformed(_)) { Tail::Garbage } else { Tail::Drop };
            Plan {
                failures: (0..turn.retries).map(|_| throttled()).collect(),
                stream: Some((events[..cut].to_vec(), tail)),
            }
        }
        None => {
            let tail = if matches!(events.last(), Some(LlmEvent::Stop(_))) { Tail::Finished } else { Tail::Open };
            Plan { failures: (0..turn.retries).map(|_| throttled()).collect(), stream: Some((events, tail)) }
        }
    }
}

fn sse(wire: &str, tail: &Tail, garbage: &str) -> Reply {
    match tail {
        Tail::Finished => Reply::sse_text(wire),
        Tail::Open => Reply::sse_text(wire).ending(End::Hold),
        Tail::Drop => Reply::sse_text(wire).ending(End::Abort),
        Tail::Garbage => Reply::sse_text(&format!("{wire}{garbage}")),
    }
}

// ---- Anthropic ----

fn anthropic_failure(error: &LlmError, retry_after_ms: u64) -> Reply {
    match error {
        LlmError::RateLimited { retry_after: Some(d), .. } => Reply::status_with(
            429,
            &[("retry-after", &retry_after_header(u64::try_from(d.as_millis()).unwrap()))],
            "{}",
        ),
        LlmError::RateLimited { .. } => {
            Reply::status(429, r#"{"type":"error","error":{"type":"rate_limit_error","message":"spend cap"}}"#)
        }
        LlmError::Overloaded => {
            Reply::status(529, r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#)
        }
        LlmError::Auth => Reply::status(
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        ),
        LlmError::ContextTooLong { .. } => Reply::status(
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 1200000 tokens > 1000000 maximum"}}"#,
        ),
        other => panic!("{other:?} (retry-after {retry_after_ms} ms) is not a request failure in the mapping"),
    }
}

fn sse_event(name: &str, data: &Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn usage_json(u: &Usage) -> Value {
    json!({"input_tokens": u.input, "output_tokens": u.output, "cache_read_input_tokens": u.cache_read, "cache_creation_input_tokens": u.cache_write})
}

fn stop_name(reason: StopReason) -> &'static str {
    match reason {
        StopReason::ToolUse => "tool_use",
        StopReason::MaxOutput => "max_tokens",
        StopReason::Refused => "refusal",
        StopReason::Paused => "pause_turn",
        _ => "end_turn",
    }
}

/// The Anthropic wire of `events`.
fn anthropic_wire(events: &[LlmEvent]) -> String {
    let mut out = String::new();
    let mut index = 0u32;
    let mut text_open = false;
    let mut last_usage: Option<Usage> = None;
    let close_text = |out: &mut String, index: &mut u32, open: &mut bool| {
        if *open {
            out.push_str(&sse_event("content_block_stop", &json!({"type": "content_block_stop", "index": *index})));
            *index += 1;
            *open = false;
        }
    };
    for event in events {
        match event {
            LlmEvent::Start { model, .. } => {
                let message = json!({"id": "msg_1", "type": "message", "role": "assistant", "content": [], "model": model, "stop_reason": null});
                out.push_str(&sse_event("message_start", &json!({"type": "message_start", "message": message})));
            }
            LlmEvent::TextDelta(t) => {
                if !text_open {
                    let block = json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}});
                    out.push_str(&sse_event("content_block_start", &block));
                    text_open = true;
                }
                let delta =
                    json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": t}});
                out.push_str(&sse_event("content_block_delta", &delta));
            }
            LlmEvent::ToolUseStart { id, name, .. } => {
                close_text(&mut out, &mut index, &mut text_open);
                let block = json!({"type": "content_block_start", "index": index, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}});
                out.push_str(&sse_event("content_block_start", &block));
            }
            LlmEvent::ToolUseInput(t) => {
                let delta = json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": t}});
                out.push_str(&sse_event("content_block_delta", &delta));
            }
            LlmEvent::ToolUseEnd { .. } => {
                out.push_str(&sse_event("content_block_stop", &json!({"type": "content_block_stop", "index": index})));
                index += 1;
            }
            LlmEvent::Opaque { block, .. } => {
                close_text(&mut out, &mut index, &mut text_open);
                let start = if block["type"] == "thinking" {
                    json!({"type": "thinking", "thinking": "", "signature": ""})
                } else {
                    block.clone()
                };
                out.push_str(&sse_event(
                    "content_block_start",
                    &json!({"type": "content_block_start", "index": index, "content_block": start}),
                ));
                if block["type"] == "thinking" {
                    let thinking = json!({"type": "content_block_delta", "index": index, "delta": {"type": "thinking_delta", "thinking": block["thinking"]}});
                    let signature = json!({"type": "content_block_delta", "index": index, "delta": {"type": "signature_delta", "signature": block["signature"]}});
                    out.push_str(&sse_event("content_block_delta", &thinking));
                    out.push_str(&sse_event("content_block_delta", &signature));
                }
                out.push_str(&sse_event("content_block_stop", &json!({"type": "content_block_stop", "index": index})));
                index += 1;
            }
            LlmEvent::Usage(u) => {
                close_text(&mut out, &mut index, &mut text_open);
                last_usage = Some(*u);
                let delta = json!({"type": "message_delta", "delta": {"stop_reason": null}, "usage": usage_json(u)});
                out.push_str(&sse_event("message_delta", &delta));
            }
            LlmEvent::Stop(reason) => {
                close_text(&mut out, &mut index, &mut text_open);
                let mut delta = json!({"type": "message_delta", "delta": {"stop_reason": stop_name(*reason), "stop_sequence": null}});
                if let Some(u) = last_usage {
                    delta["usage"] = usage_json(&u);
                }
                out.push_str(&sse_event("message_delta", &delta));
                out.push_str(&sse_event("message_stop", &json!({"type": "message_stop"})));
            }
            _ => {}
        }
    }
    out
}

fn anthropic_replies(script: &Script) -> Vec<Reply> {
    let mut replies = Vec::new();
    for turn in &script.turns {
        let plan = plan(turn, &anthropic_failure);
        replies.extend(plan.failures);
        if let Some((events, tail)) = plan.stream {
            replies.push(sse(&anthropic_wire(&events), &tail, "event: message_delta\ndata: {not json\n\n"));
        }
    }
    replies
}

// The scripts name a stand-in model; the client serves a priced one. The signature is `conformance::Make`'s.
#[allow(clippy::needless_pass_by_value)]
fn anthropic(script: Script) -> Box<dyn LlmClient> {
    let server = Server::start(anthropic_replies(&script));
    let runtime = runtime_handle();
    let client = AnthropicClient::new(&config("anthropic", &server, "claude-haiku-5-5"), &env(&runtime))
        .unwrap()
        .with_timeouts(quick())
        .with_seed(11);
    Box::new(Held { client: Box::new(client), _server: server, _runtime: runtime })
}

#[test]
fn the_anthropic_provider_passes_the_conformance_suite() {
    let harness =
        Harness::new(&anthropic).with_opaque_provider("anthropic").with_cache_writes().with_secret(common::KEY);
    assert_eq!(run(&harness), Ok(()));
}

// ---- OpenAI-compatible ----

fn openai_failure(error: &LlmError, _retry_after_ms: u64) -> Reply {
    match error {
        LlmError::RateLimited { retry_after: Some(d), .. } => Reply::status_with(
            429,
            &[("retry-after", &retry_after_header(u64::try_from(d.as_millis()).unwrap()))],
            "{}",
        ),
        LlmError::RateLimited { .. } => Reply::status(429, r#"{"error":{"message":"rate limit","type":"rate_limit"}}"#),
        LlmError::Overloaded => Reply::status(503, r#"{"error":{"message":"overloaded"}}"#),
        LlmError::Auth => Reply::status(401, r#"{"error":{"message":"Incorrect API key","code":401}}"#),
        LlmError::ContextTooLong { .. } => Reply::status(
            400,
            r#"{"error":{"message":"maximum context length is 8192 tokens","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
        ),
        other => panic!("{other:?} is not a request failure in the mapping"),
    }
}

fn chunk(model: &str, delta: &Value, finish: Option<&str>) -> String {
    let body = json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "model": model, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
    format!("data: {body}\n\n")
}

/// The OpenAI-compatible wire of `events`.
fn openai_wire(events: &[LlmEvent]) -> String {
    let mut out = String::new();
    let mut model = String::from("m");
    let mut call = -1i64;
    let mut done = false;
    for event in events {
        match event {
            LlmEvent::Start { model: m, .. } => {
                model.clone_from(m);
                out.push_str(&chunk(&model, &json!({"role": "assistant", "content": ""}), None));
            }
            LlmEvent::TextDelta(t) => out.push_str(&chunk(&model, &json!({"content": t}), None)),
            LlmEvent::ToolUseStart { id, name, .. } => {
                call += 1;
                let delta = json!({"tool_calls": [{"index": call, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}]});
                out.push_str(&chunk(&model, &delta, None));
            }
            LlmEvent::ToolUseInput(t) => {
                out.push_str(&chunk(
                    &model,
                    &json!({"tool_calls": [{"index": call, "function": {"arguments": t}}]}),
                    None,
                ));
            }
            LlmEvent::Usage(u) => {
                let usage = json!({"prompt_tokens": u.input + u.cache_read, "completion_tokens": u.output,
                                   "prompt_tokens_details": {"cached_tokens": u.cache_read}});
                let body = json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "model": model, "choices": [], "usage": usage});
                let _ = write!(out, "data: {body}\n\n");
            }
            LlmEvent::Stop(reason) => {
                let finish = match reason {
                    StopReason::ToolUse => "tool_calls",
                    StopReason::MaxOutput => "length",
                    StopReason::Refused => "content_filter",
                    _ => "stop",
                };
                out.push_str(&chunk(&model, &json!({}), Some(finish)));
                done = true;
            }
            _ => {}
        }
    }
    if done {
        out.push_str("data: [DONE]\n\n");
    }
    out
}

fn accepts() -> Reply {
    Reply::status(200, r#"{"id":"p","object":"chat.completion","choices":[]}"#)
}

// The signature is `conformance::Make`'s.
#[allow(clippy::needless_pass_by_value)]
fn openai(script: Script) -> Box<dyn LlmClient> {
    // The first two answers belong to the capability probes (tools, then images).
    let mut replies = vec![accepts(), accepts()];
    for turn in &script.turns {
        let plan = plan(turn, &openai_failure);
        replies.extend(plan.failures);
        if let Some((events, tail)) = plan.stream {
            replies.push(sse(&openai_wire(&events), &tail, "data: {not json\n\n"));
        }
    }
    let server = Server::routed(vec![("POST /v1/chat/completions", replies)]);
    let runtime = runtime_handle();
    let client = OpenAiClient::new(&config("openai-compatible", &server, "llama3.2"), &env(&runtime))
        .unwrap()
        .with_timeouts(quick())
        .with_seed(11);
    Box::new(Held { client: Box::new(client), _server: server, _runtime: runtime })
}

#[test]
fn the_openai_compatible_provider_passes_the_conformance_suite() {
    let harness = Harness::new(&openai).with_secret(common::KEY);
    assert_eq!(run(&harness), Ok(()));
}
