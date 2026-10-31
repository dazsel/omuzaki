//! The head's sculpt (#2697): the face's forms are reachable through `part.set_param` (and so MCP), move the head's
//! front and nothing at its back, give a sculpted head the `skin` program's `skin-info`, are range-checked, and blend
//! form by form when a sculpted style is mixed with an unsculpted one.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use omuzaki_char_api::{BuiltCharacter, PartId};
use omuzaki_core::mesh::MeshData;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::json;

fn kit() -> MemDoc {
    let mut doc = MemDoc::default();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "tall"}));
    doc
}

fn head(built: &BuiltCharacter) -> &MeshData {
    &built.meshes.iter().find(|m| m.mesh.name == "head-head").expect("a head mesh").mesh.geometry
}

/// The head's farthest point back.
fn back(built: &BuiltCharacter) -> f32 {
    head(built).positions.iter().map(|p| p.z).fold(f32::INFINITY, f32::min)
}

/// The head's profile down its middle: the farthest forward point in each 2 mm band of height, keyed by band.
fn profile(built: &BuiltCharacter) -> BTreeMap<i32, f32> {
    let mut bands = BTreeMap::new();
    for p in head(built).positions.iter().filter(|p| p.x.abs() < 0.008 && p.z > 0.0) {
        #[allow(clippy::cast_possible_truncation)]
        let band = (p.y / 0.002).floor() as i32;
        let z = bands.entry(band).or_insert(f32::NEG_INFINITY);
        *z = z.max(p.z);
    }
    bands
}

/// How far the most raised band of `after`'s profile stands in front of `before`'s.
fn raised(before: &BuiltCharacter, after: &BuiltCharacter) -> f32 {
    let (a, b) = (profile(before), profile(after));
    b.iter().filter_map(|(band, z)| a.get(band).map(|z0| z - z0)).fold(f32::NEG_INFINITY, f32::max)
}

fn set(doc: &mut MemDoc, path: &str, value: f64) {
    common::round_trip(
        doc,
        "part.set_param",
        json!({"character": "kit", "part": "head", "path": path, "value": value}),
    );
}

#[test]
fn a_sculpted_nose_and_chin_stand_out_of_the_front_only_and_carry_skin_info() {
    let mut doc = kit();
    let plain = common::build(&doc, "kit");
    assert!(head(&plain).extra_attribute("skin-info").is_none(), "the smooth head is as it was");
    set(&mut doc, "sculpt/nose", 0.04);
    set(&mut doc, "sculpt/chin", 0.02);
    let sculpted = common::build(&doc, "kit");
    // Built at the template's size and scaled to the tall stature: about 0.6 of the authored 0.04 m at the tip.
    let nose = raised(&plain, &sculpted);
    assert!(nose > 0.012, "the nose stands out of the face's middle: {nose}");
    let (b0, b1) = (back(&plain), back(&sculpted));
    assert!((b1 - b0).abs() < 1e-5, "the back of the head stays: {b0} -> {b1}");
    let mesh = head(&sculpted);
    let info = mesh.extra_attribute("skin-info").expect("a sculpted head carries skin-info");
    assert_eq!((info.components, info.data.len()), (4, mesh.positions.len() * 4));
    let cavities = info.data.chunks(4).map(|c| c[1]);
    assert!(cavities.clone().all(|c| (0.0..=1.0).contains(&c)), "cavity is a share");
    assert!(cavities.clone().any(|c| c < 0.95), "the folds close it somewhere");
    assert!(info.data.chunks(4).any(|c| c[2] > 0.1), "the cheeks and the nose's tip are flushed");
}

#[test]
fn a_form_out_of_range_is_refused() {
    let doc = kit();
    let e = common::refused(
        &doc,
        "part.set_param",
        json!({"character": "kit", "part": "head", "path": "sculpt/nose", "value": 0.5}),
    );
    assert_eq!(e.code, ErrorCode::Invalid, "{e:?}");
    assert!(e.message.contains("nose"), "{}", e.message);
}

#[test]
fn a_mix_with_an_unsculpted_style_blends_each_form() {
    let mut doc = kit();
    common::apply(&mut doc, "character.set_style", json!({"character": "kit", "style": "high-fidelity"}));
    let full = common::definition(&doc, "kit").parts[&PartId::new("head").unwrap()].params["sculpt"]["nose"]
        .as_f64()
        .expect("high-fidelity sculpts the nose");
    let mut doc = kit();
    common::apply(
        &mut doc,
        "character.mix_styles",
        json!({"character": "kit", "styles": [
            {"style": "high-fidelity", "weight": 1.0},
            {"style": "mature", "weight": 1.0}
        ]}),
    );
    let mixed = common::definition(&doc, "kit").parts[&PartId::new("head").unwrap()].params["sculpt"]["nose"]
        .as_f64()
        .expect("the mix keeps a sculpt");
    assert!((mixed - 0.5 * full).abs() < 1e-9, "half the nose: {mixed} against {full}");
}
