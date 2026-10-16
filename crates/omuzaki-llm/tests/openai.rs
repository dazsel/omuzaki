//! The OpenAI-compatible provider against a loopback fake server (llm-api D4, D5, D6, D10): golden transcripts for
//! Ollama-, llama.cpp- and vLLM-shaped streams, probing and its cache, the loopback rules, and the retry table.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use common::{End, Reply, Server, config, drain, env, golden, retries, runtime_handle, transcript, user};
use omuzaki_llm::{OpenAiClient, Timeouts};
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{ContentBlock, Effort, ImageMime, ImageOrigin, Message, Role, ToolDef, TurnRequest};
use omuzaki_llm_api::provider::{Privacy, ProviderConfig, ProviderEnv, RetryPolicy, RuntimeHandle};
use omuzaki_llm_api::stream::{Cancel, LlmEvent, StopReason};
use omuzaki_llm_api::usage::{PriceTable, PriceTier, Rates, Usage};

const KIND: &str = "openai-compatible";
const CHAT: &str = "POST /v1/chat/completions";

fn quick() -> Timeouts {
    Timeouts {
        first_byte: Duration::from_secs(5),
        idle: Duration::from_secs(5),
        backoff_unit: Duration::from_millis(1),
        ..Timeouts::default()
    }
}

fn fixture_reply(name: &str) -> Reply {
    Reply::sse(vec![common::fixture(&format!("openai/{name}.sse"))])
}

/// A probe answer that accepts the request.
fn accepts() -> Reply {
    Reply::status(
        200,
        r#"{"id":"p","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"h"},"finish_reason":"length"}]}"#,
    )
}

