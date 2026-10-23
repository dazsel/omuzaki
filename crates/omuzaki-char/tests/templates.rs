//! The mini, standard and tall body templates: conformance at 200 samples, the declared height against the
//! measured one, and the proportions against the canon bands each template was fitted to.
//!
//! **The bands.** Each template's landmarks were measured against public references before it was shaped (the numbers
//! and sources are in `template/standard.rs`): the US Army's ANSUR II survey (public domain) for
//! the real adult, the CC0 `VRoid` sample models for the anime model-sheet convention (6.5 to 7.9 heads, measured joint by
//! joint) and the project's own 3 head concept art for the chibi. A band is the range those references span, widened a
//! little where only one reference exists. Heights are fractions of the chin height (sole to chin), so a template's
//! head size does not enter; lengths and widths are in heads.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers look up joints and regions that must exist
#![allow(clippy::cast_precision_loss)] // small loop counters

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::template::layout::BodyLayout;
use omuzaki_char::template::mini::Mini;
use omuzaki_char::template::soft_sd::SoftSd;
use omuzaki_char::template::standard::{HEAD, Standard};
use omuzaki_char::template::tall::Tall;
use omuzaki_char::template::{Template, conformance, registry};
use omuzaki_char_api::{BodyParams, BodyRegion, BodyShape, Cranium, HandParams, Proportions};
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::Validate;

/// An inclusive range.
type Band = (f64, f64);

/// What a template was fitted to.
struct Canon {
    id: &'static str,
    heads: f64,
    /// Chin above the shoulder joint, in heads.
    shoulder_below_chin: Band,
    /// Crotch height over total height.
    crotch: Band,
    /// Hip joint, knee and ankle heights over chin height.
    hip_joint: Band,
    knee: Band,
    ankle: Band,
    /// Shoulder joint to wrist (along the bones) over chin height.
    arm: Band,
    /// The upper arm's angle from the vertical in the sculpted pose, degrees.
    arm_angle: Band,
    /// Distance between the two shoulder joints and between the two hip joints, in heads.
    shoulder_spacing: Band,
    hip_spacing: Band,
    /// Wrist to middle fingertip, heel to toe and the neck's length, in heads.
    hand: Band,
    foot: Band,
    neck: Band,
}

const CANON: [Canon; 3] = [
    Canon {
        // The 3 head concept art: shoulders tucked under the head, a third of the height in the legs, short arms.
        id: "mini",
        heads: 3.0,
        shoulder_below_chin: (0.15, 0.30),
        crotch: (0.30, 0.36),
        hip_joint: (0.50, 0.62),
        knee: (0.28, 0.36),
        ankle: (0.08, 0.14),
        arm: (0.26, 0.34),
        arm_angle: (28.0, 40.0),
        shoulder_spacing: (0.50, 0.70),
        hip_spacing: (0.28, 0.38),
        hand: (0.25, 0.35),
        foot: (0.40, 0.60),
        neck: (0.08, 0.18),
    },
    Canon {
        // VRoid: shoulders 0.33 to 0.40 under the chin, hip joint at 0.64 to 0.67 of the chin height, knee at 0.38 to
        // 0.40; ANSUR II (scaled to 6 heads): crotch at 0.48 of the height, arms 0.37 of the chin height.
        id: "standard",
        heads: 6.0,
        shoulder_below_chin: (0.33, 0.45),
        crotch: (0.47, 0.52),
        hip_joint: (0.62, 0.68),
        knee: (0.34, 0.40),
        ankle: (0.05, 0.09),
        arm: (0.30, 0.38),
        arm_angle: (25.0, 35.0),
        shoulder_spacing: (0.95, 1.20),
        hip_spacing: (0.55, 0.68),
        hand: (0.55, 0.68),
        foot: (0.70, 0.85),
        neck: (0.12, 0.25),
    },
    Canon {
        // ANSUR II: shoulder joint 0.80, crotch 0.48 and knee 0.28 of the stature (x 7.5 heads), wrist at the crotch
        // with the arm hanging; VRoid males (7.1 and 7.9 heads) for the width and the longer leg of the convention.
        id: "tall",
        heads: 7.5,
        shoulder_below_chin: (0.33, 0.45),
        crotch: (0.47, 0.52),
        hip_joint: (0.60, 0.68),
        knee: (0.33, 0.40),
        ankle: (0.04, 0.07),
        arm: (0.33, 0.40),
        arm_angle: (24.0, 32.0),
        shoulder_spacing: (1.10, 1.30),
        hip_spacing: (0.55, 0.68),
        hand: (0.68, 0.82),
        foot: (0.90, 1.05),
        neck: (0.18, 0.30),
    },
];

