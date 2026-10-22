//! The `legwear` garment (#193): the conformance suite on every template and shape for each style, what the styles
//! cover, the prototype's rows, weights and vertex colours, and the parameter checks.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::outfit::common::{FabricStyle, Garment, StripeDirection};
use omuzaki_char::outfit::legwear::{
    COLUMNS, LAYER_LEGWEAR, LegTop, Legwear, LegwearParams, LegwearStyle, rows_of_leg,
};
use omuzaki_char_api::{BuildServices, FitFact, JointRef, PartKind};
use omuzaki_face_api::{ColourRef, Validate};
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

fn params(style: LegwearStyle, top: Option<LegTop>) -> LegwearParams {
    LegwearParams { style, top, ..LegwearParams::default() }
}

#[test]
fn tights_conform_on_every_template_and_shape() {
    let rows = conformance::assert_conforms::<Legwear>(&LegwearParams::default(), &services());
    assert_eq!(rows.len(), 12);
}

#[test]
fn stockings_conform_on_every_template_and_shape() {
    conformance::assert_conforms::<Legwear>(&params(LegwearStyle::Stockings, None), &services());
}

#[test]
fn leggings_conform_on_every_template_and_shape() {
    conformance::assert_conforms::<Legwear>(&params(LegwearStyle::Leggings, None), &services());
}

#[test]
fn knee_highs_conform_on_every_template_and_shape() {
    conformance::assert_conforms::<Legwear>(&params(LegwearStyle::Tights, Some(LegTop::Knee)), &services());
}

#[test]
fn the_rows_are_the_prototypes() {
    // 8 + 28 segments: 37 body rows, hip to ankle, as fractions of the leg.
    let u = rows_of_leg();
    assert_eq!(u.len(), 37);
    assert!((u[0] - 0.02 / 0.38).abs() < 1e-12 && (u[36] - 1.0).abs() < 1e-12);
    assert_eq!(COLUMNS, 45);
}

/// The suite's table for the default tights, for the evidence.
fn built(template: &str, p: &LegwearParams) -> conformance::Stack {
    let value = serde_json::to_value(p).unwrap();
    let def = conformance::character(
        template,
        &omuzaki_char_api::BodyShape::default(),
        &[("legwear", "legwear", LAYER_LEGWEAR, value)],
    );
    conformance::build(def, &services()).unwrap()
}

fn top_of(stack: &conformance::Stack) -> f64 {
    let g = &stack.output("legwear").meshes[0].geometry;
    g.positions.iter().map(|p| f64::from(p.y)).fold(f64::NEG_INFINITY, f64::max)
}

#[test]
fn stockings_leave_the_thigh_above_their_top_bare_and_tights_do_not() {
    for template in conformance::TEMPLATES {
        let tights = top_of(&built(template, &params(LegwearStyle::Tights, None)));
        let stockings = top_of(&built(template, &params(LegwearStyle::Stockings, None)));
        let knee = top_of(&built(template, &params(LegwearStyle::Stockings, Some(LegTop::Knee))));
        assert!(stockings < tights - 0.01, "{template}: stockings top {stockings} vs tights {tights}");
        assert!(knee < stockings - 0.01, "{template}: knee-high top {knee} vs thigh-high {stockings}");
    }
}

#[test]
fn the_weights_hand_over_from_the_hips_to_the_thigh_to_the_shin() {
    let stack = built("soft-sd", &LegwearParams::default());
    let mesh = &stack.output("legwear").meshes[0];
    let g = &mesh.geometry;
    let share = |v: usize, joint: &JointRef| -> f32 {
        (0..4).filter(|&k| &mesh.joint_bones[usize::from(g.joints[v][k])] == joint).map(|k| g.weights[v][k]).sum()
    };
    let (upper, lower) = (
        JointRef::Humanoid(omuzaki_anim_api::HumanoidBone::LeftUpperLeg),
        JointRef::Humanoid(omuzaki_anim_api::HumanoidBone::LeftLowerLeg),
    );
    for v in 0..g.positions.len() {
        let total: f32 = g.weights[v].iter().sum();
        assert!((total - 1.0).abs() < 1e-5, "vertex {v}: weights sum to {total}");
        let y = g.positions[v].y;
        let left = v < g.positions.len() / 2;
        if left && y < 0.15 {
            assert!(share(v, &lower) > 0.99, "vertex {v} at y = {y} is on the shin");
        }
        if left && (0.22..0.30).contains(&y) {
            assert!(
                share(v, &upper) > 0.99 || share(v, &lower) > 0.0,
                "vertex {v} at y = {y} is on the thigh or the knee"
            );
        }
    }
}

#[test]
fn the_vertex_colour_darkens_the_inner_thigh_and_the_back_of_the_knee() {
    let stack = built("soft-sd", &LegwearParams::default());
    let g = &stack.output("legwear").meshes[0].geometry;
    let grey: Vec<f32> = g.colours.iter().map(|c| c[0]).collect();
    assert!(grey.iter().all(|&k| (0.84..=1.0).contains(&k)), "the darkening is mild");
    let darkest = grey.iter().copied().fold(1.0, f32::min);
    assert!(darkest <= 0.9 + 1e-6, "the inner thigh reaches 0.10: {darkest}");
    // The back of the knee (z behind the leg's centre, at the crease) is darker than the front of it.
    let at = |back: bool| {
        (0..g.positions.len())
            .filter(|&v| g.positions[v].x > 0.04 && (g.positions[v].y - 0.235).abs() < 0.006)
            .filter(|&v| (g.positions[v].z < 0.0) == back)
            .map(|v| grey[v])
            .fold(f32::INFINITY, f32::min)
    };
    assert!(at(true) < at(false), "behind the knee is darker");
}

#[test]
fn the_envelope_is_the_ease_and_the_thickness_outside_the_legs() {
    let stack = built("standard", &LegwearParams::default());
    let before = stack.before("legwear").get::<FitFact>().fits.last().unwrap().clone();
    let after = stack.published.get::<FitFact>().fits.last().unwrap().clone();
    let p = LegwearParams::default();
    let (a, b) = (
        before.section(omuzaki_char_api::BodyRegion::LegL, 0.5).unwrap(),
        after.section(omuzaki_char_api::BodyRegion::LegL, 0.5).unwrap(),
    );
    let grown = f64::from(b.radii.x - a.radii.x);
    assert!((grown - (p.common.ease + p.thickness)).abs() < 1e-6, "grew by {grown}");
}

#[test]
fn the_parameters_are_checked() {
    assert!(LegwearParams::default().validate().is_ok());
    let bad = |edit: &dyn Fn(&mut LegwearParams)| {
        let mut p = LegwearParams::default();
        edit(&mut p);
        p.validate().is_err()
    };
    assert!(bad(&|p| p.thickness = -0.001));
    assert!(bad(&|p| p.thickness = f64::NAN));
    assert!(bad(&|p| p.sheen = 3.0));
    assert!(bad(&|p| p.common.ease = 0.5));
    assert!(bad(&|p| p.common.fabric = FabricStyle::Stripes {
        ground: ColourRef::rgb(0, 0, 0),
        stripe: ColourRef::rgb(255, 255, 255),
        period: 0.01,
        width: 0.005,
        direction: StripeDirection::Vertical,
    }));
}

#[test]
fn the_kind_is_registered_with_its_layer_and_requirements() {
    let meta = Legwear::meta();
    assert_eq!((meta.id, meta.requires), ("legwear", &["body"][..]));
    assert!((10..=15).contains(&LAYER_LEGWEAR));
    assert_eq!(<Legwear as Garment>::open_borders(&LegwearParams::default()), 4);
}