fn rejects() -> Reply {
    Reply::status(400, r#"{"error":{"message":"this model does not support that","type":"invalid_request_error"}}"#)
}

struct Rig {
    server: Server,
    runtime: RuntimeHandle,
}

impl Rig {
    /// A server with no metadata endpoints (404) whose chat route answers `chat`; the first two replies belong to the
    /// probes (tools, then images).
    fn plain(chat: Vec<Reply>) -> Self {
        Self::routed(vec![(CHAT, chat)])
    }

    fn routed(routes: Vec<(&str, Vec<Reply>)>) -> Self {
        Self { server: Server::routed(routes), runtime: runtime_handle() }
    }

    /// A server whose chat route first accepts both probes and then plays `turns`.
    fn probed(turns: Vec<Reply>) -> Self {
        let mut chat = vec![accepts(), accepts()];
        chat.extend(turns);
        Self::plain(chat)
    }

    fn env(&self) -> ProviderEnv {
        env(&self.runtime)
    }

    fn config(&self, model: &str) -> ProviderConfig {
        config(KIND, &self.server, model)
    }

    fn client(&self) -> OpenAiClient {
        OpenAiClient::new(&self.config("llama3.2"), &self.env()).unwrap().with_timeouts(quick()).with_seed(3)
    }

    fn request() -> TurnRequest {
        TurnRequest::new(user("hello"), 256)
    }

    /// The last chat request: the turn itself, after any probes.
    fn turn_request(&self) -> common::Recorded {
        self.server.requests().into_iter().rev().find(|r| r.path == "/v1/chat/completions").unwrap()
    }
}

fn play(rig: &Rig, request: TurnRequest) -> (Vec<LlmEvent>, Option<LlmError>) {
    let mut stream = rig.client().stream(request, &Cancel::new()).unwrap();
    drain(&mut stream)
}

fn tool(name: &str) -> ToolDef {
    ToolDef::new(name, "A tool", serde_json::json!({"type": "object"}))
}

// ---- golden transcripts ----

fn golden_case(fixture: &str, request: TurnRequest) {
    let rig = Rig::probed(vec![fixture_reply(fixture)]);
    let (events, error) = play(&rig, request);
    assert_eq!(error, None);
    let text = transcript(&rig.turn_request(), &events);
    assert!(!text.contains(common::KEY));
    golden(&format!("openai/{fixture}.jsonl"), &text);
}

#[test]
fn golden_ollama_text() {
    golden_case("ollama", Rig::request().with_effort(Effort::Medium));
}

#[test]
fn golden_ollama_streamed_tool_calls() {
    golden_case("ollama_tool", Rig::request().with_tools(vec![tool("render_preview")]));
}

#[test]
fn golden_llamacpp_tool_call_streamed_in_fragments() {
    golden_case("llamacpp", Rig::request().with_tools(vec![tool("render_preview")]));
}

#[test]
fn golden_vllm_two_parallel_calls_with_cached_tokens() {
    golden_case("vllm", Rig::request().with_tools(vec![tool("render_preview")]));
}

#[test]
fn golden_refusal() {
    golden_case("refusal", Rig::request());
}

#[test]
fn the_golden_streams_fold_to_the_contract_turns() {
    let rig = Rig::probed(vec![
        fixture_reply("llamacpp"),
        fixture_reply("vllm"),
        fixture_reply("refusal"),
        fixture_reply("ollama"),
    ]);
    let client = rig.client();
    let collect =
        |request: TurnRequest| client.stream(request, &Cancel::new()).unwrap().collect(common::PATIENCE).unwrap();
    let tools = || Rig::request().with_tools(vec![tool("render_preview")]);

    let llamacpp = collect(tools());
    assert_eq!(llamacpp.stop, StopReason::ToolUse);
    assert_eq!(llamacpp.model, "gpt-3.5-turbo");
    assert_eq!(
        llamacpp.message.content,
        vec![
            ContentBlock::text("I will render it."),
            ContentBlock::tool_use(
                "BxRk2vEyQhP0aLm4cW9zNtJ7sUdG5fHo",
                "render_preview",
                serde_json::json!({"camera": "side"})
            ),
        ]
    );
    assert_eq!(llamacpp.usage, Usage::new(88, 18));

    let vllm = collect(tools());
    // llm-api D4: input is prompt_tokens minus cached_tokens, and the cached tokens are cache_read.
    assert_eq!(vllm.usage, Usage::new(18, 52).with_cache_read(192));
    let ids: Vec<&ContentBlock> = vllm.message.content.iter().collect();
    assert_eq!(ids.len(), 3, "the text and the two calls");

    let refused = collect(Rig::request());
    assert_eq!(refused.stop, StopReason::Refused);
    assert_eq!(refused.message.content, vec![ContentBlock::text("I can't help with that.")]);
    assert_eq!(refused.usage.tokens(), 16, "a refused turn still counts its usage");

    let ollama = collect(Rig::request());
    assert_eq!(ollama.message.content, vec![ContentBlock::text("Here is the render.")]);
    assert_eq!(ollama.model, "llama3.2:3b");
}

// ---- the request on the wire ----

#[test]
fn the_request_sends_a_bearer_key_only_when_one_is_configured() {
    let rig = Rig::probed(vec![fixture_reply("ollama")]);
    assert_eq!(play(&rig, Rig::request()).1, None);
    let sent = rig.turn_request();
    assert_eq!(sent.header("authorization"), Some(&*format!("Bearer {}", common::KEY)));
    assert_eq!(sent.header("user-agent"), Some("Omuzaki/0.0.0"));
    let body = sent.json();
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"], serde_json::json!({"include_usage": true}));
    assert_eq!(body["model"], "llama3.2");

    let keyless = Rig::probed(vec![fixture_reply("ollama")]);
    let config = ProviderConfig::new("t", KIND, keyless.server.url.clone()).with_model("llama3.2");
    let client = OpenAiClient::new(&config, &keyless.env()).unwrap().with_timeouts(quick());
    assert_eq!(drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap()).1, None);
    assert_eq!(keyless.turn_request().header("authorization"), None);
}

#[test]
fn a_configured_key_the_store_lacks_is_auth_and_sends_nothing() {
    let rig = Rig::probed(Vec::new());
    let env = common::env_without_keys(&rig.runtime);
    assert!(matches!(OpenAiClient::new(&rig.config("llama3.2"), &env), Err(LlmError::Auth)));
    assert!(rig.server.requests().is_empty());
}

#[test]
fn effort_is_sent_as_reasoning_effort() {
    let rig = Rig::probed(vec![fixture_reply("ollama")]);
    play(&rig, Rig::request().with_effort(Effort::High));
    assert_eq!(rig.turn_request().json()["reasoning_effort"], "high");
}

// ---- loopback and remote ----