fn template(id: &str) -> &'static dyn Template {
    registry::find(id).unwrap_or_else(|| panic!("template '{id}' is registered"))
}

fn layout_of(t: &dyn Template) -> BodyLayout {
    t.layout(&Proportions::default(), &BodyShape::default(), &HandParams::default())
}

fn joint(l: &BodyLayout, bone: HumanoidBone) -> DVec3 {
    l.joint64(bone).unwrap()
}

/// The soles' height, crown and chin of the character a template starts from, in metres.
struct Heights {
    sole: f64,
    chin: f64,
    crown: f64,
}

fn heights(t: &dyn Template, l: &BodyLayout) -> Heights {
    let c: Cranium = t.defaults().head.cranium;
    let foot = l.section64(BodyRegion::FootL, 0.5).unwrap();
    let centre = omuzaki_char::head::part::seat(&c, l.head_base().unwrap().y);
    Heights {
        sole: foot.centre.y - foot.radii.y,
        chin: centre[1] - c.half_height_down_front,
        crown: centre[1] + c.half_height_up,
    }
}

/// Wrist to middle fingertip in metres: the distal joint, plus most of the distal bone in line with the finger.
fn hand_length(l: &BodyLayout) -> f64 {
    let (wrist, middle, distal) = (
        joint(l, HumanoidBone::LeftHand),
        joint(l, HumanoidBone::LeftMiddleIntermediate),
        joint(l, HumanoidBone::LeftMiddleDistal),
    );
    wrist.distance(distal) + 0.9 * distal.distance(middle)
}

/// Every ratio of [`Canon`], measured on a template's default layout.
struct Measured {
    heads: f64,
    shoulder_below_chin: f64,
    crotch: f64,
    hip_joint: f64,
    knee: f64,
    ankle: f64,
    arm: f64,
    arm_angle: f64,
    shoulder_spacing: f64,
    hip_spacing: f64,
    hand: f64,
    foot: f64,
    neck: f64,
}

fn measure(t: &dyn Template) -> Measured {
    let l = layout_of(t);
    let h = heights(t, &l);
    let (head, chin_height) = (h.crown - h.chin, h.chin - h.sole);
    let j = |b| joint(&l, b);
    let (shoulder, elbow, wrist) =
        (j(HumanoidBone::LeftUpperArm), j(HumanoidBone::LeftLowerArm), j(HumanoidBone::LeftHand));
    let upper = elbow - shoulder;
    Measured {
        heads: (h.crown - h.sole) / head,
        shoulder_below_chin: (h.chin - shoulder.y) / head,
        crotch: (l.section64(BodyRegion::Torso, 0.0).unwrap().centre.y - h.sole) / (h.crown - h.sole),
        hip_joint: (j(HumanoidBone::LeftUpperLeg).y - h.sole) / chin_height,
        knee: (j(HumanoidBone::LeftLowerLeg).y - h.sole) / chin_height,
        ankle: (j(HumanoidBone::LeftFoot).y - h.sole) / chin_height,
        arm: (shoulder.distance(elbow) + elbow.distance(wrist)) / chin_height,
        arm_angle: upper.x.hypot(upper.z).atan2(-upper.y).to_degrees(),
        shoulder_spacing: 2.0 * shoulder.x / head,
        hip_spacing: 2.0 * j(HumanoidBone::LeftUpperLeg).x / head,
        hand: hand_length(&l) / head,
        foot: l.region(BodyRegion::FootL).unwrap().length() / head,
        neck: l.region(BodyRegion::Neck).unwrap().length() / head,
    }
}

fn within(what: &str, id: &str, value: f64, (lo, hi): Band) {
    assert!((lo..=hi).contains(&value), "{id}: {what} is {value:.3}, outside the canon band {lo} to {hi}");
}

// ------------------------------------------------------------------------------------------------ conformance

#[test]
fn mini_conforms_at_200_samples() {
    assert_eq!(conformance::SAMPLES, 200);
    if let Err(violations) = conformance::run(&Mini) {
        panic!("{violations}");
    }
}

#[test]
fn standard_conforms_at_200_samples() {
    if let Err(violations) = conformance::run(&Standard) {
        panic!("{violations}");
    }
}

#[test]
fn tall_conforms_at_200_samples() {
    if let Err(violations) = conformance::run(&Tall) {
        panic!("{violations}");
    }
}

// ------------------------------------------------------------------------------------------------ the templates

#[test]
fn the_templates_register_themselves_with_their_heights() {
    for (id, heads) in [("mini", 3.0), ("standard", 6.0), ("tall", 7.5)] {
        let t = template(id);
        assert_eq!((t.id(), t.heads_tall()), (id, heads));
        assert!(!t.summary().is_empty() && t.summary().len() < 120, "{id}: one line");
    }
    let ids: Vec<_> = registry::templates().iter().map(|t| t.id()).collect();
    assert!(["mini", "soft-sd", "standard", "tall"].iter().all(|id| ids.contains(id)), "{ids:?}");
}

