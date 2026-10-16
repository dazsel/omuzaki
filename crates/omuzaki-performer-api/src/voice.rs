//! Voices and the mouth's lead (`ai-performer-voice-face.md` §2, §3.4, §3.5, §4.1, §4.2): which voice a character
//! speaks in, what voices exist, and the TTS host's view of the mouth drive.

use std::fmt;
use std::sync::Arc;

use omuzaki_char_api::ids::CharId;
use omuzaki_core::time::Seconds;
use omuzaki_l10n_api::Locale;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{ConnectorId, VoiceId};
use crate::speech::VoiceError;

/// The slowest `speed` an assignment may set (§3.3).
pub const SPEED_MIN: f32 = 0.7;
/// The fastest `speed` an assignment may set (§3.3).
pub const SPEED_MAX: f32 = 1.4;
/// The lowest pitch shift, in semitones (§3.3): formants move with it, so it stays small.
pub const PITCH_MIN_SEMITONES: f32 = -4.0;
/// The highest pitch shift, in semitones (§3.3).
pub const PITCH_MAX_SEMITONES: f32 = 4.0;

/// Where a voice is synthesised.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VoiceProvider {
    /// A local `tts@1` model (ML-04). Costs nothing.
    Local,
    /// A hosted connector (CN-10): speech text leaves the device, and each request goes through the
    /// [`SpendGate`](crate::spend::SpendGate).
    Hosted(ConnectorId),
}

/// The consent record a cloned or imitating voice names (SE-05), looked up with `ConsentLookup::find(person,
/// Likeness::VoiceClone)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ConsentRef {
    /// The person whose voice it is, as the record names them.
    pub person: String,
}

impl ConsentRef {
    /// The record of `person`'s consent.
    pub fn new(person: impl Into<String>) -> Self {
        Self { person: person.into() }
    }
}

/// A character's voice: `characters/<id>/voice-assignment.toml` (§3.4), its own file next to the effect presets of
/// `voice.toml`, so the two writers never share a schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct VoiceAssignment {
    /// The character that speaks with it.
    pub character: CharId,
    /// The voice.
    pub voice: VoiceId,
    /// Where it is synthesised.
    pub provider: VoiceProvider,
    /// Tempo, [`SPEED_MIN`]`..=`[`SPEED_MAX`].
    pub speed: f32,
    /// Pitch shift in semitones, [`PITCH_MIN_SEMITONES`]`..=`[`PITCH_MAX_SEMITONES`], made by the declared feed rate.
    pub pitch_semitones: f32,
    /// The consent record, for a cloned or imitating voice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<ConsentRef>,
}

impl VoiceAssignment {
    /// `character` speaks in `voice` from `provider`, at speed 1 and no pitch shift.
    pub fn new(character: CharId, voice: VoiceId, provider: VoiceProvider) -> Self {
        Self { character, voice, provider, speed: 1.0, pitch_semitones: 0.0, consent: None }
    }

    /// Sets speed and pitch.
    #[must_use]
    pub fn tuned(mut self, speed: f32, pitch_semitones: f32) -> Self {
        self.speed = speed;
        self.pitch_semitones = pitch_semitones;
        self
    }

    /// Names a consent record.
    #[must_use]
    pub fn with_consent(mut self, consent: ConsentRef) -> Self {
        self.consent = Some(consent);
        self
    }

    /// Checks the assignment against the voice it names (§3.4, §3.5): speed and pitch in range, the voice from this
    /// provider, and a consent record when the voice is cloned or imitates someone. Whether the record exists is the
    /// caller's lookup.
    ///
    /// # Errors
    /// [`AssignError`] naming the first rule broken.
    pub fn check(&self, info: &VoiceInfo) -> Result<(), AssignError> {
        if !(SPEED_MIN..=SPEED_MAX).contains(&self.speed) {
            return Err(AssignError::speed(self.speed));
        }
        if !(PITCH_MIN_SEMITONES..=PITCH_MAX_SEMITONES).contains(&self.pitch_semitones) {
            return Err(AssignError::pitch(self.pitch_semitones));
        }
        if info.id != self.voice || !info.offered_by(&self.provider) {
            return Err(AssignError::Voice(VoiceError::unknown_voice(&self.voice)));
        }
        if info.needs_consent() && self.consent.is_none() {
            return Err(AssignError::Voice(VoiceError::no_consent(&self.voice)));
        }
        Ok(())
    }

    /// The feed's declared rate for a 24 kHz voice: `24_000 × 2^(p/12)` (§3.3), rounded to whole hertz.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // pitch is checked to ±4: 18 kHz to 31 kHz
    pub fn feed_rate(&self) -> u32 {
        (24_000.0 * 2f64.powf(f64::from(self.pitch_semitones) / 12.0)).round() as u32
    }
}

