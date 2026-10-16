//! The trousers garment (#195): the conformance suite on every template and shape for straight, tapered, flared, baggy
//! and short legs, the leg profile, the rise, the waistband and the cuff, its parameters, and the trousers over a shirt.
//! The posed checks (a squat, a high kick) are in `omuzaki-test-char/tests/outfit_trousers_posed.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;
#[path = "support/outfit_render.rs"]
mod render;

use omuzaki_char::outfit::common::{FabricStyle, LAYER_SHIRT, PlaidBand};
use omuzaki_char::outfit::trousers::shape::{LegProfile, Rise};
use omuzaki_char::outfit::trousers::{LAYER_TROUSERS, Trousers, TrousersParams};
use omuzaki_char_api::{BodyShape, BuildServices, PartOutput};
use omuzaki_face_api::{ColourRef, Validate};
use omuzaki_face_mock::MockRasterizer;
use serde_json::json;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

fn built(template: &str, params: &TrousersParams) -> PartOutput {
    let def = conformance::character(
        template,
        &BodyShape::default(),
        &[("garment", "trousers", LAYER_TROUSERS, serde_json::to_value(params).unwrap())],
    );
    conformance::build(def, &services()).expect("the trousers build").outputs.pop().expect("trousers").1
}

fn with_leg(leg: LegProfile) -> TrousersParams {
    TrousersParams { leg, ..TrousersParams::default() }
}

/// The lowest and highest y of the garment, and the widest |x| below `y` (the hem opening's reach).
fn extent(output: &PartOutput) -> (f32, f32) {
    let p = &output.meshes[0].geometry.positions;
    (p.iter().map(|v| v.y).fold(f32::MAX, f32::min), p.iter().map(|v| v.y).fold(f32::MIN, f32::max))
}

// ------------------------------------------------------------------------------------------------------ the suite

#[test]
fn straight_trousers_pass_the_suite_on_every_template_and_shape() {
    let rows = conformance::assert_conforms::<Trousers>(&TrousersParams::default(), &services());
    assert_eq!(rows.len(), 12, "4 templates x 3 shapes");
    for row in &rows {
        assert!(row.min_clearance_mm >= 0.5 * row.ease_mm, "{row:?}: half the ease at least");
    }
}

