//! The body-shape space (CB-04): every axis reads at -1 and +1, axes combine, bodies stay sound and apart.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers look up regions and presets that must exist
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // pixel indices
#![allow(clippy::cast_precision_loss)] // small loop counters and pixel counts
#![allow(clippy::similar_names, clippy::many_single_char_names)] // rx/ry, x/y/z and the like

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::template::conformance::{self, LATERAL_ROOT_MAX_RATIO};
use omuzaki_char::template::layout::{BodyLayout, RegionSection, section_basis};
use omuzaki_char::template::registry;
use omuzaki_char::template::shape::{self, Gain, Marks, Shape};
use omuzaki_char::template::soft_sd::TABLE;
use omuzaki_char_api::{BodyFit, BodyRegion, BodyShape, HandParams, HeadParams, Proportions};
use omuzaki_core::glam::{DVec2, DVec3};

#[path = "common/silhouette.rs"]
mod silhouette;
use silhouette::{REGIONS, View, silhouette, silhouette_of};

// ------------------------------------------------------------------------------------------------ helpers

/// The scale of a template's masks: its height to the head base over soft-sd's, so a taller body fits the same window.
fn unit_of(template: &str) -> f64 {
    layout_of(template, &BodyShape::default()).head_base().unwrap().y / 0.56
}

fn layout_of(template: &str, shape: &BodyShape) -> BodyLayout {
    registry::find(template).unwrap().layout(&Proportions::default(), shape, &HandParams::default())
}

fn soft(shape: &BodyShape) -> BodyLayout {
    layout_of("soft-sd", shape)
}

/// A shape with one axis set.
fn axis(name: &str, value: f64) -> BodyShape {
    let mut s = BodyShape::default();
    match name {
        "height" => s.height = value,
        "build" => s.build = value,
        "curves" => s.curves = value,
        "musculature" => s.musculature = value,
        "shoulder_hip" => s.shoulder_hip = value,
        _ => unreachable!("{name}"),
    }
    s
}

const AXES: [&str; 5] = ["height", "build", "curves", "musculature", "shoulder_hip"];

/// The shape a corner of the cube names: bit `i` of `corner` picks `+1` for axis `i`, else `-1`.
fn corner(corner: u32) -> BodyShape {
    let pick = |bit: u32| if corner >> bit & 1 == 1 { 1.0 } else { -1.0 };
    BodyShape {
        height: pick(0),
        build: pick(1),
        curves: pick(2),
        musculature: pick(3),
        shoulder_hip: pick(4),
        preset: None,
    }
}

// ------------------------------------------------------------------------------------------------ the contact sheet

/// The corner combinations the contact sheet shows.
const SHEET_CORNERS: [(&str, BodyShape); 4] = [
    (
        "tall, slim, straight, soft, shoulder-heavy",
        BodyShape { height: 1.0, build: -1.0, curves: -1.0, musculature: -1.0, shoulder_hip: 1.0, preset: None },
    ),
    (
        "short, broad, curvy, muscular, hip-heavy",
        BodyShape { height: -1.0, build: 1.0, curves: 1.0, musculature: 1.0, shoulder_hip: -1.0, preset: None },
    ),
    (
        "tall, broad, curvy, soft, shoulder-heavy",
        BodyShape { height: 1.0, build: 1.0, curves: 1.0, musculature: -1.0, shoulder_hip: 1.0, preset: None },
    ),
    (
        "short, slim, straight, muscular, hip-heavy",
        BodyShape { height: -1.0, build: -1.0, curves: -1.0, musculature: 1.0, shoulder_hip: -1.0, preset: None },
    ),
];

/// Writes the pictures `tools/body-shape-sheet.py` puts on the contact sheets: one grey-scale file per body and view
/// (white inside the silhouette), and `manifest.tsv` naming them. The sheets show every axis at -1, 0 and +1, four
/// corners of the cube, and the ten presets on every registered template. Run it with
/// `OMUZAKI_SHEET_DIR=<dir> cargo test -p omuzaki-char --test body_shape -- --ignored sheet`.
#[test]
#[ignore = "writes the contact sheets' pictures to $OMUZAKI_SHEET_DIR"]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn sheet() {
    let dir = PathBuf::from(std::env::var("OMUZAKI_SHEET_DIR").expect("set OMUZAKI_SHEET_DIR to a directory"));
    fs::create_dir_all(&dir).unwrap();
    let mut manifest = String::new();
    let mut write = |group: &str, label: &str, template: &str, shape: &BodyShape| {
        let l = layout_of(template, shape);
        let mut files = Vec::new();
        for view in [View::Front, View::Side] {
            let m = silhouette(&l, view, 0.001, unit_of(template));
            let name = format!("{group}-{label}-{view:?}.pgm").replace(['+', ',', ' ', ':'], "_");
            let mut bytes = format!("P5\n{} {}\n255\n", m.width, m.height).into_bytes();
            bytes.extend(m.on.iter().map(|&p| if p { 255_u8 } else { 0 }));
            fs::write(dir.join(&name), bytes).unwrap();
            files.push(name);
        }
        writeln!(manifest, "{group}\t{label}\t{}\t{}", files[0], files[1]).unwrap();
    };
    for name in AXES {
        for (label, v) in [("-1", -1.0), ("0", 0.0), ("+1", 1.0)] {
            write(name, label, "soft-sd", &axis(name, v));
        }
    }
    for (label, shape) in SHEET_CORNERS {
        write("corner", label, "soft-sd", &shape);
    }
    for c in 0..32 {
        let label = ["H", "B", "C", "M", "S"]
            .iter()
            .enumerate()
            .fold(String::new(), |l, (i, a)| l + a + if c >> i & 1 == 1 { "+" } else { "-" });
        write("corners32", &label, "soft-sd", &corner(c));
    }
    for id in templates() {
        for p in shape::presets().unwrap() {
            write(&format!("preset:{id}"), p.id.as_str(), id, &p.shape);
        }
    }
    fs::write(dir.join("manifest.tsv"), manifest).unwrap();
}

