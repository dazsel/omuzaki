//! The hoodie garment (#392): the conformance suite with its rib cuffs and hem, kangaroo pocket, drawstrings and hood,
//! its options, and its parameters.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

mod accessory_common;
#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::coat::trims::Drawstrings;
use omuzaki_char::outfit::hood::{Hood, HoodStyle};
use omuzaki_char::outfit::hoodie::{Hoodie, HoodieParams, Ribbing};
use omuzaki_char_api::BuildServices;
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

#[test]
fn the_default_hoodie_passes_the_suite() {
    let rows = conformance::assert_conforms::<Hoodie>(&HoodieParams::default(), &services());
    assert_eq!(rows.len(), 12);
}

#[test]
fn the_options_pass_the_suite() {
    let cases = [
        (
            "no pocket, no hood",
            HoodieParams {
                pocket: false,
                hood: Hood { style: HoodStyle::None, ..Hood::default() },
                ..HoodieParams::default()
            },
        ),
        ("no ribs", HoodieParams { ribbing: Ribbing { hem: 0.0, cuffs: 0.0 }, ..HoodieParams::default() }),
        ("short sleeves", HoodieParams { sleeve_length: 0.45, ..HoodieParams::default() }),
        ("cropped", HoodieParams { length: -0.08, ..HoodieParams::default() }),
        ("long", HoodieParams { length: 0.25, ..HoodieParams::default() }),
        (
            "no drawstrings",
            HoodieParams {
                drawstrings: Drawstrings { on: false, ..Drawstrings::default() },
                ..HoodieParams::default()
            },
        ),
    ];
    for (name, p) in cases {
        conformance::run::<Hoodie>(&p, &services()).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn a_raised_hood_clears_the_head_by_6_mm_on_every_template() {
    for template in accessory_common::TEMPLATES {
        let hood = serde_json::json!({ "hood": { "style": "up" } });
        let def = accessory_common::character_with(
            template,
            Some(&serde_json::json!({})),
            &[("hoodie", "hoodie", hood, None)],
        );
        let s = accessory_common::scene(&def).unwrap_or_else(|e| panic!("{template}: {e}"));
        let centre = s.head.centre();
        let over: Vec<_> =
            accessory_common::vertices_of(&s.built, "hoodie").into_iter().filter(|p| p.y > centre.y).collect();
        assert!(over.len() > 500, "{template}: only {} of the hoodie's points are over the head", over.len());
        let hood = over.iter().map(|p| accessory_common::above_head(s.head.as_ref(), *p)).fold(f32::MAX, f32::min);
        assert!(hood >= 0.006, "{template}: the hood comes {hood} m from the head");
    }
}

#[test]
fn the_parameters_are_checked() {
    assert!(HoodieParams::default().validate().is_ok());
    for bad in [
        HoodieParams { length: 0.8, ..HoodieParams::default() },
        HoodieParams { length: -0.2, ..HoodieParams::default() },
        HoodieParams { sleeve_length: 0.1, ..HoodieParams::default() },
        HoodieParams { ribbing: Ribbing { hem: 0.3, ..Ribbing::default() }, ..HoodieParams::default() },
        HoodieParams { drawstrings: Drawstrings { length: 1.0, ..Drawstrings::default() }, ..HoodieParams::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