#[test]
fn the_declared_height_in_heads_equals_the_measured_one_within_3_percent() {
    for t in [&Mini as &dyn Template, &Standard, &Tall] {
        let m = measure(t);
        eprintln!("{}: declared {} heads, measured {:.3}", t.id(), t.heads_tall(), m.heads);
        assert!((m.heads / t.heads_tall() - 1.0).abs() < 0.03, "{}: measured {}", t.id(), m.heads);
    }
}

#[test]
fn the_head_rides_the_neck_the_way_it_does_on_soft_sd() {
    // The head part places the cranium at the head base plus a fixed lift, so the chin is the same small distance above
    // the neck's end on every template, and the head is the same size.
    let gap = |t: &dyn Template| {
        let l = layout_of(t);
        heights(t, &l).chin - l.head_base().unwrap().y
    };
    let reference = gap(&SoftSd);
    for t in [&Mini as &dyn Template, &Standard, &Tall] {
        assert!((gap(t) - reference).abs() < 1e-9, "{}: chin {} above the head base", t.id(), gap(t));
        let (c, soft) = (t.defaults().head.cranium, SoftSd.defaults().head.cranium);
        let same_head = Cranium { centre: soft.centre, ..c.clone() };
        assert_eq!(same_head, soft, "{}: only the cranium's place differs", t.id());
        assert!((HEAD - (c.half_height_up + c.half_height_down_front)).abs() < 1e-9, "one head is {HEAD} m");
    }
}

#[test]
fn the_defaults_start_from_neutral_sliders_and_a_neck_that_meets_the_body() {
    for t in [&Mini as &dyn Template, &Standard, &Tall] {
        let d = t.defaults();
        assert_eq!(d.body, BodyParams::default(), "{}: the template's layout is its default proportions", t.id());
        // The head's neck is the body's neck region: the same radius at the collar and under the chin, and a length that
        // reaches into the chest as soft-sd's does (its 0.11 m is twice the region's 0.055 m).
        let l = layout_of(t);
        let (bottom, top) = (l.section64(BodyRegion::Neck, 0.0).unwrap(), l.section64(BodyRegion::Neck, 1.0).unwrap());
        let region = l.region(BodyRegion::Neck).unwrap().length();
        let close = |a: f64, b: f64| (a / b - 1.0).abs() < 0.05;
        assert!(close(d.head.neck.radius_bottom, bottom.radii.x), "{}: collar radius", t.id());
        assert!(close(d.head.neck.radius_top, top.radii.x), "{}: radius under the chin", t.id());
        assert!(close(d.head.neck.height, 2.0 * region), "{}: neck height {}", t.id(), d.head.neck.height);
    }
}

#[test]
fn the_default_head_validates_wherever_the_head_sits() {
    // A template's default head leaves its centre unset, so the head part seats it on the template's neck (#825).
    for t in [&SoftSd as &dyn Template, &Mini, &Standard, &Tall] {
        let head = t.defaults().head;
        assert_eq!(head.cranium.centre, None, "{}", t.id());
        head.validate().unwrap_or_else(|e| panic!("{}: {e}", t.id()));
    }
}

// ------------------------------------------------------------------------------------------------ proportions

#[test]
fn every_template_keeps_to_the_canon_bands_it_was_fitted_to() {
    for canon in &CANON {
        let m = measure(template(canon.id));
        eprintln!(
            "{:<9} {:.2} heads | shoulder {:.2} under the chin | crotch {:.3} H | hip joint {:.3}, knee {:.3}, ankle {:.3} of the chin height | arm {:.3} at {:.1} deg | \
             shoulders {:.2}, hips {:.2} apart | hand {:.2}, foot {:.2}, neck {:.2} heads",
            canon.id,
            m.heads,
            m.shoulder_below_chin,
            m.crotch,
            m.hip_joint,
            m.knee,
            m.ankle,
            m.arm,
            m.arm_angle,
            m.shoulder_spacing,
            m.hip_spacing,
            m.hand,
            m.foot,
            m.neck
        );
        let id = canon.id;
        assert!((m.heads / canon.heads - 1.0).abs() < 0.03, "{id}: {} heads", m.heads);
        within("the shoulder joint under the chin", id, m.shoulder_below_chin, canon.shoulder_below_chin);
        within("the crotch over the height", id, m.crotch, canon.crotch);
        within("the hip joint over the chin height", id, m.hip_joint, canon.hip_joint);
        within("the knee over the chin height", id, m.knee, canon.knee);
        within("the ankle over the chin height", id, m.ankle, canon.ankle);
        within("the arm over the chin height", id, m.arm, canon.arm);
        within("the arm angle", id, m.arm_angle, canon.arm_angle);
        within("the shoulder spacing", id, m.shoulder_spacing, canon.shoulder_spacing);
        within("the hip spacing", id, m.hip_spacing, canon.hip_spacing);
        within("the hand length", id, m.hand, canon.hand);
        within("the foot length", id, m.foot, canon.foot);
        within("the neck length", id, m.neck, canon.neck);
    }
}

