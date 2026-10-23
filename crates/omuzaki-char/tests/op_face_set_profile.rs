//! `face.set_profile` and `face.list_profiles` (CB-10, #166): a face shape blends into the head's outline and the face's
//! layout in one patch, undoes exactly, and reshapes the lower face only.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers: a panic is a clear failure

mod common;

use common::{apply, assert_error, at, definition, plan, refused, round_trip};
use omuzaki_char::head::mesh::{Resolution, build};
use omuzaki_char_api::{HeadParams, PartDef};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::{Value, json};

fn kit() -> MemDoc {
    let mut doc = MemDoc::new();
    apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    doc
}

/// The kit's head parameters as the head part reads them.
fn head(doc: &MemDoc) -> HeadParams {
    let def = definition(doc, "kit");
    let part: &PartDef = def.parts.values().find(|p| p.kind.as_str() == "head").expect("the kit has a head");
    serde_json::from_value(part.params.clone()).unwrap()
}

#[test]
fn a_preset_reshapes_head_and_layout_in_one_patch_and_undoes_exactly() {
    let mut doc = kit();
    let planned = round_trip(&mut doc, "face.set_profile", json!({"character": "kit", "profile": "egg-oval"}));
    let paths: Vec<String> = planned.patch.changes.iter().map(|c| c.path.to_string()).collect();
    println!("{paths:?}");
    assert!(paths.iter().any(|p| p.ends_with("params/outline")), "the outline is in the patch: {paths:?}");
    assert!(paths.iter().any(|p| p.contains("/face/layout")), "the layout is in the patch: {paths:?}");
    assert_eq!(at(&doc, "characters/kit/parts/head/params/preset"), Some(json!("egg-oval")), "a full blend records it");

    let preset = omuzaki_char::head::presets::ShapeProfiles::get("egg-oval").unwrap().profile;
    let def = definition(&doc, "kit");
    assert_eq!(head(&doc).outline, preset.outline);
    assert_eq!(def.face.layout, preset.layout);

    // The answer carries the numbers before and after: the layout's ratios are the preset's own, read on its chin.
    let output = planned.output.unwrap();
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
    let ratios = preset.layout.ratios(preset.outline.chin_y()).unwrap();
    assert!((output["after"]["ratios"]["mouth"].as_f64().unwrap() - ratios.mouth).abs() < 1e-4);
    assert!((output["after"]["ratios"]["drop_to_iod"].as_f64().unwrap() - ratios.drop_to_iod).abs() < 1e-4);
    assert_ne!(output["after"]["silhouette"], output["before"]["silhouette"], "the silhouette changed");
}

#[test]
fn a_preset_with_cranium_proportions_reshapes_the_cranium_and_leaving_it_restores_the_default() {
    let mut doc = kit();
    let default = head(&doc).cranium;
    let planned = round_trip(&mut doc, "face.set_profile", json!({"character": "kit", "profile": "long"}));
    let paths: Vec<String> = planned.patch.changes.iter().map(|c| c.path.to_string()).collect();
    assert!(paths.iter().any(|p| p.ends_with("params/cranium")), "the cranium is in the patch: {paths:?}");
    // Long's head is 10% narrower and 10% taller than the default; its depth and its seat are the default's.
    let long = head(&doc).cranium;
    assert!((long.half_width - 0.9 * default.half_width).abs() < 1e-12);
    assert!((long.half_height_up - 1.1 * default.half_height_up).abs() < 1e-12);
    assert_eq!((long.half_depth_front, long.centre), (default.half_depth_front, default.centre));
    // Half way to the heart (which has none) the cranium is half way back; all the way, it is the default again.
    round_trip(&mut doc, "face.set_profile", json!({"character": "kit", "profile": "heart", "blend": 0.5}));
    assert!((head(&doc).cranium.half_width - 0.95 * default.half_width).abs() < 1e-12);
    round_trip(&mut doc, "face.set_profile", json!({"character": "kit", "profile": "heart"}));
    assert_eq!(head(&doc).cranium, default);
}

#[test]
fn a_dry_run_changes_nothing_and_blend_zero_is_a_no_op() {
    let doc = kit();
    let original = doc.root().clone();
    let planned = plan(&doc, "face.set_profile", json!({"character": "kit", "profile": "mochi"})).unwrap();
    assert!(!planned.patch.is_empty());
    assert_eq!(doc.root(), &original, "a dry-run leaves the document unchanged");
    let zero = plan(&doc, "face.set_profile", json!({"character": "kit", "profile": "mochi", "blend": 0.0})).unwrap();
    assert!(zero.patch.is_empty(), "blend 0 changes nothing: {:?}", zero.patch);
}

