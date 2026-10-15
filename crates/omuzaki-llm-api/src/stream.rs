//! Events, the pull-based [`TurnStream`] and [`Cancel`] (llm-api D1, D3, D5).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::LlmError;
use crate::message::{ContentBlock, Message, Role};
use crate::usage::Usage;

/// A cancel handle. Clones share one flag, so a caller keeps a clone and the stream holds another.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A handle that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels every clone. It cannot be undone.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// True once any clone has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Why a turn stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum StopReason {
    /// The model finished.
    EndTurn,
    /// The model wants tool results.
    ToolUse,
    /// The turn hit `max_output`.
    MaxOutput,
    /// The model refused. The turn may carry partial text, and its usage costs money.
    Refused,
    /// The provider paused the turn (`pause_turn`); send it back to continue.
    Paused,
}

/// One event of a turn. Order: `Retrying`*, `Start`, content events, `Usage`, `Stop`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum LlmEvent {
    /// The client is retrying; only before `Start`.
    Retrying {
        /// Which retry this is, from 1.
        attempt: u8,
        /// How long it waits first.
        wait: Duration,
        /// Why, without any key or header.
        cause: String,
    },
    /// The turn started; the id is the model that serves it.
    Start {
        /// The served model id.
        model: String,
    },
    /// More text. Consecutive deltas form one `Text` block.
    TextDelta(String),
    /// A tool call begins.
    ToolUseStart {
        /// The call id.
        id: String,
        /// The tool name.
        name: String,
    },
    /// A raw JSON fragment of the open call's arguments.
    ToolUseInput(String),
    /// The call's arguments are complete.
    ToolUseEnd {
        /// The call id.
        id: String,
    },
    /// One complete provider block.
    Opaque {
        /// The provider that made it.
        provider: String,
        /// The block.
        block: Value,
    },
    /// Cumulative usage; the last one is final.
    Usage(Usage),
    /// The turn is over.
    Stop(StopReason),
}

/// What [`TurnStream::try_next`] found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum Next {
    /// An event.
    Event(LlmEvent),
    /// Nothing yet.
    Wait,
    /// The stream is over.
    End,
}

/// A finished turn, folded from its events.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Turn {
    /// The model that served it.
    pub model: String,
    /// The assistant message to append to the history.
    pub message: Message,
    /// The final usage.
    pub usage: Usage,
    /// Why it stopped.
    pub stop: StopReason,
    /// How many `Retrying` events came first.
    pub retries: u8,
}

/// Where a [`TurnStream`] gets events: a provider's turn task, or a script. Implementations never see `Cancel`
/// and never repeat an event after `Stop` or an error.
pub trait EventSource: Send {
    /// The next event, waiting at most `wait` (zero never blocks). Errors end the source.
    ///
    /// # Errors
    /// The turn's failure; the stream passes it on once and then reports `End`.
    fn poll(&mut self, wait: Duration) -> Result<Next, LlmError>;
}

/// The events of one turn, pulled by the caller. Dropping it drops the source, which cancels the turn.
pub struct TurnStream {
    source: Box<dyn EventSource>,
    cancel: Cancel,
    usage: Option<Usage>,
    done: bool,
}

impl TurnStream {
    /// A stream over `source` that obeys `cancel` on every call.
    pub fn new(source: Box<dyn EventSource>, cancel: &Cancel) -> Self {
        Self { source, cancel: cancel.clone(), usage: None, done: false }
    }

    /// The next event if one is ready; never blocks.
    ///
    /// # Errors
    /// The turn's error, once; `Cancelled` on the first call after `cancel()`. Every later call returns `End`.
    pub fn try_next(&mut self) -> Result<Next, LlmError> {
        self.next_timeout(Duration::ZERO)
    }

    /// The next event, blocking up to `wait`.
    ///
    /// # Errors
    /// As [`try_next`](Self::try_next).
    pub fn next_timeout(&mut self, wait: Duration) -> Result<Next, LlmError> {
        if self.done {
            return Ok(Next::End);
        }
        if self.cancel.is_cancelled() {
            self.done = true;
            return Err(LlmError::Cancelled);
        }
        match self.source.poll(wait) {
            Ok(Next::Event(event)) => {
                match &event {
                    // Reports are cumulative, so a smaller one is a glitch; keep the largest of each counter so what is
                    // charged never drops.
                    LlmEvent::Usage(u) => self.usage = Some(self.usage.map_or(*u, |old| keep_largest(old, *u))),
                    LlmEvent::Stop(_) => self.done = true,
                    _ => {}
                }
                // A source that sends `Stop` ends the stream after it; the caller sees `End` next.
                Ok(Next::Event(event))
            }
            Ok(Next::End) => {
                self.done = true;
                Ok(Next::End)
            }
            Ok(Next::Wait) => Ok(Next::Wait),
            Err(e) => {
                self.done = true;
                Err(e)
            }
        }
    }

