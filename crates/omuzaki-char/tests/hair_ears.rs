//! Pointed ears through long hair (#2449): with elf ears and long hair on the kit's tall character, no hair lies in
//! front of the ear's tip by default, while `ears: cover` keeps the hair over it as styled; human ears are left alone.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

mod common;

use omuzaki_char_api::BuiltCharacter;
use omuzaki_core::glam::Vec3;
use omuzaki_doc_mock::MemDoc;
use serde_json::json;

/// The tall kit character with long hair falling in front of the shoulders and ears of `kind`, the hair's `ears` set to
/// `ears` when given.
fn styled(kind: &str, ears: Option<&str>) -> MemDoc {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "tall"}));
    let set = |doc: &mut MemDoc, part: &str, path: &str, value: serde_json::Value| {
        common::apply(doc, "part.set_param", json!({"character": "kit", "part": part, "path": path, "value": value}));
    };
    set(&mut doc, "head", "ears/kind", json!(kind));
    set(&mut doc, "hair", "groups/c-curtain/generator/length_front", json!(0.55));
    set(&mut doc, "hair", "groups/c-curtain/generator/in_front", json!(true));
    if let Some(ears) = ears {
        set(&mut doc, "hair", "ears", json!(ears));
    }
    doc
}

/// The highest point of the left ear's mesh: its tip.
fn ear_tip(built: &BuiltCharacter) -> Vec3 {
    let ear = built.meshes.iter().find(|m| m.mesh.name == "head-ear-l").expect("a left ear");
    ear.mesh.geometry.positions.iter().copied().max_by(|a, b| a.y.total_cmp(&b.y)).unwrap()
}

/// How many hair vertices lie within 15 mm of `tip` and in front of it (toward the face).
fn hair_over(built: &BuiltCharacter, tip: Vec3) -> usize {
    built
        .meshes
        .iter()
        .filter(|m| m.part.as_str() == "hair")
        .flat_map(|m| m.mesh.geometry.positions.iter())
        .filter(|p| p.distance(tip) < 0.015 && p.z > tip.z)
        .count()
}

#[test]
fn a_pointed_ear_shows_through_long_hair_unless_the_hair_covers_it() {
    let behind = common::build(&styled("elf", None), "kit");
    let tip = ear_tip(&behind);
    assert_eq!(hair_over(&behind, tip), 0, "no lock lies over the ear's tip");

    let covered = common::build(&styled("elf", Some("cover")), "kit");
    assert!(hair_over(&covered, ear_tip(&covered)) > 0, "covered as styled: the test sees hair where it is");
}

#[test]
fn human_ears_are_covered_as_before() {
    let a = common::build(&styled("human", None), "kit");
    let b = common::build(&styled("human", Some("cover")), "kit");
    let positions = |c: &BuiltCharacter| -> Vec<Vec3> {
        c.meshes.iter().filter(|m| m.part.as_str() == "hair").flat_map(|m| m.mesh.geometry.positions.clone()).collect()
    };
    assert_eq!(positions(&a), positions(&b), "the choice changes nothing for human ears");
}
