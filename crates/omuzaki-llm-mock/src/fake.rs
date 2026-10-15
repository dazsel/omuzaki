//! [`FakeLlm`]: the scripted client.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{ContentBlock, Message, Role, TurnRequest};
use omuzaki_llm_api::stream::{Cancel, EventSource, LlmEvent, Next, TurnStream};
use omuzaki_llm_api::usage::{ImageCost, ModelInfo};

use crate::script::{Expect, Script, ScriptTurn};

/// A deterministic [`LlmClient`] that plays a [`Script`]: the same script always gives the same events. It opens no
/// socket and starts no thread; delays move a [`FakeClock`] instead of sleeping.
pub struct FakeLlm {
    script: Script,
    info: ModelInfo,
    clock: Arc<FakeClock>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    next: usize,
    requests: Vec<TurnRequest>,
}

impl FakeLlm {
    /// A fake that plays `script`.
    pub fn new(script: Script) -> Self {
        let info = ModelInfo::new(script.model.clone(), 200_000, 64_000, ImageCost::tiles(28))
            .with_vision(true)
            .with_tools(true)
            .with_effort(true)
            .with_caching(true);
        Self { script, info, clock: Arc::new(FakeClock::default()), state: Mutex::new(State::default()) }
    }

    /// A fake from a script in JSON.
    ///
    /// # Errors
    /// `Malformed` if the text is not a script.
    pub fn from_json(json: &str) -> Result<Self, LlmError> {
        let script = serde_json::from_str(json).map_err(|e| LlmError::Malformed(format!("script: {e}")))?;
        Ok(Self::new(script))
    }

    /// The clock that scripted delays move; advance it to release events to `try_next`.
    pub fn clock(&self) -> Arc<FakeClock> {
        Arc::clone(&self.clock)
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<TurnRequest> {
        self.lock().requests.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A poisoned lock only means a test thread panicked; the data is still a plain log.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl LlmClient for FakeLlm {
    fn info(&self) -> &ModelInfo {
        &self.info
    }

    fn stream(&self, req: TurnRequest, cancel: &Cancel) -> Result<TurnStream, LlmError> {
        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        let mut state = self.lock();
        let number = state.next + 1;
        state.next = number;
        let turn = self
            .script
            .turns
            .get(number - 1)
            .ok_or_else(|| LlmError::Malformed(format!("turn {number}: the script has no more turns")))?;
        if let Some(expect) = &turn.expect {
            check(expect, &req).map_err(|why| LlmError::Malformed(format!("turn {number}: {why}")))?;
        }
        state.requests.push(req);
        let source = Playback::new(turn, Arc::clone(&self.clock));
        Ok(TurnStream::new(Box::new(source), cancel))
    }
}

fn texts(blocks: &[ContentBlock], out: &mut String) {
    for block in blocks {
        match block {
            ContentBlock::Text(t) => out.push_str(t),
            ContentBlock::ToolResult { blocks, .. } => texts(blocks, out),
            _ => {}
        }
    }
}

fn check(expect: &Expect, req: &TurnRequest) -> Result<(), String> {
    let last: Option<&Message> = req.messages.last().filter(|m| m.role == Role::User);
    let last = last.ok_or("the request does not end with a user message")?;
    if let Some(needle) = &expect.user_contains {
        let mut text = String::new();
        texts(&last.content, &mut text);
        if !text.contains(needle.as_str()) {
            return Err(format!("the last user message does not contain {needle:?}"));
        }
    }
    if let Some(wanted) = &expect.answers {
        let ids: Vec<&str> = last
            .content
            .iter()
            .filter_map(|b| if let ContentBlock::ToolResult { id, .. } = b { Some(id.as_str()) } else { None })
            .collect();
        if ids != *wanted {
            return Err(format!("the last user message answers {ids:?}, not {wanted:?}"));
        }
    }
    Ok(())
}

enum Item {
    Event(LlmEvent),
    Fail(LlmError),
}

/// Plays one turn's items, releasing each after the turn's delay on the fake clock.
struct Playback {
    items: VecDeque<Item>,
    clock: Arc<FakeClock>,
    delay: f64,
    ready_at: f64,
}

/// Splits `text` into pieces of at least `size` bytes, ending on character boundaries.
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

fn expand(turn: &ScriptTurn) -> Vec<LlmEvent> {
    let Some(size) = turn.chunk else { return turn.events.clone() };
    let mut out = Vec::new();
    for event in &turn.events {
        match event {
            LlmEvent::TextDelta(t) => out.extend(pieces(t, size).into_iter().map(LlmEvent::TextDelta)),
            LlmEvent::ToolUseInput(t) => out.extend(pieces(t, size).into_iter().map(LlmEvent::ToolUseInput)),
            other => out.push(other.clone()),
        }
    }
    out
}

impl Playback {
    fn new(turn: &ScriptTurn, clock: Arc<FakeClock>) -> Self {
        let mut items: Vec<Item> = expand(turn).into_iter().map(Item::Event).collect();
        if let Some(injected) = &turn.error {
            items.insert(injected.before.min(items.len()), Item::Fail(injected.error.clone()));
        }
        let retries = (1..=turn.retries)
            .map(|attempt| Item::Event(LlmEvent::retrying(attempt, Duration::ZERO, "scripted retry")));
        let items: VecDeque<Item> = retries.chain(items).collect();
        let delay = Duration::from_millis(turn.delay_ms).as_secs_f64();
        let ready_at = clock.now().0;
        Self { items, clock, delay, ready_at }
    }
}

impl EventSource for Playback {
    fn poll(&mut self, wait: Duration) -> Result<Next, LlmError> {
        if self.items.is_empty() {
            return Ok(Next::End);
        }
        let remaining = self.ready_at - self.clock.now().0;
        if remaining > 0.0 {
            let wait = wait.as_secs_f64();
            self.clock.advance(Seconds(remaining.min(wait)));
            if wait < remaining {
                return Ok(Next::Wait);
            }
        }
        self.ready_at = self.clock.now().0 + self.delay;
        match self.items.pop_front() {
            Some(Item::Event(event)) => Ok(Next::Event(event)),
            Some(Item::Fail(error)) => {
                self.items.clear();
                Err(error)
            }
            None => Ok(Next::End),
        }
    }
}
