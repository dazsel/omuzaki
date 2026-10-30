//! `part.set_attach` (#2590) on the kit character: pinning an accessory writes the place its kind gave it as an explicit
//! attachment and leaves every vertex where it was; an offset then moves it by exactly that much; clearing gives the
//! kind its own placement back; and a part with no single bone to follow is refused with the way to attach it.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

mod common;

use omuzaki_char_api::BuiltCharacter;
use omuzaki_core::glam::Vec3;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::{Value, json};

fn kit(parts: &[(&str, &str)]) -> MemDoc {
    let mut doc = MemDoc::default();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "standard"}));
    for (kind, id) in parts {
        common::round_trip(&mut doc, "part.add", json!({"character": "kit", "kind": kind, "id": id}));
    }
    doc
}

fn points(built: &BuiltCharacter, part: &str) -> Vec<Vec3> {
    let points: Vec<Vec3> = built
        .meshes
        .iter()
        .filter(|m| m.part.as_str() == part)
        .flat_map(|m| m.mesh.geometry.positions.iter().copied())
        .collect();
    assert!(!points.is_empty(), "{part} has no mesh");
    points
}

fn drift(a: &[Vec3], b: &[Vec3]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(p, q)| p.distance(*q)).fold(0.0, f32::max)
}

#[test]
fn pinning_keeps_an_accessory_in_place_and_an_offset_then_moves_it() {
    for (kind, id) in [("hairpin", "pin"), ("hat", "hat"), ("glasses", "glasses")] {
        let mut doc = kit(&[(kind, id)]);
        let before = points(&common::build(&doc, "kit"), id);
        let pinned =
            common::round_trip(&mut doc, "part.set_attach", json!({"character": "kit", "part": id, "pin": true}));
        let attach = pinned.output.unwrap()["attach"].clone();
        assert_eq!(attach["to"], "bone", "{kind}: {attach}");
        let after = points(&common::build(&doc, "kit"), id);
        assert!(drift(&before, &after) < 1e-4, "{kind}: pinning moved it by {} m", drift(&before, &after));

        // Five centimetres up the bone's frame: every point moves by about that much.
        let mut moved: Value = attach.clone();
        let t = &mut moved["offset"]["translation"];
        t[1] = json!(t[1].as_f64().unwrap() + 0.05);
        common::round_trip(&mut doc, "part.set_attach", json!({"character": "kit", "part": id, "attach": moved}));
        let shifted = points(&common::build(&doc, "kit"), id);
        let mean = shifted.iter().zip(&after).map(|(p, q)| p.distance(*q)).sum::<f32>() / after.len() as f32;
        assert!((mean - 0.05).abs() < 0.01, "{kind}: moved by {mean} m on average");

        common::round_trip(&mut doc, "part.set_attach", json!({"character": "kit", "part": id, "clear": true}));
        assert!(common::at(&doc, &format!("characters/kit/parts/{id}/attach")).is_none());
        assert!(
            drift(&before, &points(&common::build(&doc, "kit"), id)) < 1e-4,
            "{kind}: clear gives the kind's place"
        );
    }
}

#[test]
fn one_way_at_a_time_and_parts_that_follow_no_bone_are_refused() {
    let doc = kit(&[]);
    let both = common::refused(
        &doc,
        "part.set_attach",
        json!({"character": "kit", "part": "hair", "pin": true, "clear": true}),
    );
    assert_eq!(both.code, ErrorCode::Invalid);
    assert!(both.message.contains("exactly one"), "{}", both.message);
    let hair = common::refused(&doc, "part.set_attach", json!({"character": "kit", "part": "hair", "pin": true}));
    assert_eq!(hair.code, ErrorCode::Invalid);
    assert!(hair.message.contains("can't be pinned"), "{}", hair.message);
}
