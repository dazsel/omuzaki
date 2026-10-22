//! Hearing: the [`Ears`] seam and what it reports (`ai-performer.md` §2, §5.1, §5.4, §5.5).

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The speech host's view of the `listen` tap, mapped to [`Heard`]. `omuzaki-speech` implements it; the fakes play a
/// script.
pub trait Ears: Send {
    /// The next thing heard, or `None` when nothing new has arrived. Never blocks, so the performer thread can poll it
    /// between LLM deltas and see a barge-in at once.
    fn next(&mut self) -> Option<Heard>;
}

/// One event from the speech host. Times are on the runner clock.
///
/// Order within one utterance: `Onset`, at most one `Held`, then `Final` or `Rejected`. While muted
/// (`Muted(true)`), nothing else arrives until `Muted(false)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Heard {
    /// Voice activity began.
    #[non_exhaustive]
    Onset {
        /// When.
        at: Seconds,
    },
    /// [`BARGE_IN_FRAMES`](crate::limits::BARGE_IN_FRAMES) speech frames in a row: someone really is talking. The
    /// performer cuts its speech on this (§5.4).
    #[non_exhaustive]
    Held {
        /// When.
        at: Seconds,
    },
    /// The activity was not speech (a cough, a click).
    #[non_exhaustive]
    Rejected {
        /// When.
        at: Seconds,
    },
    /// A final transcript.
    #[non_exhaustive]
    Final {
        /// The utterance's number, rising from 0.
        utterance: u64,
        /// What was said.
        text: String,
        /// When speech began.
        start: Seconds,
        /// When speech ended.
        end: Seconds,
        /// Spoken in `CohostOnly`: it gets a private turn (§5.5).
        private: bool,
    },
    /// The mic was muted (`true`) or unmuted (`false`).
    Muted(bool),
}

impl Heard {
    /// [`Heard::Onset`].
    pub fn onset(at: Seconds) -> Self {
        Self::Onset { at }
    }

    /// [`Heard::Held`].
    pub fn held(at: Seconds) -> Self {
        Self::Held { at }
    }

    /// [`Heard::Rejected`].
    pub fn rejected(at: Seconds) -> Self {
        Self::Rejected { at }
    }

    /// A public [`Heard::Final`].
    pub fn final_text(utterance: u64, text: impl Into<String>, start: Seconds, end: Seconds) -> Self {
        Self::Final { utterance, text: text.into(), start, end, private: false }
    }

    /// The same event, private if it is a `Final`; other events are unchanged.
    #[must_use]
    pub fn private(mut self) -> Self {
        if let Self::Final { private, .. } = &mut self {
            *private = true;
        }
        self
    }

    /// When it happened: `at`, or a `Final`'s `end`. `None` for `Muted`, which carries no time.
    pub fn time(&self) -> Option<Seconds> {
        match self {
            Self::Onset { at } | Self::Held { at } | Self::Rejected { at } => Some(*at),
            Self::Final { end, .. } => Some(*end),
            Self::Muted(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_round_trips() {
        let events = [
            Heard::onset(Seconds(1.0)),
            Heard::held(Seconds(1.1)),
            Heard::rejected(Seconds(1.2)),
            Heard::final_text(3, "Mochi, hi", Seconds(1.0), Seconds(2.0)),
            Heard::final_text(4, "between us", Seconds(3.0), Seconds(4.0)).private(),
            Heard::Muted(true),
        ];
        for e in events {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<Heard>(&json).unwrap(), e, "{json}");
        }
    }

    #[test]
    fn times_and_privacy() {
        assert_eq!(Heard::final_text(0, "x", Seconds(1.0), Seconds(2.0)).time(), Some(Seconds(2.0)));
        assert_eq!(Heard::Muted(false).time(), None);
        assert_eq!(Heard::held(Seconds(5.0)).private(), Heard::held(Seconds(5.0)));
        assert!(matches!(
            Heard::final_text(0, "x", Seconds(0.0), Seconds(1.0)).private(),
            Heard::Final { private: true, .. }
        ));
    }
}
