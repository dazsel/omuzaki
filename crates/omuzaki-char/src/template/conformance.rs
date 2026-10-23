//! The template conformance suite: the rules every body template passes, at every setting of the sliders and the body
//! shape.
//!
//! [`run`] builds the template's layout for [`SAMPLES`] seeded random [`Proportions`] and [`BodyShape`] values (a quarter
//! of the numbers snap to an end of their range, so the extremes are always tested) and checks each against the rules
//! in [`Rule`]. A template added later calls `conformance::run(&MyTemplate)` in one test and gets all of them; the
//! body-shape presets are samples of the same space.
//!
//! The suite reports instead of panicking: [`Violations`] lists every broken rule with the sample that broke it, and a
//! test fails with that text. The samples are a pure function of [`SEED`], so a failure reproduces.

use std::fmt;

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_char_api::{BodyRegion, BodyShape, HandParams, Proportions};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::rng::Rng;

use super::Template;
use super::layout::BodyLayout;
use super::skeleton;

/// How many random samples [`run`] draws, on top of the template's own defaults.
pub const SAMPLES: usize = 200;
/// The seed of the samples: fixed, so the suite is deterministic.
pub const SEED: u64 = 0x5eed_0442;
/// The most two mirrored joints may differ by, in metres: left and right are built once and mirrored, so this is slack
/// for rounding only.
pub const SYMMETRY_TOLERANCE: f64 = 1e-6;
/// The range a bone's length may take, as a multiple of its length in the template's default body.
pub const BONE_LENGTH_RATIO: (f32, f32) = (0.4, 2.5);
/// The most a joint may be off the layout when the skeleton is posed at its bind pose, and the most an arm bone may
/// leave the horizontal at rest, in metres.
pub const BIND_TOLERANCE: f32 = 1e-6;
/// How far a foot's sole may stand from the floor (`y = 0`), in metres.
pub const FLOOR_TOLERANCE: f64 = 1e-6;
/// How many bones a template's layout makes on its own: all but the eyes and jaw, which the head part publishes.
const LAYOUT_BONES: usize = 52;
/// Sections checked along each region (both ends included).
const SECTION_STEPS: usize = 16;
/// The leg lengths of the height sweep: the whole slider range.
const LEG_LENGTHS: [f64; 5] = [0.5, 0.875, 1.25, 1.625, 2.0];
/// The regions a body has. [`BodyRegion`] is non-exhaustive, so they are listed.
const REGIONS: [BodyRegion; 8] = [
    BodyRegion::Torso,
    BodyRegion::Neck,
    BodyRegion::ArmL,
    BodyRegion::ArmR,
    BodyRegion::LegL,
    BodyRegion::LegR,
    BodyRegion::FootL,
    BodyRegion::FootR,
];

/// A rule of the suite.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// The template builds a layout.
    Builds,
    /// The layout has every body joint, all finite.
    Joints,
    /// The joints make a skeleton `Skeleton::new` accepts: parents per `HumanoidBone::parent`, identity rest rotations.
    Skeleton,
    /// The skeleton rests in the T-pose (arms level along X) and posing it at the template's bind pose puts every joint
    /// where the layout has it, to [`BIND_TOLERANCE`].
    Bind,
    /// Left and right joints mirror each other, and centre-line joints sit on `x = 0`.
    Symmetry,
    /// Every bone's length stays within [`BONE_LENGTH_RATIO`] of its length in the default body.
    BoneLength,
    /// Every section of every region is finite with positive radii.
    Sections,
    /// The soles of both feet are on the floor (`y = 0`).
    Feet,
    /// A longer `leg_length` always makes a taller body.
    Height,
}

impl Rule {
    /// The rule's name as failure messages print it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Builds => "builds",
            Self::Joints => "joints",
            Self::Skeleton => "skeleton",
            Self::Bind => "bind",
            Self::Symmetry => "symmetry",
            Self::BoneLength => "bone-length",
            Self::Sections => "sections",
            Self::Feet => "feet",
            Self::Height => "height",
        }
    }
}

/// One setting of the sliders and the body shape.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// `0` is the template's defaults; `1..=SAMPLES` are the random draws.
    pub index: usize,
    /// The proportion sliders.
    pub proportions: Proportions,
    /// The body-shape axes.
    pub shape: BodyShape,
}

impl fmt::Display for Sample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sample {} {:?} {:?}", self.index, self.proportions, self.shape)
    }
}

/// A rule a sample broke.
#[derive(Clone, Debug, PartialEq)]
pub struct Violation {
    /// The setting that broke it.
    pub sample: Sample,
    /// Which rule.
    pub rule: Rule,
    /// What was wrong, with the numbers.
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rule '{}' broken by {}: {}", self.rule.name(), self.sample, self.detail)
    }
}