#[test]
fn the_classes_read_apart_by_the_proportions_that_tell_them_apart() {
    let (soft, mini, standard, tall) = (measure(&SoftSd), measure(&Mini), measure(&Standard), measure(&Tall));
    // Taller classes have longer legs against the height, shorter heads against it, longer necks and limbs in heads.
    assert!(mini.crotch < standard.crotch, "legs lengthen from chibi to standard");
    assert!(tall.crotch >= standard.crotch - 0.01, "and stay long on the tall figure");
    assert!(soft.heads < mini.heads && mini.heads < standard.heads && standard.heads < tall.heads);
    assert!(mini.neck < standard.neck && standard.neck < tall.neck, "the visible neck lengthens with the class");
    assert!(soft.shoulder_spacing < mini.shoulder_spacing && mini.shoulder_spacing < standard.shoulder_spacing);
    assert!(standard.shoulder_spacing < tall.shoulder_spacing, "shoulders widen in heads with the class");
    assert!(soft.hand < mini.hand && mini.hand < standard.hand && standard.hand < tall.hand);
    assert!(soft.foot < mini.foot && mini.foot < standard.foot && standard.foot < tall.foot);
    // The trunk widens with the class.
    let trunk = |t: &dyn Template| {
        let l = layout_of(t);
        (0..=200).map(|i| l.section64(BodyRegion::Torso, f64::from(i) / 200.0).unwrap().radii.x).fold(0.0, f64::max)
    };
    assert!(trunk(&SoftSd) < trunk(&Mini) && trunk(&Mini) < trunk(&Standard) && trunk(&Standard) < trunk(&Tall));
}

#[test]
fn arms_hang_clear_of_the_trunk_and_legs_do_not_cross() {
    for t in [&Mini as &dyn Template, &Standard, &Tall] {
        let l = layout_of(t);
        let shoulder = joint(&l, HumanoidBone::LeftUpperArm);
        let torso: Vec<_> = (0..=400).map(|i| l.section64(BodyRegion::Torso, f64::from(i) / 400.0).unwrap()).collect();
        let torso_half_width = |y: f64| {
            torso.iter().min_by(|a, b| (a.centre.y - y).abs().total_cmp(&(b.centre.y - y).abs())).unwrap().radii.x
        };
        // Below the shoulder cap, the arm's inner edge is outside the trunk.
        for i in 0..=100 {
            let s = l.section64(BodyRegion::ArmL, f64::from(i) / 100.0).unwrap();
            if s.centre.y < shoulder.y - HEAD {
                assert!(
                    s.centre.x - s.radii.x >= torso_half_width(s.centre.y),
                    "{}: the arm is inside the trunk at {i}%",
                    t.id()
                );
            }
        }
        for i in 0..=100 {
            let s = l.section64(BodyRegion::LegL, f64::from(i) / 100.0).unwrap();
            assert!(s.centre.x - s.radii.x > 0.0, "{}: the left leg reaches the centre line at {i}%", t.id());
        }
    }
}

#[test]
fn hands_feet_and_necks_stay_in_their_limits_at_the_ends_of_their_sliders() {
    // The conformance suite bounds each bone's length against the default; this bounds the whole part in heads, so a
    // hand or a foot is never grotesque next to the body it is on, nor the neck lost between head and shoulders.
    for canon in &CANON {
        let t = template(canon.id);
        for size in [0.5, 2.0] {
            let p = Proportions { hand_size: size, foot_size: size, neck_length: size, ..Proportions::default() };
            let l = t.layout(&p, &BodyShape::default(), &HandParams::default());
            let (hand, foot) = (hand_length(&l) / HEAD, l.region(BodyRegion::FootL).unwrap().length() / HEAD);
            within("the hand", canon.id, hand, (0.5 * canon.hand.0, 2.0 * canon.hand.1));
            within("the foot", canon.id, foot, (0.5 * canon.foot.0, 2.0 * canon.foot.1));
            assert!(
                l.head_base().unwrap().y > joint(&l, HumanoidBone::Neck).y,
                "{}: the neck keeps a length at {size}",
                canon.id
            );
        }
    }
}
