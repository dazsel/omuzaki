//! The soft-sd body template: parity of its default layout with the prototype's numbers (the fixture in
//! `fixtures/template/` was produced by running `skeleton.js`, `uniform-dims.js` and `body.js` under Node with three.js;
//! the generator script is not published), the slider and body-shape rules, and the guarantees every layout keeps at the
//! extremes: bones with a length, soles on the floor, left/right symmetry, finite and continuous sections.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers load fixtures and look up bones that must exist
#![allow(clippy::cast_possible_truncation)] // sample fractions are narrowed to the `f32` the `BodyFit` trait takes
#![allow(clippy::cast_precision_loss)] // small loop counters
#![allow(clippy::similar_names, clippy::many_single_char_names)] // rx/ry/rz are the prototype's names; f, l, t, s are the fixture, layout, fraction and section

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_anim_api::{HUMANOID_COUNT, HumanoidBone};
use omuzaki_char::kit::KitError;
use omuzaki_char::kit::js_names;
use omuzaki_char::template::layout::{self, BodyLayout, BodyTable, HandFrame, RegionSection, section_basis};
use omuzaki_char::template::soft_sd::{self, SoftSd, TABLE};
use omuzaki_char::template::{Template, registry};
use omuzaki_char_api::{BodyFit, BodyRegion, BodyShape, HandParams, HeadParams, JointRef, Proportions, Section};
use omuzaki_core::glam::{DVec3, Vec3};
use proptest::prelude::*;
use serde::Deserialize;

// ------------------------------------------------------------------------------------------------ the fixture

#[derive(Deserialize)]
struct Torso {
    y: Vec<f64>,
    rx: Vec<f64>,
    rz: Vec<f64>,
    cz: Vec<f64>,
}

#[derive(Deserialize)]
struct Leg {
    y: Vec<f64>,
    cx: Vec<f64>,
    rx: Vec<f64>,
    rz: Vec<f64>,
    cz: Vec<f64>,
}

#[derive(Deserialize)]
struct Arm {
    shoulder: [f64; 3],
    wrist: [f64; 3],
    s: Vec<f64>,
    points: Vec<[f64; 3]>,
}

#[derive(Deserialize)]
struct Hand {
    shift: f64,
    origin: [f64; 3],
    axis: [f64; 3],
    palm: [f64; 3],
    thumb: [f64; 3],
}

#[derive(Deserialize)]
struct Weights {
    y: Vec<f64>,
    w: Vec<f64>,
}

#[derive(Deserialize)]
struct Fixture {
    joints: BTreeMap<String, [f64; 3]>,
    torso: Torso,
    leg: Leg,
    arm: Arm,
    hand: Hand,
    weight_hips: Weights,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/template/soft_sd.json")).unwrap()
}

/// The default body before the blend at the shoulder and the hip: the loft the prototype's numbers describe.
fn default_layout() -> BodyLayout {
    layout::build_loft(&TABLE, &Proportions::default(), &BodyShape::default(), &HandParams::default())
        .expect("the soft-sd table is valid")
}

fn v(a: [f64; 3]) -> DVec3 {
    DVec3::from_array(a)
}

fn sample(l: &BodyLayout, region: BodyRegion, t: f64) -> RegionSection {
    l.section64(region, t).expect("the soft-sd layout has every region")
}

// ------------------------------------------------------------------------------------------------ parity

#[test]
fn default_joints_reproduce_the_prototypes_skeleton() {
    let f = fixture();
    let l = default_layout();
    let (mut worst, mut worst32) = (0.0_f64, 0.0_f64);
    for (name, expected) in &f.joints {
        let joint = js_names::joint(name).unwrap();
        let (exact, narrow) = if name == "head" {
            (l.head_base().unwrap(), l.head_base().unwrap())
        } else {
            (
                l.joint64(match joint {
                    JointRef::Humanoid(b) => b,
                    JointRef::Named(_) => panic!("{name} is a humanoid bone"),
                })
                .unwrap(),
                l.joint(&joint).unwrap().as_dvec3(),
            )
        };
        worst = worst.max(exact.distance(v(*expected)));
        worst32 = worst32.max(narrow.distance(v(*expected)));
    }
    eprintln!(
        "joints: {} compared, worst difference {worst:e} m (f64), {worst32:e} m (through BodyFit)",
        f.joints.len()
    );
    assert_eq!(f.joints.len(), 19);
    assert!(worst < 1e-9 && worst32 < 1e-6);
}