/// Everything [`run`] found wrong with a template.
#[derive(Clone, Debug, PartialEq)]
pub struct Violations {
    /// The template's id.
    pub template: String,
    /// Every broken rule, in sample order.
    pub list: Vec<Violation>,
}

impl fmt::Display for Violations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const SHOWN: usize = 8;
        writeln!(f, "template '{}' broke {} rule(s):", self.template, self.list.len())?;
        for violation in self.list.iter().take(SHOWN) {
            writeln!(f, "  {violation}")?;
        }
        match self.list.len().checked_sub(SHOWN) {
            Some(more) if more > 0 => writeln!(f, "  ... and {more} more"),
            _ => Ok(()),
        }
    }
}

impl std::error::Error for Violations {}

/// The samples [`run`] checks: the defaults, then [`SAMPLES`] draws from [`SEED`].
pub fn samples() -> Vec<Sample> {
    let mut rng = Rng::new(SEED);
    // Uniform over the validated range; one draw in four lands exactly on an end.
    let mut draw = |lo: f64, hi: f64| match rng.next_u64() % 8 {
        0 => lo,
        1 => hi,
        _ => lo + (hi - lo) * f64::from(rng.next_f32()),
    };
    let mut all = vec![Sample { index: 0, proportions: Proportions::default(), shape: BodyShape::default() }];
    for index in 1..=SAMPLES {
        let mut slider = || draw(0.5, 2.0);
        let proportions = Proportions {
            neck_length: slider(),
            torso_length: slider(),
            shoulder_width: slider(),
            hip_width: slider(),
            arm_length: slider(),
            leg_length: slider(),
            hand_size: slider(),
            foot_size: slider(),
        };
        let mut axis = || draw(-1.0, 1.0);
        let shape = BodyShape {
            height: axis(),
            build: axis(),
            curves: axis(),
            musculature: axis(),
            shoulder_hip: axis(),
            preset: None,
        };
        all.push(Sample { index, proportions, shape });
    }
    all
}

/// Checks `template` against every rule at every sample.
///
/// # Errors
/// [`Violations`], naming each broken rule and the sample that broke it.
pub fn run(template: &dyn Template) -> Result<(), Violations> {
    let mut list = Vec::new();
    let all = samples();
    let reference = layout_of(template, &all[0]).ok().map(|l| lengths(&l));
    for sample in &all {
        let mut broke = |rule, detail: String| list.push(Violation { sample: sample.clone(), rule, detail });
        let layout = match layout_of(template, sample) {
            Ok(layout) => layout,
            Err(detail) => {
                broke(Rule::Builds, detail);
                continue;
            }
        };
        for (rule, outcome) in [
            (Rule::Joints, check_joints(&layout)),
            (Rule::Skeleton, check_skeleton(&layout)),
            (Rule::Bind, check_bind(&layout)),
            (Rule::Symmetry, check_symmetry(&layout)),
            (Rule::BoneLength, check_bone_lengths(&layout, reference.as_deref())),
            (Rule::Sections, check_sections(&layout)),
            (Rule::Feet, check_feet(&layout)),
            (Rule::Height, check_height(template, sample)),
        ] {
            if let Err(detail) = outcome {
                broke(rule, detail);
            }
        }
    }
    if list.is_empty() { Ok(()) } else { Err(Violations { template: template.id().to_owned(), list }) }
}

fn layout_of(template: &dyn Template, sample: &Sample) -> Result<BodyLayout, String> {
    template.try_layout(&sample.proportions, &sample.shape, &HandParams::default()).map_err(|e| e.to_string())
}

fn check_joints(layout: &BodyLayout) -> Result<(), String> {
    let all = skeleton::joints(layout);
    let owned_by_the_body =
        |b: &HumanoidBone| !matches!(b, HumanoidBone::LeftEye | HumanoidBone::RightEye | HumanoidBone::Jaw);
    let mut body = HumanoidBone::ALL.into_iter().filter(owned_by_the_body);
    if let Some(missing) = body.find(|b| all.iter().all(|(a, _)| a != b)) {
        return Err(format!("the layout has no joint for {missing}"));
    }
    match all.iter().find(|(_, p)| !p.is_finite()) {
        Some((bone, p)) => Err(format!("{bone} is at {p}, which is not finite")),
        None => Ok(()),
    }
}

