//! The lock layer (#2480): every lock of a group takes a cross-section, a tip and a split into sub-locks, whichever
//! generator made it. On a ball head with known locks, each option is checked for what it promises; on the real kit,
//! each builds on every template with its locks clear of the head.

#![allow(clippy::expect_used, clippy::unwrap_used)] // fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use omuzaki_char::hair::compile::{CompiledEntry, CompiledGroup};
use omuzaki_char::hair::lock::{self, CLEARANCE};
use omuzaki_char::hair::tube::{OutwardFrame, RING, tube_vertices_shaped};
use omuzaki_char_api::{
    Assembler, BodyTemplateId, BuildOptions, BuildServices, Bulge, CharId, CharacterDef, Falloff, Follow, GroupId,
    HairClump, HairParams, HeadSurface, LockProfile, LockShape, LockTip, PartDef, PartId, PartKindId, PartOutput,
    Taper, build_part, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Vec2, Vec3};
use serde_json::json;

// ------------------------------------------------------------------------------------------------ on a ball head

/// A round head of radius 0.09 m at the origin.
#[derive(Debug)]
struct Ball;

impl HeadSurface for Ball {
    fn centre(&self) -> Vec3 {
        Vec3::ZERO
    }
    fn radius(&self, _: Vec3) -> f32 {
        RADIUS
    }
}

const RADIUS: f32 = 0.09;

fn frame() -> OutwardFrame {
    OutwardFrame { head: Vec3::ZERO, body_axis: Vec2::ZERO }
}

/// A lock down the side of the ball at `phi` degrees round it: 12 points over 20 cm, 1 cm off the surface, 3 cm wide.
fn clump(phi: f32) -> HairClump {
    let (s, c) = phi.to_radians().sin_cos();
    let rest = (0..12)
        .map(|i| {
            let t = i as f32 / 11.0;
            let r = RADIUS + 0.01;
            Vec3::new(s * r, 0.05 - 0.2 * t, c * r)
        })
        .collect();
    HairClump {
        group: GroupId::new("b-locks").unwrap(),
        rest,
        width: Taper::new(0.03, 0.03, 0.004),
        thickness: Bulge { base: 0.004, swell: 0.002 },
        stiffness: Falloff::default(),
        shade: 1.0,
        simulate: false,
        follow: Follow::Head,
        tint: None,
    }
}

/// Three locks round the ball shaped by `shape`.
fn shaped(shape: LockShape) -> Vec<HairClump> {
    let mut entries = vec![CompiledEntry {
        id: GroupId::new("b-locks").unwrap(),
        simulate: false,
        group: CompiledGroup::tubes(vec![clump(-40.0), clump(0.0), clump(40.0)]),
        lock: shape,
    }];
    lock::shape(&mut entries, &frame(), &Ball, None);
    entries.remove(0).group.clumps
}

fn with(f: impl FnOnce(&mut LockShape)) -> LockShape {
    let mut shape = LockShape::default();
    f(&mut shape);
    shape
}

fn length(points: &[Vec3]) -> f32 {
    points.windows(2).map(|w| w[0].distance(w[1])).sum()
}

/// The farthest any lock's points sit inside the ball's clearance, metres (0 if none do).
fn deepest(clumps: &[HairClump]) -> f32 {
    clumps.iter().flat_map(|c| &c.rest).map(|p| (RADIUS + CLEARANCE - p.length()).max(0.0)).fold(0.0, f32::max)
}

#[test]
fn the_default_shape_leaves_the_locks_as_they_were() {
    assert_eq!(shaped(LockShape::default()), vec![clump(-40.0), clump(0.0), clump(40.0)]);
}