#[test]
fn tapered_flared_and_baggy_trousers_pass_too() {
    for (name, leg) in
        [("tapered", LegProfile::tapered()), ("flared", LegProfile::flared()), ("baggy", LegProfile::baggy())]
    {
        let rows = conformance::run::<Trousers>(&with_leg(leg), &services()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(rows.len(), 12, "{name}");
    }
}

#[test]
fn shorts_pass_too() {
    let params = TrousersParams { length: 0.60, leg: LegProfile::baggy(), ..TrousersParams::default() };
    conformance::assert_conforms::<Trousers>(&params, &services());
}

#[test]
fn cuffs_creases_pockets_a_high_rise_and_a_pattern_pass_too() {
    let mut params = TrousersParams {
        rise: Rise::High,
        crease: true,
        pockets: true,
        leg: LegProfile::tapered(),
        ..TrousersParams::default()
    };
    params.cuff.roll = 0.004;
    params.common.fabric = FabricStyle::Plaid {
        ground: ColourRef::rgb(0x2a, 0x3a, 0x5a),
        period: 0.03,
        bands: vec![PlaidBand { colour: ColourRef::rgb(0xc8, 0x30, 0x30), offset: 0.004, width: 0.006 }],
    };
    conformance::assert_conforms::<Trousers>(&params, &services());
    let low = TrousersParams { rise: Rise::Low, ..TrousersParams::default() };
    conformance::assert_conforms::<Trousers>(&low, &services());
}

// ------------------------------------------------------------------------------------------------------- the shape

#[test]
fn the_leg_profile_sets_how_wide_the_hem_is() {
    let reach = |leg: LegProfile| {
        let output = built("soft-sd", &with_leg(leg));
        let (floor, _) = extent(&output);
        // The hem: the vertices within 2 mm of the lowest row.
        let p = &output.meshes[0].geometry.positions;
        p.iter().filter(|v| v.y < floor + 0.002 && v.x > 0.0).map(|v| v.x).fold(0.0, f32::max)
            - p.iter().filter(|v| v.y < floor + 0.002 && v.x > 0.0).map(|v| v.x).fold(f32::MAX, f32::min)
    };
    let (tapered, straight, flared) =
        (reach(LegProfile::tapered()), reach(LegProfile::straight()), reach(LegProfile::flared()));
    assert!(tapered < straight && straight < flared, "hem widths {tapered} < {straight} < {flared}");
    assert!(flared > 1.4 * tapered, "a flared hem is much wider than a tapered one: {flared} vs {tapered}");
}

#[test]
fn shorts_end_higher_and_the_cuff_stands_out() {
    let (long_floor, _) = extent(&built("soft-sd", &TrousersParams::default()));
    let shorts = TrousersParams { length: 0.60, ..TrousersParams::default() };
    let (short_floor, _) = extent(&built("soft-sd", &shorts));
    assert!(short_floor > long_floor + 0.1, "shorts end {short_floor}, trousers {long_floor}");
    // A cuff adds rows and stands out of the leg by the roll at the hem.
    let plain = built("soft-sd", &TrousersParams::default());
    let mut cuffed_params = TrousersParams::default();
    cuffed_params.cuff.roll = 0.005;
    let cuffed = built("soft-sd", &cuffed_params);
    let count = |o: &PartOutput| o.meshes[0].geometry.positions.len();
    assert!(count(&cuffed) > count(&plain), "the cuff's ramp has rows of its own");
    let width_at_hem = |o: &PartOutput| {
        let p = &o.meshes[0].geometry.positions;
        let floor = p.iter().map(|v| v.y).fold(f32::MAX, f32::min);
        p.iter().filter(|v| v.y < floor + 1e-4 && v.x > 0.0).map(|v| v.x).fold(0.0, f32::max)
    };
    let (a, b) = (width_at_hem(&plain), width_at_hem(&cuffed));
    assert!((b - a - 0.005).abs() < 0.0015, "the hem stands {} mm out", (b - a) * 1e3);
}

#[test]
fn the_rise_moves_the_waist() {
    let top = |rise: Rise| extent(&built("soft-sd", &TrousersParams { rise, ..TrousersParams::default() })).1;
    let (low, mid, high) = (top(Rise::Low), top(Rise::Mid), top(Rise::High));
    assert!(low < mid && mid < high, "waist heights {low} < {mid} < {high}");
    // On soft-sd the torso runs 0.325..0.545 m: the mid rise is the waist, 0.32 of the way up.
    assert!((f64::from(mid) - (0.325 + 0.22 * Rise::Mid.height())).abs() < 5e-3, "{mid}");
}

#[test]
fn the_waistband_is_thicker_than_the_cloth_below_it() {
    let reach = |thickness: f64| {
        let mut params = TrousersParams::default();
        params.waistband.thickness = thickness;
        let output = built("soft-sd", &params);
        let p = &output.meshes[0].geometry.positions;
        let top = p.iter().map(|v| v.y).fold(f32::MIN, f32::max);
        p.iter().filter(|v| v.y > top - 1e-4).map(|v| v.x).fold(0.0, f32::max)
    };
    let (flat, banded) = (reach(0.0), reach(0.006));
    assert!(banded - flat > 0.003, "the band stands {} mm out", (banded - flat) * 1e3);
}

#[test]
fn creases_and_pockets_add_closed_cords() {
    let count = |o: &PartOutput| o.meshes.len();
    let plain = built("soft-sd", &TrousersParams::default());
    let lined = built("soft-sd", &TrousersParams { crease: true, pockets: true, ..TrousersParams::default() });
    assert_eq!(count(&lined), count(&plain) + 4, "a crease and a pocket line each side");
    assert!(lined.meshes.iter().any(|m| m.name == "garment-crease-l" && m.material.as_str().contains("crease-l")));
}

// ------------------------------------------------------------------------------------------------------- stacking

#[test]
fn trousers_stand_over_a_shirt() {
    let trousers = serde_json::to_value(TrousersParams::default()).unwrap();
    for template in ["soft-sd", "standard"] {
        let def = conformance::character(
            template,
            &BodyShape::default(),
            &[
                ("shirt", "mock-garment", LAYER_SHIRT, json!({ "ease": 0.004 })),
                ("trousers", "trousers", LAYER_TROUSERS, trousers.clone()),
            ],
        );
        let stack = conformance::build(def, &services()).unwrap();
        let shirt = stack.before("trousers").outer_fit().expect("the shirt's envelope");
        let g = &stack.output("trousers").meshes[0].geometry;
        // The suite's tools rebuild the fit's tables per call: a spread of vertices and triangles.
        for p in g.positions.iter().step_by(g.positions.len() / 40) {
            assert!(!conformance::inside(shirt.as_ref(), p.as_dvec3()), "{template}: a vertex is inside the shirt");
        }
        for tri in g.indices.chunks_exact(3).step_by(g.indices.len() / 3 / 40) {
            let corners = [0, 1, 2].map(|c| g.positions[tri[c] as usize].as_dvec3());
            assert!(
                conformance::crossing(shirt.as_ref(), corners).is_none(),
                "{template}: a triangle reaches the shirt"
            );
        }
    }
}

// ------------------------------------------------------------------------------------------------------ parameters

#[test]
fn parameters_are_checked_and_round_trip() {
    let params = TrousersParams::default();
    assert!(params.validate().is_ok());
    let again: TrousersParams = serde_json::from_value(serde_json::to_value(&params).unwrap()).unwrap();
    assert_eq!(again, params);
    assert!(serde_json::from_value::<TrousersParams>(json!({ "legs": {} })).is_err(), "a typo fails");
    let bad = |edit: &dyn Fn(&mut TrousersParams)| {
        let mut p = TrousersParams::default();
        edit(&mut p);
        p.validate().expect_err("refused").to_string()
    };
    assert!(bad(&|p| p.leg.hem = 0.5).contains("hem"), "narrower than the body");
    assert!(bad(&|p| p.leg.knee = f64::NAN).contains("knee"));
    assert!(bad(&|p| p.length = 1.2).contains("length"));
    assert!(bad(&|p| p.length = 0.2).contains("length"));
    assert!(bad(&|p| p.waistband.height = 0.5).contains("waistband"));
    assert!(bad(&|p| p.cuff.roll = -0.001).contains("cuff"));
    assert!(bad(&|p| p.common.ease = 0.0).contains("ease"));
}

#[test]
fn a_hem_above_the_crotch_is_refused_naming_the_shortest() {
    let def = conformance::character(
        "soft-sd",
        &BodyShape::default(),
        &[(
            "garment",
            "trousers",
            LAYER_TROUSERS,
            serde_json::to_value(TrousersParams { length: 0.55, ..TrousersParams::default() }).unwrap(),
        )],
    );
    let error = conformance::build(def, &services()).err().expect("too short for this body").to_string();
    assert!(error.contains("ends above the crotch") && error.contains("use at least"), "{error}");
}

#[test]
fn the_build_is_deterministic() {
    let params = TrousersParams { crease: true, ..TrousersParams::default() };
    let (a, b) = (built("tall", &params), built("tall", &params));
    for (x, y) in a.meshes.iter().zip(&b.meshes) {
        assert_eq!(x.geometry.positions, y.geometry.positions);
        assert_eq!(x.geometry.indices, y.geometry.indices);
    }
}

// ----------------------------------------------------------------------------------------------------- the pictures

/// Writes pictures of the trousers from four sides to the folder `OMUZAKI_TROUSERS_PPM` names (nothing without it): a
/// straight pair on every template, and a tapered, a flared, a baggy cuffed and a short pair on the standard body. Taste
/// is judged on the pictures, not on numbers.
#[test]
fn render_the_trousers_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_TROUSERS_PPM") else { return };
    let mut cuffed = TrousersParams { leg: LegProfile::baggy(), crease: true, ..TrousersParams::default() };
    cuffed.cuff.roll = 0.004;
    let straight = TrousersParams { crease: true, pockets: true, ..TrousersParams::default() };
    let cases = conformance::TEMPLATES.map(|template| (template, "straight", straight.clone())).into_iter().chain([
        ("standard", "tapered", with_leg(LegProfile::tapered())),
        ("standard", "flared", with_leg(LegProfile::flared())),
        ("standard", "baggy-cuffed", cuffed),
        ("standard", "shorts", TrousersParams { length: 0.60, ..TrousersParams::default() }),
    ]);
    for (template, variant, params) in cases {
        let garments = [("trousers", "trousers", LAYER_TROUSERS, serde_json::to_value(&params).unwrap())];
        let def = conformance::character(template, &BodyShape::default(), &garments);
        let stack = conformance::build(def, &services()).unwrap();
        let framing = render::Framing::parts(&stack, &["trousers"]);
        render::render(&stack, framing, &format!("{template}-{variant}"), directory.as_ref());
    }
}
