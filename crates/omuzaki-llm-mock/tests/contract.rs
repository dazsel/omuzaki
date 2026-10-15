//! The acceptance checks of #1037, run on llm-api §4's worked example.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{
    ContentBlock, ImageMime, ImageOrigin, Message, Role, SystemBlock, ToolDef, TurnRequest,
};
use omuzaki_llm_api::stream::{Cancel, LlmEvent, Next, StopReason, Turn};
use omuzaki_llm_api::usage::{Ledger, MicroUsd, PriceTable, PriceTier, Rates, Usage};
use omuzaki_llm_mock::FakeLlm;

const SCRIPT: &str = r#"{"model": "claude-haiku-5-5", "turns": [
 {"expect": {"user_contains": "front"}, "chunk": 8, "events": [
   {"Start": {"model": "claude-haiku-5-5"}},
   {"ToolUseStart": {"id": "tu_1", "name": "render_preview"}},
   {"ToolUseInput": "{\"character\":\"main\",\"camera\":\"front\",\"size\":512}"},
   {"ToolUseEnd": {"id": "tu_1"}},
   {"Usage": {"input": 37, "output": 58, "cache_read": 0, "cache_write": 4800}},
   {"Stop": "ToolUse"}]},
 {"expect": {"answers": ["tu_1"]}, "chunk": 8, "events": [
   {"Start": {"model": "claude-haiku-5-5"}},
   {"TextDelta": "Here she is, from the front."},
   {"Usage": {"input": 413, "output": 23, "cache_read": 4800, "cache_write": 0}},
   {"Stop": "EndTurn"}]}]}"#;

const IDLE: Duration = Duration::from_millis(10);

fn text(role: Role, t: &str) -> Message {
    Message::new(role, vec![ContentBlock::Text(t.into())])
}

fn request(messages: Vec<Message>) -> TurnRequest {
    let tool = ToolDef::new("render_preview", "Render", "{}".parse().unwrap());
    TurnRequest::new(messages, 1024)
        .with_system(vec![SystemBlock::new("You help.").with_cache_after(true)])
        .with_tools(vec![tool])
}

/// The history of turn 2: the question, the tool call, and its result (29 bytes of text and a 512 x 512 PNG header).
fn turn_two(first: &Turn) -> TurnRequest {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
    png.extend_from_slice(&512u32.to_be_bytes());
    png.extend_from_slice(&512u32.to_be_bytes());
    let result = ContentBlock::ToolResult {
        id: "tu_1".into(),
        blocks: vec![
            ContentBlock::Text("Rendered main, front, 512 px.".into()),
            ContentBlock::Image { mime: ImageMime::Png, bytes: png, origin: ImageOrigin::Render },
        ],
        is_error: false,
    };
    request(vec![text(Role::User, "show me the front"), first.message.clone(), Message::new(Role::User, vec![result])])
}

fn haiku_prices() -> PriceTable {
    let rates = Rates::new(100_000, 500_000).with_cache_write(125_000).with_cache_read(10_000);
    PriceTable::new("2026-10-13", "test", vec![PriceTier::new(None, rates)])
}

fn play() -> (FakeLlm, Turn, Turn) {
    let llm = FakeLlm::from_json(SCRIPT).unwrap();
    let cancel = Cancel::new();
    let one = llm.stream(request(vec![text(Role::User, "show me the front")]), &cancel).unwrap().collect(IDLE).unwrap();
    let two = llm.stream(turn_two(&one), &cancel).unwrap().collect(IDLE).unwrap();
    (llm, one, two)
}

#[test]
fn collect_folds_the_worked_example_into_two_turns() {
    let (_, one, two) = play();
    assert_eq!(one.stop, StopReason::ToolUse);
    assert_eq!(one.usage, Usage::new(37, 58).with_cache_write(4800));
    assert_eq!((one.usage.prompt(), one.usage.tokens()), (4837, 4895));
    let call = ContentBlock::ToolUse {
        id: "tu_1".into(),
        name: "render_preview".into(),
        input: serde_json::json!({"character": "main", "camera": "front", "size": 512}),
    };
    assert_eq!(one.message.content, vec![call]);
    assert_eq!(two.stop, StopReason::EndTurn);
    assert_eq!(two.message, text(Role::Assistant, "Here she is, from the front."));
    assert_eq!((two.usage.prompt(), two.usage.tokens(), two.model.as_str()), (5213, 5236, "claude-haiku-5-5"));
}

