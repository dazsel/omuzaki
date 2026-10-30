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
    /// The words of the utterance in progress so far (a partial transcript): what the listening face reacts to
    /// while the person talks (#2453). Never a turn: only a `Final` is answered.
    #[non_exhaustive]
    Partial {
        /// The utterance it belongs to.
        utterance: u64,
        /// The words so far.
        text: String,
        /// How far the audio went, on the runner clock. It comes from the recogniser, later than the voice
        /// activity around it, so it is not part of the event order ([`Heard::time`] is `None`).
        at: Seconds,
        /// Spoken in `CohostOnly`: its words never leave the machine for a reaction.
        private: bool,
    },
    /// A place where a listener would nod or say "mm" while the person keeps talking (a low-pitch stretch, or a pause
    /// after a falling phrase): the speech host's backchannel cue.
    #[non_exhaustive]
    Backchannel {
        /// When.
        at: Seconds,
    },
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

    /// [`Heard::Partial`].
    pub fn partial(utterance: u64, text: impl Into<String>, at: Seconds) -> Self {
        Self::Partial { utterance, text: text.into(), at, private: false }
    }

    /// [`Heard::Backchannel`].
    pub fn backchannel(at: Seconds) -> Self {
        Self::Backchannel { at }
    }

    /// The same event, private if it is a `Final` or a `Partial`; other events are unchanged.
    #[must_use]
    pub fn private(mut self) -> Self {
        if let Self::Final { private, .. } | Self::Partial { private, .. } = &mut self {
            *private = true;
        }
        self
    }

    /// When it happened: `at`, or a `Final`'s `end`. `None` for `Muted`, which carries no time, and for a
    /// `Partial`, which arrives from the recogniser after the events around its audio.
    pub fn time(&self) -> Option<Seconds> {
        match self {
            Self::Onset { at } | Self::Held { at } | Self::Rejected { at } | Self::Backchannel { at } => Some(*at),
            Self::Final { end, .. } => Some(*end),
            Self::Muted(_) | Self::Partial { .. } => None,
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
            Heard::partial(5, "so I went to the", Seconds(5.5)),
            Heard::backchannel(Seconds(6.0)),
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