fn check_skeleton(layout: &BodyLayout) -> Result<(), String> {
    let skeleton = skeleton::of_layout(layout).map_err(|e| e.to_string())?;
    if skeleton.len() != LAYOUT_BONES {
        return Err(format!("{} bones, expected {LAYOUT_BONES}", skeleton.len()));
    }
    for bone in HumanoidBone::ALL {
        let Some(def) = skeleton.humanoid_bone(bone).and_then(|id| skeleton.get(id)) else { continue };
        if def.parent != bone.parent().and_then(|p| skeleton.humanoid_bone(p)) {
            return Err(format!("{bone} hangs from {:?}, not from {:?}", def.parent, bone.parent()));
        }
        if def.rest.rotation != Quat::IDENTITY {
            return Err(format!("{bone} has a rest rotation that is not the identity"));
        }
    }
    Ok(())
}

fn check_bind(layout: &BodyLayout) -> Result<(), String> {
    let all = skeleton::joints(layout);
    let rig = skeleton::of_layout(layout).map_err(|e| e.to_string())?;
    let at = |points: &[(HumanoidBone, Vec3)], bone| points.iter().find(|(b, _)| *b == bone).map(|(_, p)| *p);
    let rest = skeleton::joints_at(&rig, &HumanoidPose::REST);
    for (from, to) in [
        (HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm),
        (HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand),
        (HumanoidBone::RightUpperArm, HumanoidBone::RightLowerArm),
        (HumanoidBone::RightLowerArm, HumanoidBone::RightHand),
    ] {
        let (Some(a), Some(b)) = (at(&rest, from), at(&rest, to)) else { continue };
        if (b.y - a.y).abs() > BIND_TOLERANCE || (b.z - a.z).abs() > BIND_TOLERANCE {
            return Err(format!("the {from} bone rests {a} to {b}, not level along X"));
        }
    }
    let bound = skeleton::joints_at(&rig, &skeleton::bind_pose(&all));
    let worst = all
        .iter()
        .filter_map(|&(bone, p)| at(&bound, bone).map(|q| (bone, p.distance(q))))
        .max_by(|a, b| a.1.total_cmp(&b.1));
    match worst {
        Some((bone, off)) if off > BIND_TOLERANCE => {
            Err(format!("posed at the bind, {bone} is {off:e} m off the layout"))
        }
        _ => Ok(()),
    }
}

fn check_symmetry(layout: &BodyLayout) -> Result<(), String> {
    let all = skeleton::joints(layout);
    let at = |bone| all.iter().find(|(b, _)| *b == bone).map(|(_, p)| *p);
    for &(bone, p) in &all {
        let mirror = bone.mirrored();
        let gap = if mirror == bone {
            p.x.abs()
        } else if let Some(m) = at(mirror) {
            (p.x + m.x).abs().max((p.y - m.y).abs()).max((p.z - m.z).abs())
        } else {
            continue;
        };
        if f64::from(gap) > SYMMETRY_TOLERANCE {
            return Err(format!("{bone} and {mirror} are {gap:e} m from mirror images (limit {SYMMETRY_TOLERANCE:e})"));
        }
    }
    Ok(())
}

/// Each bone's length, parents first: the distance from its joint to its nearest present ancestor's.
fn lengths(layout: &BodyLayout) -> Vec<(HumanoidBone, f32)> {
    let all = skeleton::joints(layout);
    let at = |bone| all.iter().find(|(b, _)| *b == bone).map(|(_, p): &(HumanoidBone, Vec3)| *p);
    all.iter()
        .filter_map(|&(bone, p)| {
            let parent = std::iter::successors(bone.parent(), |q| q.parent()).find_map(at)?;
            Some((bone, p.distance(parent)))
        })
        .collect()
}

fn check_bone_lengths(layout: &BodyLayout, reference: Option<&[(HumanoidBone, f32)]>) -> Result<(), String> {
    let Some(reference) = reference else {
        return Err("the default body did not build, so there is nothing to compare".into());
    };
    for ((bone, length), (_, default)) in lengths(layout).into_iter().zip(reference) {
        if *default <= 0.0 {
            return Err(format!("{bone} has no length in the default body"));
        }
        let (lo, hi) = BONE_LENGTH_RATIO;
        let (shortest, longest) = (lo * default, hi * default);
        if !(shortest..=longest).contains(&length) {
            return Err(format!(
                "{bone} is {length:.5} m long, outside {shortest:.5} to {longest:.5} m ({lo} to {hi} times its default {default:.5} m)"
            ));
        }
    }
    Ok(())
}

