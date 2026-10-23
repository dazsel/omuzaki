//! Voices (`ai-performer-voice-face.md` §3.4, §3.5): what voices exist, which one a character speaks in, and the
//! consent a cloned or imitating voice needs before it may speak.
//!
//! - [`catalog`]: [`LocalVoices`], every installed or installable `tts@1` model's `[[voices]]`, as `<model>/<voice>`
//!   ids; [`Catalog`] adds the hosted connectors.
//! - [`admit`]: the one check `voice.assign`, the performer's start and the TTS session's open all run: the voice is in
//!   the catalog, speed and pitch are in range, and a cloned or imitating voice names a consent record the
//!   [`ConsentLookup`] still finds. `NoConsentRecords` (the registry's default) keeps every such voice unusable.
//! - [`figures`]: the shipped public-figure list. A match only warns, never blocks.
//! - [`preview`]: one sentence to `monitor_out`, never `program`.
//! - [`ops`]: `voice.get`, `voice.assign`, `voice.list`, `voice.preview`.
//!
//! The assignment is its own file, `characters/<id>/voice-assignment.toml`, next to the effect presets of `voice.toml`:
//! each op reads and writes only its own file.

pub mod catalog;
pub mod figures;
pub mod ops;
pub mod preview;

use std::sync::Arc;

use omuzaki_ai_api::{ConsentLookup, Likeness, ModelRegistry, ModelStatus, Session};
use omuzaki_audio_api::AudioEngine;
use omuzaki_char_api::ids::CharId;
use omuzaki_char_api::paths;
use omuzaki_doc_api::{DocPath, DocRead};
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::speech::VoiceError;
use omuzaki_performer_api::voice::{AssignError, VoiceAssignment, VoiceCatalog, VoiceInfo, VoiceLead};

pub use catalog::{Catalog, LocalVoices};
pub use figures::{Figure, PublicFigures};
pub use preview::{PREVIEW_FEED, Previewing, TtsPreview, VoicePreview};

use crate::slots::tts_slot;
use crate::tts::{TtsHost, TtsVoice};

/// The character's key for its assignment: `characters/<id>/voice-assignment`, saved as `voice-assignment.toml`.
pub const ASSIGNMENT_KEY: &str = "voice-assignment";

/// Where `character`'s assignment lives in the document.
pub fn assignment_path(character: &CharId) -> DocPath {
    paths::character(character).join(ASSIGNMENT_KEY)
}

/// `character`'s assignment as stored, `None` when it has none.
///
/// # Errors
/// The parse error, in words, when the stored value isn't an assignment.
pub fn read_assignment(doc: &dyn DocRead, character: &CharId) -> Result<Option<VoiceAssignment>, String> {
    doc.get(&assignment_path(character))
        .map(|value| serde_json::from_value(value.clone()).map_err(|e| e.to_string()))
        .transpose()
}

/// Admits `assignment` (§3.4, §3.5): its voice is one `catalog` lists for its provider, its speed and pitch are in
/// range, and a voice that is cloned or imitates someone names that person's consent record, which `consent` still
/// finds. There is no fallback to another voice: a different voice is a different character.
///
/// # Errors
/// [`AssignError`] naming the first rule broken; [`VoiceError::NoConsent`] inside it when the record is missing or
/// was revoked.
pub fn admit(
    assignment: &VoiceAssignment,
    catalog: &dyn VoiceCatalog,
    consent: &dyn ConsentLookup,
) -> Result<VoiceInfo, AssignError> {
    let voices = catalog.voices(&assignment.provider)?;
    let info = voices
        .into_iter()
        .find(|v| v.id == assignment.voice)
        .ok_or_else(|| AssignError::Voice(VoiceError::unknown_voice(&assignment.voice)))?;
    assignment.check(&info)?;
    if let Some(person) = info.imitates.as_deref().filter(|_| info.needs_consent())
        && consent.find(person, Likeness::VoiceClone).is_none()
    {
        return Err(AssignError::Voice(VoiceError::no_consent(&assignment.voice)));
    }
    Ok(info)
}

/// A `tts@1` session on the model `voice` belongs to: the installed version the slot runs if it is that model,
/// otherwise its newest ready version.
///
/// # Errors
/// [`VoiceError::UnknownVoice`] when no `tts@1` model has that id; [`VoiceError::Provider`] with the registry's
/// words when it isn't installed or won't open.
pub fn open_session(registry: &dyn ModelRegistry, voice: &VoiceId) -> Result<Box<dyn Session>, VoiceError> {
    let slot = tts_slot();
    let mut models: Vec<_> =
        registry.models(Some(&slot)).into_iter().filter(|m| m.model.id.as_str() == voice.source()).collect();
    if models.is_empty() {
        return Err(VoiceError::unknown_voice(voice));
    }
    models.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| b.model.version.cmp(&a.model.version)));
    let Some(model) = models.iter().find(|m| m.status == ModelStatus::Ready) else {
        let status = &models[0].status;
        return Err(VoiceError::Provider(format!(
            "{} isn't ready ({status:?}): install it from the Models page",
            voice.source()
        )));
    };
    registry.open(&slot, Some(&model.model)).map_err(|e| VoiceError::Provider(e.to_string()))
}

/// The performer's voice for `assignment`, admitted again first (§3.5: a revoked record stops the next show), on a
/// session of its model, writing `engine`'s `ai_voice` feeds.
///
/// # Errors
/// [`AssignError`] when the assignment isn't admitted or the host can't open.
pub fn open_host(
    registry: &dyn ModelRegistry,
    engine: Arc<dyn AudioEngine>,
    assignment: &VoiceAssignment,
    catalog: &dyn VoiceCatalog,
    consent: &dyn ConsentLookup,
    lead: Option<Box<dyn VoiceLead>>,
) -> Result<TtsHost, AssignError> {
    admit(assignment, catalog, consent)?;
    let session = open_session(registry, &assignment.voice)?;
    Ok(TtsHost::open(engine, session, TtsVoice::of(assignment), lead)?)
}
