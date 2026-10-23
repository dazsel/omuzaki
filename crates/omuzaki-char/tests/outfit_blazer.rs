//! The blazer garment (#329): the conformance suite buttoned and open, with each lapel, without pockets, with tails,
//! cropped and with short sleeves, and its parameters.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::blazer::{Blazer, BlazerParams, Closure, Pockets};
use omuzaki_char::outfit::coat::trims::{Lapel, LapelStyle};
use omuzaki_char_api::BuildServices;
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

#[test]
fn the_default_blazer_passes_the_suite() {
    let rows = conformance::assert_conforms::<Blazer>(&BlazerParams::default(), &services());
    assert_eq!(rows.len(), 12);
}

#[test]
fn every_front_and_lapel_passes_the_suite() {
    for closure in [Closure::Buttoned, Closure::Open] {
        for style in [LapelStyle::Notch, LapelStyle::Shawl, LapelStyle::None] {
            let p = BlazerParams { closure, lapel: Lapel { style, ..Lapel::default() }, ..BlazerParams::default() };
            conformance::run::<Blazer>(&p, &services()).unwrap_or_else(|e| panic!("{closure:?} {style:?}: {e}"));
        }
    }
}

#[test]
fn the_options_pass_the_suite() {
    let cases = [
        ("no pockets", BlazerParams { pockets: Pockets { hip: false, breast: false }, ..BlazerParams::default() }),
        ("tails", BlazerParams { tails: true, vent: false, ..BlazerParams::default() }),
        ("cropped", BlazerParams { length: -0.15, ..BlazerParams::default() }),
        ("long", BlazerParams { length: 0.45, ..BlazerParams::default() }),
        ("short sleeves", BlazerParams { sleeve_length: 0.45, ..BlazerParams::default() }),
    ];
    for (name, p) in cases {
        conformance::run::<Blazer>(&p, &services()).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn the_parameters_are_checked() {
    assert!(BlazerParams::default().validate().is_ok());
    for bad in [
        BlazerParams { overlap: 1.0, ..BlazerParams::default() },
        BlazerParams { length: 1.5, ..BlazerParams::default() },
        BlazerParams { break_point: 0.05, ..BlazerParams::default() },
        BlazerParams { sleeve_length: 0.0, ..BlazerParams::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