#[test]
fn default_torso_curves_reproduce_the_prototype_at_200_heights() {
    let f = fixture();
    let l = default_layout();
    let (bottom, top) = (sample(&l, BodyRegion::Torso, 0.0).centre.y, sample(&l, BodyRegion::Torso, 1.0).centre.y);
    assert!((bottom - 0.325).abs() < 1e-12 && (top - 0.545).abs() < 1e-12, "{bottom} {top}");
    let mut worst = 0.0_f64;
    let mut worst32 = 0.0_f64;
    for (i, &y) in f.torso.y.iter().enumerate() {
        let t = (y - bottom) / (top - bottom);
        let s = sample(&l, BodyRegion::Torso, t);
        assert!((s.centre.y - y.clamp(bottom, top)).abs() < 1e-9, "the axis is vertical");
        worst = worst.max((s.radii.x - f.torso.rx[i]).abs()).max((s.radii.y - f.torso.rz[i]).abs());
        worst = worst.max((s.centre.z - f.torso.cz[i]).abs()).max(s.centre.x.abs());
        let n = l.section(BodyRegion::Torso, t.clamp(0.0, 1.0) as f32).unwrap();
        worst32 = worst32
            .max(f64::from((n.radii.x - f.torso.rx[i] as f32).abs()))
            .max(f64::from((n.radii.y - f.torso.rz[i] as f32).abs()))
            .max(f64::from((n.centre.z - f.torso.cz[i] as f32).abs()));
    }
    eprintln!("TORSO: 200 heights, 3 curves: worst difference {worst:e} m (f64), {worst32:e} m (through BodyFit)");
    assert!(worst < 1e-9 && worst32 < 1e-6);
}

#[test]
fn default_leg_curves_reproduce_the_prototype_at_200_heights_for_both_legs() {
    let f = fixture();
    let l = default_layout();
    let (top, bottom) = (sample(&l, BodyRegion::LegL, 0.0).centre.y, sample(&l, BodyRegion::LegL, 1.0).centre.y);
    assert!((top - 0.42).abs() < 1e-12 && (bottom - 0.04).abs() < 1e-12, "{top} {bottom}");
    let mut worst = 0.0_f64;
    let mut worst32 = 0.0_f64;
    for (i, &y) in f.leg.y.iter().enumerate() {
        let t = (top - y) / (top - bottom);
        for (region, side) in [(BodyRegion::LegL, 1.0), (BodyRegion::LegR, -1.0)] {
            let s = sample(&l, region, t);
            worst = worst
                .max((s.centre.x - side * f.leg.cx[i]).abs())
                .max((s.centre.z - f.leg.cz[i]).abs())
                .max((s.radii.x - f.leg.rx[i]).abs())
                .max((s.radii.y - f.leg.rz[i]).abs());
            let n = l.section(region, t.clamp(0.0, 1.0) as f32).unwrap();
            worst32 = worst32
                .max(f64::from((n.centre.x - (side * f.leg.cx[i]) as f32).abs()))
                .max(f64::from((n.radii.x - f.leg.rx[i] as f32).abs()))
                .max(f64::from((n.radii.y - f.leg.rz[i] as f32).abs()));
        }
    }
    eprintln!(
        "LEG: 200 heights x 4 curves x 2 legs: worst difference {worst:e} m (f64), {worst32:e} m (through BodyFit)"
    );
    assert!(worst < 1e-9 && worst32 < 1e-6);
}

#[test]
fn the_arms_centre_line_is_the_prototypes_shoulder_elbow_wrist_path() {
    let f = fixture();
    let l = default_layout();
    let length = l.region(BodyRegion::ArmL).unwrap().length();
    let end = *f.arm.s.last().unwrap();
    assert!((length - end).abs() < 1e-9, "{length} against {end}");
    let mut worst = 0.0_f64;
    for (s, p) in f.arm.s.iter().zip(&f.arm.points) {
        let left = sample(&l, BodyRegion::ArmL, s / end).centre;
        let right = sample(&l, BodyRegion::ArmR, s / end).centre;
        worst = worst.max(left.distance(v(*p))).max(right.distance(v(*p) * DVec3::new(-1.0, 1.0, 1.0)));
    }
    eprintln!("ARM: 200 points along shoulder-elbow-wrist, both arms: worst difference {worst:e} m");
    assert!(worst < 1e-9);
    assert!(sample(&l, BodyRegion::ArmL, 0.0).centre.distance(v(f.arm.shoulder)) < 1e-9);
    assert!(sample(&l, BodyRegion::ArmL, 1.0).centre.distance(v(f.arm.wrist)) < 1e-9);
}

