//! [`SpeechEars`]: the speech host as the AI performer hears it (`ai-performer.md` §5.4, §5.5).
//!
//! The host's events become [`Heard`] on the runner clock: `SpeechStarted` is an `Onset`, `Held` the barge-in, a
//! rejected or empty utterance `Rejected`, a transcript `Final` (private when spoken in `CohostOnly`), and the mute
//! flag `Muted`, and the partial transcripts and backchannel cues the listening face reacts to (#2453). Queue drops and errors
//! stay with the host's own consumers.
//!
//! **Echo.** `listen` reads the echo-cancelled signal, but a canceller that hasn't converged lets the performer's own
//! voice back in. So a transcript that overlaps the performer's speech in time and matches its text (token Jaccard at
//! least [`ECHO_JACCARD`]) is dropped and counted. What the performer said reaches the ears through an [`EchoLog`]
//! the TTS host writes as each chunk plays.

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use omuzaki_core::time::Seconds;
use omuzaki_performer_api::ears::{Ears, Heard};
use omuzaki_performer_api::limits::ECHO_JACCARD;

use crate::host::SpeechEvent;
use crate::queue::DropOldestQueue;

/// How many of the performer's recent lines the log keeps.
const ECHO_LINES: usize = 32;
/// How far apart in time a transcript and a line may be and still be one echo, seconds: the room and the canceller
/// delay the echo, and the segmenter pads the utterance.
const ECHO_SLACK: f64 = 0.5;

/// One line the performer spoke, on the runner clock.
#[derive(Clone, Debug, PartialEq)]
struct Line {
    text: String,
    start: f64,
    end: f64,
}

/// What the performer said recently, shared by the TTS host (which writes each chunk as it plays) and the ears (which
/// read it to drop echoes). Cloning shares the log.
#[derive(Clone, Debug, Default)]
pub struct EchoLog(Arc<Mutex<VecDeque<Line>>>);

impl EchoLog {
    /// A new, empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// The performer spoke `text` from `start` to `end`.
    pub fn said(&self, text: &str, start: Seconds, end: Seconds) {
        let mut lines = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if lines.len() == ECHO_LINES {
            lines.pop_front();
        }
        lines.push_back(Line { text: text.to_owned(), start: start.0, end: end.0 });
    }

    /// Whether `text`, heard from `start` to `end`, is the performer's own voice: it overlaps lines whose words it
    /// matches, one line or the overlapping lines together, at a token Jaccard of at least [`ECHO_JACCARD`].
    pub fn is_echo(&self, text: &str, start: Seconds, end: Seconds) -> bool {
        let heard = tokens(text);
        if heard.is_empty() {
            return false;
        }
        let lines = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let overlapping: Vec<&Line> =
            lines.iter().filter(|l| l.start - ECHO_SLACK < end.0 && start.0 < l.end + ECHO_SLACK).collect();
        let together: BTreeSet<String> = overlapping.iter().flat_map(|l| tokens(&l.text)).collect();
        overlapping
            .iter()
            .map(|l| tokens(&l.text))
            .chain(std::iter::once(together))
            .any(|said| jaccard(&heard, &said) >= ECHO_JACCARD)
    }
}

/// `text`'s words, lowercase, letters and digits only.
fn tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// `|a ∩ b| / |a ∪ b|`, 0 for two empty sets.
fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f32 / union as f32
}

/// The speech host's events as [`Heard`]. Made by [`SpeechHost::ears`](crate::SpeechHost::ears); it takes the host's
/// events, so nothing else should read them while it is in use.
pub struct SpeechEars {
    events: Arc<DropOldestQueue<SpeechEvent>>,
    /// The runner time of the host's first sample: host times are offsets from it.
    origin: f64,
    echo: EchoLog,
    echoes: Arc<AtomicU64>,
}

impl SpeechEars {
    pub(crate) fn new(events: Arc<DropOldestQueue<SpeechEvent>>, origin: Seconds, echo: EchoLog) -> Self {
        Self { events, origin: origin.0, echo, echoes: Arc::new(AtomicU64::new(0)) }
    }