fn check_sections(layout: &BodyLayout) -> Result<(), String> {
    for region in REGIONS {
        for step in 0..=SECTION_STEPS {
            #[allow(clippy::cast_precision_loss)] // 16 steps
            let t = step as f64 / SECTION_STEPS as f64;
            let section = layout.section64(region, t).ok_or_else(|| format!("{region:?} has no section"))?;
            let (centre, radii) = (section.centre, section.radii);
            if !(centre.is_finite() && radii.x > 0.0 && radii.y > 0.0 && radii.is_finite()) {
                return Err(format!(
                    "{region:?} at {t:.3}: centre {centre}, radii {radii} (finite and positive wanted)"
                ));
            }
        }
    }
    Ok(())
}

fn check_feet(layout: &BodyLayout) -> Result<(), String> {
    for region in [BodyRegion::FootL, BodyRegion::FootR] {
        for step in 0..=SECTION_STEPS {
            #[allow(clippy::cast_precision_loss)] // 16 steps
            let t = step as f64 / SECTION_STEPS as f64;
            let Some(s) = layout.section64(region, t) else { continue };
            // `radii.y` of a foot is its half-height, measured up from the sole to the centre.
            let sole = s.centre.y - s.radii.y;
            if sole.abs() > FLOOR_TOLERANCE {
                return Err(format!(
                    "the sole of {region:?} at {t:.3} is {sole:e} m from the floor (limit {FLOOR_TOLERANCE:e})"
                ));
            }
        }
    }
    Ok(())
}