#[test]
fn the_hip_zone_and_the_hand_frame_match_the_prototype() {
    let f = fixture();
    let worst = f
        .weight_hips
        .y
        .iter()
        .zip(&f.weight_hips.w)
        .map(|(&y, &w)| (soft_sd::weight_hips(y) - w).abs())
        .fold(0.0, f64::max);
    eprintln!("weightHips: 200 heights: worst difference {worst:e}");
    assert!(worst < 1e-12);

    let l = default_layout();
    let wrist = l.joint64(HumanoidBone::LeftHand).unwrap();
    let elbow = l.joint64(HumanoidBone::LeftLowerArm).unwrap();
    let frame = HandFrame::of(wrist - elbow, TABLE.hand.palm_hint, TABLE.hand.thumb_hint);
    let origin = wrist + frame.axis * TABLE.hand.shift;
    let worst = [
        frame.axis.distance(v(f.hand.axis)),
        frame.palm.distance(v(f.hand.palm)),
        frame.thumb.distance(v(f.hand.thumb)),
        origin.distance(v(f.hand.origin)),
        (TABLE.hand.shift - f.hand.shift).abs(),
    ]
    .into_iter()
    .fold(0.0, f64::max);
    eprintln!("handFrame: A, N, T and O: worst difference {worst:e}");
    assert!(worst < 1e-9);
}

// ------------------------------------------------------------------------------------------------ the template

#[test]
fn the_template_registers_itself_and_declares_what_it_is() {
    let t = registry::find("soft-sd").unwrap();
    assert_eq!((t.id(), t.heads_tall()), ("soft-sd", 2.5));
    assert!(t.summary().contains("chibi"));
    assert!(registry::templates().iter().any(|t| t.id() == "soft-sd"));
    assert_eq!(t.defaults(), SoftSd.defaults());
    assert_eq!(t.defaults().body.proportions, Proportions::default());
}

#[test]
fn the_declared_height_in_heads_matches_the_default_head_within_3_percent() {
    let c = HeadParams::default().cranium;
    let (crown, chin) = (c.centre[1] + c.half_height_up, c.centre[1] - c.half_height_down_front);
    let l = default_layout();
    let sole = sample(&l, BodyRegion::FootL, 0.5).centre.y - sample(&l, BodyRegion::FootL, 0.5).radii.y;
    let measured = (crown - sole) / (crown - chin);
    eprintln!("heads tall: measured {measured:.3} against the declared {}", SoftSd.heads_tall());
    assert!((measured / SoftSd.heads_tall() - 1.0).abs() < 0.03, "{measured}");
    // The head sits on the neck: the head base is below the chin and the cranium default is where the layout expects.
    assert!(l.head_base().unwrap().y < chin);
}

/// A template whose table has a mistake in it.
struct Broken;
impl Template for Broken {
    fn id(&self) -> &'static str {
        "broken"
    }
    fn summary(&self) -> &'static str {
        "A template whose table is wrong."
    }
    fn heads_tall(&self) -> f64 {
        2.0
    }
    fn default_stature_m(&self) -> f64 {
        0.692
    }
    fn try_layout(&self, p: &Proportions, s: &BodyShape, h: &HandParams) -> Result<BodyLayout, KitError> {
        let mut table = TABLE;
        table.torso.rx = &[];
        layout::build(&table, p, s, h)
    }
    fn defaults(&self) -> omuzaki_char::template::TemplateDefaults {
        omuzaki_char::template::TemplateDefaults::default()
    }
}

#[test]
fn a_broken_table_is_an_error_and_a_template_that_cannot_build_has_no_layout() {
    let mut table = TABLE;
    table.torso.rx = &[(0.4, 0.08), (0.4, 0.09)];
    let (p, s, h) = (Proportions::default(), BodyShape::default(), HandParams::default());
    assert!(matches!(layout::build(&table, &p, &s, &h), Err(KitError::DuplicateKnot { .. })));

    assert!(Broken.try_layout(&p, &s, &h).is_err());
    let none = Broken.layout(&p, &s, &h);
    assert!(none.section(BodyRegion::Torso, 0.5).is_none() && none.joint(&JointRef::HIPS).is_none());
    assert_eq!(none.joints().count(), 0);
}

// ------------------------------------------------------------------------------------------------ guarantees

