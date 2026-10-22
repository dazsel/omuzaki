//! The consent hook (SE-05, ADR 0017 decision 9, design `docs/design/ai-api.md` section 5): a model whose card says it
//! [`imitates`](omuzaki_ai_api::ModelCard::imitates) a real person stays
//! [`NeedsConsent`](omuzaki_ai_api::ModelStatus::NeedsConsent) until the app's [`ConsentLookup`] finds that person's
//! record for the slot's [`Likeness`], and can't be selected or opened meanwhile. Until the app wires a lookup
//! (`Registry::set_consent`) the registry asks `NoConsentRecords`, so every imitating model stays unusable: the safe
//! default.
//!
//! Adding a voice model doesn't ask whether it imitates someone: [`AddModel`](omuzaki_ai_api::AddModel) has no field
//! for it. The UI asks the person and writes `card.imitates` into the manifest it passes.

use omuzaki_ai_api::{ConsentLookup, Likeness, ModelManifest, find_slot};

/// The warning shown wherever a model that imitates a real person is listed (SE-05). One text for every surface.
pub const IMPERSONATION_WARNING: &str = "This voice imitates a real person. Use it only with their recorded consent, \
and never to deceive anyone about who is speaking (SE-05).";

/// What a model's `imitates` asks of the registry, once [`refusal`] has found nothing wrong with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Imitation {
    /// It imitates no one, or the person's consent is on record.
    Fine,
    /// It imitates this person, whose consent to the slot's likeness isn't on record.
    NeedsConsent { person: String, likeness: Likeness },
}

impl Imitation {
    /// What `select` and `open` say about a model that waits for `person`'s consent: who, and what is needed.
    pub(super) fn needs_consent_reason(person: &str, likeness: Likeness) -> String {
        format!(
            "it imitates {person}, and there is no record of {person}'s consent to {} (SE-05); add their consent \
             record to the project first",
            describe(likeness)
        )
    }
}

/// The use a [`Likeness`] stands for, in words.
fn describe(likeness: Likeness) -> &'static str {
    match likeness {
        Likeness::VoiceClone => "speaking in their voice",
        Likeness::VoiceConversion => "turning a voice into theirs",
        Likeness::Face => "using their face or body",
        _ => "this use of their likeness",
    }
}

/// Why a model can never be used for imitating someone: its card says it imitates a person, and its slot's models
/// can't (`SlotInfo::likeness` is `None`). `None` otherwise; a slot that isn't registered is judged elsewhere.
pub(super) fn refusal(manifest: &ModelManifest) -> Option<String> {
    let person = manifest.card.imitates.as_deref()?;
    let slot = find_slot(&manifest.slot)?;
    slot.info()
        .likeness
        .is_none()
        .then(|| format!("its card says it imitates {person}, but {} models can't imitate a person", manifest.slot))
}

/// Asks `lookup` for the consent an imitating model needs. A model [`refusal`] refuses is `Fine` here: it is
/// disabled, which comes first.
pub(super) fn judge(manifest: &ModelManifest, lookup: &dyn ConsentLookup) -> Imitation {
    let Some(person) = manifest.card.imitates.as_deref() else { return Imitation::Fine };
    let Some(likeness) = find_slot(&manifest.slot).and_then(|s| s.info().likeness) else { return Imitation::Fine };
    match lookup.find(person, likeness) {
        Some(_) => Imitation::Fine,
        None => Imitation::NeedsConsent { person: person.to_owned(), likeness },
    }
}
