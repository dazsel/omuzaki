//! The constructors of the `#[non_exhaustive]` contract structs (ADR 0054 §4) start from the same defaults a saved
//! file or `Default` gives, and the `with_*` methods set exactly the field they name.

use omuzaki_char_api::{BodyTemplateId, BuildServices, CharacterDef, PartOutput, Published, Quality};
use omuzaki_char_api::{PartId, kind::BuildCtx};
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{FaceProfile, Palette};

#[test]
fn a_new_definition_equals_the_one_a_minimal_file_reads_as() {
    let template = BodyTemplateId::new("soft-sd").unwrap();
    let read: CharacterDef = serde_json::from_value(serde_json::json!({ "name": "A", "template": "soft-sd" })).unwrap();
    assert_eq!(CharacterDef::new("A", template.clone()), read);
    assert_eq!(CharacterDef::new("A", template).with_seed(9).seed, 9);
}

#[test]
fn a_new_output_is_empty() {
    let out = PartOutput::new();
    assert!(out.meshes.is_empty() && out.materials.is_empty() && out.textures.is_empty() && out.warnings.is_empty());
    assert!(out.runtime.is_none());
    assert_eq!(PartOutput::new().with_warnings(vec!["w".to_owned()]).warnings, ["w"]);
}

#[test]
fn a_new_context_is_not_for_export_and_has_no_attachment() {
    let (part, palette, face) = (PartId::new("p").unwrap(), Palette::default(), FaceProfile::default());
    let (published, services) = (Published::default(), BuildServices::default());
    let template = BodyTemplateId::new("soft-sd").unwrap();
    let new = || BuildCtx::new(&part, &template, &palette, &face, Rng::new(1), &published, Quality::Live, &services);
    assert!(!new().rig_export && new().attach.is_none());
    assert!(new().with_rig_export(true).rig_export);
}
