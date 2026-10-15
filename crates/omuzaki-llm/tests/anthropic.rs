//! The Anthropic provider against a loopback fake server (llm-api D5, D6, D10): golden transcripts, every retry and
//! surface case with exact request counts, cancellation latency, byte-split invariance and key hygiene.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

mod common;

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use common::{End, Reply, Server, config, drain, env, golden, retries, runtime_handle, transcript, user};
use omuzaki_llm::{AnthropicClient, Timeouts};
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{ContentBlock, Effort, ImageMime, ImageOrigin, Message, Role, TurnRequest};
use omuzaki_llm_api::provider::{ProviderConfig, ProviderEnv, RuntimeHandle};
use omuzaki_llm_api::stream::{Cancel, LlmEvent, StopReason};
use omuzaki_llm_api::usage::Usage;

const MODEL: &str = "claude-haiku-5-5";

/// Short waits, so a retry test takes milliseconds.
fn quick() -> Timeouts {
    Timeouts {
        first_byte: Duration::from_secs(5),
        idle: Duration::from_secs(5),
        backoff_unit: Duration::from_millis(1),
    }
}

struct Rig {
    server: Server,
    runtime: RuntimeHandle,
}

impl Rig {
    fn new(replies: Vec<Reply>) -> Self {
        Self { server: Server::start(replies), runtime: runtime_handle() }
    }

    fn env(&self) -> ProviderEnv {
        env(&self.runtime)
    }

    fn client(&self) -> AnthropicClient {
        AnthropicClient::new(&config("anthropic", &self.server, MODEL), &self.env())
            .unwrap()
            .with_timeouts(quick())
            .with_seed(7)
    }

    fn request() -> TurnRequest {
        TurnRequest::new(user("hello"), 256)
    }
}

fn fixture_reply(name: &str) -> Reply {
    Reply::sse(vec![common::fixture(&format!("anthropic/{name}.sse"))])
}

fn ok_reply() -> Reply {
    fixture_reply("text")
}

fn play(rig: &Rig) -> (Vec<LlmEvent>, Option<LlmError>) {
    let mut stream = rig.client().stream(Rig::request(), &Cancel::new()).unwrap();
    drain(&mut stream)
}

const OVERLOADED: &str = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;

// ---- golden transcripts ----

fn golden_case(fixture: &str, request: TurnRequest) {
    let rig = Rig::new(vec![fixture_reply(fixture)]);
    let mut stream = rig.client().stream(request, &Cancel::new()).unwrap();
    let (events, error) = drain(&mut stream);
    assert_eq!(error, None);
    let text = transcript(&rig.server.requests()[0], &events);
    assert!(!text.contains(common::KEY));
    golden(&format!("anthropic/{fixture}.jsonl"), &text);
}

#[test]
fn golden_text() {
    golden_case("text", Rig::request().with_effort(Effort::Low));
}

#[test]
fn golden_one_tool() {
    let tool = omuzaki_llm_api::message::ToolDef::new("get_weather", "Weather", serde_json::json!({"type": "object"}));
    golden_case("tool", Rig::request().with_tools(vec![tool]));
}

#[test]
fn golden_two_parallel_tools() {
    let tool =
        omuzaki_llm_api::message::ToolDef::new("render_preview", "Render", serde_json::json!({"type": "object"}));
    golden_case("parallel_tools", Rig::request().with_tools(vec![tool]));
}

#[test]
fn golden_thinking_blocks() {
    golden_case("thinking", Rig::request());
}

