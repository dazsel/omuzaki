//! The `shoes` footwear (#385): the garment conformance suite on 4 templates x 3 body shapes for every style, a heel up
//! to 8 cm keeping the foot inside the shoe, and the parameters' checks.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::shoes::{Collar, MAX_HEEL, ShoeStyle, Shoes, ShoesParams};
use omuzaki_char_api::BuildServices;
use omuzaki_face_api::Validate;

fn services() -> BuildServices {
    BuildServices::default()
}

fn style(style: ShoeStyle) -> ShoesParams {
    ShoesParams { style, ..ShoesParams::default() }
}

#[test]
fn sneakers_pass_the_suite_on_every_template_and_shape() {
    let rows = conformance::assert_conforms::<Shoes>(&style(ShoeStyle::Sneaker), &services());
    assert_eq!(rows.len(), 12, "4 templates x 3 shapes");
}

#[test]
fn flats_pass_the_suite() {
    conformance::assert_conforms::<Shoes>(&style(ShoeStyle::Flat), &services());
}

#[test]
fn loafers_pass_the_suite() {
    conformance::assert_conforms::<Shoes>(&style(ShoeStyle::Loafer), &services());
}

#[test]
fn heels_pass_the_suite_up_to_8_cm() {
    conformance::assert_conforms::<Shoes>(&style(ShoeStyle::Heel), &services());
    let tall = ShoesParams { heel_height: Some(MAX_HEEL), heel_taper: Some(0.8), ..style(ShoeStyle::Heel) };
    conformance::assert_conforms::<Shoes>(&tall, &services());
}

#[test]
fn sandals_pass_the_suite() {
    conformance::assert_conforms::<Shoes>(&style(ShoeStyle::Sandal), &services());
}

#[test]
fn a_high_collar_and_a_thin_ease_pass_too() {
    let mut p = style(ShoeStyle::Sneaker);
    p.collar = Some(Collar { back: 1.3, side: 1.1, front: 1.4 });
    p.common.ease = 0.003;
    p.laces = Some(8);
    conformance::assert_conforms::<Shoes>(&p, &services());
}

#[test]
fn parameters_out_of_range_are_refused() {
    for bad in [
        ShoesParams { heel_height: Some(0.1), ..ShoesParams::default() },
        ShoesParams { heel_taper: Some(0.9), ..ShoesParams::default() },
        ShoesParams { sole: Some(0.001), ..ShoesParams::default() },
        ShoesParams { laces: Some(9), ..ShoesParams::default() },
        ShoesParams { collar: Some(Collar { back: 2.0, side: 0.5, front: 0.5 }), ..ShoesParams::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?} is refused");
    }
    assert!(ShoesParams::default().validate().is_ok());
}

#[test]
fn a_style_alone_is_a_whole_shoe() {
    let p: ShoesParams = serde_json::from_value(serde_json::json!({ "style": "loafer" })).unwrap();
    assert_eq!(p.style, ShoeStyle::Loafer);
    assert!(p.validate().is_ok());
}