    /// The usage seen so far: the largest report of each counter. A provider bills it even when the turn then fails, so the caller charges it.
    pub fn usage_so_far(&self) -> Option<Usage> {
        self.usage
    }

    /// Reads to the end and folds the events into a [`Turn`]. It borrows the stream, so
    /// [`usage_so_far`](Self::usage_so_far) survives an error.
    ///
    /// # Errors
    /// The turn's error; `Network` if nothing arrives within `idle`; `Malformed` if the events break the order of
    /// llm-api D3 (no `Stop`, no `Usage`, an unclosed or nested call, invalid JSON arguments).
    pub fn collect(&mut self, idle: Duration) -> Result<Turn, LlmError> {
        let mut fold = Fold::default();
        let result = loop {
            match self.next_timeout(idle) {
                Ok(Next::Event(event)) => {
                    if let Err(e) = fold.push(event) {
                        break Err(e);
                    }
                }
                Ok(Next::Wait) => break Err(LlmError::Network(format!("no event within {} ms", idle.as_millis()))),
                Ok(Next::End) => break fold.finish(self.usage),
                Err(e) => break Err(e),
            }
        };
        // Whatever collect fails on ends the stream, as an error from the source does.
        if result.is_err() {
            self.done = true;
        }
        result
    }
}

/// The component-wise maximum of two cumulative usage reports.
fn keep_largest(a: Usage, b: Usage) -> Usage {
    Usage {
        input: a.input.max(b.input),
        output: a.output.max(b.output),
        cache_read: a.cache_read.max(b.cache_read),
        cache_write: a.cache_write.max(b.cache_write),
    }
}

/// An open tool call: id, name and the raw argument text so far.
struct OpenCall {
    id: String,
    name: String,
    json: String,
}

#[derive(Default)]
struct Fold {
    model: String,
    blocks: Vec<ContentBlock>,
    text: String,
    call: Option<OpenCall>,
    stop: Option<StopReason>,
    retries: u8,
}

impl Fold {
    fn flush_text(&mut self) {
        if !self.text.is_empty() {
            self.blocks.push(ContentBlock::Text(std::mem::take(&mut self.text)));
        }
    }

    fn push(&mut self, event: LlmEvent) -> Result<(), LlmError> {
        match event {
            LlmEvent::Retrying { .. } => self.retries = self.retries.saturating_add(1),
            LlmEvent::Start { model } => self.model = model,
            LlmEvent::TextDelta(t) => self.text.push_str(&t),
            LlmEvent::ToolUseStart { id, name } => {
                if let Some(open) = &self.call {
                    return Err(LlmError::Malformed(format!("tool call {id} starts inside open call {}", open.id)));
                }
                self.flush_text();
                self.call = Some(OpenCall { id, name, json: String::new() });
            }
            LlmEvent::ToolUseInput(fragment) => match &mut self.call {
                Some(call) => call.json.push_str(&fragment),
                None => return Err(LlmError::Malformed("tool input outside a tool call".into())),
            },
            LlmEvent::ToolUseEnd { id } => {
                let call = self.call.take().filter(|c| c.id == id);
                let Some(call) = call else {
                    return Err(LlmError::Malformed(format!("tool call end for unopened id {id}")));
                };
                // A call with no arguments sends no fragments; its input is the empty object.
                let source = if call.json.trim().is_empty() { "{}" } else { call.json.as_str() };
                let input = serde_json::from_str(source)
                    .map_err(|e| LlmError::Malformed(format!("tool call {id} has invalid JSON arguments: {e}")))?;
                self.blocks.push(ContentBlock::ToolUse { id: call.id, name: call.name, input });
            }
            LlmEvent::Opaque { provider, block } => {
                self.flush_text();
                self.blocks.push(ContentBlock::Opaque { provider, block });
            }
            LlmEvent::Usage(_) => {}
            LlmEvent::Stop(reason) => self.stop = Some(reason),
        }
        Ok(())
    }

