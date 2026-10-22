//! Speech out: the [`Voice`] seam the chunker speaks through, and the [`SpeechHandle`] it gets back
//! (`ai-performer.md` §2, §5.2, §5.4; `ai-performer-voice-face.md` §3).

use std::fmt;
use std::sync::Arc;

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::VoiceId;
use crate::intent::StyleTag;

/// Speaks chunks. `omuzaki-speech`'s TTS host implements it over a `tts@1` session or a hosted connector, writing the
/// `ai_voice` feeds; the App wires it to the performer.
pub trait Voice: Send {
    /// Queues `sentence` (one chunk, tags removed) in `voice`. Returns at once: synthesis runs on the host's own
    /// thread, and chunks play back to back in the order they were queued.
    ///
    /// # Errors
    /// [`VoiceError`] when the voice is unknown or the host cannot take the chunk; nothing is queued then.
    fn speak(&mut self, sentence: &str, voice: &VoiceId, how: &SpeakHow) -> Result<SpeechHandle, VoiceError>;
}

/// How a chunk is spoken: its style, and whether it is private.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SpeakHow {
    /// The style in force (§3.6), applied in order.
    pub style: Vec<StyleTag>,
    /// Private: the chunk goes to `monitor_out` only, on its own bus, never to `program` (audio-api 4.2).
    pub private: bool,
}

impl SpeakHow {
    /// Public speech in `style`.
    pub fn new(style: Vec<StyleTag>) -> Self {
        Self { style, private: false }
    }

    /// The same, private.
    #[must_use]
    pub fn private(mut self) -> Self {
        self.private = true;
        self
    }
}

/// Where a queued chunk is. `duration` is known once synthesised, before playback; `from` and `at` are program time,
/// measured from the feed's playhead.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum SpeechState {
    /// Waiting for synthesis or a free feed.
    Queued,
    /// Synthesised and waiting for its turn.
    #[non_exhaustive]
    Ready {
        /// The chunk's audio length.
        duration: Seconds,
    },
    /// Playing since `from`.
    #[non_exhaustive]
    Playing {
        /// When its first sample entered the bus.
        from: Seconds,
        /// Its audio length.
        duration: Seconds,
    },
    /// Played to the end.
    #[non_exhaustive]
    Done {
        /// When its last sample entered the bus.
        at: Seconds,
    },
    /// Cut by [`SpeechHandle::cut`] (or the turn's cancel). Final: it never plays again.
    #[non_exhaustive]
    Cut {
        /// When the feed went silent.
        at: Seconds,
        /// Characters of the chunk that played before the cut; 0 if it never started.
        spoken_chars: u32,
    },
}

impl SpeechState {
    /// [`SpeechState::Ready`].
    pub fn ready(duration: Seconds) -> Self {
        Self::Ready { duration }
    }

    /// [`SpeechState::Playing`].
    pub fn playing(from: Seconds, duration: Seconds) -> Self {
        Self::Playing { from, duration }
    }

    /// [`SpeechState::Done`].
    pub fn done(at: Seconds) -> Self {
        Self::Done { at }
    }

    /// [`SpeechState::Cut`].
    pub fn cut(at: Seconds, spoken_chars: u32) -> Self {
        Self::Cut { at, spoken_chars }
    }

    /// True once the chunk is over, played or cut.
    pub fn is_final(&self) -> bool {
        matches!(self, Self::Done { .. } | Self::Cut { .. })
    }
}

/// What stands behind a [`SpeechHandle`]: the host's shared record of one chunk.
pub trait SpeechControl: Send + Sync {
    /// The chunk's current state.
    fn state(&self) -> SpeechState;
    /// Cuts the chunk: raises its feed's close flag at once, from the calling thread (§3.2). Idempotent; a final
    /// chunk stays as it is.
    fn cut(&self);
}

