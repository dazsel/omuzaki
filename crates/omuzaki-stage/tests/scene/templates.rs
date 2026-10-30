//! The four template characters (soft-sd, mini, standard, tall), built with the real body and head kinds, for the
//! scene tests that frame them. Shared through a `#[path]` module attribute.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::collections::BTreeMap;

use omuzaki_char::template::registry;
use omuzaki_char_api::{
    BodyTemplateId, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, PartDef,
    PartId, PartKindId, RegistryBuilder,
};
use omuzaki_core::colour::Colour;
use omuzaki_face_api::{Palette, PaletteSlot};

/// The templates framing is tested on.
pub const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

/// The skin slots the template's body and head parameters name (`@skin`, ...), as `character.create` starts them.
fn skin_palette() -> Palette {
    let slots = [
        ("skin", Colour::rgb(0xf9, 0xd3, 0xc5)),
        ("skin_shade", Colour::rgb(0xf2, 0xb4, 0xab)),
        ("skin_line", Colour::rgb(0xc9, 0x8a, 0x80)),
    ];
    Palette(slots.into_iter().map(|(slot, colour)| (PaletteSlot::new(slot).unwrap(), colour)).collect())
}

fn part(kind: &str, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// A character of `template` made of the parts of `kinds` (each part is named after its kind).
pub fn build_parts(template: &str, kinds: &[&str]) -> BuiltCharacter {
    build_dressed(template, kinds, &[])
}

/// As [`build_parts`], with `extra` parts (kind and parameters, each named after its kind) on top: garments.
pub fn build_dressed(template: &str, kinds: &[&str], extra: &[(&str, serde_json::Value)]) -> BuiltCharacter {
    omuzaki_stage::link();
    let defaults = registry::find(template).expect("a registered template").defaults();
    let params = |kind: &str| match kind {
        "body" => serde_json::to_value(&defaults.body).unwrap(),
        "head" => serde_json::to_value(&defaults.head).unwrap(),
        other => panic!("no starting parameters for '{other}'"),
    };
    let parts: BTreeMap<PartId, PartDef> = kinds
        .iter()
        .map(|kind| (PartId::new(*kind).unwrap(), part(kind, params(kind))))
        .chain(extra.iter().map(|(kind, params)| (PartId::new(*kind).unwrap(), part(kind, params.clone()))))
        .collect();
    let def = CharacterDef::new("Test".to_owned(), BodyTemplateId::new(template).unwrap())
        .with_seed(1)
        .with_palette(skin_palette())
        .with_skin_tone(None)
        .with_parts(parts)
        .with_face(defaults.face)
        .with_weight_brushes(BTreeMap::new());
    RegistryBuilder
        .build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the template character builds")
}

/// The template with a body and a head.
pub fn build(template: &str) -> BuiltCharacter {
    build_parts(template, &["body", "head"])
}