    fn finish(mut self, usage: Option<Usage>) -> Result<Turn, LlmError> {
        self.flush_text();
        if self.call.is_some() {
            return Err(LlmError::Malformed("the stream ended inside a tool call".into()));
        }
        let stop = self.stop.ok_or_else(|| LlmError::Malformed("the stream ended without a stop".into()))?;
        let usage = usage.ok_or_else(|| LlmError::Malformed("the turn stopped without reporting usage".into()))?;
        let message = Message { role: Role::Assistant, content: self.blocks };
        Ok(Turn { model: self.model, message, usage, stop, retries: self.retries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source that plays a fixed list, then ends.
    struct List(std::collections::VecDeque<Result<Next, LlmError>>);

    fn stream(items: Vec<Result<Next, LlmError>>, cancel: &Cancel) -> TurnStream {
        TurnStream::new(Box::new(List(items.into())), cancel)
    }

    impl EventSource for List {
        fn poll(&mut self, _wait: Duration) -> Result<Next, LlmError> {
            self.0.pop_front().unwrap_or(Ok(Next::End))
        }
    }

    #[allow(clippy::unnecessary_wraps)] // the list holds `Result`s
    fn event(e: LlmEvent) -> Result<Next, LlmError> {
        Ok(Next::Event(e))
    }

    fn empty_object() -> Value {
        Value::Object(serde_json::Map::new())
    }

    #[test]
    fn a_smaller_usage_report_never_lowers_what_is_charged() {
        let big = Usage { input: 10, output: 5, cache_read: 3, cache_write: 2 };
        let small = Usage { input: 4, output: 9, cache_read: 1, cache_write: 0 };
        let items = vec![
            event(LlmEvent::Usage(big)),
            event(LlmEvent::Usage(small)),
            event(LlmEvent::Stop(StopReason::EndTurn)),
        ];
        let mut s = stream(items, &Cancel::new());
        let want = Usage { input: 10, output: 9, cache_read: 3, cache_write: 2 };
        assert!(matches!(s.try_next(), Ok(Next::Event(_))));
        assert!(matches!(s.try_next(), Ok(Next::Event(_))));
        assert_eq!(s.usage_so_far(), Some(want));
        let turn = s.collect(Duration::ZERO).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(turn.usage, want);
    }

    #[test]
    fn nothing_follows_stop() {
        let items = vec![event(LlmEvent::Stop(StopReason::EndTurn)), event(LlmEvent::TextDelta("late".into()))];
        let mut s = stream(items, &Cancel::new());
        assert_eq!(s.try_next(), event(LlmEvent::Stop(StopReason::EndTurn)));
        assert_eq!(s.try_next(), Ok(Next::End));
    }

    fn usage() -> LlmEvent {
        LlmEvent::Usage(Usage { input: 1, output: 1, cache_read: 0, cache_write: 0 })
    }

    #[test]
    fn a_stop_without_usage_is_malformed() {
        let mut s = stream(vec![event(LlmEvent::Stop(StopReason::EndTurn))], &Cancel::new());
        assert!(matches!(s.collect(Duration::ZERO), Err(LlmError::Malformed(_))));
    }

    #[test]
    fn a_failed_collect_ends_the_stream() {
        let items =
            vec![event(LlmEvent::ToolUseInput("{".into())), event(usage()), event(LlmEvent::Stop(StopReason::EndTurn))];
        let mut s = stream(items, &Cancel::new());
        assert!(matches!(s.collect(Duration::ZERO), Err(LlmError::Malformed(_))));
        assert_eq!(s.try_next(), Ok(Next::End));
        let mut idle = stream(vec![Ok(Next::Wait)], &Cancel::new());
        assert!(matches!(idle.collect(Duration::ZERO), Err(LlmError::Network(_))));
        assert_eq!(idle.try_next(), Ok(Next::End));
    }

    #[test]
    fn a_second_call_inside_an_open_call_is_malformed() {
        let start = |id: &str| event(LlmEvent::ToolUseStart { id: id.into(), name: "n".into() });
        let items = vec![start("a"), start("b"), event(usage()), event(LlmEvent::Stop(StopReason::ToolUse))];
        assert!(matches!(stream(items, &Cancel::new()).collect(Duration::ZERO), Err(LlmError::Malformed(_))));
    }

    #[test]
    fn a_stream_without_a_stop_is_malformed() {
        let mut s = stream(vec![event(LlmEvent::TextDelta("a".into()))], &Cancel::new());
        assert!(matches!(s.collect(Duration::ZERO), Err(LlmError::Malformed(_))));
    }

    #[test]
    fn text_around_a_call_stays_in_order_and_empty_arguments_are_an_empty_object() {
        let items = vec![
            event(LlmEvent::TextDelta("a".into())),
            event(LlmEvent::TextDelta("b".into())),
            event(LlmEvent::ToolUseStart { id: "t".into(), name: "n".into() }),
            event(LlmEvent::ToolUseEnd { id: "t".into() }),
            event(LlmEvent::TextDelta("c".into())),
            event(usage()),
            event(LlmEvent::Stop(StopReason::ToolUse)),
        ];
        let turn = stream(items, &Cancel::new()).collect(Duration::ZERO).unwrap_or_else(|e| panic!("{e}"));
        let call = ContentBlock::ToolUse { id: "t".into(), name: "n".into(), input: empty_object() };
        let want = vec![ContentBlock::Text("ab".into()), call, ContentBlock::Text("c".into())];
        assert_eq!(turn.message.content, want);
    }

    #[test]
    fn an_error_ends_the_stream_once() {
        let mut s = stream(vec![Err(LlmError::Overloaded), event(LlmEvent::TextDelta("x".into()))], &Cancel::new());
        assert_eq!(s.try_next(), Err(LlmError::Overloaded));
        assert_eq!(s.try_next(), Ok(Next::End));
    }
}
