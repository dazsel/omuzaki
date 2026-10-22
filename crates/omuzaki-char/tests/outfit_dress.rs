//! The dress garment (#328): the conformance suite for every neckline, skirt cut and sleeve, and its parameters.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::coat::NeckShape;
use omuzaki_char::outfit::dress::{Dress, DressParams, DressSleeves, SkirtCut};
use omuzaki_char_api::BuildServices;
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

#[test]
fn the_default_dress_passes_the_suite() {
    let rows = conformance::assert_conforms::<Dress>(&DressParams::default(), &services());
    assert_eq!(rows.len(), 12);
}

#[test]
fn every_neckline_passes_the_suite() {
    for shape in [NeckShape::Round, NeckShape::V, NeckShape::Square, NeckShape::High] {
        let mut p = DressParams::default();
        p.neckline.shape = shape;
        if shape == NeckShape::High {
            p.neckline.depth = 0.0;
            p.neckline.rise = 0.03;
        }
        conformance::run::<Dress>(&p, &services()).unwrap_or_else(|e| panic!("{shape:?}: {e}"));
    }
}

#[test]
fn every_skirt_and_sleeve_passes_the_suite() {
    for skirt in [SkirtCut::Circle, SkirtCut::Pencil] {
        let p = DressParams { skirt, ..DressParams::default() };
        conformance::run::<Dress>(&p, &services()).unwrap_or_else(|e| panic!("{skirt:?}: {e}"));
    }
    for sleeves in [DressSleeves::Short, DressSleeves::Puff, DressSleeves::Long] {
        let p = DressParams { sleeves, ..DressParams::default() };
        conformance::run::<Dress>(&p, &services()).unwrap_or_else(|e| panic!("{sleeves:?}: {e}"));
    }
    let long = DressParams { length: 0.92, back_drop: 0.04, ..DressParams::default() };
    conformance::run::<Dress>(&long, &services()).unwrap_or_else(|e| panic!("floor length: {e}"));
}

#[test]
fn the_parameters_are_checked() {
    assert!(DressParams::default().validate().is_ok());
    for bad in [
        DressParams { waist: 0.95, ..DressParams::default() },
        DressParams { length: 1.5, ..DressParams::default() },
        DressParams { thickness: 0.2, ..DressParams::default() },
        DressParams { folds: 500, ..DressParams::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
