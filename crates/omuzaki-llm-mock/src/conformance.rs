//! The provider conformance suite (llm-api D10): the cases every `LlmClient` must pass, written as scripts. `FakeLlm`
//! passes it directly (see the tests); a real provider passes it by building its client against a loopback server
//! that plays the same script, and handing [`run`] that builder.

use std::time::Duration;

use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{ContentBlock, Message, Role, SystemBlock, TurnRequest};
use omuzaki_llm_api::stream::{Cancel, LlmEvent, Next, StopReason, Turn};
use omuzaki_llm_api::usage::Usage;

use crate::{Injected, Script, ScriptTurn};

/// Builds a client that plays `script`: `FakeLlm::new` for the fake, a loopback server for a real provider.
pub type Make<'a> = &'a dyn Fn(Script) -> Box<dyn LlmClient>;

/// One case of the suite.
type Case = fn(Make<'_>) -> Result<(), String>;

const IDLE: Duration = Duration::from_millis(50);

/// Runs every case against clients from `make`.
///
/// # Errors
/// The first failing case, as `"<case>: <what went wrong>"`.
pub fn run(make: Make<'_>) -> Result<(), String> {
    let cases: [(&str, Case); 8] = [
        ("text only", text_only),
        ("one tool call", one_tool_call),
        ("two parallel calls", two_parallel_calls),
        ("opaque round trip", opaque_round_trip),
        ("refusal and max output", stop_reasons),
        ("usage survives a drop", usage_survives_a_drop),
        ("cancel mid-stream", cancel_mid_stream),
        ("any split gives the same turn", chunking_is_invisible),
    ];
    for (name, case) in cases {
        case(make).map_err(|why| format!("{name}: {why}"))?;
    }
    Ok(())
}

fn request() -> TurnRequest {
    let messages = vec![Message::new(Role::User, vec![ContentBlock::Text("hi".into())])];
    TurnRequest::new(messages, 64).with_system(vec![SystemBlock::new("You help.")])
}

fn usage(input: u64, output: u64) -> LlmEvent {
    LlmEvent::Usage(Usage::new(input, output))
}

fn start() -> LlmEvent {
    LlmEvent::start("m")
}

fn call(id: &str, args: &str) -> Vec<LlmEvent> {
    vec![LlmEvent::tool_use_start(id, "t"), LlmEvent::ToolUseInput(args.into()), LlmEvent::tool_use_end(id)]
}

fn script(chunk: Option<usize>, events: Vec<LlmEvent>) -> Script {
    let turn = ScriptTurn::new(events);
    Script::new("m").with_turn(match chunk {
        Some(bytes) => turn.with_chunk(bytes),
        None => turn,
    })
}

fn play(make: Make<'_>, s: Script) -> Result<Turn, String> {
    let client = make(s);
    let mut stream = client.stream(request(), &Cancel::new()).map_err(|e| e.to_string())?;
    stream.collect(IDLE).map_err(|e| e.to_string())
}

// Callers pass fresh values, so taking them by value keeps each call site to one line.
#[allow(clippy::needless_pass_by_value)]
fn same<T: PartialEq + std::fmt::Debug>(got: T, want: T) -> Result<(), String> {
    if got == want { Ok(()) } else { Err(format!("got {got:?}, wanted {want:?}")) }
}

fn text_only(make: Make<'_>) -> Result<(), String> {
    let turn = play(
        make,
        script(None, vec![start(), LlmEvent::TextDelta("hello".into()), usage(3, 2), stop(StopReason::EndTurn)]),
    )?;
    same(turn.message.content, vec![ContentBlock::Text("hello".into())])?;
    same((turn.stop, turn.usage.tokens()), (StopReason::EndTurn, 5))
}

fn stop(reason: StopReason) -> LlmEvent {
    LlmEvent::Stop(reason)
}

fn one_tool_call(make: Make<'_>) -> Result<(), String> {
    let mut events = vec![start()];
    events.extend(call("a", r#"{"x":1}"#));
    events.extend([usage(1, 1), stop(StopReason::ToolUse)]);
    let turn = play(make, script(None, events))?;
    let want = ContentBlock::tool_use("a", "t", serde_json::json!({"x": 1}));
    same(turn.message.content, vec![want])?;
    same(turn.stop, StopReason::ToolUse)
}

fn two_parallel_calls(make: Make<'_>) -> Result<(), String> {
    let mut events = vec![start()];
    events.extend(call("a", "{}"));
    events.extend(call("b", "{}"));
    events.extend([usage(1, 1), stop(StopReason::ToolUse)]);
    let turn = play(make, script(None, events))?;
    let ids: Vec<&str> = turn
        .message
        .content
        .iter()
        .filter_map(|b| if let ContentBlock::ToolUse { id, .. } = b { Some(id.as_str()) } else { None })
        .collect();
    same(ids, vec!["a", "b"])
}

fn opaque_round_trip(make: Make<'_>) -> Result<(), String> {
    let block = serde_json::json!({"type": "thinking", "signature": "s"});
    let events = vec![
        start(),
        LlmEvent::opaque("p", block.clone()),
        LlmEvent::TextDelta("x".into()),
        usage(1, 1),
        stop(StopReason::EndTurn),
    ];
    let turn = play(make, script(None, events))?;
    same(turn.message.content.first().cloned(), Some(ContentBlock::opaque("p", block)))
}

fn stop_reasons(make: Make<'_>) -> Result<(), String> {
    for reason in [StopReason::Refused, StopReason::MaxOutput] {
        let turn = play(make, script(None, vec![start(), LlmEvent::TextDelta("p".into()), usage(2, 2), stop(reason)]))?;
        same(turn.stop, reason)?;
        same(turn.usage.tokens(), 4)?;
    }
    Ok(())
}

fn usage_survives_a_drop(make: Make<'_>) -> Result<(), String> {
    let turn = ScriptTurn::new(vec![start(), usage(5, 1), LlmEvent::TextDelta("x".into()), stop(StopReason::EndTurn)])
        .with_error(Injected::new(2, LlmError::Network("reset".into())));
    let client = make(Script::new("m").with_turn(turn));
    let mut stream = client.stream(request(), &Cancel::new()).map_err(|e| e.to_string())?;
    same(stream.collect(IDLE), Err(LlmError::Network("reset".into())))?;
    same(stream.usage_so_far().map(|u| u.tokens()), Some(6))
}

fn cancel_mid_stream(make: Make<'_>) -> Result<(), String> {
    let events = vec![start(), LlmEvent::TextDelta("a".into()), LlmEvent::TextDelta("b".into())];
    let cancel = Cancel::new();
    let client = make(script(None, events));
    let mut stream = client.stream(request(), &cancel).map_err(|e| e.to_string())?;
    stream.try_next().map_err(|e| e.to_string())?;
    cancel.cancel();
    same(stream.try_next(), Err(LlmError::Cancelled))?;
    same(stream.try_next(), Ok(Next::End))
}

fn chunking_is_invisible(make: Make<'_>) -> Result<(), String> {
    let mut events = vec![start(), LlmEvent::TextDelta("héllo wörld".into())];
    events.extend(call("a", r#"{"k":"vé"}"#));
    events.extend([usage(1, 1), stop(StopReason::ToolUse)]);
    let whole = play(make, script(None, events.clone()))?;
    for size in 1..=12 {
        same(play(make, script(Some(size), events.clone()))?.message, whole.message.clone())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeLlm;

    #[test]
    fn fake_llm_passes_the_suite() {
        let make = |script: Script| -> Box<dyn LlmClient> { Box::new(FakeLlm::new(script)) };
        assert_eq!(run(&make), Ok(()));
    }
}