// ------------------------------------------------------------------------------------------------ more helpers

fn marks() -> Marks {
    TABLE.marks().unwrap()
}

fn sec(l: &BodyLayout, region: BodyRegion, t: f64) -> RegionSection {
    l.section64(region, t).unwrap()
}

fn joint(l: &BodyLayout, bone: HumanoidBone) -> DVec3 {
    l.joint64(bone).unwrap()
}

/// `0, 1/n, ..., 1`.
fn fractions(n: usize) -> impl Iterator<Item = f64> {
    (0..=n).map(move |k| k as f64 / n as f64)
}

/// Every registered template's id: the rules below hold for each, so a template added later is covered.
fn templates() -> Vec<&'static str> {
    registry::templates().iter().map(|t| t.id()).collect()
}

/// A deterministic stream of numbers in `-1..=1` (xorshift64*), so the random samples never change between runs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let bits = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        bits as f64 / (1_u64 << 52) as f64 - 1.0
    }

    fn shape(&mut self, reach: f64) -> BodyShape {
        let mut axis = || self.next() * reach;
        BodyShape {
            height: axis(),
            build: axis(),
            curves: axis(),
            musculature: axis(),
            shoulder_hip: axis(),
            preset: None,
        }
    }
}

/// How visibly two bodies differ in the outline of the `part` of them that an axis acts on: the larger of their front and
/// side silhouette differences ([`silhouette::Mask::difference`]). The whole body for the axes that move all of it; the torso alone
/// for `curves`, whose waist, bust and hips the hanging arms hide in a whole-body front view.
fn visible_difference(a: &BodyLayout, b: &BodyLayout, part: &[BodyRegion]) -> f64 {
    let pixel = 0.002;
    // One window for both bodies: the taller of the two (a body at +1 height is 12% above its template).
    let unit = (a.head_base().unwrap().y.max(b.head_base().unwrap().y) / 0.627).max(1.0);
    [View::Front, View::Side]
        .into_iter()
        .map(|v| silhouette_of(a, part, v, pixel, unit).difference(&silhouette_of(b, part, v, pixel, unit)))
        .fold(0.0, f64::max)
}

// ------------------------------------------------------------------------------------------------ identity and sanity

#[test]
fn the_zero_shape_is_the_identity_exactly_and_bad_numbers_read_as_zero() {
    let m = marks();
    let nan = BodyShape { height: f64::NAN, build: f64::INFINITY * 0.0, curves: f64::NAN, ..BodyShape::default() };
    for shape in [BodyShape::default(), nan] {
        let s = Shape::new(&shape);
        assert_eq!(
            (s.length(), s.arm_spacing(&m), s.leg_spacing(&m), s.hang_width(&m), s.arm_girth(&m)),
            (1.0, 1.0, 1.0, 1.0, 1.0)
        );
        for v in fractions(100) {
            for g in [s.torso(v, &m), s.neck(v), s.arm(v, &m), s.leg(v, &m)] {
                assert_eq!(g, Gain::ONE, "at {v}");
            }
        }
    }
    // Out-of-range numbers are held at the ends of the range.
    let wild = BodyShape { height: 9.0, build: -9.0, curves: 4.0, musculature: -2.0, shoulder_hip: 1.5, preset: None };
    let ends = BodyShape { height: 1.0, build: -1.0, curves: 1.0, musculature: -1.0, shoulder_hip: 1.0, preset: None };
    assert_eq!(soft(&wild).joints().collect::<Vec<_>>(), soft(&ends).joints().collect::<Vec<_>>());
    assert_eq!(sec(&soft(&wild), BodyRegion::Torso, 0.4), sec(&soft(&ends), BodyRegion::Torso, 0.4));
}

// ------------------------------------------------------------------------------------------------ the axes

#[test]
fn height_lengthens_the_body_from_the_sole_to_the_head_base_by_12_percent_and_not_the_girth() {
    for id in templates() {
        let base = layout_of(id, &BodyShape::default());
        let tall = layout_of(id, &axis("height", 1.0));
        let short = layout_of(id, &axis("height", -1.0));
        let top = |l: &BodyLayout| l.head_base().unwrap().y;
        assert!(
            (top(&tall) / top(&base) - 1.12).abs() < 1e-9 && (top(&short) / top(&base) - 0.88).abs() < 1e-9,
            "{id}"
        );
        // Every segment of the legs, trunk and arms grows by the same factor: it is the whole body, not just the legs.
        // Lengths along the body: a template's thigh may lean, and only its vertical reach scales with height.
        let len = |l: &BodyLayout, a, b| (joint(l, a) - joint(l, b)).y.abs();
        for (a, b) in [
            (HumanoidBone::LeftUpperLeg, HumanoidBone::LeftLowerLeg),
            (HumanoidBone::LeftLowerLeg, HumanoidBone::LeftFoot),
            (HumanoidBone::Spine, HumanoidBone::Chest),
            (HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm),
            (HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand),
        ] {
            assert!((len(&tall, a, b) / len(&base, a, b) - 1.12).abs() < 1e-9, "{id}: {a} to {b}");
        }
        // The soles stay on the floor and the sections keep their girth.
        assert!(sec(&tall, BodyRegion::FootL, 0.5).centre.y - sec(&tall, BodyRegion::FootL, 0.5).radii.y < 1e-12);
        for region in [BodyRegion::Torso, BodyRegion::LegL, BodyRegion::ArmL] {
            let girth = sec(&tall, region, 0.5).radii / sec(&base, region, 0.5).radii;
            assert!((girth - 1.0).abs().max_element() < 0.06, "{id} {region:?}: {girth}");
        }
    }
}