#[test]
fn a_remote_endpoint_needs_allow_remote_https_and_prices() {
    let runtime = runtime_handle();
    let env = env(&runtime);
    let remote =
        |endpoint: &str| ProviderConfig::new("t", KIND, endpoint).with_key(common::key_ref()).with_model("gpt-x");
    assert!(
        matches!(OpenAiClient::new(&remote("https://api.example.com/v1"), &env), Err(LlmError::Unsupported(m)) if m.contains("allow_remote"))
    );
    assert!(
        matches!(OpenAiClient::new(&remote("http://api.example.com").with_allow_remote(true), &env), Err(LlmError::Unsupported(m)) if m.contains("plain http"))
    );
    assert!(
        matches!(OpenAiClient::new(&remote("https://api.example.com").with_allow_remote(true), &env), Err(LlmError::Unsupported(m)) if m.contains("prices"))
    );
    let prices = PriceTable::new("2026-10-15", "test", vec![PriceTier::new(None, Rates::new(1, 2))]);
    let built = OpenAiClient::new(&remote("https://api.example.com").with_allow_remote(true).with_prices(prices), &env)
        .unwrap();
    // A remote host is never probed: tools and images are assumed, and no request has been made.
    assert!(built.info().tools && built.info().vision);
    assert_eq!(built.prices().as_of, "2026-10-15");
}

#[test]
fn a_camera_frame_is_refused_off_loopback() {
    let runtime = runtime_handle();
    let prices = PriceTable::new("2026-10-15", "test", vec![PriceTier::new(None, Rates::new(1, 2))]);
    let config = ProviderConfig::new("t", KIND, "https://api.example.com")
        .with_allow_remote(true)
        .with_prices(prices)
        .with_model("m");
    let client = OpenAiClient::new(&config, &env(&runtime)).unwrap();
    let frame = ContentBlock::image(ImageMime::Png, vec![1], ImageOrigin::Camera);
    let request = TurnRequest::new(vec![Message::new(Role::User, vec![frame])], 8);
    assert!(matches!(client.stream(request, &Cancel::new()), Err(LlmError::Unsupported(m)) if m.contains("camera")));
}

#[test]
fn the_manifest_privacy_follows_the_configured_endpoint() {
    omuzaki_llm::link();
    let provider = omuzaki_llm_api::provider::provider(KIND).unwrap().unwrap();
    let privacy = |endpoint: &str| provider.manifest_for(&ProviderConfig::new("t", KIND, endpoint)).privacy;
    assert_eq!(privacy("http://127.0.0.1:11434"), Privacy::Loopback);
    assert_eq!(privacy("http://localhost:8080/v1"), Privacy::Loopback);
    assert_eq!(privacy("https://api.example.com/v1"), Privacy::Hosted);
    assert_eq!(privacy("not a url"), Privacy::Hosted);
    assert_eq!((provider.manifest)().privacy, Privacy::Hosted, "without a configuration the stricter class applies");
}

// ---- probing ----

fn chat_count(rig: &Rig) -> usize {
    rig.server.count(CHAT)
}

#[test]
fn an_ollama_server_answers_capabilities_and_context_so_nothing_is_probed() {
    let show = r#"{"details":{"family":"llama"},"model_info":{"general.architecture":"llama","llama.context_length":131072},"capabilities":["completion","tools","vision"]}"#;
    let rig = Rig::routed(vec![("POST /api/show", vec![Reply::status(200, show)]), (CHAT, vec![])]);
    let client = OpenAiClient::new(&rig.config("llama3.2"), &rig.env()).unwrap();
    assert_eq!((client.info().context_window, client.info().tools, client.info().vision), (131_072, true, true));
    assert_eq!(chat_count(&rig), 0);
    assert_eq!(
        rig.server.requests().iter().find(|r| r.path == "/api/show").unwrap().json(),
        serde_json::json!({"model": "llama3.2"})
    );
}