/// The performer's handle on one queued chunk. Cheap to clone; every clone sees the same chunk.
#[derive(Clone)]
pub struct SpeechHandle(Arc<dyn SpeechControl>);

impl SpeechHandle {
    /// A handle over the host's record of the chunk.
    pub fn new(control: Arc<dyn SpeechControl>) -> Self {
        Self(control)
    }

    /// Where the chunk is.
    pub fn state(&self) -> SpeechState {
        self.0.state()
    }

    /// Cuts the chunk (barge-in, stop, cancel). Never blocks.
    pub fn cut(&self) {
        self.0.cut();
    }
}

impl fmt::Debug for SpeechHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SpeechHandle").field(&self.state()).finish()
    }
}

/// Why the voice side refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VoiceError {
    /// The voice is not in the catalog for its provider.
    #[error("voice {voice} is not installed or offered by its provider")]
    #[non_exhaustive]
    UnknownVoice {
        /// The voice asked for.
        voice: String,
    },
    /// The voice is cloned or imitates someone, and has no consent record (SE-05).
    #[error("voice {voice} needs a consent record")]
    #[non_exhaustive]
    NoConsent {
        /// The voice.
        voice: String,
    },
    /// The chunk is empty or longer than the chunker allows.
    #[error("a chunk must be 1 to {max} characters (got {len})")]
    #[non_exhaustive]
    BadChunk {
        /// Its length in characters.
        len: usize,
        /// [`CHUNK_MAX_CHARS`](crate::limits::CHUNK_MAX_CHARS).
        max: usize,
    },
    /// The provider or runtime failed.
    #[error("the voice provider failed: {0}")]
    Provider(String),
}

impl VoiceError {
    /// [`VoiceError::UnknownVoice`].
    pub fn unknown_voice(voice: &VoiceId) -> Self {
        Self::UnknownVoice { voice: voice.to_string() }
    }

    /// [`VoiceError::NoConsent`].
    pub fn no_consent(voice: &VoiceId) -> Self {
        Self::NoConsent { voice: voice.to_string() }
    }

    /// [`VoiceError::BadChunk`] for a chunk of `len` characters.
    pub fn bad_chunk(len: usize) -> Self {
        Self::BadChunk { len, max: crate::limits::CHUNK_MAX_CHARS }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct Record(Mutex<SpeechState>);

    impl SpeechControl for Record {
        fn state(&self) -> SpeechState {
            *self.0.lock().unwrap()
        }

        fn cut(&self) {
            let mut s = self.0.lock().unwrap();
            if !s.is_final() {
                *s = SpeechState::cut(Seconds(1.0), 0);
            }
        }
    }

    #[test]
    fn clones_share_one_chunk_and_a_cut_is_final() {
        let handle = SpeechHandle::new(Arc::new(Record(Mutex::new(SpeechState::ready(Seconds(0.48))))));
        let other = handle.clone();
        other.cut();
        assert_eq!(handle.state(), SpeechState::cut(Seconds(1.0), 0));
        assert!(handle.state().is_final());
        assert!(format!("{handle:?}").contains("Cut"));
    }

    #[test]
    fn states_and_errors_round_trip() {
        let states = [
            SpeechState::Queued,
            SpeechState::ready(Seconds(0.5)),
            SpeechState::playing(Seconds(0.8), Seconds(0.48)),
            SpeechState::done(Seconds(1.28)),
            SpeechState::cut(Seconds(11.905), 14),
        ];
        for s in states {
            assert_eq!(serde_json::from_str::<SpeechState>(&serde_json::to_string(&s).unwrap()).unwrap(), s);
        }
        let how = SpeakHow::new(vec![StyleTag::Whisper]).private();
        assert_eq!(serde_json::from_str::<SpeakHow>(&serde_json::to_string(&how).unwrap()).unwrap(), how);
        assert_eq!(VoiceError::bad_chunk(300).to_string(), "a chunk must be 1 to 220 characters (got 300)");
    }
}
