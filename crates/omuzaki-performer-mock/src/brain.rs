//! [`ScriptedBrain`]: a [`Brain`] that plays scripted turns of [`BrainEvent`]s on a fake clock, for tests of the
//! turn machine without a tag parser or an LLM.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::stream::Cancel;
use omuzaki_performer_api::brain::{Brain, BrainError, BrainEvent, BrainSource, Polled, TurnInput, TurnStream};

use crate::voice::lock;

/// One scripted turn: each event with its delay after the turn started.
pub type ScriptedTurn = Vec<(Seconds, BrainEvent)>;

/// A [`Brain`] that plays one scripted turn per call, in order, releasing each event once the clock passes the turn's
/// start plus its delay. It records every input. A call with no turn left fails as the LLM's `Malformed`.
pub struct ScriptedBrain {
    clock: Arc<FakeClock>,
    turns: VecDeque<ScriptedTurn>,
    inputs: Arc<Mutex<Vec<TurnInput>>>,
}

impl ScriptedBrain {
    /// A brain that plays `turns` on `clock`.
    pub fn new(clock: Arc<FakeClock>, turns: Vec<ScriptedTurn>) -> Self {
        Self { clock, turns: turns.into(), inputs: Arc::default() }
    }

    /// Every input received so far, in order.
    pub fn inputs(&self) -> Vec<TurnInput> {
        lock(&self.inputs).clone()
    }
}

impl Brain for ScriptedBrain {
    fn turn(&mut self, input: TurnInput, cancel: &Cancel) -> Result<TurnStream, BrainError> {
        if cancel.is_cancelled() {
            return Err(BrainError::Cancelled);
        }
        let number = input.turn;
        lock(&self.inputs).push(input);
        let events = self
            .turns
            .pop_front()
            .ok_or_else(|| LlmError::Malformed(format!("turn {number}: the script has no more turns")))?;
        let start = self.clock.now();
        let source = Playback { clock: Arc::clone(&self.clock), start, events: events.into() };
        Ok(TurnStream::new(Box::new(source), cancel))
    }
}

struct Playback {
    clock: Arc<FakeClock>,
    start: Seconds,
    events: VecDeque<(Seconds, BrainEvent)>,
}

impl BrainSource for Playback {
    fn poll(&mut self) -> Result<Polled, BrainError> {
        let due = self.start.0 + self.events.front().map_or(0.0, |(delay, _)| delay.0);
        match self.events.front() {
            None => Ok(Polled::End),
            Some(_) if self.clock.now().0 < due => Ok(Polled::Pending),
            Some(_) => Ok(self.events.pop_front().map_or(Polled::End, |(_, e)| Polled::Event(e))),
        }
    }
}