#[test]
fn an_ollama_model_without_the_tools_capability_is_probed_for_tools() {
    let show = r#"{"model_info":{"qwen2.context_length":32768},"capabilities":["completion"]}"#;
    let rig = Rig::routed(vec![("POST /api/show", vec![Reply::status(200, show)]), (CHAT, vec![rejects()])]);
    let client = OpenAiClient::new(&rig.config("tiny"), &rig.env()).unwrap();
    assert_eq!((client.info().tools, client.info().vision, client.info().context_window), (false, false, 32_768));
    assert_eq!(chat_count(&rig), 1, "the vision question was answered by the capabilities");
    // A request with tools is then refused before it is sent.
    let with_tools = Rig::request().with_tools(vec![tool("t")]);
    assert!(matches!(client.stream(with_tools, &Cancel::new()), Err(LlmError::Unsupported(_))));
}

#[test]
fn a_llamacpp_server_answers_context_and_vision_in_props_and_tools_are_probed() {
    let props = r#"{"default_generation_settings":{"n_ctx":4096,"params":{"temperature":0.8}},"total_slots":1,"modalities":{"vision":false}}"#;
    let rig = Rig::routed(vec![("GET /props", vec![Reply::status(200, props)]), (CHAT, vec![accepts()])]);
    let client = OpenAiClient::new(&rig.config("gpt-3.5-turbo"), &rig.env()).unwrap();
    assert_eq!((client.info().context_window, client.info().tools, client.info().vision), (4096, true, false));
    assert_eq!(chat_count(&rig), 1);
}

#[test]
fn a_vllm_server_reports_its_window_in_the_model_list() {
    let list = r#"{"object":"list","data":[{"id":"other","max_model_len":1},{"id":"Qwen/Qwen2.5-7B-Instruct","object":"model","max_model_len":32768}]}"#;
    let rig = Rig::routed(vec![("GET /v1/models", vec![Reply::status(200, list)]), (CHAT, vec![accepts(), accepts()])]);
    let client = OpenAiClient::new(&rig.config("Qwen/Qwen2.5-7B-Instruct"), &rig.env()).unwrap();
    assert_eq!(client.info().context_window, 32_768);
}

#[test]
fn rejected_probes_mark_no_tools_and_no_vision() {
    let rig = Rig::plain(vec![rejects(), rejects()]);
    let client = OpenAiClient::new(&rig.config("small"), &rig.env()).unwrap();
    assert_eq!((client.info().tools, client.info().vision), (false, false));
    let probes: Vec<_> = rig.server.requests().into_iter().filter(|r| r.path == "/v1/chat/completions").collect();
    assert_eq!(probes[0].json()["max_tokens"], 1);
    assert!(probes[0].json()["tools"].is_array(), "the first probe carries a trivial tool");
    let image = probes[1].json()["messages"][0]["content"][1]["image_url"]["url"].as_str().unwrap().to_owned();
    assert!(image.starts_with("data:image/png;base64,"), "the second probe carries a 1x1 PNG");
    let png = Message::new(Role::User, vec![ContentBlock::image(ImageMime::Png, vec![1], ImageOrigin::File)]);
    assert!(matches!(client.stream(TurnRequest::new(vec![png], 8), &Cancel::new()), Err(LlmError::Unsupported(_))));
}

#[test]
fn nothing_reporting_a_window_falls_back_to_8192_and_the_override_wins() {
    let rig = Rig::plain(vec![accepts(), accepts()]);
    assert_eq!(OpenAiClient::new(&rig.config("m"), &rig.env()).unwrap().info().context_window, 8192);
    let rig = Rig::plain(vec![accepts(), accepts()]);
    let config = rig.config("m").with_context_window(65_536);
    assert_eq!(OpenAiClient::new(&config, &rig.env()).unwrap().info().context_window, 65_536);
}

#[test]
fn probes_are_cached_per_endpoint_and_model() {
    let rig = Rig::plain(vec![accepts(), accepts(), accepts(), accepts()]);
    let first = OpenAiClient::new(&rig.config("a"), &rig.env()).unwrap();
    let second = OpenAiClient::new(&rig.config("a"), &rig.env()).unwrap();
    assert_eq!(chat_count(&rig), 2, "the second build reused the answer");
    assert_eq!(first.info(), second.info());
    OpenAiClient::new(&rig.config("b"), &rig.env()).unwrap();
    assert_eq!(chat_count(&rig), 4, "another model is probed again");
    // The same model on another endpoint is probed again too.
    let other = Rig::plain(vec![accepts(), accepts()]);
    OpenAiClient::new(&other.config("a"), &other.env()).unwrap();
    assert_eq!(chat_count(&other), 2);
}

