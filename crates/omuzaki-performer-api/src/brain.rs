//! Thinking: the [`Brain`] seam, its input and its stream of [`BrainEvent`]s (`ai-performer.md` §2, §6).
//!
//! The core's `LlmBrain` renders the prompt (persona, scene, history, this turn), streams it through `dyn LlmClient`
//! and parses the inline tags (§6.2) as text arrives, so a gesture keeps its word.

use std::fmt;

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::stream::{Cancel, StopReason};
use omuzaki_llm_api::usage::Usage;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::intent::{Intent, StyleTag};
use crate::perception::Glance;
use crate::stage::Inventory;

/// Turns one input into a stream of speech and intents.
pub trait Brain: Send {
    /// Starts a turn. Does no blocking I/O on the calling thread; the stream obeys `cancel` on every call.
    ///
    /// # Errors
    /// [`BrainError`] when the turn cannot start (the LLM refused the request, or `cancel` is already set).
    fn turn(&mut self, input: TurnInput, cancel: &Cancel) -> Result<TurnStream, BrainError>;
}

/// A turn that came before, as the prompt's history shows it (§6.3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PastTurn {
    /// What the human said, if the turn answered speech.
    pub heard: Option<String>,
    /// What the performer said: the spoken prefix only, tags removed.
    pub said: String,
    /// Characters spoken before a cut, if the turn was interrupted.
    pub interrupted_at: Option<u32>,
    /// A private turn: only a private turn's prompt may contain it (§5.5).
    pub private: bool,
}

impl PastTurn {
    /// A public turn that said `said`.
    pub fn new(heard: Option<String>, said: impl Into<String>) -> Self {
        Self { heard, said: said.into(), interrupted_at: None, private: false }
    }

    /// The same turn, cut after `spoken_chars`.
    #[must_use]
    pub fn interrupted(mut self, spoken_chars: u32) -> Self {
        self.interrupted_at = Some(spoken_chars);
        self
    }

    /// The same turn, private.
    #[must_use]
    pub fn private(mut self) -> Self {
        self.private = true;
        self
    }
}

/// Everything one turn's prompt needs besides the persona, which the brain holds. Growing: `ChatMessage`s join it
/// with #1915.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct TurnInput {
    /// The turn's number, rising from 1.
    pub turn: u64,
    /// The final transcript this turn answers, if any.
    pub heard: Option<String>,
    /// Nobody addressed the performer: "say something short or `[pass]`" (§5.7).
    pub proactive: bool,
    /// What the last turn had said when it was cut (§5.4), for the interruption note.
    pub interrupted: Option<PastTurn>,
    /// What the performer sees, when perception is on.
    pub glance: Option<Glance>,
    /// The member's names: the prompt's scene block.
    pub inventory: Inventory,
    /// Earlier turns, oldest first. The loop passes only what this turn may see: never a private turn in a public
    /// turn's input (§5.5).
    pub history: Vec<PastTurn>,
    /// A private turn: no intents may come out of it, and its speech is private.
    pub private: bool,
}

impl TurnInput {
    /// Turn number `turn` over `inventory`, with nothing heard and no history.
    pub fn new(turn: u64, inventory: Inventory) -> Self {
        Self { turn, inventory, ..Self::default() }
    }

    /// Answers `text`.
    #[must_use]
    pub fn heard(mut self, text: impl Into<String>) -> Self {
        self.heard = Some(text.into());
        self
    }

    /// A proactive turn.
    #[must_use]
    pub fn proactive(mut self) -> Self {
        self.proactive = true;
        self
    }

    /// Carries the cut turn for the interruption note.
    #[must_use]
    pub fn interrupted(mut self, turn: PastTurn) -> Self {
        self.interrupted = Some(turn);
        self
    }

    /// Attaches a glance.
    #[must_use]
    pub fn glance(mut self, glance: Glance) -> Self {
        self.glance = Some(glance);
        self
    }

