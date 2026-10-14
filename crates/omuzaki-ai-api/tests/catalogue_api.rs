//! The catalogue additions to the registry contract: they are additive (existing JSON reads as before), the third
//! origin follows the shipped rules, and a registry that takes no catalogue refuses one without changing anything.

#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

use omuzaki_ai_api::{
    CatalogueEntry, ImportError, ManifestError, ModelEntry, ModelManifest, ModelRegistry, ModelStatus, Origin,
    RegistryError, Requester, Sha256, SyncReport,
};

const MANIFEST: &str = r#"
id = "silero-vad"
version = "1.1.0"
name = "Silero VAD"
slot = "vad@1"
runtime = "onnx"
providers = ["cpu"]

[[files]]
name = "silero_vad.onnx"
sha256 = "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"
size = 2327524
urls = ["https://models.invalid/silero-vad/1.1.0/silero_vad.onnx"]

[card]
register = "Silero VAD"

[card.licence]
spdx = "MIT"
"#;

fn manifest() -> ModelManifest {
    ModelManifest::from_toml(MANIFEST).unwrap()
}

/// JSON the contract produced before the catalogue existed: it must read exactly as it did.
#[test]
fn existing_json_reads_unchanged() {
    let json = |s| serde_json::from_str::<ModelStatus>(s).unwrap();
    assert_eq!(json(r#"{"state":"ready"}"#), ModelStatus::Ready);
    assert_eq!(json(r#"{"state":"not_installed"}"#), ModelStatus::NotInstalled);
    assert_eq!(json(r#"{"state":"disabled","reason":"x"}"#), ModelStatus::Disabled { reason: "x".into() });
    assert_eq!(serde_json::from_str::<Origin>(r#""shipped""#).unwrap(), Origin::Shipped);
    assert_eq!(serde_json::from_str::<Origin>(r#""user""#).unwrap(), Origin::User);

    let entry = r#"{"model":{"id":"silero-vad","version":"1.1.0","digest":"1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"},
        "name":"Silero VAD","slot":"vad@1","runtime":"onnx","origin":"shipped","licence":{"spdx":"MIT"},
        "size":1,"status":{"state":"ready"},"active":true,"eval":null}"#;
    let entry: ModelEntry = serde_json::from_str(entry).unwrap();
    assert_eq!((entry.origin, entry.status), (Origin::Shipped, ModelStatus::Ready));
}

#[test]
fn the_new_variants_have_stable_json_names() {
    assert_eq!(serde_json::to_string(&Origin::Catalogue).unwrap(), r#""catalogue""#);
    assert_eq!(serde_json::to_string(&ModelStatus::Revoked).unwrap(), r#"{"state":"revoked"}"#);
}

#[test]
fn a_catalogue_manifest_follows_the_shipped_rules_and_not_the_user_ones() {
    let m = manifest();
    assert_eq!(m.validate(Origin::Catalogue), Ok(()));

    let mut plain = m.clone();
    plain.files[0].urls = vec!["http://insecure.invalid/x".into()];
    plain.providers = vec![omuzaki_ai_api::Provider::Cuda];
    plain.card.licence.spdx = None;
    let errors = plain.validate(Origin::Catalogue).unwrap_err();
    assert_eq!(errors, plain.validate(Origin::Shipped).unwrap_err(), "the same problems, in the same order");

    let mut user = m;
    user.id = "user-voice".parse().unwrap();
    assert_eq!(user.validate(Origin::Catalogue), Err(vec![ManifestError::ReservedId("user-voice".into())]));
    assert_eq!(user.validate(Origin::Shipped), Ok(()), "shipped checks are unchanged");
}

/// A registry written before the catalogue: it implements none of the new method.
struct Old;

impl ModelRegistry for Old {
    fn slots(&self) -> Vec<omuzaki_ai_api::SlotStatus> {
        vec![]
    }
    fn models(&self, _: Option<&omuzaki_ai_api::SlotRef>) -> Vec<ModelEntry> {
        vec![]
    }
    fn open(
        &self,
        _: &omuzaki_ai_api::SlotRef,
        _: Option<&omuzaki_ai_api::ModelRef>,
    ) -> Result<Box<dyn omuzaki_ai_api::Session>, RegistryError> {
        Err(RegistryError::Cancelled)
    }
    fn open_stream(
        &self,
        _: &omuzaki_ai_api::SlotRef,
        _: Option<&omuzaki_ai_api::ModelRef>,
    ) -> Result<omuzaki_ai_api::Stream, RegistryError> {
        Err(RegistryError::Cancelled)
    }
    fn install(&self, _: &omuzaki_ai_api::ModelRef, _: &dyn omuzaki_ai_api::Progress) -> Result<(), RegistryError> {
        Ok(())
    }
    fn rollback(&self, s: &omuzaki_ai_api::SlotRef) -> Result<omuzaki_ai_api::ModelRef, RegistryError> {
        Err(RegistryError::NoRollback(s.clone()))
    }
    fn add(&self, _: omuzaki_ai_api::AddModel, _: &dyn omuzaki_ai_api::Progress) -> Result<ModelEntry, RegistryError> {
        Err(RegistryError::Cancelled)
    }
    fn acknowledge(&self, _: &omuzaki_ai_api::ModelRef, _: &str) -> Result<(), RegistryError> {
        Ok(())
    }
    fn select(
        &self,
        _: &omuzaki_ai_api::SlotRef,
        _: Option<&omuzaki_ai_api::ModelRef>,
        _: Requester,
    ) -> Result<(), RegistryError> {
        Ok(())
    }
    fn remove(&self, _: &omuzaki_ai_api::ModelRef) -> Result<(), RegistryError> {
        Ok(())
    }
}

#[test]
fn a_registry_without_catalogue_support_refuses_by_default() {
    let entries = [CatalogueEntry { manifest: manifest(), manifest_sha256: Sha256([7; 32]) }];
    let err = Old.sync_catalogue(1, 1, &entries).unwrap_err();
    assert!(matches!(err, ImportError::Io(_)), "{err:?}");
    assert_eq!(SyncReport::default().imported.len(), 0);
}