/// Why an assignment is refused.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum AssignError {
    /// `speed` out of range.
    #[error("speed must be {min} to {max} (got {got})")]
    #[non_exhaustive]
    Speed {
        /// Given.
        got: f32,
        /// [`SPEED_MIN`].
        min: f32,
        /// [`SPEED_MAX`].
        max: f32,
    },
    /// `pitch_semitones` out of range.
    #[error("pitch must be {min} to {max} semitones (got {got})")]
    #[non_exhaustive]
    Pitch {
        /// Given.
        got: f32,
        /// [`PITCH_MIN_SEMITONES`].
        min: f32,
        /// [`PITCH_MAX_SEMITONES`].
        max: f32,
    },
    /// The voice itself is refused.
    #[error(transparent)]
    Voice(#[from] VoiceError),
}

impl AssignError {
    /// [`AssignError::Speed`] for a speed of `got`.
    pub fn speed(got: f32) -> Self {
        Self::Speed { got, min: SPEED_MIN, max: SPEED_MAX }
    }

    /// [`AssignError::Pitch`] for a shift of `got` semitones.
    pub fn pitch(got: f32) -> Self {
        Self::Pitch { got, min: PITCH_MIN_SEMITONES, max: PITCH_MAX_SEMITONES }
    }
}

/// One voice a provider offers: a local model's `[[voices]]` row, a project's own `voices/<name>/voice.toml`, or a
/// hosted connector's list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct VoiceInfo {
    /// The voice.
    pub id: VoiceId,
    /// The languages it speaks well.
    pub languages: Vec<Locale>,
    /// The real person it sounds like, if any: it then needs that person's consent record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imitates: Option<String>,
    /// Cloned from a recording: it needs a consent record, the streamer's own included.
    #[serde(default)]
    pub cloned: bool,
}

impl VoiceInfo {
    /// A stock voice (no consent needed) speaking `languages`.
    pub fn new(id: VoiceId, languages: Vec<Locale>) -> Self {
        Self { id, languages, imitates: None, cloned: false }
    }

    /// The same voice, cloned from a recording of `person`.
    #[must_use]
    pub fn cloned_from(mut self, person: impl Into<String>) -> Self {
        self.imitates = Some(person.into());
        self.cloned = true;
        self
    }

    /// Whether using it needs a consent record (§3.5).
    pub fn needs_consent(&self) -> bool {
        self.cloned || self.imitates.is_some()
    }

    /// Whether `provider` can offer this voice: a hosted voice's source is its connector.
    pub fn offered_by(&self, provider: &VoiceProvider) -> bool {
        match provider {
            VoiceProvider::Local => true,
            VoiceProvider::Hosted(c) => self.id.source() == c.as_str(),
        }
    }
}

/// Answers "what voices are there" for a provider, over the model registry and the connectors (App-side).
pub trait VoiceCatalog: Send + Sync {
    /// The voices `provider` offers, each once.
    ///
    /// # Errors
    /// [`VoiceError::Provider`] when the list cannot be read (a connector offline).
    fn voices(&self, provider: &VoiceProvider) -> Result<Vec<VoiceInfo>, VoiceError>;
}

/// Where a feed's cut stands, as the mouth drive reads it (§4.2).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CutState {
    /// The feed is open.
    Live,
    /// A close was requested; the audio thread hasn't gone silent yet. Release nothing.
    Raised,
    /// The audio thread went silent at `at`: what played before it did play.
    #[non_exhaustive]
    Cut {
        /// Program time of the silence.
        at: Seconds,
    },
}

impl CutState {
    /// [`CutState::Cut`].
    pub fn cut(at: Seconds) -> Self {
        Self::Cut { at }
    }
}

/// What stands behind a [`CutProbe`]: read-only views of one feed's close flag and playhead. Until
/// `omuzaki-audio-api` gains `FeedCloser` and `FeedPlayhead` (§3.3), the speech host implements it over its own.
pub trait CutSource: Send + Sync {
    /// The feed's cut state now. Never blocks.
    fn state(&self) -> CutState;
}

/// The drive's own, pump-independent view of one feed (§4.2): it checks this immediately before each handoff, so no
/// stamp past a cut ever reaches the stage, whatever the pump does.
#[derive(Clone)]
pub struct CutProbe(Arc<dyn CutSource>);

impl CutProbe {
    /// A probe over `source`.
    pub fn new(source: Arc<dyn CutSource>) -> Self {
        Self(source)
    }

    /// The feed's cut state now.
    pub fn state(&self) -> CutState {
        self.0.state()
    }
}

impl fmt::Debug for CutProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CutProbe").field(&self.state()).finish()
    }
}