#[test]
fn the_crown_of_the_default_head_rises_by_about_7_percent_at_plus_one_height() {
    // The head keeps its size and sits on the head base, so the crown moves by the head base's rise only.
    let c = HeadParams::default().cranium;
    let crown = c.centre()[1] + c.half_height_up;
    let (base, tall) = (soft(&BodyShape::default()), soft(&axis("height", 1.0)));
    let rise = tall.head_base().unwrap().y - base.head_base().unwrap().y;
    let share = rise / crown;
    eprintln!("height +1: the body is 12% longer, the crown {:.1}% higher", share * 100.0);
    assert!((0.06..0.08).contains(&share), "{share}");
}

/// The bones whose length is the body's length: the trunk and the limbs, not the lateral roots (hips to leg, chest to
/// shoulder, shoulder to arm), which are offsets across the body that build spaces out.
const LENGTH_BONES: [HumanoidBone; 8] = [
    HumanoidBone::Spine,
    HumanoidBone::Chest,
    HumanoidBone::UpperChest,
    HumanoidBone::Neck,
    HumanoidBone::LeftLowerArm,
    HumanoidBone::LeftHand,
    HumanoidBone::LeftLowerLeg,
    HumanoidBone::LeftFoot,
];

#[test]
fn build_changes_no_bone_length() {
    // Joint to joint, not a region's arc length (a swung arm starts further in from a narrow shoulder, which moves its
    // section path but no bone). Rotating and spacing keep lengths to float noise (1e-12 of a metre-scale distance).
    for id in templates() {
        let base = layout_of(id, &BodyShape::default());
        for b in [-1.0, -0.5, 0.5, 1.0] {
            let l = layout_of(id, &axis("build", b));
            for bone in LENGTH_BONES {
                let parent = bone.parent().unwrap();
                let len = |l: &BodyLayout| joint(l, bone).distance(joint(l, parent));
                assert!(
                    (len(&l) / len(&base) - 1.0).abs() < 1e-9,
                    "{id}: {bone} at build {b}: {} against {}",
                    len(&l),
                    len(&base)
                );
            }
        }
    }
}

#[test]
fn build_never_makes_any_region_thinner_as_it_grows() {
    for id in templates() {
        let steps = [-1.0, -0.5, 0.0, 0.5, 1.0].map(|b| layout_of(id, &axis("build", b)));
        for region in [BodyRegion::Torso, BodyRegion::Neck, BodyRegion::ArmL, BodyRegion::LegL] {
            for t in fractions(60) {
                for pair in steps.windows(2) {
                    let (sa, sb) = (sec(&pair[0], region, t), sec(&pair[1], region, t));
                    let (a, b) = (sa.radii, sb.radii);
                    // A thigh in the pelvis, and just below it where it blends away from the pelvis's outline,
                    // follows that outline: how wide it is there is the torso's to say (its slope at the bottom, which
                    // build changes), so only the thigh below the blend is held to growing.
                    let (bottom, top) = (
                        sec(&pair[0], BodyRegion::Torso, 0.0).centre.y,
                        sec(&pair[0], BodyRegion::Torso, 1.0).centre.y,
                    );
                    if region == BodyRegion::LegL && sa.centre.y >= bottom - 0.25 * (top - bottom) {
                        continue;
                    }
                    assert!(b.x >= a.x - 1e-12 && b.y >= a.y - 1e-12, "{id} {region:?} at {t}: {a} then {b}");
                }
            }
        }
    }
}

/// The torso's proportions in front and side view: the waist's half-width over the hips' (the torso's bottom) and over
/// the chest's (at the arms' roots), the hips over the chest, and the waist's depth over the seat's.
struct Torso {
    waist_hip: f64,
    waist_chest: f64,
    hips_chest: f64,
    waist_seat: f64,
}

fn torso_ratios(l: &BodyLayout, m: &Marks) -> Torso {
    let at = |t| sec(l, BodyRegion::Torso, t).radii;
    let seat = fractions(20).map(|k| at(0.15 * k).y).fold(0.0, f64::max);
    Torso {
        waist_hip: at(m.waist).x / at(0.0).x,
        waist_chest: at(m.waist).x / at(m.arm_root).x,
        hips_chest: at(0.0).x / at(m.arm_root).x,
        waist_seat: at(m.waist).y / seat,
    }
}

/// The least the waist may be of the hips' width in front at `curves = 1` on any corner of the cube. Stylised drawing
/// guides put the idealised waist-to-hip ratio between 0.5 and 0.75 (an hourglass about 0.7); the template's own is
/// 0.84 and a straight body's 1.0.
const WAIST_HIP_FLOOR: f64 = 0.55;

