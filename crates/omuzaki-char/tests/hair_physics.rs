//! Hair motion (#321): the presets measurably differ after a 25 degree turn, the colliders made from the body sit inside
//! it and reach its surface on every template at five shapes, and `hair.physics.set`, `hair.physics.preset` and
//! `hair.group.set_dynamics` are undoable edits that refuse a bad field or value by name.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // small counts; colliders in f64 to f32

mod common;

use common::{assert_error, at, refused, round_trip};
use omuzaki_char::cloth::collide::BodyDepth;
use omuzaki_char::hair::physics::colliders;
use omuzaki_char::template::registry;
use omuzaki_char_api::{BodyShape, HandParams, HeadSurface, JointRef, Proportions};
use omuzaki_core::glam::Vec3;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::{Value, json};

/// The kit, whose hair part is `hair` with a simulated curtain `c-curtain` and framing locks `d-framing`.
fn kit() -> MemDoc {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    doc
}

const HAIR: &str = "characters/kit/parts/hair/params";

/// `hair.settle`'s answer for the kit's hair.
fn settle(doc: &MemDoc, turn: f64, seconds: f64) -> Value {
    let plan = common::plan(
        doc,
        "hair.settle",
        json!({"character": "kit", "part": "hair", "turn_deg": turn, "seconds": seconds}),
    )
    .unwrap();
    plan.output.expect("the settle answers")
}

#[test]
fn the_presets_settle_in_order_after_a_25_degree_turn() {
    let mut measured = Vec::new();
    for preset in ["silky", "bouncy", "heavy", "stiff"] {
        let mut doc = kit();
        common::apply(&mut doc, "hair.physics.preset", json!({"character": "kit", "part": "hair", "preset": preset}));
        let answer = settle(&doc, 25.0, 6.0);
        println!("{preset}: {answer}");
        assert_eq!(answer["simulated"], json!(true));
        let time = answer["settle_s"].as_f64().unwrap_or_else(|| panic!("{preset} settles within 6 s: {answer}"));
        let swing = answer["swing_mm"].as_f64().unwrap();
        let sag = answer["rest"]["max_mm"].as_f64().unwrap();
        assert!(sag < 40.0, "{preset}: the hair hangs near its styled shape: {answer}");
        measured.push((preset, time, swing));
    }
    // Slowest first, each clearly slower than the next; the stiff hair never leaves the 2 mm band at all.
    for pair in measured.windows(2) {
        let ((a, ta, sa), (b, tb, sb)) = (pair[0], pair[1]);
        assert!(ta > tb * 1.5, "{a} ({ta} s) settles clearly slower than {b} ({tb} s)");
        assert!(sa > sb * 1.5, "{a} ({sa} mm) swings clearly further than {b} ({sb} mm)");
    }
    let stiff = measured[3];
    assert!(stiff.1 == 0.0 && stiff.2 < 2.0, "stiff hair barely moves: {stiff:?}");
    assert!(measured[2].2 > 2.0, "heavy hair still swings: {:?}", measured[2]);
}

#[test]
fn settle_reports_rest_and_refuses_bad_ranges() {
    let doc = kit();
    let still = settle(&doc, 0.0, 1.0);
    assert_eq!(still["settle_s"], json!(0.0), "no turn, nothing to settle: {still}");
    assert!(still["particles"].as_u64().unwrap() > 50);
    let e = refused(&doc, "hair.settle", json!({"character": "kit", "part": "hair", "turn_deg": 200.0}));
    assert_error(&e, ErrorCode::Invalid, "turn_deg", &["200"], &["0 to 180"]);
    let e = refused(&doc, "hair.settle", json!({"character": "kit", "part": "hair", "seconds": 0.0}));
    assert_error(&e, ErrorCode::Invalid, "seconds", &[], &["20"]);
    let e = refused(&doc, "hair.settle", json!({"character": "kit", "part": "body"}));
    assert_error(&e, ErrorCode::Invalid, "part", &["not hair"], &["hair"]);
}

#[test]
fn a_hair_with_nothing_simulated_says_so() {
    let mut doc = kit();
    for group in ["c-curtain", "d-framing"] {
        common::apply(
            &mut doc,
            "hair.group.set_dynamics",
            json!({"character": "kit", "part": "hair", "group": group, "simulate": false}),
        );
    }
    let answer = settle(&doc, 25.0, 1.0);
    assert_eq!(answer["simulated"], json!(false), "{answer}");
}

