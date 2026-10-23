//! The realtime voice seam (#2347, AP-02, AP-03): a full-duplex speech-to-speech model as the performer's ears, brain
//! and voice at once, as an optional bring-your-own-key provider. The local ASR, LLM and TTS path stays the default.
//!
//! - A [`Duplex`] provider hears the microphone itself (the App hands it the `listen` tap) and decides when the person
//!   has finished. The performer's loop only [`poll`](Duplex::poll)s its [`DuplexEvent`]s: what the person said, the
//!   reply's audio and transcript as they stream, and each reply's bill.
//! - The reply's audio goes to a [`PcmVoice`] (the TTS host's feeds, its cut and the mouth drive), so a realtime reply
//!   reaches the mix, the lip sync and the barge-in exactly as a synthesised one does.
//! - A speech-to-speech model emits no intent tags: the decision layer reads its live transcript for the face and
//!   body (#2348).
//! - Money goes through the [`SpendGate`](crate::spend::SpendGate): each reply reserves a bound from the
//!   [`DuplexPrices`] and settles with its [`DuplexUsage`].

use omuzaki_llm_api::usage::MicroUsd;
use serde::{Deserialize, Serialize};

use crate::speech::{SpeechHandle, VoiceError};

/// What a realtime provider reports, in the order it happened.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum DuplexEvent {
    /// The provider heard the person start talking. A reply that is playing is cut (barge-in).
    SpeechStarted,
    /// The person stopped talking.
    SpeechStopped,
    /// What the person said, transcribed.
    #[non_exhaustive]
    Heard {
        /// The words.
        text: String,
        /// Some of it was spoken in `CohostOnly` (the microphone tap's `private` flag): the reply is private.
        private: bool,
    },
    /// A reply began.
    #[non_exhaustive]
    ReplyStarted {
        /// The provider's id for it.
        reply: String,
    },
    /// Reply audio, in order: mono samples at `rate` Hz.
    #[non_exhaustive]
    ReplyAudio {
        /// The reply.
        reply: String,
        /// The provider's id of the output item the audio belongs to, for a truncation.
        item: String,
        /// The samples, -1.0 to 1.0.
        pcm: Vec<f32>,
        /// Their rate.
        rate: u32,
    },
    /// More of the reply's transcript.
    #[non_exhaustive]
    ReplyText {
        /// The reply.
        reply: String,
        /// The new text.
        delta: String,
    },
    /// The reply ended: completed, cut or failed.
    #[non_exhaustive]
    ReplyDone {
        /// The reply.
        reply: String,
        /// Whether it was cancelled before it finished.
        cancelled: bool,
        /// What it used, when the provider said.
        usage: Option<DuplexUsage>,
    },
    /// The provider reported an error. A `fatal` one ends the session.
    #[non_exhaustive]
    Error {
        /// What it said (never a key).
        message: String,
        /// The session is over.
        fatal: bool,
    },
}

impl DuplexEvent {
    /// [`DuplexEvent::Heard`].
    pub fn heard(text: impl Into<String>) -> Self {
        Self::Heard { text: text.into(), private: false }
    }

    /// A private [`DuplexEvent::Heard`].
    pub fn heard_private(text: impl Into<String>) -> Self {
        Self::Heard { text: text.into(), private: true }
    }

    /// [`DuplexEvent::ReplyStarted`].
    pub fn reply_started(reply: impl Into<String>) -> Self {
        Self::ReplyStarted { reply: reply.into() }
    }

    /// [`DuplexEvent::ReplyAudio`].
    pub fn reply_audio(reply: impl Into<String>, item: impl Into<String>, pcm: Vec<f32>, rate: u32) -> Self {
        Self::ReplyAudio { reply: reply.into(), item: item.into(), pcm, rate }
    }

    /// [`DuplexEvent::ReplyText`].
    pub fn reply_text(reply: impl Into<String>, delta: impl Into<String>) -> Self {
        Self::ReplyText { reply: reply.into(), delta: delta.into() }
    }