    /// Sets the history.
    #[must_use]
    pub fn history(mut self, history: Vec<PastTurn>) -> Self {
        self.history = history;
        self
    }

    /// Makes the turn private.
    #[must_use]
    pub fn private(mut self) -> Self {
        self.private = true;
        self
    }
}

/// Why a bracket run was dropped (§6.2). It is never spoken.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ParseIssueKind {
    /// The verb is not one of the grammar's.
    UnknownVerb,
    /// The verb is known but its arguments don't parse.
    BadArguments,
    /// Longer than [`TAG_MAX_CHARS`](crate::limits::TAG_MAX_CHARS).
    TooLong,
    /// The stream ended inside `[`.
    Unclosed,
}

/// A bracket run the parser dropped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ParseIssue {
    /// The run as the model wrote it, cut to [`TAG_MAX_CHARS`](crate::limits::TAG_MAX_CHARS).
    pub run: String,
    /// Why it was dropped.
    pub kind: ParseIssueKind,
    /// Its offset in the turn's spoken text, in characters.
    pub at_char: u32,
}

impl ParseIssue {
    /// A dropped run.
    pub fn new(run: impl Into<String>, kind: ParseIssueKind, at_char: u32) -> Self {
        Self { run: run.into(), kind, at_char }
    }
}

/// One step of a turn. Order: content events, then exactly one `Done` (unless the stream errors or is cancelled).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum BrainEvent {
    /// Speakable text, tags and markdown removed. Consecutive texts continue one another.
    Text(String),
    /// An intent tag, at a character offset of the turn's spoken text (the text sent so far).
    #[non_exhaustive]
    Intent {
        /// The parsed intent, before validation and clamping.
        intent: Intent,
        /// Characters of spoken text before it.
        at_char: u32,
    },
    /// `[style …]`: from the next chunk to the end of the turn.
    Style(Vec<StyleTag>),
    /// `[pass]`: the performer chose to say nothing.
    Pass,
    /// A dropped bracket run.
    Issue(ParseIssue),
    /// The turn is over: its final usage (from the LLM's last `Usage` event) and why it stopped.
    #[non_exhaustive]
    Done {
        /// Final usage; the spend gate settles from it.
        usage: Usage,
        /// Why the model stopped.
        stop: StopReason,
    },
}

impl BrainEvent {
    /// [`BrainEvent::Intent`].
    pub fn intent(intent: Intent, at_char: u32) -> Self {
        Self::Intent { intent, at_char }
    }

    /// [`BrainEvent::Done`].
    pub fn done(usage: Usage, stop: StopReason) -> Self {
        Self::Done { usage, stop }
    }
}

/// What [`TurnStream::try_next`] found.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Polled {
    /// The next event.
    Event(BrainEvent),
    /// Nothing yet; poll again.
    Pending,
    /// The stream is over.
    End,
}

/// The brain's side of a [`TurnStream`].
pub trait BrainSource: Send {
    /// The next event, without blocking.
    ///
    /// # Errors
    /// [`BrainError`] when the turn fails; the stream ends with it.
    fn poll(&mut self) -> Result<Polled, BrainError>;
}

/// One turn's events, pulled without blocking, so the performer thread can watch its ears between polls.
pub struct TurnStream {
    source: Box<dyn BrainSource>,
    cancel: Cancel,
    over: bool,
}

impl TurnStream {
    /// A stream over `source` that obeys `cancel`.
    pub fn new(source: Box<dyn BrainSource>, cancel: &Cancel) -> Self {
        Self { source, cancel: cancel.clone(), over: false }
    }

