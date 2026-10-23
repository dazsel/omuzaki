//! Hair guides (#180): a guides group builds a lock through its control points; freezing a procedural group into
//! guides keeps its curves within 1 mm; mirroring twice is the identity and a jitter is the same for its seed; every
//! `hair.guide.*` op applies and undoes exactly; and a point anchored on the head's surface moves when the head widens.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

mod common;

use common::{assert_error, at, refused, round_trip};
use omuzaki_char::hair::edit;
use omuzaki_char_api::{BuildServices, CharId, Generator, GroupId, GuidePoint, GuidesGen, HairParams, PartId};
use omuzaki_core::glam::Vec3;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::{Value, json};

fn kit() -> MemDoc {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    doc
}

const GROUPS: &str = "characters/kit/parts/hair/params/groups";

/// The kit with a guides group `e-guides` of two clumps on the left of the head.
fn with_guides() -> MemDoc {
    let mut doc = kit();
    for (theta, phi) in [(70.0, 40.0), (75.0, 60.0)] {
        common::apply(
            &mut doc,
            "hair.guide.add_clump",
            json!({"character": "kit", "part": "hair", "group": "e-guides", "clump": {"points": [
                {"surface": [theta, phi, 0.0]}, {"head": [0.08, -0.06, 0.02]}, {"head": [0.09, -0.16, 0.0]}
            ]}}),
        );
    }
    doc
}

/// The hair parameters as the kind normalises them.
fn hair(doc: &MemDoc) -> HairParams {
    let def = common::definition(doc, "kit");
    let stored = &def.parts[&PartId::new("hair").unwrap()].params;
    let kind = omuzaki_char_api::find_kind("hair").unwrap();
    serde_json::from_value((kind.check)(stored).unwrap()).unwrap()
}

/// The compiled clumps of `group`, before mirroring.
fn clumps(doc: &MemDoc, group: &str) -> Vec<Vec<Vec3>> {
    let def = common::definition(doc, "kit");
    let services = BuildServices::default().with_rasterizer(|| Box::new(omuzaki_face_mock::MockRasterizer));
    let (id, part, group) = (CharId::new("kit").unwrap(), PartId::new("hair").unwrap(), GroupId::new(group).unwrap());
    let frozen = edit::freeze(&id, &def, &part, &hair(doc), &group, &services).unwrap();
    frozen.clumps.into_iter().map(|c| c.rest).collect()
}

fn guides(doc: &MemDoc, group: &str) -> GuidesGen {
    match &hair(doc).groups[&GroupId::new(group).unwrap()].generator {
        Generator::Guides(g) => g.clone(),
        other => panic!("{group} is not guides: {other:?}"),
    }
}

#[test]
fn a_guides_group_builds_a_lock_through_its_points() {
    let doc = with_guides();
    let built = common::build(&doc, "kit");
    assert!(built.meshes.iter().any(|m| m.part.as_str() == "hair"));
    let curves = clumps(&doc, "e-guides");
    assert_eq!(curves.len(), 2);
    for curve in &curves {
        assert!(curve.len() >= 8, "resampled: {} points", curve.len());
        let length: f32 = curve.windows(2).map(|w| w[0].distance(w[1])).sum();
        assert!((0.1..0.4).contains(&length), "a lock to the shoulder: {length} m");
    }
}

#[test]
fn freezing_a_group_keeps_its_curves_within_a_millimetre() {
    for group in ["b-bangs", "c-curtain", "d-framing"] {
        let mut doc = kit();
        let before = clumps(&doc, group);
        let planned =
            round_trip(&mut doc, "hair.group.to_guides", json!({"character": "kit", "part": "hair", "group": group}));
        let answer = planned.output.unwrap();
        let after = clumps(&doc, group);
        assert_eq!(after.len(), before.len(), "{group}: one guide per clump");
        // Every point of the original lies within 1 mm of the frozen curve (the other way, the frozen curve is smooth
        // where the original's chords cut corners, so it is reported, not held to the same bound).
        let (mut worst, mut back) = (0.0f32, 0.0f32);
        for (a, b) in after.iter().zip(&before) {
            worst = worst.max(edit::deviation(a, b));
            back = back.max(edit::deviation(b, a));
        }
        let points = answer["points"].as_u64().unwrap();
        println!(
            "{group}: {} clumps frozen into {points} control points; the original lies within {:.3} mm of the frozen \
             curves, which lie within {:.3} mm of its chords",
            before.len(),
            worst * 1e3,
            back * 1e3
        );
        assert!(worst <= 0.001, "{group}: the frozen curves are {worst} m off");
        let rest: usize = before.iter().map(Vec::len).sum();
        assert!(usize::try_from(points).unwrap() < rest, "{group}: fewer control points than rest points");
    }
    let doc = kit();
    let e = refused(&doc, "hair.group.to_guides", json!({"character": "kit", "part": "hair", "group": "a-cap"}));
    assert_error(&e, ErrorCode::Invalid, "group", &["shell"], &["fringe"]);
}