#[test]
fn the_golden_turns_fold_to_the_contract_turns() {
    let rig = Rig::new(vec![fixture_reply("tool"), fixture_reply("parallel_tools"), fixture_reply("thinking")]);
    let tool = omuzaki_llm_api::message::ToolDef::new("get_weather", "W", serde_json::json!({"type": "object"}));
    let turn = rig
        .client()
        .stream(Rig::request().with_tools(vec![tool]), &Cancel::new())
        .unwrap()
        .collect(common::PATIENCE)
        .unwrap();
    assert_eq!(turn.stop, StopReason::ToolUse);
    assert_eq!(turn.usage, Usage::new(37, 89).with_cache_write(4800));
    assert_eq!(
        turn.message.content,
        vec![
            ContentBlock::text("Okay, let's check the weather for San Francisco, CA:"),
            ContentBlock::tool_use(
                "toolu_01T1x1fJ34qAmk2tNTrN7Up6",
                "get_weather",
                serde_json::json!({"location": "San Francisco, CA"})
            ),
        ]
    );
    let parallel = rig.client().stream(Rig::request(), &Cancel::new()).unwrap().collect(common::PATIENCE).unwrap();
    assert_eq!(parallel.message.content.len(), 2);
    assert_eq!(parallel.usage, Usage::new(100, 61).with_cache_read(4800));
    let thinking = rig.client().stream(Rig::request(), &Cancel::new()).unwrap().collect(common::PATIENCE).unwrap();
    assert!(matches!(&thinking.message.content[0], ContentBlock::Opaque { provider, .. } if provider == "anthropic"));
    assert!(matches!(&thinking.message.content[1], ContentBlock::Opaque { .. }));
    assert_eq!(thinking.message.content[2], ContentBlock::text("The GCD of 1071 and 462 is 21."));
    assert_eq!(thinking.model, MODEL);
}

// ---- the request on the wire ----

