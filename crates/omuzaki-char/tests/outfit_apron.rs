//! The apron garment (#394): the conformance suite with its bib, crossed straps, frilled panel and bow, its options,
//! and its parameters.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::apron::{Apron, ApronParams};
use omuzaki_char_api::BuildServices;
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

#[test]
fn the_default_apron_passes_the_suite() {
    let rows = conformance::assert_conforms::<Apron>(&ApronParams::default(), &services());
    assert_eq!(rows.len(), 12);
}

#[test]
fn the_options_pass_the_suite() {
    let cases = [
        ("no bib", ApronParams { bib: false, ..ApronParams::default() }),
        ("no frill, no bow", ApronParams { frill: 0.0, bow: false, ..ApronParams::default() }),
        ("long and wide", ApronParams { length: 0.8, width: 1.2, ..ApronParams::default() }),
        ("short", ApronParams { length: 0.0, ..ApronParams::default() }),
    ];
    for (name, p) in cases {
        conformance::run::<Apron>(&p, &services()).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn the_parameters_are_checked() {
    assert!(ApronParams::default().validate().is_ok());
    for bad in [
        ApronParams { length: 1.5, ..ApronParams::default() },
        ApronParams { width: 2.0, ..ApronParams::default() },
        ApronParams { bib_top: 1.2, ..ApronParams::default() },
        ApronParams { frill: 0.2, ..ApronParams::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