/// Every rule a layout keeps, whatever the numbers: panics with `what` and the broken rule.
fn assert_sound(l: &BodyLayout, what: &str) {
    // The body owns 51 bones; the head part owns the other four.
    let owned: Vec<_> = l.joints().map(|(b, _)| b).collect();
    assert_eq!(owned.len(), HUMANOID_COUNT - 4, "{what}");
    for head_bone in [HumanoidBone::Head, HumanoidBone::LeftEye, HumanoidBone::RightEye, HumanoidBone::Jaw] {
        assert!(l.joint64(head_bone).is_none(), "{what}: {head_bone} belongs to the head part");
    }
    for (bone, p) in l.joints() {
        assert!(p.is_finite(), "{what}: {bone} is not finite");
        // Every bone has a length, and the two sides mirror each other.
        if let Some(parent) = bone.parent() {
            let from = l.joint64(parent).unwrap();
            assert!(p.distance(from) > 1e-6, "{what}: {bone} has no length from {parent}");
        }
        let other = l.joint64(bone.mirrored()).unwrap();
        assert!(other.distance(DVec3::new(-p.x, p.y, p.z)) < 1e-6, "{what}: {bone} is not mirrored");
    }
    let (neck, head) = (l.joint64(HumanoidBone::Neck).unwrap(), l.head_base().unwrap());
    assert!(head.y > neck.y + 1e-6 && head.x == 0.0, "{what}: the neck bone has a length");

    // Feet rest on y = 0, the toes are above it.
    for region in [BodyRegion::FootL, BodyRegion::FootR] {
        for k in 0..=20 {
            let s = sample(l, region, f64::from(k) / 20.0);
            assert!((s.centre.y - s.radii.y).abs() < 1e-12, "{what}: {region:?} sole is at {}", s.centre.y - s.radii.y);
        }
    }
    for toes in [HumanoidBone::LeftToes, HumanoidBone::RightToes] {
        assert!(l.joint64(toes).unwrap().y > 0.0, "{what}: the toes are above the floor");
    }

    // Sections are finite, have positive radii and unit axes, mirror, and change continuously.
    for region in ALL_REGIONS {
        let mut last: Option<RegionSection> = None;
        for k in 0..=400 {
            let s = sample(l, region, f64::from(k) / 400.0);
            assert!(s.centre.is_finite() && s.axis.is_finite() && s.radii.is_finite(), "{what}: {region:?} at {k}");
            assert!(s.radii.min_element() > 0.0, "{what}: {region:?} has a radius {:?} at {k}", s.radii);
            assert!((s.axis.length() - 1.0).abs() < 1e-9, "{what}: {region:?} axis");
            if let Some(p) = last {
                let jump = p.centre.distance(s.centre).max((p.radii - s.radii).abs().max_element());
                assert!(jump < 0.01 && p.axis.dot(s.axis) > 0.99, "{what}: {region:?} jumps by {jump} at {k}");
            }
            last = Some(s);
            if let Some(twin) = mirror_of(region) {
                let m = sample(l, twin, f64::from(k) / 400.0);
                assert!(
                    m.centre.distance(DVec3::new(-s.centre.x, s.centre.y, s.centre.z)) < 1e-6,
                    "{what}: {region:?}"
                );
                assert!((m.radii - s.radii).abs().max_element() < 1e-6, "{what}: {region:?}");
            }
        }
    }
    // The neck begins exactly where the torso ends.
    let (top, base) = (sample(l, BodyRegion::Torso, 1.0), sample(l, BodyRegion::Neck, 0.0));
    assert!(top.centre.distance(base.centre) < 1e-9 && (top.radii - base.radii).abs().max_element() < 1e-9, "{what}");
    assert_continuous_at_joints(l, what);
}

const ALL_REGIONS: [BodyRegion; 8] = [
    BodyRegion::Torso,
    BodyRegion::Neck,
    BodyRegion::ArmL,
    BodyRegion::ArmR,
    BodyRegion::LegL,
    BodyRegion::LegR,
    BodyRegion::FootL,
    BodyRegion::FootR,
];

fn mirror_of(region: BodyRegion) -> Option<BodyRegion> {
    match region {
        BodyRegion::ArmL => Some(BodyRegion::ArmR),
        BodyRegion::LegL => Some(BodyRegion::LegR),
        BodyRegion::FootL => Some(BodyRegion::FootR),
        _ => None,
    }
}

/// A section just before and just after a joint of its region are the same section (C0), where the axis turns (the
/// elbow) or the curves change meaning (a knot at a joint).
fn assert_continuous_at_joints(l: &BodyLayout, what: &str) {
    let on_axis: [(BodyRegion, &[HumanoidBone]); 3] = [
        (
            BodyRegion::Torso,
            &[
                HumanoidBone::Hips,
                HumanoidBone::Spine,
                HumanoidBone::Chest,
                HumanoidBone::UpperChest,
                HumanoidBone::Neck,
            ],
        ),
        (BodyRegion::ArmL, &[HumanoidBone::LeftLowerArm]),
        (BodyRegion::LegL, &[HumanoidBone::LeftUpperLeg, HumanoidBone::LeftLowerLeg, HumanoidBone::LeftFoot]),
    ];
    for (region, bones) in on_axis {
        let r = l.region(region).unwrap();
        let start = sample(l, region, 0.0).centre;
        for j in bones.iter().map(|b| l.joint64(*b).unwrap()) {
            let t = start.distance(j) / r.length();
            let (a, b) = (sample(l, region, t - 1e-9), sample(l, region, t + 1e-9));
            let gap =
                a.centre.distance(b.centre).max((a.radii - b.radii).abs().max_element()).max(a.axis.distance(b.axis));
            assert!(gap < 1e-6, "{what}: {region:?} breaks by {gap} at the joint {j}");
        }
    }
}

#[test]
fn the_default_layout_is_sound() {
    assert_sound(&default_layout(), "default");
}