#[test]
fn a_split_makes_side_by_side_sub_locks_that_fan_apart_toward_their_tips() {
    for n in 2..=4u8 {
        let clumps = shaped(with(|s| s.split = n));
        assert_eq!(clumps.len(), 3 * usize::from(n));
        let subs = &clumps[usize::from(n)..2 * usize::from(n)]; // the middle lock's
        let (first, last) = (&subs[0].rest, &subs[subs.len() - 1].rest);
        // Side by side at the root, the outer two `spread` lock widths apart at their tips (0.6 x 3 cm).
        let root_gap = first[1].distance(last[1]);
        let tip_gap = first[first.len() - 1].distance(last[last.len() - 1]);
        assert!(root_gap < 0.03, "split {n}: {root_gap} apart at the root");
        assert!(
            tip_gap > root_gap && tip_gap > 0.9 * 0.6 * 0.03,
            "split {n}: {root_gap} at the root, {tip_gap} at the tip"
        );
        // Each is narrower than the lock, but together they cover it.
        let w = subs[0].width.at(0.3);
        assert!(w < 0.03 && w * f32::from(n) >= 0.03, "split {n}: each {w} wide");
        // Respaced evenly by arc length, as every clump is.
        let gaps: Vec<f32> = first.windows(2).map(|p| p[0].distance(p[1])).collect();
        let mean = gaps.iter().sum::<f32>() / gaps.len() as f32;
        assert!(gaps.iter().all(|g| (g - mean).abs() < 0.05 * mean), "uneven: {gaps:?}");
        // Not all the same length.
        let lengths: Vec<f32> = subs.iter().map(|s| length(&s.rest)).collect();
        assert!(lengths.iter().any(|l| (l - lengths[0]).abs() > 1e-3), "{lengths:?}");
    }
}

#[test]
fn spread_and_split_from_set_how_far_and_where_the_sub_locks_part() {
    let tip_gap = |shape: LockShape| {
        let c = shaped(shape);
        let (a, b) = (&c[2].rest, &c[3].rest); // the middle lock's two sub-locks
        a[a.len() - 1].distance(b[b.len() - 1])
    };
    let base = with(|s| (s.split, s.spread) = (2, 1.5));
    assert!(tip_gap(base) > tip_gap(LockShape { spread: 0.2, ..base }) + 0.01);
    let mid_gap = |shape: LockShape| {
        let c = shaped(shape);
        c[2].rest[6].distance(c[3].rest[6])
    };
    assert!(mid_gap(LockShape { split_from: 0.1, ..base }) > mid_gap(LockShape { split_from: 0.8, ..base }) + 0.005);
}

#[test]
fn a_fork_parts_each_lock_in_two_over_its_last_quarter() {
    let clumps = shaped(with(|s| s.tip = LockTip::Fork));
    assert_eq!(clumps.len(), 6);
    let (a, b) = (&clumps[2].rest, &clumps[3].rest);
    let at = |k: usize| a[k].distance(b[k]);
    // Overlapping down the lock (each prong is 5/8 of its width), `spread` widths apart at the tips.
    assert!(at(6) < 0.5 * 0.03, "together down the lock: {}", at(6));
    assert!(at(11) > 0.9 * 0.6 * 0.03 && at(11) > at(6), "parted at the tip: {} against {}", at(11), at(6));
    // Forks of three sub-locks: six tubes per lock.
    assert_eq!(shaped(with(|s| (s.tip, s.split) = (LockTip::Fork, 3))).len(), 18);
}

#[test]
fn a_blunt_tip_keeps_most_of_the_width() {
    let clumps = shaped(with(|s| s.tip = LockTip::Blunt));
    assert!(clumps[1].width.at(1.0) >= 0.75 * 0.03 - 1e-6);
    assert!(clump(0.0).width.at(1.0) < 0.01);
}

#[test]
fn a_curl_bends_the_last_third_under_or_out_and_keeps_its_length() {
    let straight = clump(0.0);
    let tip = |shape| shaped(shape)[1].rest.last().copied().unwrap();
    let (under, out) = (tip(with(|s| s.tip = LockTip::CurlIn)), tip(with(|s| s.tip = LockTip::CurlOut)));
    let end = *straight.rest.last().unwrap();
    // The straight lock hangs at z = 0.1; a curl under turns its tip toward the head (and up), a flick out away.
    assert!(under.y > end.y + 0.02, "the curl rises: {under} against {end}");
    assert!(out.z > end.z + 0.01, "the flick goes out: {out} against {end}");
    assert!(under.z < out.z);
    for shape in [with(|s| s.tip = LockTip::CurlIn), with(|s| s.tip = LockTip::CurlOut)] {
        let curled = &shaped(shape)[1].rest;
        assert!((length(curled) - length(&straight.rest)).abs() < 0.01 * length(&straight.rest));
    }
}

