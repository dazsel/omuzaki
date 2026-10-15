//! After `cancel()`, a turn's connection closes within 50 ms, the worst of 100 runs, measured at the server (llm-api
//! D5). These tests have a binary of their own, and take turns inside it, so no other test competes for the CPU while
//! a stopwatch is running.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::{End, Reply, Server, config, runtime_handle, user};
use omuzaki_llm::{AnthropicClient, OpenAiClient};
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::TurnRequest;
use omuzaki_llm_api::stream::{Cancel, LlmEvent, Next};

const RUNS: usize = 100;
const LIMIT: Duration = Duration::from_millis(50);

/// One latency test at a time.
static ALONE: Mutex<()> = Mutex::new(());

/// Starts `RUNS` turns on `client`, cancels each once its first text has arrived, and returns the longest time the
/// server saw between `cancel()` and the connection closing.
fn worst_close(server: &Server, client: &dyn LlmClient) -> Duration {
    let mut worst = Duration::ZERO;
    for run in 0..RUNS {
        let cancel = Cancel::new();
        let mut stream = client.stream(TurnRequest::new(user("hello"), 64), &cancel).unwrap();
        // Read until text arrives, so the connection is open and busy.
        loop {
            match stream.next_timeout(common::PATIENCE).unwrap() {
                Next::Event(LlmEvent::TextDelta(_)) => break,
                Next::Event(_) => {}
                other => panic!("{other:?}"),
            }
        }
        // Turns run one at a time, so the newest request is this turn's.
        let index = server.requests().len() - 1;
        let at = Instant::now();
        cancel.cancel();
        let closed = loop {
            if let Some(closed) = server.closed_at(index) {
                break closed;
            }
            assert!(at.elapsed() < Duration::from_secs(2), "run {run}: the connection never closed");
            std::thread::sleep(Duration::from_millis(1));
        };
        worst = worst.max(closed.saturating_duration_since(at));
        // The caller touched the stream only after the hang-up, so the turn task's own 10 ms tick did the closing.
        assert_eq!(stream.try_next(), Err(LlmError::Cancelled));
        assert_eq!(stream.try_next(), Ok(Next::End));
    }
    worst
}

#[test]
fn an_anthropic_turn_hangs_up_within_50_ms_worst_of_100() {
    let _alone = ALONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let hello = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\"}}\n\n\
                  event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
                  event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n"
        .to_vec();
    let server = Server::start((0..RUNS).map(|_| Reply::sse(vec![hello.clone()]).ending(End::Hold)).collect());
    let runtime = runtime_handle();
    let client =
        AnthropicClient::new(&config("anthropic", &server, "claude-haiku-5-5"), &common::env(&runtime)).unwrap();
    let worst = worst_close(&server, &client);
    eprintln!("worst of {RUNS} runs: {worst:?}");
    assert!(worst <= LIMIT, "the worst of {RUNS} runs closed {worst:?} after cancel()");
}

#[test]
fn an_openai_compatible_turn_hangs_up_within_50_ms_worst_of_100() {
    let _alone = ALONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let hello = b"data: {\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n".to_vec();
    let accepts = || Reply::status(200, r#"{"id":"p","object":"chat.completion","choices":[]}"#);
    // The first two answers belong to the capability probes.
    let mut chat = vec![accepts(), accepts()];
    chat.extend((0..RUNS).map(|_| Reply::sse(vec![hello.clone()]).ending(End::Hold)));
    let server = Server::routed(vec![("POST /v1/chat/completions", chat)]);
    let runtime = runtime_handle();
    let client = OpenAiClient::new(&config("openai-compatible", &server, "llama3.2"), &common::env(&runtime)).unwrap();
    let worst = worst_close(&server, &client);
    eprintln!("worst of {RUNS} runs: {worst:?}");
    assert!(worst <= LIMIT, "the worst of {RUNS} runs closed {worst:?} after cancel()");
}
