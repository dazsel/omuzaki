//! Voice fakes: [`FakeVoice`] (instant synthesis, fixed audio per character, chunks back to back), [`FakeCatalog`],
//! [`RecordingLead`] and [`FakeCut`].

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::limits::CHUNK_MAX_CHARS;
use omuzaki_performer_api::speech::{SpeakHow, SpeechControl, SpeechHandle, SpeechState, Voice, VoiceError};
use omuzaki_performer_api::voice::{CutProbe, CutSource, CutState, VoiceCatalog, VoiceInfo, VoiceLead, VoiceProvider};

/// Locks a fake's record; a poisoned lock only means a test thread panicked, and the record is still a plain log.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Audio per character: 60 ms, as `ai-performer.md` §9 and §10 time it.
pub const MS_PER_CHAR: f64 = 60.0;
/// The fake engine's block: a cut lands this long after `cut()`, as the audio thread reads the flag at the next block.
pub const BLOCK_S: f64 = 0.005;

/// One chunk the fake was asked to speak.
#[derive(Clone, Debug, PartialEq)]
pub struct Spoken {
    /// The chunk.
    pub text: String,
    /// The voice asked for.
    pub voice: VoiceId,
    /// How.
    pub how: SpeakHow,
    /// When playback started (or will).
    pub start: Seconds,
    /// Its audio length.
    pub duration: Seconds,
    /// When it went silent and how many characters played, if it was cut.
    pub cut: Option<(Seconds, u32)>,
}

impl Spoken {
    /// When it ends (or ended): its cut, or the end of its audio.
    pub fn end(&self) -> Seconds {
        self.cut.map_or(Seconds(self.start.0 + self.duration.0), |(at, _)| at)
    }
}

/// One chunk's shared record.
struct Chunk {
    clock: Arc<FakeClock>,
    spoken: Mutex<Spoken>,
}

impl SpeechControl for Chunk {
    fn state(&self) -> SpeechState {
        let now = self.clock.now().0;
        let s = lock(&self.spoken);
        match s.cut {
            Some((at, spoken)) if now >= at.0 => SpeechState::cut(at, spoken),
            _ if now < s.start.0 => SpeechState::ready(s.duration),
            _ if now < s.start.0 + s.duration.0 => SpeechState::playing(s.start, s.duration),
            Some((at, spoken)) => SpeechState::cut(at, spoken),
            None => SpeechState::done(Seconds(s.start.0 + s.duration.0)),
        }
    }

    fn cut(&self) {
        let now = self.clock.now().0;
        let mut s = lock(&self.spoken);
        if s.cut.is_some() || now >= s.start.0 + s.duration.0 {
            return;
        }
        let at = now + BLOCK_S;
        let played = ((at - s.start.0).max(0.0) * 1000.0 / MS_PER_CHAR).floor();
        let chars = s.text.chars().count();
        // `played` is a whole, non-negative count, capped at the chunk's length.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let spoken = (played as usize).min(chars);
        s.cut = Some((Seconds(at), u32::try_from(spoken).unwrap_or(u32::MAX)));
    }
}

/// A [`Voice`] on a [`FakeClock`]: synthesis is instant, each character is [`MS_PER_CHAR`] of audio, a chunk starts
/// `lead` after it is queued and never before the previous one ends, and a cut lands one [`BLOCK_S`] later. It
/// records every chunk.
pub struct FakeVoice {
    clock: Arc<FakeClock>,
    lead: Seconds,
    chunks: Vec<Arc<Chunk>>,
}

impl FakeVoice {
    /// A voice on `clock` whose chunks start `lead` after `speak`.
    pub fn new(clock: Arc<FakeClock>, lead: Seconds) -> Self {
        Self { clock, lead, chunks: Vec::new() }
    }

    /// Every chunk spoken so far, in order.
    pub fn log(&self) -> Vec<Spoken> {
        self.chunks.iter().map(|c| lock(&c.spoken).clone()).collect()
    }
}