    /// [`DuplexEvent::ReplyDone`].
    pub fn reply_done(reply: impl Into<String>, cancelled: bool, usage: Option<DuplexUsage>) -> Self {
        Self::ReplyDone { reply: reply.into(), cancelled, usage }
    }

    /// [`DuplexEvent::Error`].
    pub fn error(message: impl Into<String>, fatal: bool) -> Self {
        Self::Error { message: message.into(), fatal }
    }
}

/// What one reply used, as the provider bills it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DuplexUsage {
    /// Audio tokens read (the person's speech and the conversation so far), not from the cache.
    pub input_audio: u64,
    /// Text tokens read (instructions, transcripts), not from the cache.
    pub input_text: u64,
    /// Tokens read from the provider's cache.
    pub cached_input: u64,
    /// Audio tokens spoken.
    pub output_audio: u64,
    /// Text tokens written (the transcript).
    pub output_text: u64,
}

impl DuplexUsage {
    /// A usage of these counts.
    pub fn new(input_audio: u64, input_text: u64, cached_input: u64, output_audio: u64, output_text: u64) -> Self {
        Self { input_audio, input_text, cached_input, output_audio, output_text }
    }

    /// All its tokens.
    pub fn tokens(&self) -> u64 {
        self.input_audio + self.input_text + self.cached_input + self.output_audio + self.output_text
    }
}

/// A realtime model's prices in micro-dollars per million tokens, and how many tokens a second of audio is.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DuplexPrices {
    /// Audio input.
    pub input_audio: u64,
    /// Text input.
    pub input_text: u64,
    /// Cached input of either kind.
    pub cached_input: u64,
    /// Audio output.
    pub output_audio: u64,
    /// Text output.
    pub output_text: u64,
    /// Audio tokens per second of the person's speech.
    pub input_tokens_per_s: f64,
    /// Audio tokens per second of the reply's speech.
    pub output_tokens_per_s: f64,
}

impl DuplexPrices {
    /// Prices per million tokens (in micro-dollars) and audio token rates.
    #[allow(clippy::too_many_arguments)] // one price per kind of token, named at the call site
    pub fn new(
        input_audio: u64,
        input_text: u64,
        cached_input: u64,
        output_audio: u64,
        output_text: u64,
        input_tokens_per_s: f64,
        output_tokens_per_s: f64,
    ) -> Self {
        Self { input_audio, input_text, cached_input, output_audio, output_text, input_tokens_per_s, output_tokens_per_s }
    }

    /// What `usage` costs, rounded up.
    pub fn cost(&self, usage: &DuplexUsage) -> MicroUsd {
        let sum = u128::from(usage.input_audio) * u128::from(self.input_audio)
            + u128::from(usage.input_text) * u128::from(self.input_text)
            + u128::from(usage.cached_input) * u128::from(self.cached_input)
            + u128::from(usage.output_audio) * u128::from(self.output_audio)
            + u128::from(usage.output_text) * u128::from(self.output_text);
        MicroUsd(u64::try_from(sum.div_ceil(1_000_000)).unwrap_or(u64::MAX))
    }

    /// A minute of the person talking, as fresh audio input.
    pub fn listening_per_minute(&self) -> MicroUsd {
        Self::per_minute(self.input_tokens_per_s, self.input_audio)
    }

    /// A minute of the character talking, as audio output.
    pub fn speaking_per_minute(&self) -> MicroUsd {
        Self::per_minute(self.output_tokens_per_s, self.output_audio)
    }

    /// The most a reply of `seconds` of speech, reading `context` tokens of conversation, can cost: its audio output,
    /// its transcript (about as many text tokens as audio seconds times 3) and the context at the full input price.
    pub fn reply_bound(&self, seconds: f64, context: u64) -> MicroUsd {
        // Token counts from seconds of speech: small, positive and rounded up.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let audio = (seconds * self.output_tokens_per_s).ceil().max(0.0) as u64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let text = (seconds * 3.0).ceil().max(0.0) as u64;
        self.cost(&DuplexUsage::new(context, 0, 0, audio, text))
    }

