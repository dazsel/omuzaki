//! [`LlmBrain`]: the [`Brain`] over any [`LlmClient`] (`ai-performer.md` §2, §6): renders the prompt, streams the
//! reply and parses its tags as text arrives, so a gesture keeps its word.

use std::sync::Arc;

use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::stream::{self, Cancel, LlmEvent, Next, StopReason};
use omuzaki_llm_api::usage::Usage;
use omuzaki_performer_api::brain::{Brain, BrainError, BrainEvent, BrainSource, Polled, TurnInput, TurnStream};

use crate::parser::TagParser;
use crate::persona::Persona;
use crate::prompt;

/// The brain: a persona and a client.
pub struct LlmBrain {
    client: Arc<dyn LlmClient>,
    persona: Persona,
}

impl LlmBrain {
    /// A brain for `persona` over `client`.
    pub fn new(client: Arc<dyn LlmClient>, persona: Persona) -> Self {
        Self { client, persona }
    }

    /// The persona it speaks as.
    pub fn persona(&self) -> &Persona {
        &self.persona
    }

    /// Replaces the persona from the next turn.
    pub fn set_persona(&mut self, persona: Persona) {
        self.persona = persona;
    }
}

impl std::fmt::Debug for LlmBrain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmBrain").field("persona", &self.persona.name).finish_non_exhaustive()
    }
}

impl Brain for LlmBrain {
    fn turn(&mut self, input: TurnInput, cancel: &Cancel) -> Result<TurnStream, BrainError> {
        if cancel.is_cancelled() {
            return Err(BrainError::Cancelled);
        }
        let request = prompt::render(&self.persona, &input);
        let stream = self.client.stream(request, cancel)?;
        let source = Parsing {
            stream,
            parser: TagParser::new(),
            queue: Vec::new(),
            usage: Usage::default(),
            stop: None,
            done: false,
        };
        Ok(TurnStream::new(Box::new(source), cancel))
    }
}

/// The LLM stream, parsed.
struct Parsing {
    stream: stream::TurnStream,
    parser: TagParser,
    /// Events parsed and not yet handed out, in reverse order (popped from the end).
    queue: Vec<BrainEvent>,
    usage: Usage,
    stop: Option<StopReason>,
    done: bool,
}

impl Parsing {
    fn enqueue(&mut self, events: Vec<BrainEvent>) {
        // `queue` is popped from the end, so keep it reversed.
        let mut events = events;
        events.reverse();
        events.extend(std::mem::take(&mut self.queue));
        self.queue = events;
    }
}

impl BrainSource for Parsing {
    fn poll(&mut self) -> Result<Polled, BrainError> {
        loop {
            if let Some(event) = self.queue.pop() {
                return Ok(Polled::Event(event));
            }
            if self.done {
                return Ok(Polled::End);
            }
            match self.stream.try_next()? {
                Next::Event(LlmEvent::TextDelta(delta)) => {
                    let events = self.parser.push(&delta);
                    self.enqueue(events);
                }
                Next::Event(LlmEvent::Usage(usage)) => self.usage = usage,
                Next::Event(LlmEvent::Stop(stop)) => self.stop = Some(stop),
                Next::Event(_) => {}
                Next::End => {
                    let mut events = self.parser.finish();
                    events.push(BrainEvent::done(self.usage, self.stop.unwrap_or(StopReason::EndTurn)));
                    self.enqueue(events);
                    self.done = true;
                }
                // `Wait`, and any kind this build doesn't know yet: nothing yet.
                _ => return Ok(Polled::Pending),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_llm_mock::{FakeLlm, Script, ScriptTurn};
    use omuzaki_performer_api::stage::Inventory;

    use super::*;

    fn drain(stream: &mut TurnStream) -> Vec<BrainEvent> {
        let mut out = Vec::new();
        loop {
            match stream.try_next() {
                Ok(Polled::Event(e)) => out.push(e),
                Ok(Polled::End) => break,
                Ok(_) => {}
                Err(e) => panic!("{e}"),
            }
        }
        out
    }

    #[test]
    fn a_streamed_reply_becomes_text_intents_and_done() {
        let llm = FakeLlm::new(
            Script::new("fake").with_turn(
                ScriptTurn::new(vec![
                    LlmEvent::start("fake"),
                    LlmEvent::TextDelta("Hi chat! [emote hap".into()),
                    LlmEvent::TextDelta("py 0.8] Fold it [bogus] and see!".into()),
                    LlmEvent::Usage(Usage::new(400, 30)),
                    LlmEvent::Stop(StopReason::EndTurn),
                ])
                .with_chunk(5),
            ),
        );
        let mut brain = LlmBrain::new(Arc::new(llm), Persona::named("Mochi"));
        let mut stream = brain.turn(TurnInput::new(1, Inventory::default()).heard("hi"), &Cancel::new()).unwrap();
        let events = drain(&mut stream);
        let text: String =
            events.iter().filter_map(|e| if let BrainEvent::Text(t) = e { Some(t.as_str()) } else { None }).collect();
        assert_eq!(text, "Hi chat! Fold it and see!");
        assert!(events.iter().any(|e| matches!(e, BrainEvent::Intent { at_char: 8, .. })));
        assert!(events.iter().any(|e| matches!(e, BrainEvent::Issue(_))));
        assert_eq!(events.last(), Some(&BrainEvent::done(Usage::new(400, 30), StopReason::EndTurn)));
    }

    #[test]
    fn a_cancelled_turn_does_not_start() {
        let llm = FakeLlm::new(Script::new("fake"));
        let mut brain = LlmBrain::new(Arc::new(llm), Persona::named("Mochi"));
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(matches!(brain.turn(TurnInput::new(1, Inventory::default()), &cancel), Err(BrainError::Cancelled)));
    }
}