#[test]
fn the_request_carries_the_key_version_and_user_agent_and_nothing_else_secret() {
    let rig = Rig::new(vec![ok_reply()]);
    let (_, error) = play(&rig);
    assert_eq!(error, None);
    let request = &rig.server.requests()[0];
    assert_eq!((request.method.as_str(), request.path.as_str()), ("POST", "/v1/messages"));
    assert_eq!(request.header("x-api-key"), Some(common::KEY));
    assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(request.header("user-agent"), Some("Omuzaki/0.0.0"));
    assert_eq!(request.header("accept"), Some("text/event-stream"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert!(request.headers.iter().all(|(n, v)| n == "x-api-key" || !v.contains(common::KEY)));
    assert!(!String::from_utf8_lossy(&request.body).contains(common::KEY));
}

#[test]
fn thinking_blocks_go_back_byte_for_byte_on_the_next_turn() {
    let rig = Rig::new(vec![fixture_reply("thinking"), ok_reply()]);
    let first = rig.client().stream(Rig::request(), &Cancel::new()).unwrap().collect(common::PATIENCE).unwrap();
    let mut history = user("hello");
    history.push(first.message.clone());
    history.push(Message::new(Role::User, vec![ContentBlock::text("again")]));
    let second =
        rig.client().stream(TurnRequest::new(history, 64), &Cancel::new()).unwrap().collect(common::PATIENCE).unwrap();
    assert_eq!(second.stop, StopReason::EndTurn);
    let sent = rig.server.requests()[1].json();
    let assistant = &sent["messages"][1]["content"];
    assert_eq!(assistant[0]["type"], "thinking");
    assert_eq!(assistant[0]["signature"], "EqQBCgIYAhIM1gbcDa9GJwZA2b3hGgxBdjrkzLoky3dl1pkiMOYds");
    assert_eq!(assistant[1]["type"], "redacted_thinking");
    assert_eq!(assistant[1]["data"], "EmwKAhgBEgy3va3pzix/LafPsn4aDFIT2Xlxh0L5L8rLVyIwxtE3rAFBa8cr3qpP");
}

// ---- D5: retried ----

fn retry_succeeds(first: Reply, expected_retries: usize) {
    let rig = Rig::new(vec![first, ok_reply()]);
    let (events, error) = play(&rig);
    assert_eq!(error, None);
    assert_eq!(retries(&events), expected_retries);
    assert!(matches!(events[expected_retries], LlmEvent::Start { .. }), "Retrying comes before Start: {events:?}");
    assert_eq!(rig.server.requests().len(), expected_retries + 1);
}

#[test]
fn a_429_with_retry_after_is_retried_once() {
    retry_succeeds(Reply::status_with(429, &[("retry-after", "0")], "{}"), 1);
}

#[test]
fn a_retry_after_sets_the_wait_in_the_retrying_event() {
    let rig = Rig::new(vec![Reply::status_with(429, &[("retry-after", "1")], "{}"), ok_reply()]);
    let started = Instant::now();
    let (events, error) = play(&rig);
    assert_eq!(error, None);
    assert!(
        matches!(&events[0], LlmEvent::Retrying { attempt: 1, wait, .. } if *wait == Duration::from_secs(1)),
        "{events:?}"
    );
    assert!(started.elapsed() >= Duration::from_millis(950), "it really waited");
}

#[test]
fn the_5xx_family_is_retried() {
    for status in [500, 502, 503, 504, 529] {
        retry_succeeds(Reply::status(status, "{}"), 1);
    }
}

#[test]
fn an_sse_overloaded_error_before_start_is_retried() {
    let error_event = format!("event: error\ndata: {OVERLOADED}\n\n");
    retry_succeeds(Reply::sse_text(&error_event), 1);
}

#[test]
fn five_529s_surface_overloaded_after_exactly_four_retries() {
    let rig = Rig::new((0..5).map(|_| Reply::status(529, OVERLOADED)).collect());
    let (events, error) = play(&rig);
    assert_eq!(error, Some(LlmError::Overloaded));
    assert_eq!(retries(&events), 4);
    assert_eq!(rig.server.requests().len(), 5);
}

#[test]
fn exhausted_retries_surface_the_last_cause() {
    let rig = Rig::new((0..5).map(|_| Reply::status(500, "{}")).collect());
    let (events, error) = play(&rig);
    assert!(matches!(error, Some(LlmError::Network(_))), "{error:?}");
    assert_eq!((retries(&events), rig.server.requests().len()), (4, 5));
}

#[test]
fn a_retry_policy_of_zero_retries_nothing() {
    let rig = Rig::new(vec![Reply::status(529, OVERLOADED), ok_reply()]);
    let config = config("anthropic", &rig.server, MODEL).with_retry(omuzaki_llm_api::provider::RetryPolicy::new(0));
    let client = AnthropicClient::new(&config, &rig.env()).unwrap().with_timeouts(quick());
    let (events, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
    assert_eq!((retries(&events), error), (0, Some(LlmError::Overloaded)));
    assert_eq!(rig.server.requests().len(), 1);
}

#[test]
fn a_refused_connection_is_retried_then_a_network_error() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let runtime = runtime_handle();
    let config = ProviderConfig::new("t", "anthropic", format!("http://127.0.0.1:{port}"))
        .with_key(common::key_ref())
        .with_model(MODEL);
    let client = AnthropicClient::new(&config, &env(&runtime)).unwrap().with_timeouts(quick());
    let (events, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
    assert!(matches!(error, Some(LlmError::Network(_))), "{error:?}");
    assert_eq!(retries(&events), 4);
}

#[test]
fn no_first_byte_in_time_is_retried() {
    let slow = ok_reply().after(Duration::from_millis(400));
    let rig = Rig::new(vec![slow, ok_reply()]);
    let timeouts = Timeouts { first_byte: Duration::from_millis(100), ..quick() };
    let client =
        AnthropicClient::new(&config("anthropic", &rig.server, MODEL), &rig.env()).unwrap().with_timeouts(timeouts);
    let (events, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
    assert_eq!((error, retries(&events)), (None, 1));
}

// ---- D5: surfaced at once ----

fn surfaces(reply: Reply, want: &LlmError) {
    let rig = Rig::new(vec![reply, ok_reply()]);
    let (events, error) = play(&rig);
    assert_eq!(error.as_ref(), Some(want));
    assert_eq!(retries(&events), 0, "no retry for {want:?}");
    assert_eq!(rig.server.requests().len(), 1);
}

#[test]
fn auth_failures_surface_at_once() {
    surfaces(
        Reply::status(401, r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#),
        &LlmError::Auth,
    );
    surfaces(Reply::status(403, "{}"), &LlmError::Auth);
}

#[test]
fn a_429_without_retry_after_surfaces_without_a_retry() {
    surfaces(
        Reply::status(429, r#"{"type":"error","error":{"type":"rate_limit_error","message":"spend cap"}}"#),
        &LlmError::rate_limited(None),
    );
}

#[test]
fn context_errors_name_the_models_window() {
    let window = 1_000_000;
    let too_long = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 1200000 tokens > 1000000 maximum"}}"#;
    surfaces(Reply::status(400, too_long), &LlmError::context_too_long(window));
    surfaces(Reply::status(413, "{}"), &LlmError::context_too_long(window));
}

#[test]
fn another_400_is_malformed_and_carries_the_servers_message() {
    let rig = Rig::new(vec![Reply::status(
        400,
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: required"}}"#,
    )]);
    let (_, error) = play(&rig);
    assert!(matches!(error, Some(LlmError::Malformed(m)) if m.contains("max_tokens: required")));
}

#[test]
fn a_drop_after_start_is_a_network_error_and_is_never_retried() {
    let partial = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-haiku-5-5\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n".to_vec();
    let rig = Rig::new(vec![Reply::sse(vec![partial]).ending(End::Abort), ok_reply()]);
    let mut stream = rig.client().stream(Rig::request(), &Cancel::new()).unwrap();
    let (events, error) = drain(&mut stream);
    assert!(matches!(error, Some(LlmError::Network(_))), "{error:?} after {events:?}");
    assert_eq!(retries(&events), 0);
    assert_eq!(rig.server.requests().len(), 1);
    // The usage seen before the drop is still readable, so the host can charge it.
    assert_eq!(stream.usage_so_far().map(|u| u.tokens()), Some(6));
}

#[test]
fn a_clean_close_before_the_stop_is_a_network_error() {
    let partial = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n".to_vec();
    let rig = Rig::new(vec![Reply::sse(vec![partial])]);
    let (events, error) = play(&rig);
    assert!(matches!(error, Some(LlmError::Network(_))), "{error:?}");
    assert_eq!(retries(&events), 0);
}

#[test]
fn an_sse_overloaded_error_after_start_is_not_retried() {
    let wire = format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"model\":\"m\"}}}}\n\nevent: error\ndata: {OVERLOADED}\n\n"
    );
    let rig = Rig::new(vec![Reply::sse_text(&wire), ok_reply()]);
    let (events, error) = play(&rig);
    assert_eq!((error, retries(&events), rig.server.requests().len()), (Some(LlmError::Overloaded), 0, 1));
}

#[test]
fn silence_after_start_is_a_network_error_at_the_idle_timeout() {
    let partial =
        b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\"}}\n\n".to_vec();
    let rig = Rig::new(vec![Reply::sse(vec![partial]).ending(End::Hold), ok_reply()]);
    let timeouts = Timeouts { idle: Duration::from_millis(150), ..quick() };
    let client =
        AnthropicClient::new(&config("anthropic", &rig.server, MODEL), &rig.env()).unwrap().with_timeouts(timeouts);
    let started = Instant::now();
    let (_, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
    assert!(matches!(error, Some(LlmError::Network(_))), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(rig.server.requests().len(), 1);
}

#[test]
fn malformed_event_streams_are_malformed_errors() {
    for wire in ["this is not sse\n\n", "event: message_start\ndata: {nope\n\n"] {
        let rig = Rig::new(vec![Reply::sse_text(wire), ok_reply()]);
        let (_, error) = play(&rig);
        assert!(matches!(error, Some(LlmError::Malformed(_))), "{wire:?}: {error:?}");
        assert_eq!(rig.server.requests().len(), 1);
    }
}

// ---- building and validating ----

#[test]
fn a_missing_key_is_auth_and_sends_nothing() {
    let rig = Rig::new(vec![ok_reply()]);
    let env = common::env_without_keys(&rig.runtime);
    let client = AnthropicClient::new(&config("anthropic", &rig.server, MODEL), &env).unwrap().with_timeouts(quick());
    let (_, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
    assert_eq!(error, Some(LlmError::Auth));
    assert!(rig.server.requests().is_empty());
}

#[test]
fn an_unpriced_model_cannot_be_built() {
    let rig = Rig::new(Vec::new());
    for model in ["claude-unpriced-9", ""] {
        let built = AnthropicClient::new(&config("anthropic", &rig.server, model), &rig.env());
        assert!(matches!(built, Err(LlmError::Unsupported(_))), "{model:?}");
    }
    let no_model = ProviderConfig::new("t", "anthropic", rig.server.url.clone());
    assert!(
        matches!(AnthropicClient::new(&no_model, &rig.env()), Err(LlmError::Unsupported(m)) if m.contains("no model"))
    );
}

#[test]
fn an_endpoint_off_loopback_must_be_https_and_carry_no_credentials() {
    let rig = Rig::new(Vec::new());
    for endpoint in ["http://api.example.com", "https://user:pw@api.example.com", "ftp://x", "api.example.com"] {
        let config = ProviderConfig::new("t", "anthropic", endpoint).with_model(MODEL);
        assert!(matches!(AnthropicClient::new(&config, &rig.env()), Err(LlmError::Unsupported(_))), "{endpoint}");
    }
    let hosted = ProviderConfig::new("t", "anthropic", "https://api.example.com").with_model(MODEL);
    assert!(AnthropicClient::new(&hosted, &rig.env()).is_ok());
    let default_endpoint = ProviderConfig::new("t", "anthropic", "").with_model(MODEL);
    assert!(AnthropicClient::new(&default_endpoint, &rig.env()).is_ok());
}

#[test]
fn requests_the_model_cannot_take_are_refused_before_anything_is_sent() {
    let rig = Rig::new(vec![ok_reply()]);
    let client = rig.client();
    let too_big = TurnRequest::new(user("x"), 1_000_000);
    assert!(matches!(client.stream(too_big, &Cancel::new()), Err(LlmError::Unsupported(_))));
    let big_image = ContentBlock::image(ImageMime::Png, vec![0; 6 * 1024 * 1024], ImageOrigin::File);
    let with_image = TurnRequest::new(vec![Message::new(Role::User, vec![big_image])], 8);
    assert!(matches!(client.stream(with_image, &Cancel::new()), Err(LlmError::Unsupported(_))));
    let cancelled = Cancel::new();
    cancelled.cancel();
    assert!(matches!(client.stream(Rig::request(), &cancelled), Err(LlmError::Cancelled)));
    assert!(rig.server.requests().is_empty());
}

#[test]
fn a_camera_frame_is_refused_off_loopback_and_allowed_on_loopback() {
    let frame = ContentBlock::image(ImageMime::Png, vec![1, 2, 3], ImageOrigin::Camera);
    let request = || TurnRequest::new(vec![Message::new(Role::User, vec![frame.clone()])], 8);
    let runtime = runtime_handle();
    let remote =
        ProviderConfig::new("t", "anthropic", "https://api.example.com").with_key(common::key_ref()).with_model(MODEL);
    let client = AnthropicClient::new(&remote, &env(&runtime)).unwrap();
    assert!(matches!(client.stream(request(), &Cancel::new()), Err(LlmError::Unsupported(m)) if m.contains("camera")));
    let rig = Rig::new(vec![ok_reply()]);
    assert!(rig.client().stream(request(), &Cancel::new()).is_ok());
}

#[test]
fn the_context_window_override_replaces_the_shipped_one() {
    let rig = Rig::new(Vec::new());
    let config = config("anthropic", &rig.server, MODEL).with_context_window(50_000);
    let client = AnthropicClient::new(&config, &rig.env()).unwrap();
    assert_eq!(client.info().context_window, 50_000);
    let default = rig.client();
    assert_eq!(default.info().context_window, 1_000_000);
    assert_eq!(default.prices().as_of, "2026-10-15");
}

// ---- cancellation ----

#[test]
fn a_cancelled_turn_hangs_up_within_50_ms_worst_of_100() {
    let hello = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\"}}\n\n\
                  event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
                  event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n"
        .to_vec();
    let rig = Rig::new((0..100).map(|_| Reply::sse(vec![hello.clone()]).ending(End::Hold)).collect());
    let client = rig.client();
    let mut worst = Duration::ZERO;
    for i in 0..100 {
        let cancel = Cancel::new();
        let mut stream = client.stream(Rig::request(), &cancel).unwrap();
        // Read until the text arrives, so the connection is open and busy.
        loop {
            match stream.next_timeout(common::PATIENCE).unwrap() {
                omuzaki_llm_api::stream::Next::Event(LlmEvent::TextDelta(_)) => break,
                omuzaki_llm_api::stream::Next::Event(_) => {}
                other => panic!("{other:?}"),
            }
        }
        let at = Instant::now();
        cancel.cancel();
        assert_eq!(stream.try_next(), Err(LlmError::Cancelled));
        assert_eq!(stream.try_next(), Ok(omuzaki_llm_api::stream::Next::End));
        let closed = loop {
            if let Some(closed) = rig.server.closed_at(i) {
                break closed;
            }
            assert!(at.elapsed() < Duration::from_secs(2), "run {i}: the connection never closed");
            std::thread::sleep(Duration::from_millis(1));
        };
        worst = worst.max(closed.saturating_duration_since(at));
    }
    assert!(worst <= Duration::from_millis(50), "the worst of 100 runs closed {worst:?} after cancel()");
}

#[test]
fn cancelling_during_a_backoff_ends_the_turn_at_once() {
    let rig = Rig::new(vec![Reply::status_with(429, &[("retry-after", "30")], "{}"), ok_reply()]);
    let cancel = Cancel::new();
    let mut stream = rig.client().stream(Rig::request(), &cancel).unwrap();
    let next = stream.next_timeout(common::PATIENCE).unwrap();
    assert!(matches!(next, omuzaki_llm_api::stream::Next::Event(LlmEvent::Retrying { .. })), "{next:?}");
    let at = Instant::now();
    cancel.cancel();
    assert_eq!(stream.try_next(), Err(LlmError::Cancelled));
    assert!(at.elapsed() < Duration::from_millis(50));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(rig.server.requests().len(), 1, "no second request after the cancel");
}

#[test]
fn dropping_the_stream_hangs_up_too() {
    let rig =
        Rig::new(vec![Reply::sse(vec![b"event: ping\ndata: {\"type\":\"ping\"}\n\n".to_vec()]).ending(End::Hold)]);
    let stream = rig.client().stream(Rig::request(), &Cancel::new()).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    drop(stream);
    let deadline = Instant::now() + Duration::from_secs(2);
    while rig.server.closed_at(0).is_none() {
        assert!(Instant::now() < deadline, "the connection stayed open after the stream was dropped");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---- splitting, back-pressure, secrets ----

#[test]
fn splitting_the_body_at_any_size_gives_identical_events() {
    let whole = {
        let rig = Rig::new(vec![fixture_reply("tool")]);
        play(&rig).0
    };
    for size in [1, 2, 3, 5, 7, 64] {
        let rig = Rig::new(vec![fixture_reply("tool").split_every(size)]);
        assert_eq!(play(&rig).0, whole, "split into {size}-byte pieces");
    }
}

#[test]
fn a_long_turn_arrives_whole_through_the_bounded_channel() {
    let mut wire = String::from(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
    );
    wire.push_str("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n");
    for i in 0..500 {
        let _ = write!(
            wire,
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{i},\"}}}}\n\n"
        );
    }
    wire.push_str("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":500}}\n\n");
    let rig = Rig::new(vec![Reply::sse_text(&wire)]);
    let mut stream = rig.client().stream(Rig::request(), &Cancel::new()).unwrap();
    // Let the task fill the channel before anyone reads.
    std::thread::sleep(Duration::from_millis(100));
    let turn = stream.collect(common::PATIENCE).unwrap();
    let want = (0..500).fold(String::new(), |mut acc, i| {
        let _ = write!(acc, "{i},");
        acc
    });
    assert_eq!(turn.message.content, vec![ContentBlock::text(want)]);
}

#[test]
fn the_key_appears_in_no_debug_error_or_echoed_message() {
    let rig = Rig::new(vec![Reply::status(
        400,
        &format!(
            r#"{{"type":"error","error":{{"type":"invalid_request_error","message":"bad key {}"}}}}"#,
            common::KEY
        ),
    )]);
    let config = config("anthropic", &rig.server, MODEL);
    let client = AnthropicClient::new(&config, &rig.env()).unwrap().with_timeouts(quick());
    let (events, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
    let shown = format!(
        "{client:?} {config:?} {events:?} {error:?} {}",
        error.as_ref().map(ToString::to_string).unwrap_or_default()
    );
    assert!(!shown.contains(common::KEY), "{shown}");
    assert!(!shown.contains("sk-test"), "{shown}");
}

#[test]
fn listing_models_follows_pages_and_marks_unpriced_ones_unusable() {
    let page1 = r#"{"data":[{"id":"claude-haiku-5-5","type":"model"},{"id":"claude-brand-new-1","type":"model","max_input_tokens":500000,"max_tokens":64000}],"has_more":true,"last_id":"claude-brand-new-1"}"#;
    let page2 = r#"{"data":[{"id":"claude-opus-5-5","type":"model"}],"has_more":false,"last_id":"claude-opus-5-5"}"#;
    let rig = Rig::new(vec![Reply::status(200, page1), Reply::status(200, page2)]);
    let config = config("anthropic", &rig.server, MODEL);
    omuzaki_llm::link();
    let provider = omuzaki_llm_api::provider::provider("anthropic").unwrap().unwrap();
    let models = (provider.list_models)(&config, &rig.env()).unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["claude-haiku-5-5", "claude-brand-new-1", "claude-opus-5-5"]);
    assert_eq!(models[1].context_window, 500_000);
    assert!(!models[1].tools, "an unpriced model is listed but not usable");
    let requests = rig.server.requests();
    assert_eq!(requests[0].path, "/v1/models?limit=100");
    assert_eq!(requests[1].path, "/v1/models?limit=100&after_id=claude-brand-new-1");
    assert_eq!(requests[0].header("x-api-key"), Some(common::KEY));
    let unusable = AnthropicClient::new(&config.clone().with_model("claude-brand-new-1"), &rig.env());
    assert!(matches!(unusable, Err(LlmError::Unsupported(_))));
}

#[test]
fn the_registered_manifest_is_hosted_whatever_the_endpoint() {
    omuzaki_llm::link();
    let provider = omuzaki_llm_api::provider::provider("anthropic").unwrap().unwrap();
    for endpoint in ["https://api.anthropic.com", "http://127.0.0.1:1", "not a url"] {
        let manifest = provider.manifest_for(&ProviderConfig::new("t", "anthropic", endpoint));
        assert_eq!(manifest.privacy, omuzaki_llm_api::provider::Privacy::Hosted, "{endpoint}");
    }
}