impl Voice for FakeVoice {
    fn speak(&mut self, sentence: &str, voice: &VoiceId, how: &SpeakHow) -> Result<SpeechHandle, VoiceError> {
        let chars = sentence.chars().count();
        if chars == 0 || chars > CHUNK_MAX_CHARS {
            return Err(VoiceError::bad_chunk(chars));
        }
        let queued = self.clock.now().0 + self.lead.0;
        let after = self.chunks.last().map_or(queued, |c| lock(&c.spoken).end().0);
        // Chunk lengths stay far below 2^52 characters.
        #[allow(clippy::cast_precision_loss)]
        let duration = Seconds(chars as f64 * MS_PER_CHAR / 1000.0);
        let spoken = Spoken {
            text: sentence.to_owned(),
            voice: voice.clone(),
            how: how.clone(),
            start: Seconds(queued.max(after)),
            duration,
            cut: None,
        };
        let chunk = Arc::new(Chunk { clock: Arc::clone(&self.clock), spoken: Mutex::new(spoken) });
        self.chunks.push(Arc::clone(&chunk));
        Ok(SpeechHandle::new(chunk))
    }
}

/// A [`VoiceCatalog`] with fixed lists per provider.
#[derive(Default)]
pub struct FakeCatalog {
    voices: BTreeMap<String, Vec<VoiceInfo>>,
}

fn key(provider: &VoiceProvider) -> String {
    match provider {
        VoiceProvider::Hosted(c) => format!("hosted:{c}"),
        _ => "local".to_owned(),
    }
}

impl FakeCatalog {
    /// An empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `voices` to `provider`'s list.
    #[must_use]
    pub fn with(mut self, provider: &VoiceProvider, voices: Vec<VoiceInfo>) -> Self {
        self.voices.entry(key(provider)).or_default().extend(voices);
        self
    }
}

impl VoiceCatalog for FakeCatalog {
    fn voices(&self, provider: &VoiceProvider) -> Result<Vec<VoiceInfo>, VoiceError> {
        Ok(self.voices.get(&key(provider)).cloned().unwrap_or_default())
    }
}

/// One call a [`RecordingLead`] received.
#[derive(Clone, Debug, PartialEq)]
pub enum LeadCall {
    /// `attach`, with the probe's state at the time.
    Attach(CutState),
    /// `push`: when, the rate, how many samples, private.
    Push {
        /// Program time of the first sample.
        at: Seconds,
        /// Sample rate.
        rate: u32,
        /// Samples.
        len: usize,
        /// Private PCM.
        private: bool,
    },
    /// `cut`.
    Cut(Seconds),
}

/// A [`VoiceLead`] that records its calls (the TTS host's tests read them).
#[derive(Clone, Default)]
pub struct RecordingLead(Arc<Mutex<Vec<LeadCall>>>);

impl RecordingLead {
    /// An empty record.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call so far.
    pub fn calls(&self) -> Vec<LeadCall> {
        lock(&self.0).clone()
    }
}

impl VoiceLead for RecordingLead {
    fn attach(&mut self, probe: CutProbe) {
        lock(&self.0).push(LeadCall::Attach(probe.state()));
    }

    fn push(&mut self, at: Seconds, rate: u32, pcm: &[f32], private: bool) {
        lock(&self.0).push(LeadCall::Push { at, rate, len: pcm.len(), private });
    }

    fn cut(&mut self, at: Seconds) {
        lock(&self.0).push(LeadCall::Cut(at));
    }
}

/// A feed's cut state that a test sets: the source behind a [`CutProbe`].
#[derive(Clone)]
pub struct FakeCut(Arc<Mutex<CutState>>);

impl FakeCut {
    /// A live feed.
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(CutState::Live)))
    }

    /// Sets the state.
    pub fn set(&self, state: CutState) {
        *lock(&self.0) = state;
    }

    /// A probe reading this state.
    pub fn probe(&self) -> CutProbe {
        CutProbe::new(Arc::new(self.clone()))
    }
}

impl Default for FakeCut {
    fn default() -> Self {
        Self::new()
    }
}

impl CutSource for FakeCut {
    fn state(&self) -> CutState {
        *lock(&self.0)
    }
}