#[test]
fn a_probe_that_says_nothing_is_an_error_and_is_not_cached() {
    let rig = Rig::plain(vec![Reply::status(500, "{}"), accepts(), accepts()]);
    let config = rig.config("flaky");
    assert!(matches!(OpenAiClient::new(&config, &rig.env()), Err(LlmError::Network(_))));
    assert!(OpenAiClient::new(&config, &rig.env()).is_ok(), "the next build asks again");
}

#[test]
fn a_refused_key_while_probing_is_auth() {
    let rig = Rig::plain(vec![Reply::status(401, r#"{"error":{"message":"bad key","code":401}}"#)]);
    assert!(matches!(OpenAiClient::new(&rig.config("locked"), &rig.env()), Err(LlmError::Auth)));
}

#[test]
fn an_unreachable_server_is_a_network_error_at_build() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let runtime = runtime_handle();
    let config = ProviderConfig::new("t", KIND, format!("http://127.0.0.1:{port}")).with_model("m");
    assert!(matches!(OpenAiClient::new(&config, &env(&runtime)), Err(LlmError::Network(_))));
}

#[test]
fn listing_models_reads_ids_windows_and_vision_hints() {
    let list = r#"{"object":"list","data":[{"id":"a","max_model_len":2048},{"id":"b","architecture":{"input_modalities":["text","image"]}},{"object":"model"}]}"#;
    let rig = Rig::routed(vec![("GET /v1/models", vec![Reply::status(200, list)])]);
    omuzaki_llm::link();
    let provider = omuzaki_llm_api::provider::provider(KIND).unwrap().unwrap();
    let models = (provider.list_models)(&rig.config("a"), &rig.env()).unwrap();
    let summary: Vec<(&str, u64, bool)> = models.iter().map(|m| (m.id.as_str(), m.context_window, m.vision)).collect();
    assert_eq!(summary, [("a", 2048, false), ("b", 8192, true)]);
    let remote = ProviderConfig::new("t", KIND, "https://api.example.com");
    assert!(matches!((provider.list_models)(&remote, &rig.env()), Err(LlmError::Unsupported(_))));
}

// ---- D5 on this wire ----

#[test]
fn a_429_with_retry_after_and_a_503_are_retried_and_the_count_is_exact() {
    for first in [Reply::status_with(429, &[("retry-after", "0")], "{}"), Reply::status(503, "loading model")] {
        let rig = Rig::probed(vec![first, fixture_reply("ollama")]);
        let (events, error) = play(&rig, Rig::request());
        assert_eq!((error, retries(&events)), (None, 1));
        assert_eq!(chat_count(&rig), 4, "two probes, the failed turn and the retry");
    }
}

#[test]
fn five_503s_surface_overloaded_after_four_retries() {
    let rig = Rig::probed((0..5).map(|_| Reply::status(503, "{}")).collect());
    let (events, error) = play(&rig, Rig::request());
    assert_eq!((error, retries(&events)), (Some(LlmError::Overloaded), 4));
    assert_eq!(chat_count(&rig), 7);
}

#[test]
fn auth_context_and_malformed_answers_surface_at_once() {
    let cases = [
        (Reply::status(401, "{}"), LlmError::Auth),
        (Reply::status(429, "{}"), LlmError::rate_limited(None)),
        (
            Reply::status(400, r#"{"error":{"message":"too long","code":"context_length_exceeded"}}"#),
            LlmError::context_too_long(8192),
        ),
        (Reply::status(413, ""), LlmError::context_too_long(8192)),
    ];
    for (reply, want) in cases {
        let rig = Rig::probed(vec![reply, fixture_reply("ollama")]);
        let (events, error) = play(&rig, Rig::request());
        assert_eq!((error.as_ref(), retries(&events)), (Some(&want), 0));
        assert_eq!(chat_count(&rig), 3, "no retry");
    }
    let rig = Rig::probed(vec![Reply::status(400, r#"{"error":{"message":"bad field x"}}"#)]);
    assert!(matches!(play(&rig, Rig::request()).1, Some(LlmError::Malformed(m)) if m.contains("bad field x")));
}

#[test]
fn a_drop_after_the_first_chunk_is_a_network_error_and_never_retried() {
    let chunk = b"data: {\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n\n".to_vec();
    let rig = Rig::probed(vec![Reply::sse(vec![chunk]).ending(End::Abort), fixture_reply("ollama")]);
    let (events, error) = play(&rig, Rig::request());
    assert!(matches!(error, Some(LlmError::Network(_))), "{error:?}");
    assert_eq!(retries(&events), 0);
    assert_eq!(chat_count(&rig), 3);
    assert!(events.contains(&LlmEvent::TextDelta("par".into())));
}

#[test]
fn length_and_content_filter_finish_reasons_map_to_stop_reasons() {
    for (reason, want) in [("length", StopReason::MaxOutput), ("content_filter", StopReason::Refused)] {
        let wire = format!(
            "data: {{\"model\":\"m\",\"choices\":[{{\"delta\":{{\"content\":\"x\"}},\"finish_reason\":\"{reason}\"}}]}}\n\n\
             data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":3,\"completion_tokens\":1}}}}\n\ndata: [DONE]\n\n"
        );
        let rig = Rig::probed(vec![Reply::sse_text(&wire)]);
        let turn = rig.client().stream(Rig::request(), &Cancel::new()).unwrap().collect(common::PATIENCE).unwrap();
        assert_eq!((turn.stop, turn.usage.tokens()), (want, 4), "{reason}");
    }
}

#[test]
fn a_server_that_closes_after_the_finish_chunk_without_done_still_completes() {
    let wire = "data: {\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n";
    let rig = Rig::probed(vec![Reply::sse_text(wire)]);
    let turn = rig.client().stream(Rig::request(), &Cancel::new()).unwrap().collect(common::PATIENCE).unwrap();
    assert_eq!(turn.stop, StopReason::EndTurn);
    assert_eq!(turn.usage.tokens(), 5);
}

#[test]
fn a_retry_policy_of_zero_retries_nothing() {
    let rig = Rig::probed(vec![Reply::status(503, "{}"), fixture_reply("ollama")]);
    let config = rig.config("llama3.2").with_retry(RetryPolicy::new(0));
    let client = OpenAiClient::new(&config, &rig.env()).unwrap().with_timeouts(quick());
    let (events, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
    assert_eq!((retries(&events), error), (0, Some(LlmError::Overloaded)));
}

// ---- cancellation, splitting, secrets ----

#[test]
fn splitting_the_body_at_any_size_gives_identical_events() {
    for fixture in ["ollama", "llamacpp", "vllm"] {
        let whole =
            play(&Rig::probed(vec![fixture_reply(fixture)]), Rig::request().with_tools(vec![tool("render_preview")])).0;
        for size in [1, 3, 7, 64] {
            let rig = Rig::probed(vec![fixture_reply(fixture).split_every(size)]);
            assert_eq!(
                play(&rig, Rig::request().with_tools(vec![tool("render_preview")])).0,
                whole,
                "{fixture} in {size}-byte pieces"
            );
        }
    }
}

#[test]
fn the_key_appears_in_no_debug_error_or_echoed_message() {
    let echo = format!(r#"{{"error":{{"message":"unauthorised: {}","type":"invalid_request_error"}}}}"#, common::KEY);
    let rig = Rig::probed(vec![Reply::status(400, &echo)]);
    let config = rig.config("llama3.2");
    let client = OpenAiClient::new(&config, &rig.env()).unwrap().with_timeouts(quick());
    let (events, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
    let shown = format!("{client:?} {config:?} {events:?} {error:?}");
    assert!(!shown.contains("sk-test"), "{shown}");
}

// ---- metadata, keep-alives, hosted prices ----

const CHUNK: &str = "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"h\"}}]}\n\n";

#[test]
fn a_model_list_error_never_echoes_the_key() {
    let echo = format!(r#"{{"error":{{"message":"bad key {}","type":"invalid_request_error"}}}}"#, common::KEY);
    let rig = Rig::routed(vec![("GET /v1/models", vec![Reply::status(400, &echo)])]);
    omuzaki_llm::link();
    let provider = omuzaki_llm_api::provider::provider(KIND).unwrap().unwrap();
    let error = (provider.list_models)(&rig.config("a"), &rig.env()).unwrap_err();
    let shown = format!("{error:?} {error}");
    assert!(shown.contains("bad key"), "the message must reach the error for this test to mean anything: {shown}");
    assert!(!shown.contains("sk-test"), "{shown}");
}

#[test]
fn a_hosted_model_with_empty_or_all_zero_prices_is_refused() {
    let runtime = runtime_handle();
    let provider_env = env(&runtime);
    let hosted = |prices| {
        ProviderConfig::new("t", KIND, "https://api.example.com")
            .with_key(common::key_ref())
            .with_model("gpt-x")
            .with_allow_remote(true)
            .with_prices(prices)
    };
    let empty = PriceTable::new("2026-10-15", "test", vec![]);
    let zero = PriceTable::new("2026-10-15", "test", vec![PriceTier::new(None, Rates::default())]);
    for prices in [empty, zero] {
        assert!(matches!(OpenAiClient::new(&hosted(prices), &provider_env), Err(LlmError::Unsupported(_))));
    }
}

#[test]
fn a_metadata_request_that_never_answers_ends_at_the_metadata_timeout() {
    let hang = Reply::sse(vec![]).after(Duration::from_secs(5));
    let rig = Rig::routed(vec![("GET /v1/models", vec![hang])]);
    let timeouts = Timeouts { metadata: Duration::from_millis(150), ..quick() };
    let started = std::time::Instant::now();
    let error = omuzaki_llm::openai::list_models_within(&rig.config("a"), &rig.env(), &timeouts).unwrap_err();
    assert!(matches!(error, LlmError::Network(_)), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_probe_that_never_answers_ends_at_the_probe_deadline() {
    let hang = Reply::sse(vec![]).after(Duration::from_secs(5));
    let rig = Rig::plain(vec![hang]);
    let timeouts = Timeouts { probe: Duration::from_millis(200), ..quick() };
    let started = std::time::Instant::now();
    let error = OpenAiClient::new_within(&rig.config("m"), &rig.env(), &timeouts).map(|_| ()).unwrap_err();
    assert!(matches!(error, LlmError::Network(_)), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn keep_alive_comments_outlast_the_idle_timeout() {
    let wire = common::fixture("openai/ollama.sse");
    let cut = wire.windows(2).position(|w| w == b"\n\n").unwrap() + 2;
    let mut chunks = vec![wire[..cut].to_vec()];
    chunks.extend((0..5).map(|_| b": ka\n\n".to_vec()));
    chunks.push(wire[cut..].to_vec());
    let reply = Reply::sse(chunks).pausing(Duration::from_millis(120));
    let rig = Rig::probed(vec![reply]);
    let timeouts = Timeouts { idle: Duration::from_millis(300), ..quick() };
    let client = OpenAiClient::new(&rig.config("llama3.2"), &rig.env()).unwrap().with_timeouts(timeouts);
    let (events, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
    assert_eq!(error, None, "{events:?}");
}

#[test]
fn silence_after_an_event_times_out_at_one_idle_interval() {
    let reply = Reply::sse(vec![CHUNK.as_bytes().to_vec()]).ending(End::Hold);
    let rig = Rig::probed(vec![reply]);
    let timeouts = Timeouts { idle: Duration::from_millis(300), ..quick() };
    let config = rig.config("llama3.2").with_retry(RetryPolicy::new(0));
    let client = OpenAiClient::new(&config, &rig.env()).unwrap().with_timeouts(timeouts);
    let started = std::time::Instant::now();
    let (_, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
    assert!(matches!(error, Some(LlmError::Network(_))), "{error:?}");
    assert!(started.elapsed() < Duration::from_millis(550), "{:?}", started.elapsed());
}

#[test]
fn a_turn_error_message_is_redacted_whatever_its_json_spelling() {
    let escaped: String = common::KEY.chars().map(|c| format!("\\u{:04x}", u32::from(c))).collect();
    for key_text in [common::KEY.to_owned(), escaped] {
        let echo = format!(r#"{{"error":{{"message":"unauthorised {key_text}","type":"invalid_request_error"}}}}"#);
        let rig = Rig::probed(vec![Reply::status(400, &echo)]);
        let client = OpenAiClient::new(&rig.config("llama3.2"), &rig.env()).unwrap().with_timeouts(quick());
        let (events, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
        let shown = format!("{events:?} {error:?}");
        assert!(shown.contains("unauthorised"), "{shown}");
        assert!(!shown.contains("sk-test"), "{shown}");
    }
}

#[test]
fn a_retry_after_on_a_5xx_or_overload_sets_the_wait() {
    for status in [500, 503] {
        let rig = Rig::probed(vec![Reply::status_with(status, &[("retry-after", "1")], "{}"), fixture_reply("ollama")]);
        let client = OpenAiClient::new(&rig.config("llama3.2"), &rig.env()).unwrap().with_timeouts(quick());
        let (events, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
        assert_eq!(error, None, "{status}");
        assert!(
            matches!(&events[0], LlmEvent::Retrying { attempt: 1, wait, .. } if *wait == Duration::from_secs(1)),
            "{status}: {events:?}"
        );
    }
}

#[test]
fn a_turn_with_no_usage_report_is_an_error_not_a_free_turn() {
    let finish = "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
    let wire = format!("{CHUNK}{finish}data: [DONE]\n\n");
    let rig = Rig::probed(vec![Reply::sse_text(&wire)]);
    let client = OpenAiClient::new(&rig.config("llama3.2"), &rig.env()).unwrap().with_timeouts(quick());
    let (events, error) = drain(&mut client.stream(Rig::request(), &Cancel::new()).unwrap());
    assert!(matches!(error, Some(LlmError::Malformed(ref m)) if m.contains("usage")), "{error:?}");
    assert!(!events.iter().any(|e| matches!(e, LlmEvent::Stop(_))), "{events:?}");
}

#[test]
fn parallel_tool_results_with_images_reach_the_server_with_every_tool_reply_first() {
    let calls = vec![
        ContentBlock::tool_use("a", "look", serde_json::json!({})),
        ContentBlock::tool_use("b", "look", serde_json::json!({})),
    ];
    let shot = |id: &str, byte: u8| {
        ContentBlock::tool_result(
            id,
            vec![ContentBlock::text(id), ContentBlock::image(ImageMime::Png, vec![byte], ImageOrigin::Render)],
        )
    };
    let history = vec![
        Message::new(Role::User, vec![ContentBlock::text("go")]),
        Message::new(Role::Assistant, calls),
        Message::new(Role::User, vec![shot("a", 1), shot("b", 2)]),
    ];
    let rig = Rig::probed(vec![fixture_reply("ollama")]);
    let client = OpenAiClient::new(&rig.config("llama3.2"), &rig.env()).unwrap().with_timeouts(quick());
    let (_, error) = drain(&mut client.stream(TurnRequest::new(history, 64), &Cancel::new()).unwrap());
    assert_eq!(error, None);
    let sent = rig.server.requests().pop().unwrap().json();
    let roles: Vec<&str> = sent["messages"].as_array().unwrap().iter().filter_map(|m| m["role"].as_str()).collect();
    assert_eq!(roles, ["user", "assistant", "tool", "tool", "user", "user"]);
}

#[test]
fn encoding_a_large_history_happens_off_the_calling_thread() {
    // An 8 x 8 PNG header followed by padding up to 4 MiB: small on the long side, large in bytes.
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
    png.extend([0, 0, 0, 8, 0, 0, 0, 8, 8, 6, 0, 0, 0]);
    png.resize(4 * 1024 * 1024, 7);
    let image = || ContentBlock::image(ImageMime::Png, png.clone(), ImageOrigin::File);
    let history = vec![Message::new(Role::User, vec![ContentBlock::text("look"), image(), image(), image()])];
    let rig = Rig::probed(vec![fixture_reply("ollama")]);
    let client = OpenAiClient::new(&rig.config("llama3.2"), &rig.env()).unwrap().with_timeouts(quick());
    let started = std::time::Instant::now();
    let mut stream = client.stream(TurnRequest::new(history, 64), &Cancel::new()).unwrap();
    let call = started.elapsed();
    let (_, error) = drain(&mut stream);
    assert_eq!(error, None);
    assert!(call < Duration::from_millis(15), "stream() took {call:?} on the calling thread");
}