#[test]
fn curves_gives_the_waist_bust_and_hips_their_own_profile_in_front_and_side_and_leaves_the_thighs() {
    let (m, base, curvy) = (marks(), soft(&BodyShape::default()), soft(&axis("curves", 1.0)));
    let at = |l: &BodyLayout, t| sec(l, BodyRegion::Torso, t);
    let (waist, bust) = (m.waist, m.bust);

    // Front view: the waist narrows and the hips widen into an hourglass of a stylised, not an exaggerated, kind.
    let (n, c, s) = (torso_ratios(&base, &m), torso_ratios(&curvy, &m), torso_ratios(&soft(&axis("curves", -1.0)), &m));
    eprintln!(
        "waist:hip {:.2} (template {:.2}, curves -1 {:.2}); waist:chest {:.2} ({:.2}, {:.2}); hips:chest {:.2} ({:.2}, {:.2}); waist:seat depth {:.2} ({:.2}, {:.2})",
        c.waist_hip,
        n.waist_hip,
        s.waist_hip,
        c.waist_chest,
        n.waist_chest,
        s.waist_chest,
        c.hips_chest,
        n.hips_chest,
        s.hips_chest,
        c.waist_seat,
        n.waist_seat,
        s.waist_seat
    );
    assert!((0.58..0.68).contains(&c.waist_hip), "waist:hip {}", c.waist_hip);
    assert!(c.hips_chest <= 1.12, "the hips stay near the chest's width: {}", c.hips_chest);
    assert!(c.waist_chest < n.waist_chest - 0.1 && s.waist_hip > 0.95, "{} {}", c.waist_chest, s.waist_hip);
    assert!(c.waist_seat < n.waist_seat - 0.08, "the waist is indented in depth too: {}", c.waist_seat);

    // Side view: the bust stands out in front, the seat behind, and the waist is indented from both sides.
    let front = |l: &BodyLayout, t| at(l, t).centre.z + at(l, t).radii.y;
    let back = |l: &BodyLayout, t| at(l, t).centre.z - at(l, t).radii.y;
    let (bust_out, seat_out) = (front(&curvy, bust) - front(&base, bust), back(&base, 0.08) - back(&curvy, 0.08));
    eprintln!("curves +1: bust {:.1} mm forward, seat {:.1} mm back", bust_out * 1e3, seat_out * 1e3);
    assert!(bust_out > 0.012 && seat_out > 0.008, "{bust_out} {seat_out}");
    assert!(front(&base, waist) - front(&curvy, waist) > 0.004 && back(&curvy, waist) - back(&base, waist) > 0.002);

    // Independent of the legs: the thigh's radii move by under a third of what the waist's do.
    let thigh_t = m.hip_joint + 0.5 * (m.knee - m.hip_joint);
    let thigh = sec(&curvy, BodyRegion::LegL, thigh_t).radii / sec(&base, BodyRegion::LegL, thigh_t).radii;
    let trunk = at(&curvy, waist).radii / at(&base, waist).radii;
    let (thigh_change, trunk_change) = ((thigh - 1.0).abs().max_element(), (trunk - 1.0).abs().max_element());
    eprintln!(
        "curves +1: the waist's radii change by {:.0}%, the mid-thigh's by {:.0}%",
        trunk_change * 100.0,
        thigh_change * 100.0
    );
    assert!(trunk_change > 3.0 * thigh_change, "waist {trunk_change}, thigh {thigh_change}");

    // The other end is a straight body: the waist fills towards the chest and the bust flattens.
    let straight = soft(&axis("curves", -1.0));
    assert!(at(&straight, waist).radii.x > 1.1 * at(&base, waist).radii.x);
    assert!(front(&straight, bust) < front(&base, bust) - 0.008);
}

#[test]
fn every_curvy_corner_keeps_the_waist_at_least_55_percent_of_the_hips_in_front() {
    let m = marks();
    let mut worst = (f64::MAX, 0);
    for c in (0..32).filter(|c| corner(*c).curves > 0.0) {
        let ratio = torso_ratios(&soft(&corner(c)), &m).waist_hip;
        if ratio < worst.0 {
            worst = (ratio, c);
        }
        assert!(ratio >= WAIST_HIP_FLOOR, "corner {c:05b}: waist:hip {ratio}");
    }
    eprintln!("curvy corners: waist:hip at least {:.2} (corner {:05b})", worst.0, worst.1);
}

/// The front and back edges of a section (the z of its extremes; for an arm, whose axis leans in the front plane, that is
/// the front-to-back extent as the body sees it).
fn front_back(s: &RegionSection) -> (f64, f64) {
    (s.centre.z + s.radii.y, s.centre.z - s.radii.y)
}

/// The least a limb's radius may keep between two bulges, as a share of the lower bulge: the template tapers, so the
/// shape must not add beads (a bulge, a pinch, a bulge). Measured over both radii along the limb ([`worst_dip`]).
const DIP_FLOOR: f64 = 0.92;

#[test]
fn musculature_gives_each_muscle_its_own_sides_and_tapers_through_the_elbow_and_knee() {
    let (m, base, strong) = (marks(), soft(&BodyShape::default()), soft(&axis("musculature", 1.0)));
    let pair = |region, t| (sec(&strong, region, t), sec(&base, region, t));

    // The arm. The deltoid caps the shoulder, outward; the biceps lies in front and the triceps behind; the forearm's
    // extensors reach outward; the elbow and the wrist carry on the taper.
    let e = m.elbow;
    let (d, d0) = pair(BodyRegion::ArmL, 0.16 * e);
    assert!(d.radii.x / d0.radii.x > 1.15 && d.centre.x - d0.centre.x > 0.0015, "deltoid: {d:?} {d0:?}");
    let (u, u0) = pair(BodyRegion::ArmL, 0.55 * e);
    let ((front, back), (front0, back0)) = (front_back(&u), front_back(&u0));
    assert!(
        front - front0 > 0.004 && back0 - back > 0.002 && front - front0 > back0 - back,
        "biceps in front, triceps behind"
    );
    let (f, f0) = pair(BodyRegion::ArmL, e + 0.28 * (1.0 - e));
    assert!(f.radii.x / f0.radii.x > 1.08 && f.centre.x - f0.centre.x > 0.0008, "forearm: {f:?} {f0:?}");
    let (elbow, elbow0) = pair(BodyRegion::ArmL, e);
    let (wrist, wrist0) = pair(BodyRegion::ArmL, 1.0);
    let flat = elbow.radii / elbow0.radii;
    eprintln!(
        "arm at +1: deltoid x{:.2}, biceps front {:.1} mm and triceps back {:.1} mm, forearm x{:.2}, elbow x{:.2}/{:.2}, wrist x{:.2}",
        d.radii.x / d0.radii.x,
        (front - front0) * 1e3,
        (back0 - back) * 1e3,
        f.radii.x / f0.radii.x,
        flat.x,
        flat.y,
        wrist.radii.x / wrist0.radii.x
    );
    assert!(flat.max_element() < 1.12, "the elbow takes the taper gently, no pinch and no bead: {flat}");
    assert!((wrist.radii.x / wrist0.radii.x - 1.0).abs() < 0.03, "a bare wrist");

    // The leg. Quadriceps in front and outward, hamstrings behind, the inside of the thigh as it was; the calf behind and
    // to both sides; the knee carries on the taper.
    let thigh_t = m.hip_joint + 0.40 * (m.knee - m.hip_joint);
    let (t, t0) = pair(BodyRegion::LegL, thigh_t);
    let outer = |s: &RegionSection| s.centre.x + s.radii.x;
    let inner = |s: &RegionSection| s.centre.x - s.radii.x;
    assert!(outer(&t) - outer(&t0) > 0.2 * t0.radii.x && (inner(&t) - inner(&t0)).abs() < 1e-9, "outward only");
    let ((front, back), (front0, back0)) = (front_back(&t), front_back(&t0));
    assert!(front - front0 > 0.006 && back0 - back > 0.003, "quads in front, hamstrings behind");
    let calf_t = m.knee + 0.34 * (m.ankle - m.knee);
    let (c, c0) = pair(BodyRegion::LegL, calf_t);
    let ((front, back), (front0, back0)) = (front_back(&c), front_back(&c0));
    assert!(back0 - back > 0.25 * c0.radii.y && front - front0 < 0.3 * (back0 - back), "the calf is on the back");
    assert!(inner(&c0) - inner(&c) > 0.001 && outer(&c) - outer(&c0) > 0.001, "and to both sides");
    let knee = pair(BodyRegion::LegL, m.knee);
    let knee_gain = knee.0.radii / knee.1.radii;
    eprintln!(
        "leg at +1: thigh outer +{:.1} mm, calf back +{:.1} mm, knee x{:.2}/{:.2}",
        (outer(&t) - outer(&t0)) * 1e3,
        (back0 - back) * 1e3,
        knee_gain.x,
        knee_gain.y
    );
    assert!(knee_gain.max_element() < 1.12, "{knee_gain}");

    // The trunk and neck: a broader shoulder line, a thicker neck, and a soft body is the other way.
    let shoulder = |l: &BodyLayout| sec(l, BodyRegion::Torso, m.shoulder).radii.x;
    let neck = |l: &BodyLayout| sec(l, BodyRegion::Neck, 0.5).radii.x;
    let soft_body = soft(&axis("musculature", -1.0));
    assert!(shoulder(&strong) > 1.1 * shoulder(&base) && shoulder(&soft_body) < 0.9 * shoulder(&base));
    assert!(neck(&strong) > 1.2 * neck(&base) && neck(&soft_body) < 0.88 * neck(&base));
    assert!(sec(&soft_body, BodyRegion::ArmL, 0.16 * e).radii.x < 0.9 * sec(&base, BodyRegion::ArmL, 0.16 * e).radii.x);
}

