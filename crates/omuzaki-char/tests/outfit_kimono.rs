//! The kimono garment (#393) and its obi (#212): the conformance suite with its wrap, collars, sleeve pouches and obi,
//! its options (short and furisode sleeves, each bow, no cord, no obi), the wrap's side, and its parameters.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::coat::Front;
use omuzaki_char::outfit::kimono::obi::{BowStyle, Obi};
use omuzaki_char::outfit::kimono::{Collars, Kimono, KimonoParams, KimonoSleeves};
use omuzaki_char_api::BuildServices;
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

#[test]
fn the_default_kimono_passes_the_suite() {
    let rows = conformance::assert_conforms::<Kimono>(&KimonoParams::default(), &services());
    assert_eq!(rows.len(), 12);
}

#[test]
fn the_options_pass_the_suite() {
    let sleeves = |drop: f64| KimonoParams {
        sleeves: KimonoSleeves { drop, ..KimonoSleeves::default() },
        ..KimonoParams::default()
    };
    let obi = |obi: Obi| KimonoParams { obi, ..KimonoParams::default() };
    let cases = [
        ("furisode", sleeves(1.2)),
        ("no pouches", sleeves(0.0)),
        ("butterfly", obi(Obi { bow: BowStyle::Butterfly, ..Obi::default() })),
        ("no bow, no cord", obi(Obi { bow: BowStyle::None, cord: None, ..Obi::default() })),
        ("no obi", obi(Obi { on: false, ..Obi::default() })),
        (
            "no collars",
            KimonoParams {
                collars: Collars { width: 0.0, under: 0.0, ..Collars::default() },
                ..KimonoParams::default()
            },
        ),
        ("knee length", KimonoParams { length: 0.45, ..KimonoParams::default() }),
    ];
    for (name, p) in cases {
        conformance::run::<Kimono>(&p, &services()).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn it_wraps_left_over_right() {
    // The wearer's left panel crosses past the centre front over the right.
    let Front::Open { gap, .. } = KimonoParams::default().cut().front else { panic!("a kimono is open at the front") };
    assert!(gap < 0.0, "the left panel's edge stops {gap} rad short of the centre");
}

#[test]
fn the_parameters_are_checked() {
    assert!(KimonoParams::default().validate().is_ok());
    for bad in [
        KimonoParams { length: 0.1, ..KimonoParams::default() },
        KimonoParams { overlap: 0.0, ..KimonoParams::default() },
        KimonoParams { sleeves: KimonoSleeves { drop: 3.0, ..KimonoSleeves::default() }, ..KimonoParams::default() },
        KimonoParams { obi: Obi { height: 0.9, ..Obi::default() }, ..KimonoParams::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