#[test]
fn every_proportion_slider_at_both_ends_keeps_the_layout_sound() {
    // All 2^8 corners of the proportion space, in the identity shape and with the default hands.
    for corner in 0..256_u32 {
        let pick = |bit: u32| if corner >> bit & 1 == 1 { 2.0 } else { 0.5 };
        let p = Proportions {
            neck_length: pick(0),
            torso_length: pick(1),
            shoulder_width: pick(2),
            hip_width: pick(3),
            arm_length: pick(4),
            leg_length: pick(5),
            hand_size: pick(6),
            foot_size: pick(7),
        };
        let l = SoftSd.layout(&p, &BodyShape::default(), &HandParams::default());
        assert_sound(&l, &format!("proportions {p:?}"));
    }
}

#[test]
fn every_body_shape_corner_keeps_the_layout_sound() {
    // All 3^5 combinations of each axis at -1, 0 and 1, with the proportions at their extremes too.
    for combo in 0..243_u32 {
        let axis = |i: u32| f64::from((combo / 3_u32.pow(i)) % 3) - 1.0;
        let shape = BodyShape {
            height: axis(0),
            build: axis(1),
            curves: axis(2),
            musculature: axis(3),
            shoulder_hip: axis(4),
            preset: None,
        };
        for k in [0.5, 1.0, 2.0] {
            let p = Proportions {
                torso_length: k,
                leg_length: k,
                shoulder_width: k,
                hip_width: k,
                ..Proportions::default()
            };
            assert_sound(&SoftSd.layout(&p, &shape, &HandParams::default()), &format!("{shape:?} {k}"));
        }
    }
}

fn proportions() -> impl Strategy<Value = Proportions> {
    let k = 0.5_f64..=2.0;
    (k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), k).prop_map(
        |(a, b, c, d, e, f, g, h)| Proportions {
            neck_length: a,
            torso_length: b,
            shoulder_width: c,
            hip_width: d,
            arm_length: e,
            leg_length: f,
            hand_size: g,
            foot_size: h,
        },
    )
}

fn shape() -> impl Strategy<Value = BodyShape> {
    let a = -1.0_f64..=1.0;
    (a.clone(), a.clone(), a.clone(), a.clone(), a).prop_map(|(height, build, curves, musculature, shoulder_hip)| {
        BodyShape { height, build, curves, musculature, shoulder_hip, preset: None }
    })
}

