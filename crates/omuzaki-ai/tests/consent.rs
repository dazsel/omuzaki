//! The consent hook (SE-05): a model whose card says it imitates a real person stays `NeedsConsent` until the app's
//! lookup finds that person's record, and can't be selected meanwhile. Against the mock's slots: `test_stream@1`
//! declares a voice-conversion likeness, `test_echo@1` none.
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

mod common;

use std::sync::Arc;

use common::rig::{PORTS_X_Y, Rig, add, echo, manifest, reason, stream};
use omuzaki_ai::{IMPERSONATION_WARNING, Registry};
use omuzaki_ai_api::{
    ConsentLookup, Likeness, ModelEntry, ModelRegistry, ModelStatus, RegistryError, Requester, SlotRef,
};
use omuzaki_store_api::Consent;

/// The stream slot's manifests need their `[stream]` table, which comes before the card.
const STREAM: &str = "stream = { sample_rate = 16000, block = 160 }";

/// A lookup that holds one person's consent to one use.
struct Holds(&'static str, Likeness);

impl ConsentLookup for Holds {
    fn find(&self, person: &str, likeness: Likeness) -> Option<Consent> {
        (person == self.0 && likeness == self.1).then(|| Consent {
            person: person.to_owned(),
            scope: likeness.scope().to_owned(),
            document: None,
        })
    }
}

/// Adds an acknowledged identity model to `slot`, imitating `person` if given.
fn add_voice(rig: &Rig, registry: &Registry, slot: &SlotRef, person: Option<&str>) -> ModelEntry {
    let file = rig.fake("voice.fake.toml", "1.0", PORTS_X_Y);
    let mut given = manifest(slot, &[&file], if *slot == stream() { STREAM } else { "" });
    given.card.imitates = person.map(str::to_owned);
    add(registry, slot, vec![file], Some(given), Some("Pat"))
}

#[test]
fn an_imitating_model_that_passes_everything_else_needs_consent_and_cannot_be_selected() {
    let rig = Rig::new();
    let registry = rig.registry();
    let entry = add_voice(&rig, &registry, &stream(), Some("Ada Example"));
    assert_eq!(entry.status, ModelStatus::NeedsConsent, "{entry:?}");
    for by in [Requester::Person, Requester::Agent] {
        let err = registry.select(&stream(), Some(&entry.model), by).unwrap_err();
        let RegistryError::NotUsable { reason, .. } = err else { panic!("{by:?}: {err:?}") };
        assert!(reason.contains("Ada Example") && reason.contains("consent"), "{by:?}: {reason}");
    }
}

#[test]
fn with_the_record_on_file_it_is_ready_and_only_that_person_and_use_count() {
    let rig = Rig::new();
    let registry = rig.registry();
    let entry = add_voice(&rig, &registry, &stream(), Some("Ada Example"));
    registry.set_consent(Arc::new(Holds("Someone Else", Likeness::VoiceConversion)));
    assert_eq!(registry.models(None)[0].status, ModelStatus::NeedsConsent, "another person's record");
    registry.set_consent(Arc::new(Holds("Ada Example", Likeness::VoiceClone)));
    assert_eq!(registry.models(None)[0].status, ModelStatus::NeedsConsent, "consent to another use");
    registry.set_consent(Arc::new(Holds("Ada Example", Likeness::VoiceConversion)));
    assert_eq!(registry.models(None)[0].status, ModelStatus::Ready);
    registry.select(&stream(), Some(&entry.model), Requester::Agent).unwrap();
}

#[test]
fn acknowledgement_comes_before_consent() {
    let rig = Rig::new();
    let registry = rig.registry();
    let file = rig.fake("voice.fake.toml", "1.0", PORTS_X_Y);
    let mut given = manifest(&stream(), &[&file], STREAM);
    given.card.imitates = Some("Ada Example".into());
    let entry = add(&registry, &stream(), vec![file], Some(given), None);
    assert_eq!(entry.status, ModelStatus::NeedsAcknowledgement);
    registry.acknowledge(&entry.model, "Pat").unwrap();
    assert_eq!(registry.models(None)[0].status, ModelStatus::NeedsConsent);
}

#[test]
fn a_slot_whose_models_cannot_imitate_anyone_disables_a_model_that_claims_to() {
    let rig = Rig::new();
    let registry = rig.registry();
    let entry = add_voice(&rig, &registry, &echo(), Some("Ada Example"));
    assert!(reason(&entry).contains("test_echo@1 models can't imitate a person"), "{entry:?}");
}

#[test]
fn a_model_that_imitates_no_one_is_unaffected() {
    let rig = Rig::new();
    let registry = rig.registry();
    assert_eq!(add_voice(&rig, &registry, &stream(), None).status, ModelStatus::Ready);
    assert_eq!(add_voice(&rig, &registry, &echo(), None).status, ModelStatus::Ready);
}

#[test]
fn the_warning_is_the_se05_text() {
    assert!(IMPERSONATION_WARNING.starts_with("This voice imitates a real person."));
    assert!(IMPERSONATION_WARNING.contains("recorded consent") && IMPERSONATION_WARNING.ends_with("(SE-05)."));
}
