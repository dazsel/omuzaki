//! The bodysuit garment (#2447): the conformance suite for its necks and sleeves, how close it follows the body, and its
//! parameters.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::bodysuit::{Bodysuit, BodysuitParams, LAYER_BODYSUIT};
use omuzaki_char::outfit::coat::NeckShape;
use omuzaki_char::outfit::dress::DressSleeves;
use omuzaki_char_api::BuildServices;
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;
use omuzaki_render_api::AlphaMode;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

#[test]
fn the_default_bodysuit_passes_the_suite() {
    let rows = conformance::assert_conforms::<Bodysuit>(&BodysuitParams::default(), &services());
    assert_eq!(rows.len(), 12);
}

#[test]
fn every_neck_and_sleeve_passes_the_suite() {
    for shape in [NeckShape::High, NeckShape::Halter, NeckShape::Scoop, NeckShape::Round] {
        let mut p = BodysuitParams { sleeves: DressSleeves::Straps, ..BodysuitParams::default() };
        p.neckline.shape = shape;
        if !shape.stands() {
            p.neckline.depth = 0.08;
            p.neckline.rise = -0.08;
        }
        conformance::run::<Bodysuit>(&p, &services()).unwrap_or_else(|e| panic!("{shape:?}: {e}"));
    }
    for sleeves in [DressSleeves::Short, DressSleeves::Puff] {
        let p = BodysuitParams { sleeves, ..BodysuitParams::default() };
        conformance::run::<Bodysuit>(&p, &services()).unwrap_or_else(|e| panic!("{sleeves:?}: {e}"));
    }
}

#[test]
fn it_follows_the_bust_and_the_waist_within_4_mm() {
    for (label, shape) in conformance::shapes() {
        let value = serde_json::to_value(BodysuitParams::default()).unwrap();
        let (gap, at) = conformance::torso_gap("bodysuit", LAYER_BODYSUIT, value, &shape, &services());
        assert!(gap <= 0.004, "{label}: its inside stands {:.1} mm off the body at {at}", gap * 1e3);
    }
}

#[test]
fn a_sheer_bodysuit_blends_its_cloth_over_the_skin_and_an_opaque_one_does_not() {
    let fabric_of = |sheer: f64| {
        let mut p = BodysuitParams::default();
        p.common.sheer = sheer;
        let value = serde_json::to_value(&p).unwrap();
        let def =
            conformance::character("tall", &conformance::shapes()[0].1, &[("suit", "bodysuit", LAYER_BODYSUIT, value)]);
        let stack = conformance::build(def, &services()).unwrap();
        let output = stack.output("suit");
        output.materials.iter().find(|(name, _)| name.as_str() == "suit-fabric").expect("the cloth").1.clone()
    };
    let opaque = fabric_of(0.0);
    assert_eq!(opaque.alpha, AlphaMode::Opaque);
    assert_eq!(opaque.base.a, 255);
    let sheer = fabric_of(0.6);
    assert_eq!(sheer.alpha, AlphaMode::Blend, "the cloth blends over what it covers");
    assert_eq!(sheer.base.a, 102, "40% of the cloth covers");
    assert!(!sheer.receives_shadows, "skin seen through it isn't shadowed twice");
    assert!(!sheer.double_sided, "its inside doesn't lay the cloth over the skin a second time");
    assert!(
        serde_json::to_value(BodysuitParams::default()).unwrap()["common"].get("sheer").is_none(),
        "opaque isn't written"
    );
}

#[test]
fn the_parameters_are_checked() {
    assert!(BodysuitParams::default().validate().is_ok());
    let mut halter = BodysuitParams::default();
    halter.neckline.shape = NeckShape::Halter;
    for bad in [
        halter,
        BodysuitParams { length: 0.5, ..BodysuitParams::default() },
        BodysuitParams { thickness: 0.05, ..BodysuitParams::default() },
        BodysuitParams { strap_width: 0.2, ..BodysuitParams::default() },
        BodysuitParams {
            common: omuzaki_char::outfit::common::GarmentCommon { sheer: 0.95, ..BodysuitParams::default().common },
            ..BodysuitParams::default()
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