fn hand() -> impl Strategy<Value = HandParams> {
    let k = 0.5_f64..=2.0;
    (k.clone(), k.clone(), k.clone(), k.clone(), k.clone(), -30.0_f64..=30.0, k).prop_map(
        |(palm_width, palm_length, finger_length, finger_thickness, thumb_length, thumb_spread_deg, taper)| {
            HandParams {
                palm_width,
                palm_length,
                finger_length,
                finger_thickness,
                thumb_length,
                thumb_spread_deg,
                taper,
            }
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn any_valid_numbers_give_a_sound_layout(p in proportions(), s in shape(), h in hand()) {
        assert_sound(&SoftSd.layout(&p, &s, &h), &format!("{p:?} {s:?} {h:?}"));
    }

    #[test]
    fn layouts_are_deterministic(p in proportions(), s in shape(), h in hand()) {
        let (a, b) = (SoftSd.layout(&p, &s, &h), SoftSd.layout(&p, &s, &h));
        for region in ALL_REGIONS {
            prop_assert_eq!(sample(&a, region, 0.37), sample(&b, region, 0.37));
        }
        prop_assert_eq!(a.joints().collect::<Vec<_>>(), b.joints().collect::<Vec<_>>());
    }
}

#[test]
fn nan_and_out_of_range_numbers_still_build_a_sound_layout() {
    let wild = Proportions {
        neck_length: f64::NAN,
        torso_length: 50.0,
        hip_width: -3.0,
        leg_length: f64::INFINITY,
        ..Proportions::default()
    };
    let shape = BodyShape {
        height: f64::NAN,
        build: 9.0,
        curves: -9.0,
        musculature: f64::INFINITY,
        shoulder_hip: 4.0,
        preset: None,
    };
    let hand = HandParams { thumb_spread_deg: 400.0, palm_width: f64::NAN, ..HandParams::default() };
    assert_sound(&SoftSd.layout(&wild, &shape, &hand), "wild numbers");
}

// ------------------------------------------------------------------------------------------------ sliders

fn with(p: &Proportions) -> BodyLayout {
    SoftSd.layout(p, &BodyShape::default(), &HandParams::default())
}

fn joint(l: &BodyLayout, b: HumanoidBone) -> DVec3 {
    l.joint64(b).unwrap()
}

#[test]
fn neck_and_torso_length_stretch_along_y_and_keep_girth() {
    let base = default_layout();
    let neck = with(&Proportions { neck_length: 2.0, ..Proportions::default() });
    let lift = neck.head_base().unwrap().y - base.head_base().unwrap().y;
    assert!((lift - 0.04).abs() < 1e-12, "the neck bone doubles: {lift}");
    assert_eq!(joint(&neck, HumanoidBone::Chest), joint(&base, HumanoidBone::Chest), "only things above the neck move");
    let (a, b) = (sample(&base, BodyRegion::Neck, 0.5), sample(&neck, BodyRegion::Neck, 0.5));
    assert_eq!(a.radii.x, b.radii.x, "the neck is longer, not wider");

    let torso = with(&Proportions { torso_length: 1.5, ..Proportions::default() });
    for bone in [HumanoidBone::Spine, HumanoidBone::Chest, HumanoidBone::Neck] {
        let (t, d) = (joint(&torso, bone), joint(&base, bone));
        let hips = joint(&base, HumanoidBone::Hips).y;
        assert!((t.y - hips - 1.5 * (d.y - hips)).abs() < 1e-12, "{bone}");
    }
    assert_eq!(joint(&torso, HumanoidBone::Hips), joint(&base, HumanoidBone::Hips));
    // Girth: the section a given fraction up the torso is the same width (stretching moves the features, not the radii).
    let waist = |l: &BodyLayout| sample(l, BodyRegion::Torso, 0.5).radii;
    assert!((waist(&torso).x - waist(&base).x).abs() < 3e-3, "features slide, widths stay");
    assert!(torso.region(BodyRegion::Torso).unwrap().length() > base.region(BodyRegion::Torso).unwrap().length());
}

#[test]
fn shoulder_and_hip_width_scale_the_roots_and_the_torsos_radii_at_those_heights() {
    let base = default_layout();
    let wide = with(&Proportions { shoulder_width: 1.5, hip_width: 0.5, ..Proportions::default() });
    let arm = |l: &BodyLayout| joint(l, HumanoidBone::LeftUpperArm).x;
    let leg = |l: &BodyLayout| joint(l, HumanoidBone::LeftUpperLeg).x;
    assert!((arm(&wide) - 1.5 * arm(&base)).abs() < 1e-12 && (leg(&wide) - 0.5 * leg(&base)).abs() < 1e-12);
    // At the shoulder line and in the pelvis the torso's x radius follows; the neck does not.
    let at = |l: &BodyLayout, y: f64| {
        let r = l.region(BodyRegion::Torso).unwrap();
        let (b, t) = (sample(l, BodyRegion::Torso, 0.0).centre.y, sample(l, BodyRegion::Torso, 1.0).centre.y);
        r.section((y - b) / (t - b)).radii
    };
    assert!((at(&wide, 0.485).x / at(&base, 0.485).x - 1.5).abs() < 1e-9);
    assert!((at(&wide, 0.34).x / at(&base, 0.34).x - 0.5).abs() < 1e-9);
    assert_eq!(at(&wide, 0.545), at(&base, 0.545), "the neck end is unchanged");
    assert_eq!(at(&wide, 0.485).y, at(&base, 0.485).y, "depth is unchanged");
}

#[test]
fn arm_and_leg_length_scale_segments_and_the_hips_ride_the_legs() {
    let base = default_layout();
    let arms = with(&Proportions { arm_length: 1.5, ..Proportions::default() });
    let seg = |l: &BodyLayout, a, b| joint(l, a).distance(joint(l, b));
    let (up, low) = (HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm);
    assert!((seg(&arms, up, low) / seg(&base, up, low) - 1.5).abs() < 1e-12);
    assert!((seg(&arms, low, HumanoidBone::LeftHand) / seg(&base, low, HumanoidBone::LeftHand) - 1.5).abs() < 1e-12);

    let legs = with(&Proportions { leg_length: 2.0, ..Proportions::default() });
    let shank = |l: &BodyLayout| joint(l, HumanoidBone::LeftLowerLeg).y - joint(l, HumanoidBone::LeftFoot).y;
    assert!((shank(&legs) - 2.0 * shank(&base)).abs() < 1e-12);
    let rise = joint(&legs, HumanoidBone::Hips).y - joint(&base, HumanoidBone::Hips).y;
    assert!((rise - 2.0 * (0.19 + 0.13) + (0.19 + 0.13)).abs() < 1e-12, "hips rise by the extra leg: {rise}");
    assert_eq!(joint(&legs, HumanoidBone::LeftFoot), joint(&base, HumanoidBone::LeftFoot), "the feet stay put");
    assert!(joint(&legs, HumanoidBone::Neck).y > joint(&base, HumanoidBone::Neck).y);
}

#[test]
fn hand_and_foot_size_scale_their_sub_skeletons() {
    let base = default_layout();
    let big = with(&Proportions { hand_size: 2.0, foot_size: 2.0, ..Proportions::default() });
    for bone in [HumanoidBone::LeftIndexDistal, HumanoidBone::LeftThumbProximal, HumanoidBone::LeftLittleProximal] {
        let from_wrist = |l: &BodyLayout| joint(l, bone) - joint(l, HumanoidBone::LeftHand);
        let (a, b) = (from_wrist(&base), from_wrist(&big));
        assert!(((b.length() / a.length()) - 2.0).abs() < 1e-9, "{bone}");
    }
    let (f, g) = (joint(&base, HumanoidBone::LeftFoot), joint(&big, HumanoidBone::LeftFoot));
    assert!((g.y - 2.0 * f.y).abs() < 1e-12, "the ankle is as high as the foot is big");
    let toe = |l: &BodyLayout| joint(l, HumanoidBone::LeftToes) - joint(l, HumanoidBone::LeftFoot);
    assert!((toe(&big).z - 2.0 * toe(&base).z).abs() < 1e-12);
    let length = |l: &BodyLayout| l.region(BodyRegion::FootL).unwrap().length();
    assert!((length(&big) / length(&base) - 2.0).abs() < 1e-12);
}

// ------------------------------------------------------------------------------------------------ hands

#[test]
fn all_thirty_finger_bones_sit_on_the_hands_and_follow_the_hand_parameters() {
    let base = default_layout();
    let fingers: Vec<_> = HumanoidBone::ALL
        .into_iter()
        .filter(|b| {
            b.name().contains("Thumb")
                || b.name().contains("Index")
                || b.name().contains("Middle")
                || b.name().contains("Ring")
                || b.name().contains("Little")
        })
        .collect();
    assert_eq!(fingers.len(), 30);
    for bone in &fingers {
        let p = joint(&base, *bone);
        let hand = joint(
            &base,
            if bone.name().starts_with("left") { HumanoidBone::LeftHand } else { HumanoidBone::RightHand },
        );
        assert!(p.distance(hand) > 0.005 && p.distance(hand) < 0.1, "{bone} is within a hand's reach");
    }
    // Along each finger the joints run outward from the wrist.
    for finger in ["Index", "Middle", "Ring", "Little"] {
        let d = |part: &str| {
            joint(&base, HumanoidBone::from_name(&format!("left{finger}{part}")).unwrap())
                .distance(joint(&base, HumanoidBone::LeftHand))
        };
        assert!(d("Proximal") < d("Intermediate") && d("Intermediate") < d("Distal"), "{finger}");
    }
    // The middle finger reaches furthest along the forearm; the little finger the least.
    let frame = HandFrame::of(
        joint(&base, HumanoidBone::LeftHand) - joint(&base, HumanoidBone::LeftLowerArm),
        TABLE.hand.palm_hint,
        TABLE.hand.thumb_hint,
    );
    let reach = |name: &str| {
        (joint(&base, HumanoidBone::from_name(name).unwrap()) - joint(&base, HumanoidBone::LeftHand)).dot(frame.axis)
    };
    assert!(
        reach("leftMiddleDistal") > reach("leftIndexDistal") && reach("leftIndexDistal") > reach("leftLittleDistal")
    );

    let l = |h: HandParams| SoftSd.layout(&Proportions::default(), &BodyShape::default(), &h);
    let wrist = joint(&base, HumanoidBone::LeftHand);
    let along = |l: &BodyLayout, bone| joint(l, bone) - wrist;

    let long = l(HandParams { finger_length: 2.0, ..HandParams::default() });
    let finger = |l: &BodyLayout| {
        (joint(l, HumanoidBone::LeftMiddleDistal) - joint(l, HumanoidBone::LeftMiddleProximal)).dot(frame.axis)
    };
    assert!((finger(&long) / finger(&base) - 2.0).abs() < 1e-9, "fingers are twice as long");
    assert!(
        joint(&long, HumanoidBone::LeftMiddleProximal).distance(joint(&base, HumanoidBone::LeftMiddleProximal)) < 1e-12,
        "knuckles stay"
    );

    let palm = l(HandParams { palm_length: 2.0, ..HandParams::default() });
    assert!(
        along(&palm, HumanoidBone::LeftMiddleProximal).length()
            > 1.7 * along(&base, HumanoidBone::LeftMiddleProximal).length()
    );

    let wide = l(HandParams { palm_width: 2.0, ..HandParams::default() });
    let across =
        |l: &BodyLayout| joint(l, HumanoidBone::LeftIndexProximal).distance(joint(l, HumanoidBone::LeftLittleProximal));
    assert!((across(&wide) / across(&base) - 2.0).abs() < 1e-9);

    let thumb = l(HandParams { thumb_length: 2.0, ..HandParams::default() });
    let phalanx =
        |l: &BodyLayout| joint(l, HumanoidBone::LeftThumbDistal).distance(joint(l, HumanoidBone::LeftThumbProximal));
    assert!((phalanx(&thumb) / phalanx(&base) - 2.0).abs() < 1e-9);
}

#[test]
fn thumb_spread_opens_the_thumb_away_from_the_palm_and_zero_is_the_template() {
    let angle = |spread: f64| {
        let l = SoftSd.layout(
            &Proportions::default(),
            &BodyShape::default(),
            &HandParams { thumb_spread_deg: spread, ..HandParams::default() },
        );
        let wrist = joint(&l, HumanoidBone::LeftHand);
        let frame =
            HandFrame::of(wrist - joint(&l, HumanoidBone::LeftLowerArm), TABLE.hand.palm_hint, TABLE.hand.thumb_hint);
        let d = joint(&l, HumanoidBone::LeftThumbDistal) - joint(&l, HumanoidBone::LeftThumbMetacarpal);
        let along = d.dot(frame.axis);
        let aside = d.dot(frame.thumb);
        aside.atan2(along).to_degrees()
    };
    let (closed, rest, open) = (angle(-30.0), angle(0.0), angle(30.0));
    assert!(closed < rest && rest < open, "{closed} < {rest} < {open}");
    assert!(
        ((open - rest) - 30.0).abs() < 1e-6 && ((rest - closed) - 30.0).abs() < 1e-6,
        "an offset in degrees: {open} {rest} {closed}"
    );
}

// ------------------------------------------------------------------------------------------------ body shape

fn shaped(s: &BodyShape) -> BodyLayout {
    SoftSd.layout(&Proportions::default(), s, &HandParams::default())
}

// The axes' own rules (the 12% of height, build's monotonicity, the curves' profile, musculature's masses,
// shoulder/hip's mean) live in `body_shape.rs`; this file only needs `shaped` to build bodies for the garment tests.

// ------------------------------------------------------------------------------------------------ garments read it

/// What a shirt does with a body: ring the torso, the sleeves and the neck from the published fit.
fn wrap(fit: &dyn BodyFit, region: BodyRegion, rings: usize) -> Vec<Vec3> {
    let mut points = Vec::new();
    for k in 0..rings {
        let s: Section = fit.section(region, k as f32 / (rings - 1) as f32).expect("the body has every region");
        let (across, through) = section_basis(region, s.axis);
        assert!((across.length() - 1.0).abs() < 1e-5 && (through.length() - 1.0).abs() < 1e-5);
        assert!(
            across.dot(s.axis).abs() < 1e-5 && through.dot(s.axis).abs() < 1e-5 && across.dot(through).abs() < 1e-5
        );
        for j in 0..12 {
            let a = j as f32 / 12.0 * std::f32::consts::TAU;
            points.push(s.centre + across * (s.radii.x * a.sin()) + through * (s.radii.y * a.cos()));
        }
    }
    points
}

#[test]
fn a_shirt_like_consumer_can_sample_every_region_and_joint_through_the_published_fit() {
    let fit: Arc<dyn BodyFit> = Arc::new(default_layout());
    for region in ALL_REGIONS {
        let ring = wrap(fit.as_ref(), region, 9);
        assert_eq!(ring.len(), 9 * 12);
        assert!(ring.iter().all(|p| p.is_finite()), "{region:?}");
    }
    for bone in HumanoidBone::ALL {
        let owned =
            !matches!(bone, HumanoidBone::Head | HumanoidBone::LeftEye | HumanoidBone::RightEye | HumanoidBone::Jaw);
        assert_eq!(fit.joint(&JointRef::Humanoid(bone)).is_some(), owned, "{bone}");
    }
    assert!(fit.joint(&JointRef::Named(omuzaki_char_api::BoneName::new("hair_back_1").unwrap())).is_none());
    assert!(fit.section(BodyRegion::Torso, f32::NAN).is_none());
    // The rings of the shirt's torso hug the hips-to-neck span, and the sleeve ring starts at the shoulder.
    let torso = wrap(fit.as_ref(), BodyRegion::Torso, 5);
    assert!(torso.iter().all(|p| p.y > 0.3 && p.y < 0.56));
    let arm = fit.section(BodyRegion::ArmL, 0.0).unwrap();
    assert!((arm.centre - Vec3::new(0.092, 0.484, 0.0)).length() < 1e-5);
}

#[test]
fn a_custom_table_reuses_the_builder() {
    // A template other than soft-sd is a table: here a soft-sd with a taller neck and a different hip zone.
    let table: BodyTable = BodyTable { zones: layout::Zones { waist: 0.42, ..TABLE.zones }, ..TABLE };
    let l = layout::build(
        &table,
        &Proportions::default(),
        &BodyShape { curves: 1.0, ..BodyShape::default() },
        &HandParams::default(),
    )
    .unwrap();
    let default_waist = shaped(&BodyShape { curves: 1.0, ..BodyShape::default() });
    let t = (0.42 - 0.325) / 0.22;
    assert!(sample(&l, BodyRegion::Torso, t).radii.x < sample(&default_waist, BodyRegion::Torso, t).radii.x);
}