#[test]
fn no_limb_gets_a_bead_at_either_end_of_musculature_or_at_any_corner() {
    let mut worst = (1.0_f64, 1.0_f64);
    for shape in [axis("musculature", 1.0), axis("musculature", -1.0)].into_iter().chain((0..32).map(corner)) {
        let l = soft(&shape);
        let (arm, leg) = (worst_dip(&l, BodyRegion::ArmL, 0.0), worst_dip(&l, BodyRegion::LegL, marks().crotch));
        worst = (worst.0.min(arm), worst.1.min(leg));
        assert!(arm >= DIP_FLOOR && leg >= DIP_FLOOR, "{shape:?}: arm {arm:.3}, leg {leg:.3}");
    }
    assert!(
        (worst_dip(&soft(&BodyShape::default()), BodyRegion::LegL, marks().crotch) - 1.0).abs() < 1e-12,
        "the template tapers"
    );
    eprintln!("worst dip over the ends of musculature and the 32 corners: arm {:.3}, leg {:.3}", worst.0, worst.1);
}

#[test]
fn shoulder_hip_trades_the_shoulder_line_against_the_hips_and_keeps_their_mean() {
    let (m, base) = (marks(), soft(&BodyShape::default()));
    let at = |l: &BodyLayout, t| sec(l, BodyRegion::Torso, t).radii.x;
    for s in [1.0, -1.0] {
        let l = soft(&axis("shoulder_hip", s));
        let (shoulders, hips) = (at(&l, m.shoulder) / at(&base, m.shoulder), at(&l, 0.0) / at(&base, 0.0));
        // (The gain is sampled at 64 knots and interpolated, so a point between two knots is a hair off the peak.)
        assert!(
            (shoulders - (1.0 + 0.18 * s)).abs() < 1e-3 && (hips - (1.0 - 0.18 * s)).abs() < 1e-3,
            "{shoulders} {hips}"
        );
        assert!((f64::midpoint(shoulders, hips) - 1.0).abs() < 1e-3, "the mean stays");
        // The arms ride out with the shoulder line and the legs close in with the hips.
        let (arm, leg) = (joint(&l, HumanoidBone::LeftUpperArm).x, joint(&l, HumanoidBone::LeftUpperLeg).x);
        let (arm0, leg0) = (joint(&base, HumanoidBone::LeftUpperArm).x, joint(&base, HumanoidBone::LeftUpperLeg).x);
        assert!((arm / arm0 - 1.0) * s > 0.1, "arms {}", arm / arm0);
        assert!((leg / leg0 - 1.0) * s < -0.05, "legs {}", leg / leg0);
    }
}