fn check_height(template: &dyn Template, sample: &Sample) -> Result<(), String> {
    let mut previous: Option<(f64, f64)> = None;
    for leg_length in LEG_LENGTHS {
        let proportions = Proportions { leg_length, ..sample.proportions.clone() };
        let layout = template
            .try_layout(&proportions, &sample.shape, &HandParams::default())
            .map_err(|e| format!("leg_length {leg_length}: {e}"))?;
        let top = layout.head_base().map_or(f64::NAN, |p| p.y);
        if let Some((shorter, was)) = previous
            && top.partial_cmp(&was) != Some(std::cmp::Ordering::Greater)
        {
            return Err(format!("leg_length {shorter} -> {leg_length} moves the head from {was:.5} m to {top:.5} m"));
        }
        previous = Some((leg_length, top));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use omuzaki_char_api::{BodyShape, HandParams, Proportions};

    use super::*;
    use crate::kit::KitError;
    use crate::template::TemplateDefaults;
    use crate::template::layout::{self, BodyTable};
    use crate::template::soft_sd::{SoftSd, TABLE};

    /// What a [`Broken`] template does when asked for a layout.
    #[derive(Copy, Clone)]
    enum Outcome {
        Build,
        Empty,
        Fail,
    }

    /// The soft-sd template with one thing wrong: a table, a rewrite of the sliders, or no layout at all.
    struct Broken {
        table: BodyTable,
        sliders: fn(&Proportions) -> Proportions,
        outcome: Outcome,
    }

    impl Broken {
        fn with(table: &BodyTable) -> Self {
            Self { table: *table, sliders: Proportions::clone, outcome: Outcome::Build }
        }

        fn sliders(sliders: fn(&Proportions) -> Proportions) -> Self {
            Self { sliders, ..Self::with(&TABLE) }
        }

        fn outcome(outcome: Outcome) -> Self {
            Self { outcome, ..Self::with(&TABLE) }
        }
    }

    impl Template for Broken {
        fn id(&self) -> &'static str {
            "broken"
        }

        fn summary(&self) -> &'static str {
            "soft-sd with something wrong"
        }

        fn heads_tall(&self) -> f64 {
            2.5
        }

        fn default_stature_m(&self) -> f64 {
            0.692
        }

        fn try_layout(&self, p: &Proportions, s: &BodyShape, h: &HandParams) -> Result<BodyLayout, KitError> {
            match self.outcome {
                Outcome::Fail => Err(KitError::Degenerate { what: "a deliberately broken table" }),
                Outcome::Empty => Ok(BodyLayout::empty()),
                Outcome::Build => layout::build(&self.table, &(self.sliders)(p), s, h),
            }
        }

        fn defaults(&self) -> TemplateDefaults {
            TemplateDefaults::default()
        }
    }

    /// The first violation of `rule` in what `broken` reports, and the whole report as text.
    fn caught(broken: &Broken, rule: Rule) -> (Violation, String) {
        let violations = run(broken).unwrap_err();
        let first = violations
            .list
            .iter()
            .find(|v| v.rule == rule)
            .unwrap_or_else(|| panic!("{rule:?} not named: {violations}"));
        (first.clone(), violations.to_string())
    }

    #[test]
    #[allow(clippy::float_cmp)] // the draws that snap to an end land on it exactly, which is what is asserted
    fn the_samples_are_the_same_every_time_and_reach_the_ends_of_the_ranges() {
        let all = samples();
        assert_eq!(all.len(), SAMPLES + 1);
        assert_eq!(all, samples());
        assert!(
            all.iter().any(|s| s.proportions.hip_width == 2.0) && all.iter().any(|s| s.proportions.hip_width == 0.5)
        );
        assert!(all.iter().any(|s| s.shape.height == 1.0) && all.iter().any(|s| s.shape.height == -1.0));
        assert!(all.iter().all(|s| (0.5..=2.0).contains(&s.proportions.leg_length) && s.shape.build.abs() <= 1.0));
    }

    #[test]
    fn soft_sd_conforms() {
        if let Err(violations) = run(&SoftSd) {
            panic!("{violations}");
        }
    }

    #[test]
    fn a_template_that_cannot_build_is_reported_for_every_sample_with_its_reason() {
        let violations = run(&Broken::outcome(Outcome::Fail)).unwrap_err();
        assert_eq!(violations.list.len(), SAMPLES + 1);
        assert!(violations.list.iter().all(|v| v.rule == Rule::Builds && v.detail.contains("deliberately broken")));
    }

    #[test]
    fn a_layout_without_joints_breaks_the_joint_and_skeleton_rules() {
        let (violation, _) = caught(&Broken::outcome(Outcome::Empty), Rule::Joints);
        assert!(violation.detail.contains("no joint for hips"), "{violation}");
        caught(&Broken::outcome(Outcome::Empty), Rule::Skeleton);
    }

    #[test]
    fn a_negative_radius_is_named_with_its_region_and_the_sample() {
        let mut table = TABLE;
        table.torso.rx = &[(0.325, 0.09), (0.545, -0.01)];
        let (violation, text) = caught(&Broken::with(&table), Rule::Sections);
        assert_eq!(violation.sample.index, 0, "the defaults break it, so the first sample is named");
        assert!(violation.detail.contains("Torso"), "{violation}");
        assert!(
            text.contains("template 'broken' broke") && text.contains("rule 'sections' broken by sample 0"),
            "{text}"
        );
    }

    #[test]
    fn a_floating_sole_is_named() {
        let mut table = TABLE;
        table.foot.through = Some(&[(-0.035, 0.05)]);
        let (violation, _) = caught(&Broken::with(&table), Rule::Feet);
        assert!(violation.detail.contains("FootL") && violation.detail.contains("from the floor"), "{violation}");
    }

    #[test]
    fn a_bone_that_balloons_with_a_slider_is_named_with_its_ratio() {
        // Each template below has a default one quarter of the full slider, so a full-length bone is four times it.
        // Short bones count as much as long ones: the neck, the hand and the foot are the ones a loose floor let by.
        type Sliders = fn(&Proportions) -> Proportions;
        let cases: [(&str, Sliders, &str); 6] = [
            ("arm_length", |p| Proportions { arm_length: quartered(p.arm_length), ..p.clone() }, "leftLowerArm"),
            ("neck_length", |p| Proportions { neck_length: quartered(p.neck_length), ..p.clone() }, "head"),
            ("hand_size", |p| Proportions { hand_size: quartered(p.hand_size), ..p.clone() }, "leftThumbMetacarpal"),
            ("foot_size", |p| Proportions { foot_size: quartered(p.foot_size), ..p.clone() }, "leftToes"),
            ("hip_width", |p| Proportions { hip_width: quartered(p.hip_width), ..p.clone() }, "leftUpperLeg"),
            (
                "shoulder_width",
                |p| Proportions { shoulder_width: quartered(p.shoulder_width), ..p.clone() },
                "leftShoulder",
            ),
        ];
        for (slider, sliders, bone) in cases {
            let (violation, _) = caught(&Broken::sliders(sliders), Rule::BoneLength);
            assert!(
                violation.detail.starts_with(&format!("{bone} is ")) && violation.detail.contains("times its default"),
                "{slider} should break {bone}: {violation}"
            );
        }
    }

    /// The slider value a [`Broken`] template turns `value` into: the full range's top for anything above 1.2, a quarter
    /// of it (the default's value, 1.0, included) otherwise.
    fn quartered(value: f64) -> f64 {
        if value > 1.2 { 2.0 } else { 0.5 }
    }

    #[test]
    fn a_slider_that_does_not_move_the_head_breaks_the_height_rule() {
        let (violation, _) = caught(&Broken::sliders(|p| Proportions { leg_length: 1.0, ..p.clone() }), Rule::Height);
        assert!(violation.detail.contains("moves the head"), "{violation}");
    }
}