    /// Transcripts dropped as the performer's own echo so far; readable after the ears move to the performer.
    pub fn echoes(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.echoes)
    }

    fn at(&self, host: f64) -> Seconds {
        Seconds(self.origin + host)
    }

    /// The event as the performer hears it, `None` for one it doesn't.
    fn heard(&self, event: SpeechEvent) -> Option<Heard> {
        match event {
            SpeechEvent::SpeechStarted { at } => Some(Heard::onset(self.at(at))),
            SpeechEvent::Held { at } => Some(Heard::held(self.at(at))),
            SpeechEvent::SpeechRejected { end, .. } => Some(Heard::rejected(self.at(end))),
            SpeechEvent::Muted(muted) => Some(Heard::Muted(muted)),
            SpeechEvent::Backchannel { at } => Some(Heard::backchannel(self.at(at))),
            SpeechEvent::Partial { utterance, start, end, text, private } => {
                let text = text.trim();
                // Its own voice coming back is not the person talking.
                if text.is_empty() || self.echo.is_echo(text, self.at(start), self.at(end)) {
                    return None;
                }
                let heard = Heard::partial(utterance, text, self.at(end));
                Some(if private { heard.private() } else { heard })
            }
            SpeechEvent::Final { utterance, start, end, text, private, .. } => {
                let (start, end) = (self.at(start), self.at(end));
                let text = text.trim();
                if text.is_empty() {
                    // Whisper heard no words: to the performer that was not speech.
                    return Some(Heard::rejected(end));
                }
                if self.echo.is_echo(text, start, end) {
                    self.echoes.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
                let heard = Heard::final_text(utterance, text, start, end);
                Some(if private { heard.private() } else { heard })
            }
            _ => None,
        }
    }
}

impl Ears for SpeechEars {
    fn next(&mut self) -> Option<Heard> {
        while let Some(event) = self.events.try_pop() {
            if let Some(heard) = self.heard(event) {
                return Some(heard);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_echo_overlaps_in_time_and_matches_in_words() {
        let log = EchoLog::new();
        log.said("Hi chat! Welcome back to the stream.", Seconds(10.0), Seconds(12.0));
        log.said("Today we're building a boat.", Seconds(12.2), Seconds(14.0));
        assert!(log.is_echo("hi chat welcome back to the stream", Seconds(10.1), Seconds(12.3)));
        // Two lines heard as one utterance.
        assert!(log.is_echo(
            "Hi chat, welcome back to the stream. Today we're building a boat.",
            Seconds(10.0),
            Seconds(14.1)
        ));
        // The same words long after: the person quoting the performer, not an echo.
        assert!(!log.is_echo("hi chat welcome back to the stream", Seconds(30.0), Seconds(32.0)));
        // Overlapping in time but different words: the person talking over the performer.
        assert!(!log.is_echo("wait what boat are you building", Seconds(12.5), Seconds(13.5)));
        assert!(!log.is_echo("", Seconds(10.0), Seconds(12.0)));
    }

    #[test]
    fn jaccard_is_on_word_sets() {
        let a = tokens("The boat, the BOAT!");
        assert_eq!(a.len(), 2);
        assert_eq!(jaccard(&a, &tokens("a boat")), 1.0 / 3.0);
        assert_eq!(jaccard(&BTreeSet::new(), &BTreeSet::new()), 0.0);
    }

    #[test]
    fn the_log_keeps_the_newest_lines() {
        let log = EchoLog::new();
        for i in 0..40 {
            log.said(&format!("line {i}"), Seconds(f64::from(i)), Seconds(f64::from(i) + 0.5));
        }
        assert_eq!(log.0.lock().unwrap().len(), ECHO_LINES);
        assert!(!log.is_echo("line 0", Seconds(0.0), Seconds(0.5)));
        assert!(log.is_echo("line 39", Seconds(39.0), Seconds(39.5)));
    }
}