#[test]
fn a_partial_blend_lands_between_and_records_no_preset() {
    let mut doc = kit();
    let before = head(&doc).outline;
    round_trip(&mut doc, "face.set_profile", json!({"character": "kit", "profile": "long"}));
    round_trip(&mut doc, "face.set_profile", json!({"character": "kit", "profile": "heart", "blend": 0.5}));
    assert_eq!(at(&doc, "characters/kit/parts/head/params/preset"), None, "a half blend is no longer `long`");
    let long = omuzaki_char::head::presets::ShapeProfiles::get("long").unwrap().profile;
    let heart = omuzaki_char::head::presets::ShapeProfiles::get("heart").unwrap().profile;
    let half = long.lerp(&heart, 0.5);
    let now = head(&doc).outline;
    assert!((now.exponent - half.outline.exponent).abs() < 1e-12 && (now.top - half.outline.top).abs() < 1e-12);
    assert_ne!(now, before);
}

#[test]
fn an_inline_profile_is_taken_as_written() {
    let mut doc = kit();
    let mochi = omuzaki_char::head::presets::ShapeProfiles::get("mochi").unwrap().profile;
    let mut profile = serde_json::to_value(mochi).unwrap();
    profile["outline"]["cheek_puff"] = json!(0.004);
    round_trip(&mut doc, "face.set_profile", json!({"character": "kit", "profile": profile}));
    assert!((head(&doc).outline.cheek_puff - 0.004).abs() < 1e-12);
    assert_eq!(at(&doc, "characters/kit/parts/head/params/preset"), None, "an inline profile names no preset");
}

/// A face shape moves the face, never the cranium: nothing above the brows moves, and most of what moves is below the
/// eye line. The rest is the cheeks and temples between the eye line and the brows, which the outline's width at its top
/// sets (egg-oval is widest at the temples).
#[test]
fn the_reshape_is_in_the_face_not_the_cranium() {
    let mut doc = kit();
    let before = build(&head(&doc), Resolution::LIVE, None).unwrap();
    apply(&mut doc, "face.set_profile", json!({"character": "kit", "profile": "egg-oval", "blend": 0.5}));
    let after = build(&head(&doc), Resolution::LIVE, None).unwrap();
    let layout = definition(&doc, "kit").face.layout;
    let centre = head(&doc).cranium.centre()[1];
    assert_eq!(before.positions.len(), after.positions.len());
    let (mut below, mut total, mut above_brows) = (0.0f64, 0.0f64, 0.0f64);
    for (a, b) in before.positions.iter().zip(&after.positions) {
        let (moved, y) = (f64::from(a.distance(*b)), f64::from(a.y) - centre);
        total += moved;
        if y < layout.eye_y {
            below += moved;
        }
        if y > layout.eye_y + layout.brow_gap {
            above_brows = above_brows.max(moved);
        }
    }
    println!(
        "{:.1}% of the displacement is below the eye line ({total:.3} m summed); above the brows nothing moves more than \
         {above_brows:e} m",
        100.0 * below / total
    );
    assert!(total > 0.0, "the head changed");
    assert!(above_brows < 1e-6, "the cranium moved by {above_brows} m");
    assert!(below / total > 0.75, "{:.1}% below the eye line", 100.0 * below / total);
}

#[test]
fn bad_input_is_refused_with_the_way_out() {
    let doc = kit();
    let e = refused(&doc, "face.set_profile", json!({"character": "kit", "profile": "triangle"}));
    assert_error(&e, ErrorCode::Invalid, "profile", &["triangle"], &["round-u", "egg-oval", "face.list_profiles"]);
    let e = refused(&doc, "face.set_profile", json!({"character": "kit", "profile": "mochi", "blend": 1.5}));
    assert_error(&e, ErrorCode::Invalid, "blend", &["1.5"], &["0", "1"]);
    let e = refused(&doc, "face.set_profile", json!({"character": "kit", "profile": "mochi", "part": "hair"}));
    assert_error(&e, ErrorCode::Invalid, "part", &["not a head"], &["head part"]);
    let e = refused(&doc, "face.set_profile", json!({"character": "kit", "profile": "mochi", "part": "nope"}));
    assert_error(&e, ErrorCode::NotFound, "part", &["nope"], &["head"]);
}

#[test]
fn the_list_carries_every_preset_and_its_numbers() {
    let doc = kit();
    let output = plan(&doc, "face.list_profiles", json!({})).unwrap().output.unwrap();
    let presets = output["presets"].as_array().unwrap();
    let ids: Vec<&str> = presets.iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(ids, omuzaki_char_api::PRESET_IDS);
    for p in presets {
        assert!(p["summary"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(p["measured"]["silhouette"]["face_length_to_width"].is_number(), "{p}");
        assert!(p["measured"]["ratios"]["drop_to_iod"].is_number(), "{p}");
        assert!(!p["profile"]["outline"].is_null() && p["profile"]["layout"] != Value::Null);
    }
}
