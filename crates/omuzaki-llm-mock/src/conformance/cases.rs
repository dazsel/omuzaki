//! The cases of the conformance suite (llm-api D10). Each is a script, a request and what must come out.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{
    ContentBlock, ImageMime, ImageOrigin, Message, Role, SystemBlock, ToolDef, TurnRequest,
};
use omuzaki_llm_api::stream::{Cancel, LlmEvent, Next, StopReason, Turn};
use omuzaki_llm_api::usage::{MicroUsd, PriceTable, PriceTier, Rates, Usage};

use super::Harness;
use crate::{Injected, Script, ScriptTurn};

/// How long a case waits for the next event. A real provider answers over a socket, so this is not tight.
const IDLE: Duration = Duration::from_secs(5);

/// One case of the suite.
type Case = fn(&Harness<'_>) -> Result<(), String>;

/// The cases for `harness`, in the order they run. A case that tests something the provider does not have is left out.
pub(super) fn all(harness: &Harness<'_>) -> Vec<(&'static str, Case)> {
    let mut cases: Vec<(&'static str, Case)> =
        vec![("text only", text_only), ("one tool call", one_tool_call), ("two parallel calls", two_parallel_calls)];
    if harness.opaque_provider.is_some() {
        cases.push(("opaque round trip", opaque_round_trip));
    }
    cases.extend([
        ("cumulative usage", cumulative_usage as Case),
        ("refusal and max output", stop_reasons),
        ("429 with retry-after retries once", rate_limited_with_retry_after),
        ("429 without retry-after is not retried", rate_limited_without_retry_after),
        ("retries stop after four", retries_stop_after_four),
        ("401 is Auth and not retried", auth_is_not_retried),
        ("context error names the window", context_error),
        ("a drop after Start is never retried", drop_after_start),
        ("usage survives a drop", usage_survives_a_drop),
        ("malformed transport data", malformed_transport),
        ("cancel mid-stream", cancel_mid_stream),
        ("cancel during backoff", cancel_during_backoff),
        ("any split gives the same turn", chunking_is_invisible),
        ("the key never appears", key_never_appears),
        ("requests the model cannot take are refused first", refused_first),
    ]);
    if harness.cache_writes {
        cases.push(("the worked example costs 633, 101 and 734", worked_example));
    }
    cases
}

fn user(text: &str) -> Vec<Message> {
    vec![Message::new(Role::User, vec![ContentBlock::Text(text.into())])]
}

fn request() -> TurnRequest {
    TurnRequest::new(user("hi"), 64).with_system(vec![SystemBlock::new("You help.")])
}

fn usage(input: u64, output: u64) -> LlmEvent {
    LlmEvent::Usage(Usage::new(input, output))
}

fn start() -> LlmEvent {
    LlmEvent::start("m")
}

fn stop(reason: StopReason) -> LlmEvent {
    LlmEvent::Stop(reason)
}

fn text(t: &str) -> LlmEvent {
    LlmEvent::TextDelta(t.into())
}

fn call(id: &str, args: &str) -> Vec<LlmEvent> {
    vec![LlmEvent::tool_use_start(id, "t"), LlmEvent::ToolUseInput(args.into()), LlmEvent::tool_use_end(id)]
}

/// A script of one turn.
fn one(turn: ScriptTurn) -> Script {
    Script::new("m").with_turn(turn)
}

/// A turn of a short answer: the base the failure cases start from.
fn answer() -> Vec<LlmEvent> {
    vec![start(), text("ok"), usage(3, 2), stop(StopReason::EndTurn)]
}

// Callers pass fresh values, so taking them by value keeps each call site to one line.
#[allow(clippy::needless_pass_by_value)]
fn same<T: PartialEq + std::fmt::Debug>(got: T, want: T) -> Result<(), String> {
    if got == want { Ok(()) } else { Err(format!("got {got:?}, wanted {want:?}")) }
}

fn play(h: &Harness<'_>, script: Script) -> Result<Turn, String> {
    let client = (h.make)(script);
    let mut stream = client.stream(request(), &Cancel::new()).map_err(|e| e.to_string())?;
    stream.collect(IDLE).map_err(|e| e.to_string())
}

/// Everything a stream gave: its events, the error that ended it, and the usage it saw.
struct Run {
    events: Vec<LlmEvent>,
    error: Option<LlmError>,
    usage: Option<Usage>,
}

impl Run {
    fn retrying(&self) -> Vec<&LlmEvent> {
        self.events.iter().filter(|e| matches!(e, LlmEvent::Retrying { .. })).collect()
    }
}

/// Pulls a turn to its end with `next_timeout`, event by event.
fn drain(h: &Harness<'_>, script: Script) -> Result<Run, String> {
    let client = (h.make)(script);
    let mut stream = client.stream(request(), &Cancel::new()).map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    let error = loop {
        match stream.next_timeout(IDLE) {
            Ok(Next::Event(e)) => events.push(e),
            Ok(Next::End) => break None,
            Ok(_) => return Err(format!("the stream went quiet after {events:?}")),
            Err(e) => break Some(e),
        }
    };
    Ok(Run { events, error, usage: stream.usage_so_far() })
}

fn text_only(h: &Harness<'_>) -> Result<(), String> {
    let turn = play(h, one(ScriptTurn::new(vec![start(), text("hello"), usage(3, 2), stop(StopReason::EndTurn)])))?;
    same(turn.message.content, vec![ContentBlock::Text("hello".into())])?;
    same((turn.stop, turn.usage.tokens(), turn.retries), (StopReason::EndTurn, 5, 0))
}

fn one_tool_call(h: &Harness<'_>) -> Result<(), String> {
    let mut events = vec![start()];
    events.extend(call("a", r#"{"x":1}"#));
    events.extend([usage(1, 1), stop(StopReason::ToolUse)]);
    let turn = play(h, one(ScriptTurn::new(events)))?;
    same(turn.message.content, vec![ContentBlock::tool_use("a", "t", serde_json::json!({"x": 1}))])?;
    same(turn.stop, StopReason::ToolUse)
}

fn two_parallel_calls(h: &Harness<'_>) -> Result<(), String> {
    let mut events = vec![start()];
    events.extend(call("a", "{}"));
    events.extend(call("b", "{}"));
    events.extend([usage(1, 1), stop(StopReason::ToolUse)]);
    let turn = play(h, one(ScriptTurn::new(events)))?;
    let ids: Vec<&str> = turn
        .message
        .content
        .iter()
        .filter_map(|b| if let ContentBlock::ToolUse { id, .. } = b { Some(id.as_str()) } else { None })
        .collect();
    same(ids, vec!["a", "b"])
}

fn opaque_round_trip(h: &Harness<'_>) -> Result<(), String> {
    let provider = h.opaque_provider.ok_or("no opaque provider")?;
    let block = serde_json::json!({"type": "thinking", "thinking": "hmm", "signature": "s"});
    let events =
        vec![start(), LlmEvent::opaque(provider, block.clone()), text("x"), usage(1, 1), stop(StopReason::EndTurn)];
    let turn = play(h, one(ScriptTurn::new(events)))?;
    same(turn.message.content.first().cloned(), Some(ContentBlock::opaque(provider, block)))?;
    same(turn.message.content.get(1).cloned(), Some(ContentBlock::Text("x".into())))
}

fn cumulative_usage(h: &Harness<'_>) -> Result<(), String> {
    let first = Usage::new(10, 1).with_cache_read(3);
    let last = Usage::new(10, 7).with_cache_read(3);
    let events = vec![start(), LlmEvent::Usage(first), text("x"), LlmEvent::Usage(last), stop(StopReason::EndTurn)];
    let turn = play(h, one(ScriptTurn::new(events.clone())))?;
    same(turn.usage, last)?;
    // The caller that pulls events one by one sees the same final figure, and charges it.
    let run = drain(h, one(ScriptTurn::new(events)))?;
    same(run.usage, Some(last))
}

fn stop_reasons(h: &Harness<'_>) -> Result<(), String> {
    for reason in [StopReason::Refused, StopReason::MaxOutput] {
        let turn = play(h, one(ScriptTurn::new(vec![start(), text("p"), usage(2, 2), stop(reason)])))?;
        same(turn.stop, reason)?;
        // A refused or cut-off turn is not an error: its text and its usage, which costs money, are kept.
        same(turn.message.content, vec![ContentBlock::Text("p".into())])?;
        same(turn.usage.tokens(), 4)?;
    }
    Ok(())
}

fn rate_limited_with_retry_after(h: &Harness<'_>) -> Result<(), String> {
    let turn = ScriptTurn::new(answer()).with_retries(1).with_retry_after_ms(1000);
    let run = drain(h, one(turn))?;
    same(run.error.clone(), None)?;
    same(run.retrying().len(), 1)?;
    match run.events.first() {
        Some(LlmEvent::Retrying { attempt: 1, wait, .. }) if *wait == Duration::from_secs(1) => {}
        other => return Err(format!("the first event should be Retrying after 1 s (the retry-after), got {other:?}")),
    }
    same(run.events.get(1), Some(&start()))
}

fn rate_limited_without_retry_after(h: &Harness<'_>) -> Result<(), String> {
    let turn = ScriptTurn::new(answer()).with_error(Injected::new(0, LlmError::rate_limited(None)));
    let run = drain(h, one(turn))?;
    same(run.error, Some(LlmError::rate_limited(None)))?;
    same(run.events.len(), 0)
}

fn retries_stop_after_four(h: &Harness<'_>) -> Result<(), String> {
    let turn = ScriptTurn::new(answer()).with_retries(4).with_error(Injected::new(0, LlmError::Overloaded));
    let run = drain(h, one(turn))?;
    same(run.error, Some(LlmError::Overloaded))?;
    let attempts: Vec<u8> = run
        .events
        .iter()
        .filter_map(|e| if let LlmEvent::Retrying { attempt, .. } = e { Some(*attempt) } else { None })
        .collect();
    same(attempts, vec![1, 2, 3, 4])?;
    same(run.events.len(), 4)
}

fn auth_is_not_retried(h: &Harness<'_>) -> Result<(), String> {
    let turn = ScriptTurn::new(answer()).with_error(Injected::new(0, LlmError::Auth));
    let run = drain(h, one(turn))?;
    same(run.error, Some(LlmError::Auth))?;
    same(run.events.len(), 0)
}

fn context_error(h: &Harness<'_>) -> Result<(), String> {
    let window = (h.make)(Script::new("m")).info().context_window;
    let turn = ScriptTurn::new(answer()).with_error(Injected::new(0, LlmError::context_too_long(window)));
    let run = drain(h, one(turn))?;
    same(run.error, Some(LlmError::context_too_long(window)))?;
    same(run.events.len(), 0)
}

fn drop_after_start(h: &Harness<'_>) -> Result<(), String> {
    let events = vec![start(), text("par"), usage(3, 1), stop(StopReason::EndTurn)];
    let turn = ScriptTurn::new(events).with_error(Injected::new(1, LlmError::Network("reset".into())));
    let run = drain(h, one(turn))?;
    if !run.retrying().is_empty() {
        return Err(format!("a Retrying followed Start: {:?}", run.events));
    }
    if !matches!(run.error, Some(LlmError::Network(_))) {
        return Err(format!("the drop should end the turn with Network, got {:?}", run.error));
    }
    same(run.events.first(), Some(&start()))
}

fn usage_survives_a_drop(h: &Harness<'_>) -> Result<(), String> {
    let turn = ScriptTurn::new(vec![start(), usage(5, 1), text("x"), stop(StopReason::EndTurn)])
        .with_error(Injected::new(2, LlmError::Network("reset".into())));
    let client = (h.make)(one(turn));
    let mut stream = client.stream(request(), &Cancel::new()).map_err(|e| e.to_string())?;
    let got = stream.collect(IDLE);
    if !matches!(got, Err(LlmError::Network(_))) {
        return Err(format!("collect should fail with Network, got {got:?}"));
    }
    // `collect` borrows the stream, so the usage the provider billed is still there to charge.
    same(stream.usage_so_far().map(|u| u.tokens()), Some(6))
}

fn malformed_transport(h: &Harness<'_>) -> Result<(), String> {
    let turn = ScriptTurn::new(answer()).with_error(Injected::new(2, LlmError::Malformed("not events".into())));
    let run = drain(h, one(turn))?;
    if !matches!(run.error, Some(LlmError::Malformed(_))) {
        return Err(format!("bad transport data should be Malformed, got {:?}", run.error));
    }
    same(run.retrying().len(), 0)
}

/// Pulls until the first event arrives, so the turn is under way.
fn first_event(stream: &mut omuzaki_llm_api::stream::TurnStream) -> Result<LlmEvent, String> {
    match stream.next_timeout(IDLE) {
        Ok(Next::Event(e)) => Ok(e),
        other => Err(format!("wanted a first event, got {other:?}")),
    }
}

fn cancel_mid_stream(h: &Harness<'_>) -> Result<(), String> {
    // No `Stop`: a real server leaves the connection open after the last event.
    let events = vec![start(), text("a"), text("b")];
    let cancel = Cancel::new();
    let client = (h.make)(one(ScriptTurn::new(events)));
    let mut stream = client.stream(request(), &cancel).map_err(|e| e.to_string())?;
    first_event(&mut stream)?;
    cancel.cancel();
    same(stream.try_next(), Err(LlmError::Cancelled))?;
    same(stream.try_next(), Ok(Next::End))
}

fn cancel_during_backoff(h: &Harness<'_>) -> Result<(), String> {
    let turn = ScriptTurn::new(answer()).with_retries(1).with_retry_after_ms(30_000);
    let cancel = Cancel::new();
    let client = (h.make)(one(turn));
    let mut stream = client.stream(request(), &cancel).map_err(|e| e.to_string())?;
    let first = first_event(&mut stream)?;
    if !matches!(first, LlmEvent::Retrying { .. }) {
        return Err(format!("the turn should be in a backoff, got {first:?}"));
    }
    let at = Instant::now();
    cancel.cancel();
    same(stream.try_next(), Err(LlmError::Cancelled))?;
    if at.elapsed() > Duration::from_secs(1) {
        return Err(format!("cancelling a 30 s backoff took {:?}", at.elapsed()));
    }
    same(stream.try_next(), Ok(Next::End))
}

fn chunking_is_invisible(h: &Harness<'_>) -> Result<(), String> {
    let mut events = vec![start(), text("héllo wörld")];
    events.extend(call("a", r#"{"k":"vé"}"#));
    events.extend([usage(1, 1), stop(StopReason::ToolUse)]);
    let whole = play(h, one(ScriptTurn::new(events.clone())))?;
    for size in 1..=12 {
        same(play(h, one(ScriptTurn::new(events.clone()).with_chunk(size)))?.message, whole.message.clone())?;
    }
    Ok(())
}

fn key_never_appears(h: &Harness<'_>) -> Result<(), String> {
    if h.secret.is_empty() {
        return Ok(());
    }
    let client = (h.make)(one(ScriptTurn::new(answer())));
    let mut shown = format!("{:?}", client.info());
    for turn in [ScriptTurn::new(answer()), ScriptTurn::new(answer()).with_error(Injected::new(0, LlmError::Auth))] {
        let run = drain(h, one(turn))?;
        let _ = write!(shown, "{:?} {:?} {:?}", run.events, run.error, run.usage);
        if let Some(e) = &run.error {
            let _ = write!(shown, "{e}");
        }
    }
    if shown.contains(h.secret) {
        return Err("the key appears in an event, an error or a Debug output".into());
    }
    Ok(())
}

fn refused_first(h: &Harness<'_>) -> Result<(), String> {
    let client = (h.make)(one(ScriptTurn::new(answer())));
    let info = client.info().clone();
    let cancel = Cancel::new();
    let over = u32::try_from(info.max_output + 1).map_err(|e| e.to_string())?;
    let too_big = TurnRequest::new(user("hi"), over);
    if !matches!(client.stream(too_big, &cancel), Err(LlmError::Unsupported(_))) {
        return Err(format!("max_output {over} is above the model's {} and should be Unsupported", info.max_output));
    }
    let huge = ContentBlock::image(ImageMime::Png, vec![0; 5 * 1024 * 1024 + 1], ImageOrigin::File);
    let with_image = TurnRequest::new(vec![Message::new(Role::User, vec![huge])], 8);
    if !matches!(client.stream(with_image, &cancel), Err(LlmError::Unsupported(_))) {
        return Err("an image over 5 MB should be Unsupported".into());
    }
    // Neither refusal used up the scripted turn.
    let turn =
        client.stream(request(), &cancel).map_err(|e| e.to_string())?.collect(IDLE).map_err(|e| e.to_string())?;
    same(turn.message.content, vec![ContentBlock::Text("ok".into())])
}

/// llm-api §4: the script, the request of each turn, and the prices.
const WORKED: &str = r#"{"model": "claude-haiku-5-5", "turns": [
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

fn haiku_prices() -> PriceTable {
    let rates = Rates::new(100_000, 500_000).with_cache_write(125_000).with_cache_read(10_000);
    PriceTable::new("2026-10-13", "llm-api §4", vec![PriceTier::new(None, rates)])
}

fn worked_example(h: &Harness<'_>) -> Result<(), String> {
    let script: Script = serde_json::from_str(WORKED).map_err(|e| e.to_string())?;
    let client = (h.make)(script);
    let tool = ToolDef::new("render_preview", "Render", serde_json::json!({"type": "object"}));
    let system = vec![SystemBlock::new("You help.").with_cache_after(true)];
    let ask = |messages| TurnRequest::new(messages, 1024).with_system(system.clone()).with_tools(vec![tool.clone()]);
    let cancel = Cancel::new();
    let one = client
        .stream(ask(user("show me the front")), &cancel)
        .map_err(|e| e.to_string())?
        .collect(IDLE)
        .map_err(|e| e.to_string())?;
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
    png.extend_from_slice(&512u32.to_be_bytes());
    png.extend_from_slice(&512u32.to_be_bytes());
    let result = ContentBlock::tool_result(
        "tu_1",
        vec![
            ContentBlock::Text("Rendered main, front, 512 px.".into()),
            ContentBlock::image(ImageMime::Png, png, ImageOrigin::Render),
        ],
    );
    let history =
        vec![user("show me the front").remove(0), one.message.clone(), Message::new(Role::User, vec![result])];
    let two =
        client.stream(ask(history), &cancel).map_err(|e| e.to_string())?.collect(IDLE).map_err(|e| e.to_string())?;
    let prices = haiku_prices();
    same((one.usage.prompt(), one.usage.tokens()), (4837, 4895))?;
    same((two.usage.prompt(), two.usage.tokens()), (5213, 5236))?;
    same((one.usage.cost(&prices), two.usage.cost(&prices)), (MicroUsd(633), MicroUsd(101)))?;
    let total = Usage::new(37 + 413, 58 + 23).with_cache_read(4800).with_cache_write(4800);
    same(total.tokens(), 10_131)?;
    same(MicroUsd(one.usage.cost(&prices).0 + two.usage.cost(&prices).0), MicroUsd(734))?;
    same(two.message.content, vec![ContentBlock::Text("Here she is, from the front.".into())])
}