/// The TTS host's view of the mouth drive: PCM ahead of playback, stamped with its program time (§4.1). The pump
/// calls it every 10 ms; implementations copy what they need and return at once.
pub trait VoiceLead: Send {
    /// A new feed starts: the probe the drive checks before each release.
    fn attach(&mut self, probe: CutProbe);

    /// `pcm` (mono, `rate` Hz) will start playing at program time `at`. Private PCM moves nothing (§4.3).
    fn push(&mut self, at: Seconds, rate: u32, pcm: &[f32], private: bool);

    /// The pump saw a cut at `at`. Only tidies: the drive's release decisions come from the probe, never from this.
    fn cut(&mut self, at: Seconds);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kokoro() -> VoiceId {
        VoiceId::new("kokoro-82m/af_heart").unwrap()
    }

    fn mochi() -> CharId {
        CharId::new("mochi").unwrap()
    }

    #[test]
    fn voice_assignment_toml_round_trips() {
        let local = VoiceAssignment::new(mochi(), kokoro(), VoiceProvider::Local).tuned(1.1, -2.0);
        let text = toml::to_string(&local).unwrap();
        assert_eq!(
            text,
            "character = \"mochi\"\nvoice = \"kokoro-82m/af_heart\"\nprovider = \"local\"\nspeed = 1.1\npitch_semitones = -2.0\n"
        );
        assert_eq!(toml::from_str::<VoiceAssignment>(&text).unwrap(), local);
        let hosted = VoiceAssignment::new(
            mochi(),
            VoiceId::new("tts-cloud/Rachel").unwrap(),
            VoiceProvider::Hosted(ConnectorId::new("tts-cloud").unwrap()),
        )
        .with_consent(ConsentRef::new("The Streamer"));
        let text = toml::to_string(&hosted).unwrap();
        assert_eq!(toml::from_str::<VoiceAssignment>(&text).unwrap(), hosted, "{text}");
        assert!(toml::from_str::<VoiceAssignment>("character = \"mochi\"\nvoice = \"k/a\"\nprovider = \"local\"\nspeed = 1.0\npitch_semitones = 0.0\nfx = 1\n").is_err());
    }

    #[test]
    fn voice_info_toml_round_trips() {
        let info = VoiceInfo::new(kokoro(), vec![Locale::EN, Locale::JA]).cloned_from("The Streamer");
        let text = toml::to_string(&info).unwrap();
        assert_eq!(toml::from_str::<VoiceInfo>(&text).unwrap(), info, "{text}");
    }

    #[test]
    fn assignments_are_checked_against_the_voice() {
        let stock = VoiceInfo::new(kokoro(), vec![Locale::EN]);
        let ok = VoiceAssignment::new(mochi(), kokoro(), VoiceProvider::Local);
        assert_eq!(ok.check(&stock), Ok(()));
        assert!(matches!(ok.clone().tuned(1.5, 0.0).check(&stock), Err(AssignError::Speed { .. })));
        assert!(matches!(ok.clone().tuned(1.0, 4.5).check(&stock), Err(AssignError::Pitch { .. })));
        let cloned = stock.clone().cloned_from("The Streamer");
        assert_eq!(ok.check(&cloned), Err(AssignError::Voice(VoiceError::no_consent(&kokoro()))));
        assert_eq!(ok.clone().with_consent(ConsentRef::new("The Streamer")).check(&cloned), Ok(()));
        let hosted =
            VoiceAssignment::new(mochi(), kokoro(), VoiceProvider::Hosted(ConnectorId::new("tts-cloud").unwrap()));
        assert_eq!(hosted.check(&stock), Err(AssignError::Voice(VoiceError::unknown_voice(&kokoro()))));
        assert!(ok.check(&VoiceInfo::new(VoiceId::new("kokoro-82m/bf_emma").unwrap(), vec![])).is_err());
    }

    #[test]
    fn pitch_is_the_declared_rate() {
        let a = VoiceAssignment::new(mochi(), kokoro(), VoiceProvider::Local);
        assert_eq!(a.feed_rate(), 24_000);
        assert_eq!(a.clone().tuned(1.0, 4.0).feed_rate(), 30_238);
        assert_eq!(a.tuned(1.0, -4.0).feed_rate(), 19_049);
    }

    struct Flag(CutState);

    impl CutSource for Flag {
        fn state(&self) -> CutState {
            self.0
        }
    }

    #[test]
    fn a_probe_reads_its_source() {
        let probe = CutProbe::new(Arc::new(Flag(CutState::cut(Seconds(0.9)))));
        assert_eq!(probe.clone().state(), CutState::cut(Seconds(0.9)));
        assert!(format!("{probe:?}").contains("Cut"));
    }
}
