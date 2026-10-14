//! Consent for models that reproduce a real person (SE-05): which likeness a slot's models reproduce, and the lookup
//! the registry asks before such a model may run.
//!
//! The records live with the licence records in the project document and the app implements
//! [`ConsentLookup`] over them; the registry is the consumer, so the trait lives here. A model whose card says
//! [`imitates`](crate::ModelCard::imitates) stays [`NeedsConsent`](crate::ModelStatus::NeedsConsent) until the lookup
//! finds a record, and what it makes carries that record.

use omuzaki_store_api::Consent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What of a real person a model reproduces, which is what their consent must cover. A slot declares it in
/// [`SlotInfo::likeness`](crate::SlotInfo::likeness).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Likeness {
    /// Speaking new words in their voice (text to speech with a cloned voice).
    VoiceClone,
    /// Turning a live voice into theirs (ML-14).
    VoiceConversion,
    /// Their face or body.
    Face,
}

impl Likeness {
    /// The scope written in a [`Consent`] record for this use; what a lookup matches on.
    pub fn scope(self) -> &'static str {
        match self {
            Likeness::VoiceClone => "voice_clone",
            Likeness::VoiceConversion => "voice_conversion",
            Likeness::Face => "face",
        }
    }
}

/// Finds the consent a real person gave for a use of their likeness. Implemented by the app over the project's licence
/// records; cold (it may read the document).
pub trait ConsentLookup: Send + Sync {
    /// The record of `person`'s consent to `likeness`, if one exists.
    fn find(&self, person: &str, likeness: Likeness) -> Option<Consent>;
}

/// A lookup with no records: until consent records exist, every model that imitates someone stays unusable, which is
/// the safe default.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoConsentRecords;

impl ConsentLookup for NoConsentRecords {
    fn find(&self, _: &str, _: Likeness) -> Option<Consent> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_records_nothing_is_consented_and_scopes_are_stable() {
        assert_eq!(NoConsentRecords.find("Anyone", Likeness::VoiceConversion), None);
        // Scopes are stored in licence records, so they never change once written.
        let scopes = [Likeness::VoiceClone, Likeness::VoiceConversion, Likeness::Face].map(Likeness::scope);
        assert_eq!(scopes, ["voice_clone", "voice_conversion", "face"]);
    }
}
