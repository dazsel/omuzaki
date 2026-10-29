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
fn the_parameters_are_checked() {
    assert!(BodysuitParams::default().validate().is_ok());
    let mut halter = BodysuitParams::default();
    halter.neckline.shape = NeckShape::Halter;
    for bad in [
        halter,
        BodysuitParams { length: 0.5, ..BodysuitParams::default() },
        BodysuitParams { thickness: 0.05, ..BodysuitParams::default() },
        BodysuitParams { strap_width: 0.2, ..BodysuitParams::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
