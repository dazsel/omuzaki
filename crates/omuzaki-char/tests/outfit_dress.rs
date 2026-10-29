//! The dress garment (#328): the conformance suite for every neckline, skirt cut and sleeve, and its parameters.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::coat::NeckShape;
use omuzaki_char::outfit::dress::LAYER_DRESS;
use omuzaki_char::outfit::dress::{Dress, DressParams, DressSleeves, SkirtCut};
use omuzaki_char_api::{BodyRegion, BodyShape, BuildServices, FitFact};
use omuzaki_core::glam::DVec3;
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
    for shape in [NeckShape::Round, NeckShape::V, NeckShape::Square, NeckShape::High, NeckShape::Halter] {
        let mut p = DressParams::default();
        p.neckline.shape = shape;
        if shape.stands() {
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

/// The shell's vertices of the dress `p` on the tall template's default body, and the torso's top and its centre's z.
fn shell_of(p: &DressParams) -> (Vec<DVec3>, f64, f64) {
    let value = serde_json::to_value(p).unwrap();
    let def =
        conformance::character("tall", &BodyShape::default(), &[(conformance::PART, "dress", LAYER_DRESS, value)]);
    let stack = conformance::build(def, &services()).unwrap();
    let fit = &stack.before(conformance::PART).get::<FitFact>().fits[0];
    let top = fit.section(BodyRegion::Torso, 1.0).unwrap();
    let shell = stack.output(conformance::PART).meshes.iter().find(|m| m.name.ends_with("-shell")).unwrap();
    let points = shell.geometry.positions.iter().map(|v| v.as_dvec3()).collect();
    (points, f64::from(top.centre.y), f64::from(top.centre.z))
}

#[test]
fn straps_go_over_the_shoulders_and_a_halter_bares_the_back() {
    // Straps: cloth over each shoulder, out beside the neck and above the torso's top, front and back of the shoulder.
    let (points, top, _) = shell_of(&DressParams::default());
    for side in [1.0, -1.0] {
        let over = points.iter().filter(|p| p.x * side > 0.06 && p.x * side < 0.12 && p.y > top).count();
        assert!(over > 50, "a strap crosses the shoulder on side {side}: {over} points over it");
    }
    let strapless = DressParams { strap_width: 0.0, ..DressParams::default() };
    let (points, top, _) = shell_of(&strapless);
    assert_eq!(points.iter().filter(|p| p.x.abs() > 0.06 && p.y > top).count(), 0, "strapless: nothing over it");

    // A halter's back is bare between the armholes' floor and the collar; a high neck's is covered up to it.
    let mut halter = DressParams::default();
    halter.neckline.shape = NeckShape::Halter;
    let (points, top, z) = shell_of(&halter);
    let back = |points: &[DVec3]| {
        points.iter().filter(|p| p.x.abs() < 0.02 && p.z < z - 0.03 && p.y > top - 0.12 && p.y < top - 0.02).count()
    };
    assert_eq!(back(&points), 0, "the halter's back is bare below the collar");
    let mut high = halter.clone();
    high.neckline.shape = NeckShape::High;
    let (points, _, _) = shell_of(&high);
    assert!(back(&points) > 20, "the high neck's back is covered");
    // Both stand round the neck: cloth above the torso's top all round.
    for p in [&halter, &high] {
        let (points, top, _) = shell_of(p);
        let collar = points.iter().filter(|q| q.y > top + 0.008).count();
        assert!(collar > 200, "{:?} has a collar band: {collar} points above the neck's base", p.neckline.shape);
    }
    let bad = DressParams { sleeves: DressSleeves::Long, ..halter };
    assert!(bad.validate().is_err(), "a halter with sleeves is refused");
}