#[test]
fn every_shape_keeps_the_moved_points_clear_of_the_head() {
    for tip in [LockTip::Point, LockTip::Fork, LockTip::Blunt, LockTip::CurlIn, LockTip::CurlOut] {
        for split in 1..=4 {
            let clumps = shaped(LockShape { tip, split, spread: 2.0, ..LockShape::default() });
            assert!(deepest(&clumps) < 1e-4, "{tip:?} split {split}: {} m into the head", deepest(&clumps));
        }
    }
}

#[test]
fn each_profile_has_its_own_cross_section() {
    let lock = clump(0.0);
    let section = |profile| {
        let n = lock.rest.len() * RING;
        let (mut p, mut nrm) = (vec![Vec3::ZERO; n], vec![Vec3::ZERO; n]);
        tube_vertices_shaped(
            &lock.rest,
            RING,
            &lock.width,
            &lock.thickness,
            profile,
            |q| q.normalize(),
            &mut p,
            &mut nrm,
        )
        .unwrap();
        // The ring halfway down: its extent across (x) and through (z, the outward direction here).
        let ring = &p[6 * RING..7 * RING];
        let span = |f: fn(Vec3) -> f32| {
            ring.iter().map(|v| f(*v)).fold(f32::MIN, f32::max) - ring.iter().map(|v| f(*v)).fold(f32::MAX, f32::min)
        };
        assert!(nrm.iter().all(|v| (v.length() - 1.0).abs() < 1e-3));
        (span(|v| v.x), span(|v| v.z))
    };
    let (lens, card, round) = (section(LockProfile::Lens), section(LockProfile::Card), section(LockProfile::Round));
    assert!((lens.0 - card.0).abs() < 1e-4 && (lens.0 - round.0).abs() < 1e-4, "all as wide");
    assert!(card.1 < 0.5 * lens.1, "a card is thin: {card:?} against {lens:?}");
    assert!(round.1 > 0.8 * round.0, "a round lock is as thick as it is wide: {round:?}");
}

// ------------------------------------------------------------------------------------------------ on the kit

fn part(kind: &str, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The default hair on `template`, its bangs shaped by `shape`.
fn hair_on(template: &str, shape: LockShape) -> PartOutput {
    omuzaki_char::link();
    let mut def: CharacterDef = mock_character().unwrap();
    def.template = BodyTemplateId::new(template).unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", json!({})));
    let mut params = HairParams::default();
    params.groups.get_mut(&GroupId::new("b-bangs").unwrap()).unwrap().lock = shape;
    def.parts.insert(PartId::new("hair").unwrap(), part("hair", serde_json::to_value(params).unwrap()));
    let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
    let mut assembler = Assembler::new(&id, &def, &options);
    let mut result = None;
    for p in plan(&def).unwrap() {
        let out = build_part(&def, &p, &options, &services, assembler.published())
            .unwrap_or_else(|e| panic!("{template} with {shape:?}: {e}"));
        if p.as_str() == "hair" {
            result = Some(build_part(&def, &p, &options, &services, assembler.published()).unwrap());
        }
        assembler.add(&p, out).unwrap();
    }
    result.unwrap()
}

fn static_vertices(out: &PartOutput) -> usize {
    out.meshes.iter().find(|m| m.name == "hair-static").map_or(0, |m| m.geometry.positions.len())
}

#[test]
fn every_option_builds_on_every_template() {
    for template in ["soft-sd", "mini", "standard", "tall"] {
        let plain = static_vertices(&hair_on(template, LockShape::default()));
        for profile in [LockProfile::Lens, LockProfile::Card, LockProfile::Round] {
            for tip in [LockTip::Point, LockTip::Fork, LockTip::Blunt, LockTip::CurlIn, LockTip::CurlOut] {
                let shape = LockShape { profile, tip, ..LockShape::default() };
                let out = hair_on(template, shape);
                let vertices = static_vertices(&out);
                // A fork doubles the bangs' tubes; the rest keep their count.
                assert!(
                    if tip == LockTip::Fork { vertices > plain } else { vertices == plain },
                    "{template} {profile:?} {tip:?}: {vertices} vertices against {plain}"
                );
            }
        }
        assert!(static_vertices(&hair_on(template, LockShape { split: 3, ..LockShape::default() })) > plain);
    }
}