#[test]
fn freezing_then_widening_the_head_reshapes_the_guides() {
    let mut doc = kit();
    common::apply(&mut doc, "hair.group.to_guides", json!({"character": "kit", "part": "hair", "group": "c-curtain"}));
    let roots = |doc: &MemDoc| clumps(doc, "c-curtain").iter().map(|c| c[0]).collect::<Vec<_>>();
    let narrow = roots(&doc);
    let width = at(&doc, "characters/kit/parts/head/params/cranium/half_width").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let width = if width > 0.0 { width } else { 0.09 };
    common::apply(
        &mut doc,
        "part.set_param",
        json!({"character": "kit", "part": "head", "path": "cranium/half_width", "value": width * 1.3}),
    );
    let wide = roots(&doc);
    let moved = narrow.iter().zip(&wide).filter(|(a, b)| b.x.abs() > a.x.abs() + 0.001).count();
    println!("widening the head 30% carries {moved} of {} frozen roots outward", narrow.len());
    assert!(moved * 2 > narrow.len(), "the roots sit on the head and move with it");
}

#[test]
fn moving_a_surface_point_follows_the_head() {
    let mut doc = with_guides();
    common::apply(
        &mut doc,
        "hair.guide.move_point",
        json!({"character": "kit", "part": "hair", "group": "e-guides", "clump": 0, "index": 1, "point": {"surface": [80.0, 70.0, 0.01]}}),
    );
    let before = clumps(&doc, "e-guides")[0].clone();
    let width =
        at(&doc, "characters/kit/parts/head/params/cranium/half_width").and_then(|v| v.as_f64()).unwrap_or(0.09);
    common::apply(
        &mut doc,
        "part.set_param",
        json!({"character": "kit", "part": "head", "path": "cranium/half_width", "value": width * 1.3}),
    );
    let after = clumps(&doc, "e-guides")[0].clone();
    let shift = before.iter().zip(&after).map(|(a, b)| b.x - a.x).fold(0.0f32, f32::max);
    println!("widening the head moves the anchored clump {:.1} mm outward", shift * 1e3);
    assert!(shift > 0.005, "{shift}");
}

#[test]
fn a_point_dropped_near_the_head_anchors_to_it() {
    let mut doc = with_guides();
    // A head-frame point 1 cm off the left of the head: anchored, it is stored on the surface.
    let width =
        at(&doc, "characters/kit/parts/head/params/cranium/half_width").and_then(|v| v.as_f64()).unwrap_or(0.09);
    let near = json!({"head": [width + 0.01, 0.0, 0.0]});
    round_trip(
        &mut doc,
        "hair.guide.move_point",
        json!({"character": "kit", "part": "hair", "group": "e-guides", "clump": 1, "index": 1, "point": near}),
    );
    let stored = at(&doc, &format!("{GROUPS}/e-guides/generator/clumps/1/points/1")).unwrap();
    assert!(stored.get("surface").is_some(), "anchored to the surface: {stored}");
    round_trip(
        &mut doc,
        "hair.guide.move_point",
        json!({"character": "kit", "part": "hair", "group": "e-guides", "clump": 1, "index": 1, "point": near, "anchor": false}),
    );
    assert_eq!(at(&doc, &format!("{GROUPS}/e-guides/generator/clumps/1/points/1")), Some(near), "as given");
}

#[test]
fn mirror_twice_is_the_identity_and_jitter_is_the_same_for_a_seed() {
    let doc = with_guides();
    let g = guides(&doc, "e-guides");
    assert_eq!(edit::mirrored(&edit::mirrored(&g)), g);
    assert_ne!(edit::mirrored(&g), g);
    let (a, b, c) = (edit::jittered(&g, 0.01, 4), edit::jittered(&g, 0.01, 4), edit::jittered(&g, 0.01, 5));
    assert_eq!(a, b, "the same seed, the same hair");
    assert_ne!(a, c, "another seed, other hair");
    for (jittered, original) in a.clumps.iter().zip(&g.clumps) {
        assert_eq!(jittered.points[0], original.points[0], "roots stay put");
    }
}