#[test]
fn the_ledger_bounds_turn_two_at_1177_micro_dollars() {
    let (llm, one, _) = play();
    let mut ledger = Ledger::new();
    let first = llm.requests().remove(0);
    ledger.observe(&first, &one);
    let req = turn_two(&one);
    assert_eq!(ledger.prompt_estimate(&req, llm.info()), 5313);
    assert_eq!(ledger.bound(&req, llm.info(), &haiku_prices()), MicroUsd(1177));
}

#[test]
fn a_first_turn_is_estimated_from_everything() {
    let llm = FakeLlm::from_json(SCRIPT).unwrap();
    let req = request(vec![text(Role::User, "hello")]);
    // system 9 + 8, tool (14 + 6 + 2) + 8, message 5 + 8.
    assert_eq!(Ledger::new().prompt_estimate(&req, llm.info()), 60);
}

#[test]
fn a_cancel_mid_script_fails_the_next_call_and_nothing_follows() {
    let llm = FakeLlm::from_json(SCRIPT).unwrap();
    let cancel = Cancel::new();
    let mut stream = llm.stream(request(vec![text(Role::User, "front")]), &cancel).unwrap();
    assert!(matches!(stream.try_next(), Ok(Next::Event(LlmEvent::Start { .. }))));
    cancel.cancel();
    assert_eq!(stream.try_next(), Err(LlmError::Cancelled));
    assert_eq!(stream.try_next(), Ok(Next::End));
    assert_eq!(stream.next_timeout(IDLE), Ok(Next::End));
}

#[test]
fn a_mismatch_is_malformed_and_names_the_turn() {
    let llm = FakeLlm::from_json(SCRIPT).unwrap();
    let cancel = Cancel::new();
    let wrong = llm.stream(request(vec![text(Role::User, "behind")]), &cancel).err();
    assert!(matches!(&wrong, Some(LlmError::Malformed(m)) if m.starts_with("turn 1:")), "{wrong:?}");
    let wrong = llm.stream(request(vec![text(Role::User, "front")]), &cancel).err();
    assert!(matches!(&wrong, Some(LlmError::Malformed(m)) if m.starts_with("turn 2:")), "{wrong:?}");
    let spent = llm.stream(request(vec![text(Role::User, "front")]), &cancel).err();
    assert!(matches!(&spent, Some(LlmError::Malformed(m)) if m.starts_with("turn 3:")), "{spent:?}");
}

#[test]
fn the_same_script_gives_the_same_events_100_times() {
    let events = || {
        let llm = FakeLlm::from_json(SCRIPT).unwrap();
        let mut stream = llm.stream(request(vec![text(Role::User, "front")]), &Cancel::new()).unwrap();
        let mut seen = Vec::new();
        while let Next::Event(e) = stream.next_timeout(IDLE).unwrap() {
            seen.push(e);
        }
        seen
    };
    let first = events();
    assert!((0..100).all(|_| events() == first));
    // chunk: 8 splits the 52-byte argument text into six fragments.
    let fragments = first.iter().filter(|e| matches!(e, LlmEvent::ToolUseInput(_))).count();
    assert_eq!(fragments, 6);
}

#[test]
fn an_injected_error_keeps_the_usage_seen_before_it() {
    let json = r#"{"model": "m", "turns": [{"error": {"before": 3, "error": {"Network": "reset"}}, "retries": 2, "events": [
        {"Start": {"model": "m"}}, {"Usage": {"input": 5, "output": 1, "cache_read": 0, "cache_write": 0}},
        {"TextDelta": "hi"}, {"Stop": "EndTurn"}]}]}"#;
    let llm = FakeLlm::from_json(json).unwrap();
    let mut stream = llm.stream(request(vec![text(Role::User, "x")]), &Cancel::new()).unwrap();
    assert_eq!(stream.collect(IDLE), Err(LlmError::Network("reset".into())));
    assert_eq!(stream.usage_so_far().map(|u| u.tokens()), Some(6));
    assert_eq!(stream.try_next(), Ok(Next::End));
}

#[test]
fn retries_come_first_and_are_counted() {
    let json = r#"{"model": "m", "turns": [{"retries": 2, "events": [
        {"Start": {"model": "m"}}, {"TextDelta": "é"},
        {"Usage": {"input": 1, "output": 1, "cache_read": 0, "cache_write": 0}}, {"Stop": "EndTurn"}]}]}"#;
    let llm = FakeLlm::from_json(json).unwrap();
    let turn = llm.stream(request(vec![text(Role::User, "x")]), &Cancel::new()).unwrap().collect(IDLE).unwrap();
    assert_eq!(turn.retries, 2);
}