    /// The next event, [`Polled::Pending`] if none is ready, [`Polled::End`] after the last.
    ///
    /// # Errors
    /// [`BrainError::Cancelled`] once `cancel` is set (checked before every poll); the source's error otherwise.
    /// After an error the stream is over.
    pub fn try_next(&mut self) -> Result<Polled, BrainError> {
        if self.over {
            return Ok(Polled::End);
        }
        if self.cancel.is_cancelled() {
            self.over = true;
            return Err(BrainError::Cancelled);
        }
        let next = self.source.poll();
        if matches!(next, Err(_) | Ok(Polled::End)) {
            self.over = true;
        }
        next
    }
}

impl fmt::Debug for TurnStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnStream").field("over", &self.over).finish_non_exhaustive()
    }
}

/// Why a turn failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum BrainError {
    /// The turn was cancelled (barge-in, stop, pause, revoke, budget).
    #[error("the turn was cancelled")]
    Cancelled,
    /// The spend gate refused the request's bound: the show's cap is reached.
    #[error("the show's money cap is reached")]
    Budget,
    /// The LLM failed.
    #[error(transparent)]
    Llm(#[from] LlmError),
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use omuzaki_anim_api::score::ClipId;

    use super::*;

    struct Fixed(VecDeque<Result<Polled, BrainError>>);

    impl BrainSource for Fixed {
        fn poll(&mut self) -> Result<Polled, BrainError> {
            self.0.pop_front().unwrap_or(Ok(Polled::End))
        }
    }

    fn stream(items: Vec<Result<Polled, BrainError>>, cancel: &Cancel) -> TurnStream {
        TurnStream::new(Box::new(Fixed(items.into())), cancel)
    }

    #[test]
    fn a_stream_ends_and_stays_ended() {
        let cancel = Cancel::new();
        let mut s = stream(vec![Ok(Polled::Pending), Ok(Polled::Event(BrainEvent::Pass))], &cancel);
        assert_eq!(s.try_next(), Ok(Polled::Pending));
        assert_eq!(s.try_next(), Ok(Polled::Event(BrainEvent::Pass)));
        assert_eq!(s.try_next(), Ok(Polled::End));
        assert_eq!(s.try_next(), Ok(Polled::End));
    }

    #[test]
    fn cancel_holds_on_the_next_poll_and_errors_end_the_stream() {
        let cancel = Cancel::new();
        let mut s = stream(vec![Ok(Polled::Event(BrainEvent::Text("Hi".into())))], &cancel);
        cancel.cancel();
        assert_eq!(s.try_next(), Err(BrainError::Cancelled));
        assert_eq!(s.try_next(), Ok(Polled::End));
        let mut s = stream(vec![Err(BrainError::Budget), Ok(Polled::Pending)], &Cancel::new());
        assert_eq!(s.try_next(), Err(BrainError::Budget));
        assert_eq!(s.try_next(), Ok(Polled::End));
    }

    #[test]
    fn events_round_trip() {
        let events = [
            BrainEvent::Text("Hi chat!".into()),
            BrainEvent::intent(Intent::gesture(ClipId::new("wave").unwrap(), 1.0), 8),
            BrainEvent::Style(vec![StyleTag::Whisper]),
            BrainEvent::Pass,
            BrainEvent::Issue(ParseIssue::new("[dance wildly]", ParseIssueKind::UnknownVerb, 3)),
            BrainEvent::done(Usage::new(120, 30), StopReason::EndTurn),
        ];
        for e in events {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<BrainEvent>(&json).unwrap(), e, "{json}");
        }
    }

    #[test]
    fn turn_input_builds_up() {
        let input = TurnInput::new(3, Inventory::default())
            .heard("What's your favourite fold?")
            .history(vec![PastTurn::new(None, "Hi chat!"), PastTurn::new(None, "psst").private()])
            .interrupted(PastTurn::new(None, "The crane").interrupted(9));
        assert_eq!(input.turn, 3);
        assert!(input.history[1].private);
        assert_eq!(input.interrupted.and_then(|t| t.interrupted_at), Some(9));
        assert!(!TurnInput::new(1, Inventory::default()).proactive().private().history.iter().any(|t| t.private));
    }
}