    fn per_minute(tokens_per_s: f64, price: u64) -> MicroUsd {
        // A minute of tokens: a small positive count.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let tokens = (tokens_per_s * 60.0).ceil().max(0.0) as u64;
        MicroUsd(u64::try_from((u128::from(tokens) * u128::from(price)).div_ceil(1_000_000)).unwrap_or(u64::MAX))
    }
}

/// Why a provider refused a request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DuplexError {
    /// The session is closed (it failed, or was stopped).
    #[error("the realtime session is closed")]
    Closed,
    /// The provider or the connection failed.
    #[error("the realtime provider failed: {0}")]
    Provider(String),
}

/// A full-duplex speech-to-speech provider. Never blocks the caller: requests are queued for its connection thread.
pub trait Duplex: Send {
    /// Its kind (`openai-realtime`).
    fn kind(&self) -> &str;

    /// The prices it bills at.
    fn prices(&self) -> DuplexPrices;

    /// The events since the last poll, oldest first.
    fn poll(&mut self) -> Vec<DuplexEvent>;

    /// Asks for a reply now; `instructions` replace the session's for this reply only (`performer.say`'s line).
    ///
    /// # Errors
    /// [`DuplexError::Closed`] when the session is over.
    fn reply(&mut self, instructions: Option<&str>) -> Result<(), DuplexError>;

    /// Cancels reply `reply` and tells the provider only `played_ms` of item `item`'s audio was heard, so its memory
    /// of the conversation matches what the person heard.
    ///
    /// # Errors
    /// [`DuplexError::Closed`] when the session is over.
    fn cancel(&mut self, reply: &str, item: Option<&str>, played_ms: u64) -> Result<(), DuplexError>;

    /// Whether the provider answers by itself when the person stops talking (`true`), or only when asked
    /// ([`reply`](Self::reply)): the performer asks itself when it must check who was addressed first.
    ///
    /// # Errors
    /// [`DuplexError::Closed`] when the session is over.
    fn auto_reply(&mut self, on: bool) -> Result<(), DuplexError>;

    /// Ends the session.
    fn close(&mut self);
}

/// A voice that plays PCM it is given (a realtime reply's audio) through the same feeds, cut and mouth drive as
/// synthesised speech.
pub trait PcmVoice: Send {
    /// The rate it plays at, in Hz.
    fn rate(&self) -> u32;

    /// Queues `pcm` (mono, [`rate`](Self::rate) Hz) after what is queued, gaplessly; `private` plays it on the
    /// monitor only.
    ///
    /// # Errors
    /// [`VoiceError::Provider`] when the host can't take it.
    fn play(&mut self, pcm: Vec<f32>, private: bool) -> Result<SpeechHandle, VoiceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gpt-realtime's audio prices (October 2026): $32 in, $0.40 cached, $64 out per million tokens; text $4 in and
    /// $16 out; 10 audio tokens a second heard, 20 spoken.
    fn prices() -> DuplexPrices {
        DuplexPrices::new(32_000_000, 4_000_000, 400_000, 64_000_000, 16_000_000, 10.0, 20.0)
    }

    #[test]
    fn a_minute_of_listening_and_of_speaking_costs_what_the_price_sheet_says() {
        // 600 tokens × $32/M = $0.0192; 1200 tokens × $64/M = $0.0768.
        assert_eq!(prices().listening_per_minute(), MicroUsd(19_200));
        assert_eq!(prices().speaking_per_minute(), MicroUsd(76_800));
    }

    #[test]
    fn a_reply_costs_its_usage_and_is_bounded_before_it_starts() {
        let usage = DuplexUsage::new(1_000, 200, 5_000, 400, 60);
        // 1000×32 + 200×4 + 5000×0.4 + 400×64 + 60×16 = 32000 + 800 + 2000 + 25600 + 960 µ$·M / M
        assert_eq!(prices().cost(&usage), MicroUsd(61_360));
        assert_eq!(usage.tokens(), 6_660);
        // 20 s of speech (400 audio tokens, 60 text) reading 1000 tokens of context.
        assert_eq!(prices().reply_bound(20.0, 1_000), MicroUsd(32_000 + 25_600 + 960));
    }
}
