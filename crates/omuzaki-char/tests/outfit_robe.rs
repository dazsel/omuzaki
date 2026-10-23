//! The robe garment (#324, #334, #384, #194): the conformance suite with its bell sleeves, band, clasps and hood, its
//! options, the band's pattern, and the parameters. (The issues' parity against a private prototype's dumps is not
//! part of this project: the robe is the project's own.)

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::coat::trims::Clasps;
use omuzaki_char::outfit::common::FabricStyle;
use omuzaki_char::outfit::hood::{Hood, HoodStyle};
use omuzaki_char::outfit::robe::{Robe, RobeBand, RobeParams, RobeSleeves};
use omuzaki_char_api::BuildServices;
use omuzaki_face_api::{ColourRef, Validate};
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

#[test]
fn the_default_robe_passes_the_suite() {
    let rows = conformance::assert_conforms::<Robe>(&RobeParams::default(), &services());
    assert_eq!(rows.len(), 12);
}

#[test]
fn the_options_pass_the_suite() {
    let plain =
        RobeBand { fabric: FabricStyle::Plain { colour: ColourRef::rgb(0x80, 0x70, 0x50) }, ..RobeBand::default() };
    let damask = RobeBand {
        fabric: FabricStyle::Damask {
            ground: ColourRef::rgb(0x40, 0x3a, 0x34),
            figure: ColourRef::rgb(0x58, 0x50, 0x48),
            period: 0.03,
        },
        ..RobeBand::default()
    };
    let cases = [
        (
            "short sleeves",
            RobeParams {
                sleeves: RobeSleeves { length: 0.5, bell: 0.02, ..RobeSleeves::default() },
                ..RobeParams::default()
            },
        ),
        (
            "wide bell",
            RobeParams {
                sleeves: RobeSleeves { bell: 0.11, past: 0.08, ..RobeSleeves::default() },
                ..RobeParams::default()
            },
        ),
        ("no band", RobeParams { band: RobeBand { width: 0.0, ..RobeBand::default() }, ..RobeParams::default() }),
        ("plain band", RobeParams { band: plain, ..RobeParams::default() }),
        ("damask band", RobeParams { band: damask, ..RobeParams::default() }),
        ("no hood", RobeParams { hood: Hood { style: HoodStyle::None, ..Hood::default() }, ..RobeParams::default() }),
        ("pendant", RobeParams { clasps: Clasps { pendant: true, ..Clasps::default() }, ..RobeParams::default() }),
        ("knee length", RobeParams { length: 0.4, ..RobeParams::default() }),
        (
            "plain cloth",
            RobeParams {
                common: omuzaki_char::outfit::common::GarmentCommon {
                    fabric: FabricStyle::Plain { colour: ColourRef::rgb(0x30, 0x30, 0x38) },
                    ..RobeParams::default().common
                },
                ..RobeParams::default()
            },
        ),
    ];
    for (name, p) in cases {
        conformance::run::<Robe>(&p, &services()).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn the_band_patterns_tile_and_are_checked() {
    use omuzaki_char::outfit::common::fabric_tile;
    let colour = |c: &ColourRef| match c {
        ColourRef::Fixed(c) => Ok(*c),
        ColourRef::Slot(_) => Ok(omuzaki_core::colour::Colour::rgb(0, 0, 0)),
    };
    for fabric in [
        RobeBand::default().fabric,
        FabricStyle::Damask { ground: ColourRef::rgb(1, 2, 3), figure: ColourRef::rgb(9, 9, 9), period: 0.05 },
    ] {
        let tile = fabric_tile(&fabric, 2, colour, |m| panic!("{m}")).unwrap().expect("a pattern paints a tile");
        assert!(!tile.list.is_empty());
        assert!(tile.px >= 16, "{fabric:?}: {} px", tile.px);
    }
    let bad = FabricStyle::Damask { ground: ColourRef::rgb(1, 2, 3), figure: ColourRef::rgb(9, 9, 9), period: 5.0 };
    assert!(bad.validate().is_err());
}

#[test]
fn the_parameters_are_checked() {
    assert!(RobeParams::default().validate().is_ok());
    for bad in [
        RobeParams { length: 1.5, ..RobeParams::default() },
        RobeParams { sleeves: RobeSleeves { bell: 0.5, ..RobeSleeves::default() }, ..RobeParams::default() },
        RobeParams { band: RobeBand { width: 0.5, ..RobeBand::default() }, ..RobeParams::default() },
        RobeParams { v_point: 0.05, ..RobeParams::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}