#[test]
fn physics_set_edits_one_field_undoably_and_refuses_by_name() {
    let mut doc = kit();
    for (field, value) in [
        ("damping", json!(0.95)),
        ("gravity_scale", json!(0.6)),
        ("wind_scale", json!(2.0)),
        ("iterations", json!(4)),
        ("collider_margin", json!(0.01)),
        ("fixed_hz", json!(120.0)),
    ] {
        round_trip(
            &mut doc,
            "hair.physics.set",
            json!({"character": "kit", "part": "hair", "field": field, "value": value}),
        );
        assert_eq!(at(&doc, &format!("{HAIR}/physics/{field}")), Some(value), "{field}");
    }
    let e = refused(
        &doc,
        "hair.physics.set",
        json!({"character": "kit", "part": "hair", "field": "dampening", "value": 0.9}),
    );
    assert_error(&e, ErrorCode::Invalid, "field", &["dampening"], &["damping"]);
    let e = refused(
        &doc,
        "hair.physics.set",
        json!({"character": "kit", "part": "hair", "field": "damping", "value": 1.5}),
    );
    assert_error(&e, ErrorCode::Invalid, "value", &["damping"], &[]);
    let e = refused(
        &doc,
        "hair.physics.set",
        json!({"character": "kit", "part": "hair", "field": "iterations", "value": 0}),
    );
    assert_error(&e, ErrorCode::Invalid, "value", &["iterations"], &[]);
}

#[test]
fn a_preset_sets_the_feel_and_only_the_simulated_groups_stiffness() {
    let mut doc = kit();
    common::apply(
        &mut doc,
        "hair.physics.set",
        json!({"character": "kit", "part": "hair", "field": "fixed_hz", "value": 120.0}),
    );
    let cap_before = at(&doc, &format!("{HAIR}/groups/a-cap/dynamics"));
    let planned =
        round_trip(&mut doc, "hair.physics.preset", json!({"character": "kit", "part": "hair", "preset": "heavy"}));
    assert_eq!(planned.output.as_ref().unwrap()["groups"], json!(["c-curtain", "d-framing"]));
    assert_eq!(at(&doc, &format!("{HAIR}/physics/damping")), Some(json!(0.9)));
    assert_eq!(at(&doc, &format!("{HAIR}/physics/fixed_hz")), Some(json!(120.0)), "the rate is the hair's fit, kept");
    assert_eq!(at(&doc, &format!("{HAIR}/groups/c-curtain/dynamics/stiffness/tip")), Some(json!(0.15)));
    assert_eq!(at(&doc, &format!("{HAIR}/groups/a-cap/dynamics")), cap_before, "a rigid group is left alone");
    // Back to silky: the defaults again.
    round_trip(&mut doc, "hair.physics.preset", json!({"character": "kit", "part": "hair", "preset": "silky"}));
    assert_eq!(at(&doc, &format!("{HAIR}/physics/damping")), Some(json!(0.965)));
    let e = refused(&doc, "hair.physics.preset", json!({"character": "kit", "part": "hair", "preset": "floaty"}));
    assert_eq!(e.code, ErrorCode::Invalid, "{e:?}");
    assert!(e.message.contains("floaty"), "{e:?}");
}

#[test]
fn set_dynamics_edits_a_group_undoably_and_merges_stiffness() {
    let mut doc = kit();
    let path = format!("{HAIR}/groups/c-curtain/dynamics");
    round_trip(
        &mut doc,
        "hair.group.set_dynamics",
        json!({"character": "kit", "part": "hair", "group": "c-curtain", "stiffness": {"tip": 0.2}}),
    );
    assert_eq!(at(&doc, &format!("{path}/stiffness/tip")), Some(json!(0.2)));
    assert_eq!(at(&doc, &format!("{path}/stiffness/root")), Some(json!(1.0)), "the other fields are kept");
    round_trip(
        &mut doc,
        "hair.group.set_dynamics",
        json!({"character": "kit", "part": "hair", "group": "b-bangs", "simulate": true, "follow": "head"}),
    );
    assert_eq!(at(&doc, &format!("{HAIR}/groups/b-bangs/dynamics/simulate")), Some(json!(true)));

    let e = refused(
        &doc,
        "hair.group.set_dynamics",
        json!({"character": "kit", "part": "hair", "group": "z-tail", "simulate": true}),
    );
    assert_error(&e, ErrorCode::NotFound, "group", &["z-tail"], &["c-curtain"]);
    let e = refused(&doc, "hair.group.set_dynamics", json!({"character": "kit", "part": "hair", "group": "c-curtain"}));
    assert_error(&e, ErrorCode::Invalid, "simulate", &["at least one"], &[]);
    let e = refused(
        &doc,
        "hair.group.set_dynamics",
        json!({"character": "kit", "part": "hair", "group": "c-curtain", "stiffness": {"tipp": 0.2}}),
    );
    assert_error(&e, ErrorCode::Invalid, "stiffness", &["tipp"], &["tip"]);
    let e = refused(
        &doc,
        "hair.group.set_dynamics",
        json!({"character": "kit", "part": "hair", "group": "c-curtain", "stiffness": {"tip": 3.0}}),
    );
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("stiffness")), "{e:?}");
}