#[test]
fn every_guide_op_applies_and_undoes_exactly() {
    let mut doc = with_guides();
    let args = |extra: Value| {
        let mut base = json!({"character": "kit", "part": "hair", "group": "e-guides"});
        base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        base
    };
    round_trip(&mut doc, "hair.guide.add_point", args(json!({"clump": 0, "point": {"head": [0.1, -0.22, 0.0]}})));
    assert_eq!(guides(&doc, "e-guides").clumps[0].points.len(), 4);
    round_trip(
        &mut doc,
        "hair.guide.add_point",
        args(json!({"clump": 0, "index": 1, "point": {"body": [0.1, 1.0, 0.0]}, "anchor": false})),
    );
    assert_eq!(
        guides(&doc, "e-guides").clumps[0].points[1],
        GuidePoint::Body([0.1, 1.0, 0.0]),
        "inserted in the middle"
    );
    round_trip(&mut doc, "hair.guide.remove_point", args(json!({"clump": 0, "index": 1})));
    round_trip(&mut doc, "hair.guide.mirror", args(json!({})));
    round_trip(&mut doc, "hair.guide.jitter", args(json!({"amount": 0.004, "seed": 9})));
    let added = round_trip(
        &mut doc,
        "hair.guide.add_clump",
        args(
            json!({"clump": {"points": [{"surface": [60.0, -30.0, 0.0]}, {"head": [-0.07, -0.1, 0.03]}], "shade": 0.8}}),
        ),
    );
    assert_eq!(added.output.unwrap()["index"], json!(2));
    round_trip(&mut doc, "hair.guide.remove_clump", args(json!({"clump": 0})));
    assert_eq!(guides(&doc, "e-guides").clumps.len(), 2);
    let made = round_trip(
        &mut doc,
        "hair.guide.add_clump",
        json!({"character": "kit", "part": "hair", "group": "f-new", "clump": {"points": [{"surface": [40.0, 0.0, 0.0]}, {"head": [0.0, 0.1, 0.12]}]}}),
    );
    assert_eq!(made.output.unwrap()["created_group"], json!(true));
}

#[test]
fn guide_ops_refuse_by_name() {
    let doc = with_guides();
    let e = refused(
        &doc,
        "hair.guide.move_point",
        json!({"character": "kit", "part": "hair", "group": "c-curtain", "clump": 0, "index": 0, "point": {"head": [0.0, 0.0, 0.0]}}),
    );
    assert_error(&e, ErrorCode::Invalid, "group", &["curtain"], &["hair.group.to_guides"]);
    let e = refused(
        &doc,
        "hair.guide.move_point",
        json!({"character": "kit", "part": "hair", "group": "e-guides", "clump": 5, "index": 0, "point": {"head": [0.0, 0.0, 0.0]}}),
    );
    assert_error(&e, ErrorCode::NotFound, "clump", &["2 clumps"], &["add_clump"]);
    let e = refused(
        &doc,
        "hair.guide.move_point",
        json!({"character": "kit", "part": "hair", "group": "e-guides", "clump": 0, "index": 9, "point": {"head": [0.0, 0.0, 0.0]}}),
    );
    assert_error(&e, ErrorCode::NotFound, "index", &["3 points"], &["add_point"]);
    let e = refused(
        &doc,
        "hair.guide.move_point",
        json!({"character": "kit", "part": "hair", "group": "e-guides", "clump": 0, "index": 0, "point": {"surface": [200.0, 0.0, 0.0]}}),
    );
    assert_error(&e, ErrorCode::Invalid, "point", &["theta"], &["0 to 180"]);
    let mut two = with_guides();
    common::apply(
        &mut two,
        "hair.guide.remove_point",
        json!({"character": "kit", "part": "hair", "group": "e-guides", "clump": 0, "index": 1}),
    );
    let e = refused(
        &two,
        "hair.guide.remove_point",
        json!({"character": "kit", "part": "hair", "group": "e-guides", "clump": 0, "index": 1}),
    );
    assert_error(&e, ErrorCode::Invalid, "index", &["only two"], &["remove_clump"]);
    let e = refused(
        &doc,
        "hair.guide.add_clump",
        json!({"character": "kit", "part": "hair", "group": "e-guides", "clump": {"points": [{"head": [0.0, 0.0, 0.0]}]}}),
    );
    assert_error(&e, ErrorCode::Invalid, "clump", &["two points"], &[]);
    let e = refused(
        &doc,
        "hair.guide.jitter",
        json!({"character": "kit", "part": "hair", "group": "e-guides", "amount": 0.5}),
    );
    assert_error(&e, ErrorCode::Invalid, "amount", &["0.5"], &["0.05"]);
    let e = refused(&doc, "hair.guide.mirror", json!({"character": "kit", "part": "hair", "group": "z-none"}));
    assert_error(&e, ErrorCode::NotFound, "group", &["z-none"], &["e-guides"]);
    let e = refused(&doc, "hair.group.to_guides", json!({"character": "kit", "part": "hair", "group": "e-guides"}));
    assert_error(&e, ErrorCode::Invalid, "group", &["already guides"], &["hair.guide"]);
}