#[test]
fn delays_move_the_fake_clock_not_the_wall_clock() {
    let json =
        r#"{"model": "m", "turns": [{"delay_ms": 100, "events": [{"Start": {"model": "m"}}, {"Stop": "EndTurn"}]}]}"#;
    let llm = FakeLlm::from_json(json).unwrap();
    let mut stream = llm.stream(request(vec![text(Role::User, "x")]), &Cancel::new()).unwrap();
    assert!(matches!(stream.try_next(), Ok(Next::Event(LlmEvent::Start { .. }))));
    assert_eq!(stream.try_next(), Ok(Next::Wait));
    llm.clock().advance(omuzaki_core::time::Seconds(0.1));
    assert_eq!(stream.try_next(), Ok(Next::Event(LlmEvent::Stop(StopReason::EndTurn))));
    // collect gives up when an event is later than `idle`.
    let slow =
        r#"{"model": "m", "turns": [{"delay_ms": 500, "events": [{"Start": {"model": "m"}}, {"Stop": "EndTurn"}]}]}"#;
    let llm = FakeLlm::from_json(slow).unwrap();
    let mut stream = llm.stream(request(vec![text(Role::User, "x")]), &Cancel::new()).unwrap();
    assert!(matches!(stream.collect(IDLE), Err(LlmError::Network(_))));
}

#[test]
fn a_bad_call_is_malformed() {
    let json = r#"{"model": "m", "turns": [{"events": [{"Start": {"model": "m"}},
        {"ToolUseStart": {"id": "a", "name": "t"}}, {"ToolUseInput": "{oops"}, {"ToolUseEnd": {"id": "a"}}, {"Stop": "EndTurn"}]}]}"#;
    let llm = FakeLlm::from_json(json).unwrap();
    let got = llm.stream(request(vec![text(Role::User, "x")]), &Cancel::new()).unwrap().collect(IDLE);
    assert!(matches!(got, Err(LlmError::Malformed(m)) if m.contains("invalid JSON")));
}

#[test]
fn the_fake_registers_as_a_provider() {
    omuzaki_llm_mock::link();
    let provider = omuzaki_llm_api::provider::provider("fake").unwrap().unwrap();
    assert_eq!((provider.manifest)().cost, omuzaki_llm_api::provider::CostModel::Free);
    assert_eq!(omuzaki_llm_api::provider::check_providers(), Ok(()));
    let recorded = omuzaki_core::link::registered();
    assert!(recorded.iter().any(|r| r.kind == "llm_provider" && r.name == "\"fake\""), "{recorded:?}");
}

fn load_error(json: &str) -> String {
    match FakeLlm::from_json(json) {
        Err(LlmError::Malformed(why)) => why,
        other => panic!("the script must not load: {:?}", other.map(|_| ())),
    }
}

#[test]
fn a_misspelled_field_in_a_script_fails_to_load_at_every_level() {
    // A turn: `expect` misspelled.
    let why = load_error(r#"{"model":"m","turns":[{"expct":{"user_contains":"x"},"events":[]}]}"#);
    assert!(why.contains("expct"), "{why}");
    // The script itself.
    assert!(load_error(r#"{"model":"m","turn":[]}"#).contains("turn"));
    // An expectation.
    let why = load_error(r#"{"model":"m","turns":[{"expect":{"user_contain":"x"},"events":[]}]}"#);
    assert!(why.contains("user_contain"), "{why}");
    // An injected error.
    let why = load_error(r#"{"model":"m","turns":[{"events":[],"error":{"befor":1,"error":"Auth"}}]}"#);
    assert!(why.contains("befor"), "{why}");
    // An event's fields, and the usage inside one.
    let why = load_error(r#"{"model":"m","turns":[{"events":[{"Start":{"modle":"m"}}]}]}"#);
    assert!(why.contains("modle"), "{why}");
    let why = load_error(
        r#"{"model":"m","turns":[{"events":[{"Usage":{"input":1,"output":1,"cache_read":0,"cache_write":0,"cache_wrte":1}}]}]}"#,
    );
    assert!(why.contains("cache_wrte"), "{why}");
}

#[test]
fn a_correct_script_still_loads() {
    let json = r#"{"model":"m","turns":[{"expect":{"user_contains":"x"},"events":[{"Start":{"model":"m"}}],"chunk":4}]}"#;
    assert!(FakeLlm::from_json(json).is_ok());
}
