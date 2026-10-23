//! The cloak garment (#330) and the hood (#325): the conformance suite from a short cape to the floor, with the hood
//! down or without one, open wide or with a pendant, and the parameters.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

mod accessory_common;
#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::cloak::{Cloak, CloakParams};
use omuzaki_char::outfit::coat::trims::Clasps;
use omuzaki_char::outfit::hood::{Hood, HoodStyle};
use omuzaki_char_api::BuildServices;
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

#[test]
fn the_default_cloak_passes_the_suite() {
    let rows = conformance::assert_conforms::<Cloak>(&CloakParams::default(), &services());
    assert_eq!(rows.len(), 12);
}

#[test]
fn every_length_and_option_passes_the_suite() {
    let cases = [
        ("cape", CloakParams { length: -0.1, ..CloakParams::default() }),
        ("floor", CloakParams { length: 0.92, back_drop: 0.08, ..CloakParams::default() }),
        ("no hood", CloakParams { hood: Hood { style: HoodStyle::None, ..Hood::default() }, ..CloakParams::default() }),
        (
            "deep hood",
            CloakParams { hood: Hood { size: 1.6, pouch: 0.03, ..Hood::default() }, ..CloakParams::default() },
        ),
        ("open wide", CloakParams { opening: 0.5, ..CloakParams::default() }),
        ("pendant", CloakParams { clasps: Clasps { pendant: true, ..Clasps::default() }, ..CloakParams::default() }),
        ("no clasps", CloakParams { clasps: Clasps { on: false, ..Clasps::default() }, ..CloakParams::default() }),
    ];
    for (name, p) in cases {
        conformance::run::<Cloak>(&p, &services()).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn a_raised_hood_needs_a_head() {
    let p = CloakParams { hood: Hood { style: HoodStyle::Up, ..Hood::default() }, ..CloakParams::default() };
    let err = conformance::run::<Cloak>(&p, &services()).expect_err("the suite's bodies have no head");
    assert!(err.contains("head"), "{err}");
}

#[test]
fn a_raised_hood_clears_the_head_by_6_mm_on_every_template() {
    for template in accessory_common::TEMPLATES {
        let hood = serde_json::json!({ "hood": { "style": "up" } });
        let def =
            accessory_common::character_with(template, Some(&serde_json::json!({})), &[("cloak", "cloak", hood, None)]);
        let s = accessory_common::scene(&def).unwrap_or_else(|e| panic!("{template}: {e}"));
        let centre = s.head.centre();
        let over: Vec<_> =
            accessory_common::vertices_of(&s.built, "cloak").into_iter().filter(|p| p.y > centre.y).collect();
        assert!(over.len() > 500, "{template}: only {} of the cloak's points are over the head", over.len());
        let hood = over.iter().map(|p| accessory_common::above_head(s.head.as_ref(), *p)).fold(f32::MAX, f32::min);
        assert!(hood >= 0.006, "{template}: the hood comes {hood} m from the head");
    }
}

#[test]
fn the_parameters_are_checked() {
    assert!(CloakParams::default().validate().is_ok());
    for bad in [
        CloakParams { length: 1.5, ..CloakParams::default() },
        CloakParams { opening: 0.0, ..CloakParams::default() },
        CloakParams { hood: Hood { clearance: 0.001, ..Hood::default() }, ..CloakParams::default() },
        CloakParams { folds: 500, ..CloakParams::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