// ------------------------------------------------------------------------------------------------- the colliders

/// A round head, for the colliders' head sphere; the body spheres are what is measured.
#[derive(Debug)]
struct Ball;

impl HeadSurface for Ball {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 1.4, 0.0)
    }

    fn radius(&self, _: Vec3) -> f32 {
        0.08
    }
}

fn shapes() -> [BodyShape; 5] {
    [
        BodyShape::default(),
        BodyShape { build: 1.0, musculature: 1.0, ..BodyShape::default() },
        BodyShape { build: -1.0, height: 1.0, ..BodyShape::default() },
        BodyShape { curves: 1.0, shoulder_hip: -1.0, ..BodyShape::default() },
        BodyShape { curves: -1.0, shoulder_hip: 1.0, height: -1.0, ..BodyShape::default() },
    ]
}

/// `n` directions spread evenly over the sphere (a Fibonacci lattice).
fn directions(n: usize) -> Vec<Vec3> {
    let golden = std::f32::consts::PI * (3.0 - 5.0f32.sqrt());
    (0..n)
        .map(|k| {
            let y = 1.0 - 2.0 * (k as f32 + 0.5) / n as f32;
            let r = (1.0 - y * y).sqrt();
            let a = golden * k as f32;
            Vec3::new(r * a.cos(), y, r * a.sin())
        })
        .collect()
}

#[test]
fn the_body_colliders_sit_inside_the_body_and_reach_its_surface() {
    let dirs = directions(400);
    let templates = registry::templates();
    assert!(templates.len() >= 3);
    let (mut deepest_poke, mut widest_gap) = (f32::MIN, f32::MIN);
    for template in &templates {
        for shape in shapes() {
            let fit = template.layout(&Proportions::default(), &shape, &HandParams::default());
            let spheres = colliders::from_character(&Ball, Some(&fit));
            assert!(
                spheres.len() >= 9,
                "{}: head, neck, six on the back, two shoulders: {}",
                template.id(),
                spheres.len()
            );
            let depth = BodyDepth::new(&fit);
            for s in spheres.iter().filter(|s| s.bone != JointRef::HEAD) {
                let centre = Vec3::from(s.centre.map(|c| c as f32));
                let radius = s.radius as f32;
                let out = |band: f32| {
                    dirs.iter()
                        .filter(|d| d.y.abs() <= band)
                        .map(|d| depth.depth(centre + *d * radius))
                        .fold(f32::MIN, f32::max)
                };
                // Inside: around the sphere's waist, where its sections fit it. Over its top the neck sphere meets the
                // head, which a body fit leaves out. The depth over-estimates (it is measured from the nearest
                // section's centre): 1.5 mm covers its error.
                let (waist, anywhere) = (out(0.15), out(1.0));
                deepest_poke = deepest_poke.max(waist);
                widest_gap = widest_gap.max(-anywhere);
                assert!(waist <= 0.0015, "{} {shape:?}: {:?} pokes {waist} m out of the body", template.id(), s.bone);
                // And it reaches the surface: somewhere within 4 mm of it.
                assert!(
                    anywhere >= -0.004,
                    "{} {shape:?}: {:?} stops {} m short of the body's surface",
                    template.id(),
                    s.bone,
                    -anywhere
                );
            }
        }
    }
    println!(
        "body colliders: at most {:.2} mm out of the body, at most {:.2} mm short of its surface",
        deepest_poke.max(0.0) * 1e3,
        widest_gap.max(0.0) * 1e3
    );
}