#[test]
fn axes_multiply_so_any_order_of_applying_them_gives_the_same_gain() {
    let m = marks();
    let mut rng = Rng(0x00B0_D15E);
    for _ in 0..50 {
        let shape = rng.shape(0.3); // inside the range where gains are exact (see `limit`)
        let together = Shape::new(&shape);
        let mut order = [0, 1, 2, 3, 4];
        for i in (1..5).rev() {
            order.swap(i, (f64::midpoint(rng.next(), 1.0) * (i as f64 + 1.0)) as usize % (i + 1));
        }
        let single = |k: usize| {
            let mut s = BodyShape::default();
            let v = [shape.height, shape.build, shape.curves, shape.musculature, shape.shoulder_hip][k];
            [&mut s.height, &mut s.build, &mut s.curves, &mut s.musculature, &mut s.shoulder_hip][k].clone_from(&v);
            Shape::new(&s)
        };
        for v in fractions(40) {
            for (region, name) in ["torso", "neck", "arm", "leg"].into_iter().enumerate() {
                let gain = |s: &Shape| match region {
                    0 => s.torso(v, &m),
                    1 => s.neck(v),
                    2 => s.arm(v, &m),
                    _ => s.leg(v, &m),
                };
                let a = gain(&together);
                let b = order.iter().map(|&k| gain(&single(k))).fold(Gain::ONE, |a, b| a * b);
                let worst = [a.outer - b.outer, a.inner - b.inner, a.front - b.front, a.back - b.back];
                assert!(
                    worst.iter().all(|d| d.abs() < 1e-9),
                    "{name} at {v}: {a:?} against {b:?} ({shape:?}, order {order:?})"
                );
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ distinct in silhouette

const TORSO: &[BodyRegion] = &[BodyRegion::Torso];

/// The least a single axis end may change the silhouette of the part it acts on by ([`visible_difference`], as a share of
/// the pixels either body covers). About 60% of what the shipped numbers measure, so an axis that stops reading at an end
/// fails here before an artist sees it.
const AXIS_FLOORS: [(&str, &[BodyRegion], f64, f64); 5] = [
    // (axis, the part it acts on, floor at -1, floor at +1)
    ("height", &REGIONS, 0.20, 0.20),
    ("build", &REGIONS, 0.10, 0.14),
    ("curves", TORSO, 0.045, 0.05),
    ("musculature", &REGIONS, 0.05, 0.09),
    ("shoulder_hip", &REGIONS, 0.13, 0.12),
];

#[test]
fn every_axis_end_changes_the_silhouette_by_more_than_its_floor() {
    for id in templates() {
        let base = layout_of(id, &BodyShape::default());
        assert_eq!(visible_difference(&base, &base, &REGIONS), 0.0);
        for (name, part, minus, plus) in AXIS_FLOORS {
            for (value, floor) in [(-1.0, minus), (1.0, plus)] {
                let d = visible_difference(&base, &layout_of(id, &axis(name, value)), part);
                eprintln!(
                    "{id} {name} {value:+}: silhouette difference {:.1}% (floor {:.1}%)",
                    d * 100.0,
                    floor * 100.0
                );
                assert!(d > floor, "{id} {name} {value:+}: {d} is not above {floor}");
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ presets

const PRESET_IDS: [&str; 10] =
    ["average", "slim", "broad", "curvy", "athletic", "stocky", "pear", "hourglass", "lean-tall", "petite"];

/// The least two presets may differ by, as [`visible_difference`] of the whole body (the closest pair, curvy and pear, measures 9%).
const PRESET_FLOOR: f64 = 0.05;

#[test]
fn the_library_has_the_ten_presets_each_five_valid_numbers_and_average_is_the_template() {
    let all = shape::presets().unwrap();
    assert_eq!(all.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), PRESET_IDS);
    for p in all {
        assert_eq!(p.shape.preset.as_ref(), Some(&p.id), "{}", p.id.as_str());
        assert!(omuzaki_face_api::Validate::validate(&p.shape).is_ok() && !p.summary.is_empty(), "{}", p.id.as_str());
    }
    let average = shape::preset("average").unwrap().unwrap();
    assert_eq!(BodyShape { preset: None, ..average.shape.clone() }, BodyShape::default());
    assert!(shape::preset("no-such-preset").unwrap().is_none());
}

#[test]
fn all_presets_are_pairwise_distinct_in_silhouette_on_every_template() {
    let all = shape::presets().unwrap();
    for id in templates() {
        let layouts: Vec<BodyLayout> = all.iter().map(|p| layout_of(id, &p.shape)).collect();
        let mut closest = (f64::MAX, "", "");
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate().skip(i + 1) {
                let d = visible_difference(&layouts[i], &layouts[j], &REGIONS);
                if d < closest.0 {
                    closest = (d, a.id.as_str(), b.id.as_str());
                }
            }
        }
        eprintln!("{id}: the closest presets are {} and {}, {:.1}% apart", closest.1, closest.2, closest.0 * 100.0);
        assert!(closest.0 > PRESET_FLOOR, "{id}: {} and {} differ by only {}", closest.1, closest.2, closest.0);
    }
}

#[test]
fn a_blend_of_two_presets_lies_between_them() {
    let (slim, broad) = (shape::preset("slim").unwrap().unwrap(), shape::preset("broad").unwrap().unwrap());
    let (a, b) = (soft(&slim.shape), soft(&broad.shape));
    let mid = soft(&slim.shape.lerp(&broad.shape, 0.5));
    let whole = visible_difference(&a, &b, &REGIONS);
    assert!(visible_difference(&a, &mid, &REGIONS) < whole && visible_difference(&mid, &b, &REGIONS) < whole);
}

// ------------------------------------------------------------------------------------------------ the whole space is sound

/// The section of a vertical region (torso, leg) at height `y`, if the region reaches it. A vertical region's height
/// runs evenly along it, so the fraction is a ratio.
fn at_height(l: &BodyLayout, region: BodyRegion, y: f64) -> Option<RegionSection> {
    let (first, last) = (sec(l, region, 0.0).centre.y, sec(l, region, 1.0).centre.y);
    let t = (y - first) / (last - first);
    (0.0..=1.0).contains(&t).then(|| sec(l, region, t))
}

/// How far outside the ellipse of a vertical region's section the point `p` is, in metres along the ray from the
/// section's centre: negative inside, positive outside.
fn outside_by(s: &RegionSection, p: DVec3) -> f64 {
    let (dx, dz) = (p.x - s.centre.x, p.z - s.centre.z);
    let f = (dx / s.radii.x).powi(2) + (dz / s.radii.y).powi(2);
    dx.hypot(dz) * (1.0 - 1.0 / f.sqrt().max(1e-12))
}

/// The points around a section of `region` (`n` of them, in its own plane).
fn ring(s: &RegionSection, region: BodyRegion, n: usize) -> Vec<DVec3> {
    let (across, through) = section_basis(region, s.axis.as_vec3());
    (0..n)
        .map(|k| {
            let a = k as f64 / n as f64 * std::f64::consts::TAU;
            s.centre + across.as_dvec3() * (s.radii.x * a.sin()) + through.as_dvec3() * (s.radii.y * a.cos())
        })
        .collect()
}

/// How far the section `inner` of a vertical region pokes out of the section `outer` of another (the least it must
/// have been inside), in metres; negative if it is inside.
fn poke(inner: &RegionSection, region: BodyRegion, outer: &RegionSection) -> f64 {
    ring(inner, region, 48).into_iter().map(|p| outside_by(outer, p)).fold(f64::MIN, f64::max)
}

/// The most a leg may poke out of the torso it runs up inside, as a gate: the template's own thighs reach 0.7 mm.
const LEG_POKE: f64 = 1e-3;
/// The share of the torso's length, above its bottom, where the thigh is leaving the pelvis (see `assert_clear`).
const HIP_LEAVING: f64 = 0.1;
/// The most a thigh may reach out of the torso's front and back there: still a sliver (see `assert_clear`).
const LEAVING_POKE: f64 = 2.5e-2;
/// The least an arm keeps from the torso below the shoulder, in metres (the template's is over 50 mm).
const ARM_CLEARANCE: f64 = 3e-3;
/// The least gap between the thighs, as a share of a thigh's width, at every corner of the cube (the template's is 3.5%:
/// its thighs nearly touch).
const THIGH_GAP: f64 = 0.08;

/// The rules the shape space keeps on top of the layout's own (`template_soft_sd.rs`'s `assert_sound`): no section is
/// inverted or empty, the legs do not overlap and stay inside the torso they run up into, the arms stay attached at the
/// shoulder and hang clear of the trunk, the legs and each other.
fn assert_clear(l: &BodyLayout, what: &str) {
    for region in REGIONS {
        for t in fractions(100) {
            let s = sec(l, region, t);
            assert!(
                s.radii.min_element() > 0.0 && s.radii.is_finite() && s.centre.is_finite(),
                "{what}: {region:?} at {t}: {s:?}"
            );
        }
    }
    for t in fractions(100) {
        let s = sec(l, BodyRegion::LegL, t);
        assert!(s.centre.x - s.radii.x > 0.0, "{what}: the legs overlap at {t}: {}", s.centre.x - s.radii.x);
        // Where the legs run up inside the torso they must stay inside it in the horizontal section, however it narrows
        // (a waist) or flattens (a slim build), to within a millimetre: the gain is sampled at 64 points and interpolated.
        // The last tenth of the torso's length above its bottom is where the thigh leaves the pelvis: it follows the
        // pelvis's outline there (below the bottom there is no pelvis to be inside), so a round thigh may reach past a thin pelvis's front and back (6 mm at the worst of mini and soft-sd's
        // corners, 22 mm at the standard's and tall's thin-pelvis, thick-thigh corner).
        let (bottom, top) = (sec(l, BodyRegion::Torso, 0.0).centre.y, sec(l, BodyRegion::Torso, 1.0).centre.y);
        let leaving = s.centre.y < bottom + HIP_LEAVING * (top - bottom);
        if let Some(torso) = at_height(l, BodyRegion::Torso, s.centre.y).filter(|_| s.centre.y >= bottom) {
            let out = poke(&s, BodyRegion::LegL, &torso);
            let most = if leaving { LEAVING_POKE } else { LEG_POKE };
            assert!(out < most, "{what}: a leg pokes {out} m out of the torso at {t}, height {:.3}", s.centre.y);
        }
    }
    for (arm, side) in [(BodyRegion::ArmL, 1.0), (BodyRegion::ArmR, -1.0)] {
        // The arm stays attached: its first section reaches the torso at the shoulder.
        let root = sec(l, arm, 0.0);
        if let Some(torso) = at_height(l, BodyRegion::Torso, root.centre.y) {
            let gap = outside_by(&torso, root.centre);
            assert!(gap < root.radii.x, "{what}: {arm:?} floats {gap} m off the shoulder");
        }
        // From the deltoid down (the root of the arm is inside the shoulder by design) it hangs clear.
        for t in fractions(40).filter(|t| *t >= 0.35) {
            let s = sec(l, arm, t);
            for p in ring(&s, arm, 16) {
                assert!(p.x * side > 0.0, "{what}: {arm:?} crosses the midline at {t}");
                for body in [BodyRegion::Torso, BodyRegion::LegL, BodyRegion::LegR] {
                    if let Some(b) = at_height(l, body, p.y) {
                        let gap = outside_by(&b, p);
                        assert!(
                            gap > ARM_CLEARANCE,
                            "{what}: {arm:?} is only {gap} m from {body:?} at {t}, height {:.3}",
                            p.y
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn every_corner_of_the_cube_a_grid_and_random_shapes_stay_clear_on_every_template() {
    let mut rng = Rng(0x5EED_0444);
    for id in templates() {
        let mut shapes: Vec<BodyShape> = (0..32).map(corner).collect();
        shapes.extend((0..243_u32).map(|c| {
            let at = |i: u32| f64::from((c / 3_u32.pow(i)) % 3) - 1.0;
            BodyShape {
                height: at(0),
                build: at(1),
                curves: at(2),
                musculature: at(3),
                shoulder_hip: at(4),
                preset: None,
            }
        }));
        shapes.extend((0..200).map(|_| rng.shape(1.0)));
        for shape in &shapes {
            assert_clear(&layout_of(id, shape), &format!("{id} {shape:?}"));
        }
    }
}

#[test]
fn every_template_passes_the_conformance_suite_with_the_full_shape_space_in_its_samples() {
    for template in registry::templates() {
        if let Err(violations) = conformance::run(template) {
            panic!("{violations}");
        }
    }
}

#[test]
fn the_thighs_read_as_two_at_every_corner_of_the_cube() {
    let m = marks();
    let mut least = f64::MAX;
    for c in 0..32 {
        let l = soft(&corner(c));
        for t in fractions(60).map(|k| m.crotch + k * (m.knee - m.crotch)) {
            let s = sec(&l, BodyRegion::LegL, t);
            let gap = (s.centre.x - s.radii.x) / s.radii.x;
            least = least.min(gap);
            assert!(gap >= THIGH_GAP, "corner {c:05b}: the gap between the thighs is {gap:.3} of a thigh at {t}");
        }
    }
    eprintln!("least gap between the thighs at a corner: {:.1}% of a thigh's width", least * 100.0);
}

#[test]
fn every_corner_at_the_slider_extremes_keeps_every_bone_a_sensible_length() {
    // The conformance suite's bone-length rule (0.4 to 2.5 times the default, 2.8 for the lateral roots), over all 32
    // corners of the shape cube at all 256 corners of the proportion sliders (its own samples are random). That is
    // every pair in the nightly run (`OMUZAKI_FULL_TESTS`); the pull request gate, whose test shards have a time limit
    // (#2300), takes every seventh pair, from a different start on each template. Seven and 256 share no factor, so each
    // template still meets every shape corner and every slider corner.
    let stride = if std::env::var_os("OMUZAKI_FULL_TESTS").is_some() { 1 } else { 7 };
    let lateral = [
        HumanoidBone::LeftUpperLeg,
        HumanoidBone::RightUpperLeg,
        HumanoidBone::LeftShoulder,
        HumanoidBone::RightShoulder,
        HumanoidBone::LeftUpperArm,
        HumanoidBone::RightUpperArm,
    ];
    for (n, id) in templates().into_iter().enumerate() {
        let template = registry::find(id).unwrap();
        let lengths = |l: &BodyLayout| -> Vec<(HumanoidBone, f64)> {
            l.joints().filter_map(|(b, p)| Some((b, p.distance(l.joint64(b.parent()?)?)))).collect()
        };
        let default = lengths(&layout_of(id, &BodyShape::default()));
        for pair in (n % stride..32 * 256).step_by(stride) {
            let (shape_corner, slider_corner) =
                (u32::try_from(pair / 256).unwrap(), u32::try_from(pair % 256).unwrap());
            let pick = |bit: u32| if slider_corner >> bit & 1 == 1 { 2.0 } else { 0.5 };
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
            let l = template.layout(&p, &corner(shape_corner), &HandParams::default());
            for ((bone, now), (_, was)) in lengths(&l).into_iter().zip(&default) {
                let (low, high) = (0.4, if lateral.contains(&bone) { f64::from(LATERAL_ROOT_MAX_RATIO) } else { 2.5 });
                assert!(
                    (low..=high).contains(&(now / was)),
                    "{id}: {bone} is {:.2} times its default length at shape corner {shape_corner} and {p:?}",
                    now / was
                );
            }
        }
    }
}

#[test]
fn the_fit_changes_continuously_with_the_shape_so_outfits_refit_without_jumps() {
    let mut rng = Rng(0xF17);
    let step = 2e-3;
    let mut worst = 0.0_f64;
    for id in templates() {
        for _ in 0..40 {
            let base = rng.shape(0.95);
            let a = layout_of(id, &base);
            for name in AXES {
                let mut moved = base.clone();
                match name {
                    "height" => moved.height += step,
                    "build" => moved.build += step,
                    "curves" => moved.curves += step,
                    "musculature" => moved.musculature += step,
                    _ => moved.shoulder_hip += step,
                }
                let b = layout_of(id, &moved);
                for region in REGIONS {
                    for t in fractions(50) {
                        let (p, q) = (sec(&a, region, t), sec(&b, region, t));
                        let jump = p.centre.distance(q.centre).max((p.radii - q.radii).abs().max_element());
                        worst = worst.max(jump / step);
                    }
                }
                for (bone, p) in a.joints() {
                    worst = worst.max(p.distance(joint(&b, bone)) / step);
                }
            }
        }
    }
    eprintln!("the fit moves at most {worst:.3} m per unit of any axis");
    assert!(worst < 0.4, "{worst}");
}

#[test]
fn a_shaped_body_is_a_valid_bodyfit_for_garments() {
    // What a shirt does: sample every region through the published trait at the extremes.
    let fit: &dyn BodyFit = &soft(&corner(0b10101));
    for region in REGIONS {
        for t in fractions(20) {
            let s = fit.section(region, t as f32).unwrap();
            assert!(s.centre.is_finite() && s.radii.min_element() > 0.0, "{region:?} {t}");
        }
    }
}

/// The worst dip along a limb: for every trough in a radius profile (`rx` or `ry` along the region) between two peaks,
/// the trough over the lower of the peaks; `1` for a profile that never rises again after falling. A template tapers,
/// so a lower value is a bead the shape put there. Measured from the fraction `from` of the limb's length (an arm's collar
/// bone is inside the shoulder, and a leg's top inside the pelvis, where each follows the torso's outline).
fn worst_dip(l: &BodyLayout, region: BodyRegion, from: f64) -> f64 {
    let mut worst = 1.0_f64;
    for pick in [|r: DVec2| r.x, |r: DVec2| r.y] {
        let values: Vec<f64> =
            (0..=400).map(|k| pick(sec(l, region, from + (1.0 - from) * f64::from(k) / 400.0).radii)).collect();
        // Turning points, ignoring changes below a tenth of a millimetre.
        let mut turns: Vec<(bool, f64)> = Vec::new(); // (is a peak, value)
        let mut last = values[0];
        let mut rising: Option<bool> = None;
        for &v in &values[1..] {
            if (v - last).abs() < 1e-7 {
                continue;
            }
            let up = v > last;
            if rising.is_some_and(|r| r != up) {
                turns.push((!up, last));
            }
            rising = Some(up);
            last = v;
        }
        for w in turns.windows(3) {
            if !w[1].0 && w[0].0 && w[2].0 {
                worst = worst.min(w[1].1 / w[0].1.min(w[2].1));
            }
        }
    }
    worst
}
