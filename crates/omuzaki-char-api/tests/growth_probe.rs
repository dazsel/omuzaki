//! The acceptance test for ADR 0054 §4: a new field on a contract struct edits only this crate.
//!
//! With the `growth-probe` feature, each of `BuildCtx`, `PartOutput`, `BuiltCharacter` and `CharacterDef` gains one
//! defaulted field. Every other crate in the workspace builds these types through their constructors and never writes
//! a literal or an exhaustive pattern, so
//!
//! ```sh
//! cargo check --workspace --all-targets --features omuzaki-char-api/growth-probe
//! ```
//!
//! compiling is the proof, and this file checks the field behaves like the others (defaults, survives `fork`, is not saved). It is empty without the feature.
#![cfg(feature = "growth-probe")]

use omuzaki_char_api::{
    BodyTemplateId, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, PartId,
    PartOutput, Published, Quality, RegistryBuilder,
};
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{FaceProfile, Palette};

use omuzaki_char_api::kind::BuildCtx;

mod common;

fn def() -> CharacterDef {
    CharacterDef::new("Probe", BodyTemplateId::new("soft-sd").unwrap())
}

#[test]
fn the_probe_field_defaults_on_every_type_the_constructors_build() {
    let (part, palette, face) = (PartId::new("probe").unwrap(), Palette::default(), FaceProfile::default());
    let (published, services) = (Published::default(), BuildServices::default());
    let template = BodyTemplateId::new("soft-sd").unwrap();
    let ctx = BuildCtx::new(&part, &template, &palette, &face, Rng::new(0), &published, Quality::Live, &services);
    assert_eq!(ctx.growth_probe, 0);
    assert_eq!(PartOutput::new().growth_probe, 0);
    assert_eq!(def().growth_probe, 0);
    let id = CharId::new("probe").unwrap();
    let with_body = def().with_parts([(PartId::new("body").unwrap(), common::body_part())].into());
    let built = RegistryBuilder.build(&id, &with_body, &BuildOptions::default(), &services).unwrap();
    assert_eq!(built.growth_probe, 0, "the builder's constructor path");
    assert_eq!(BuiltCharacter::new(id, def(), built.rig.clone()).growth_probe, 0);
}

#[test]
fn the_probe_field_survives_a_fork() {
    let mut out = PartOutput::new();
    out.growth_probe = 7;
    assert_eq!(out.fork().expect("no simulation, so it forks").growth_probe, 7);
}

#[test]
fn the_probe_field_is_not_saved() {
    let saved = serde_json::to_value(def()).unwrap();
    assert!(saved.get("growth_probe").is_none(), "saved projects are unchanged");
}
