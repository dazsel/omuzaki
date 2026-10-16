//! The `curtain` generator's guides on a synthetic head and envelope: counts, rest points, clearance from the head,
//! the random stream's order, and `u`. Everything is synthetic.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use omuzaki_char::hair::compile::{
    CompiledGroup, Dispatch, Forked, GroupGeometry, HairGenCtx, UnitStream, compile_groups,
};
use omuzaki_char::hair::r#gen::curtain::{self, GUIDE_POINTS};
use omuzaki_char_api::{CurtainGen, Dynamics, Generator, GroupId, HairGroup, HairParams, HeadSurface, Quality};
use omuzaki_core::glam::Vec3;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::Palette;

/// An ellipsoid head: centre `(0, 1.6, 0)`, semi-axes `(0.08, 0.11, 0.095)`.
#[derive(Debug)]
struct Egg;

impl HeadSurface for Egg {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 1.6, 0.0)
    }
    fn radius(&self, dir: Vec3) -> f32 {
        1.0 / (dir / Vec3::new(0.08, 0.11, 0.095)).length()
    }
}

/// A synthetic envelope: the hair stands further from the body axis the lower it goes, from the neck to the hip.
fn params(count: u16) -> CurtainGen {
    CurtainGen {
        count,
        envelope: vec![[1.6, 0.11], [1.4, 0.17], [1.2, 0.22], [0.9, 0.26], [0.5, 0.3]],
        root_tuck_deg: 0.0, // the prototype's own guides; the tuck has its own tests
        ..CurtainGen::default()
    }
}

/// The stream a test scripts: `values` cycle, and every draw is counted.
struct Script {
    values: Vec<f64>,
    drawn: usize,
}

impl UnitStream for Script {
    fn next_unit(&mut self) -> f64 {
        let v = self.values[self.drawn % self.values.len()];
        self.drawn += 1;
        v
    }
}

/// The [`Egg`] moved `dx` along x.
#[derive(Debug)]
struct Moved(f32);

impl HeadSurface for Moved {
    fn centre(&self) -> Vec3 {
        Egg.centre() + Vec3::X * self.0
    }
    fn radius(&self, dir: Vec3) -> f32 {
        Egg.radius(dir)
    }
}

fn compile_on(head: &dyn HeadSurface, g: &CurtainGen, stream: &mut dyn UnitStream) -> CompiledGroup {
    let (palette, id, dynamics) = (Palette::default(), GroupId::new("curtain").unwrap(), Dynamics::default());
    let mut ctx = HairGenCtx::new(head, &palette, Quality::Live, &id, &dynamics, stream);
    curtain::compile(g, &mut ctx)
}

fn compile_with(g: &CurtainGen, stream: &mut dyn UnitStream) -> CompiledGroup {
    compile_on(&Egg, g, stream)
}

fn compile_forked(g: &CurtainGen, seed: u64) -> CompiledGroup {
    let mut forked = Rng::new(seed);
    compile_with(g, &mut Forked(&mut forked))
}

fn gap(p: Vec3) -> f32 {
    let d = p - Egg.centre();
    d.length() - Egg.radius(d.normalize())
}

#[test]
fn every_count_builds_guides_of_26_points_and_matches_clump_count() {
    for count in 8..=80 {
        let g = params(count);
        let group = compile_forked(&g, 3);
        assert_eq!(group.clumps.len(), curtain::clump_count(&g), "count {count}");
        assert_eq!(group.clumps.len(), usize::from(count));
        assert!(group.clumps.iter().all(|c| c.rest.len() == GUIDE_POINTS && c.rest.iter().all(|p| p.is_finite())));
    }
    assert_eq!(GUIDE_POINTS, 26);
}

#[test]
fn rest_points_keep_3_mm_from_the_head() {
    for (count, seed) in [(8, 1), (44, 2), (80, 3)] {
        for curl in [0.0, 0.045, -0.2] {
            let mut g = params(count);
            g.wave.curl = curl;
            for (n, c) in compile_forked(&g, seed).clumps.iter().enumerate() {
                for (k, p) in c.rest.iter().enumerate() {
                    assert!(gap(*p) >= 0.003, "count {count} curl {curl} guide {n} point {k}: gap {}", gap(*p));
                }
            }
        }
    }
}

#[test]
fn guides_hang_from_under_the_braid_line_and_fall_by_their_length() {
    let g = params(44);
    let group = compile_forked(&g, 5);
    for c in &group.clumps {
        let (root, tip) = (c.rest[0], c.rest[GUIDE_POINTS - 1]);
        assert!(root.y < Egg.centre().y + g.root_height_front as f32, "root under the braid line");
        let fall = root.y - tip.y;
        assert!(fall > 0.0 && fall < g.length_back as f32 * 1.1, "fall {fall}");
        assert!(c.rest.windows(2).all(|w| w[1].distance(w[0]) > 0.0), "no repeated points");
    }
}

#[test]
fn u_has_one_entry_per_clump_in_unit_range_and_increasing() {
    let g = params(44);
    let GroupGeometry::Strip { columns, u } = compile_forked(&g, 1).geometry else { panic!("a strip") };
    assert_eq!(columns, g.columns);
    assert_eq!(u.len(), 44);
    assert_eq!((u[0], u[43]), (0.0, 1.0));
    assert!(u.windows(2).all(|w| w[0] < w[1]) && u.iter().all(|x| (0.0..=1.0).contains(x)));
    let GroupGeometry::Strip { u: lone, .. } = compile_forked(&CurtainGen { count: 1, ..g }, 1).geometry else {
        panic!("a strip")
    };
    assert_eq!(lone, vec![0.5]);
}

#[test]
fn each_guide_draws_six_numbers_and_nothing_else() {
    let g = params(20);
    let mut s = Script { values: vec![0.5], drawn: 0 };
    compile_with(&g, &mut s);
    assert_eq!(s.drawn, 6 * 20);
}

/// The synthetic envelope, evaluated by hand: linear between rows, the rows running down from the neck.
fn envelope_by_hand(y: f64) -> f64 {
    let rows = [(1.6, 0.11), (1.4, 0.17), (1.2, 0.22), (0.9, 0.26), (0.5, 0.3)];
    let (hi, lo) = rows.windows(2).map(|w| (w[0], w[1])).find(|(hi, lo)| y <= hi.0 && y >= lo.0).expect("in range");
    hi.1 + (lo.1 - hi.1) * (hi.0 - y) / (hi.0 - lo.0)
}

/// Where the tip of guide `n` of `count` must stand for the six draws `d` of its turn (each in `0..1`), worked out from
/// the issue's formulas alone: at `t = 1` the smoothsteps are all 1, and the resample ends exactly on the last point.
fn expected_tip(g: &CurtainGen, n: usize, root_y: f64, d: [f64; 6]) -> [f64; 3] {
    let u = n as f64 / f64::from(g.count - 1);
    let azimuth = ((-g.spread_deg + 2.0 * g.spread_deg * u).to_radians() + std::f64::consts::PI)
        .rem_euclid(std::f64::consts::TAU);
    let (sin, cos) = (azimuth.sin(), azimuth.cos());
    let backness = cos.abs();
    let jitter = |lo: f64, hi: f64, v: f64| lo + (hi - lo) * v;
    let length = (g.length_front + (g.length_back - g.length_front) * backness)
        * (0.95 + 0.05 * (23.0 * u).sin() + jitter(-0.03, 0.03, d[0]));
    let amplitude = g.wave.amplitude + jitter(-0.006, 0.006, d[1]);
    let swing = std::f64::consts::TAU * g.wave.frequency
        + jitter(-1.0, 1.0, d[2])
        + g.wave.phase
        + 10.0 * u
        + jitter(-0.6, 0.6, d[3]);
    let curl = jitter(0.5, 1.0, d[4]) * if d[5] < 0.5 { -1.0 } else { 1.0 } * g.wave.curl;
    let tip_y = root_y - length;
    let outer = envelope_by_hand(tip_y) * 0.98 * (1.0 - 0.2 * backness);
    let out = outer + swing.cos() * amplitude * 0.7;
    let side = swing.sin() * amplitude + curl;
    // The curl's pull and lift at the prototype's curl size (`wave.curl = 0.045`).
    [sin * (out - 0.015) + cos * side, tip_y + 0.045, -0.03 + cos * (out - 0.015) - sin * side]
}

#[test]
fn the_six_draws_come_in_the_prototypes_order() {
    let mut g = params(8);
    g.wave.curl = 0.045;
    // Distinct values, so that swapping any two draws moves the tip; the last decides the curl's side.
    for draws in [[0.2, 0.9, 0.7, 0.1, 0.8, 0.3], [0.6, 0.15, 0.95, 0.4, 0.05, 0.8]] {
        let mut s = Script { values: draws.to_vec(), drawn: 0 };
        let group = compile_with(&g, &mut s);
        for n in [0, 3, 7] {
            let rest = &group.clumps[n].rest;
            let want = expected_tip(&g, n, f64::from(rest[0].y), draws);
            let tip = rest[GUIDE_POINTS - 1];
            let got = [f64::from(tip.x), f64::from(tip.y), f64::from(tip.z)];
            for axis in 0..3 {
                assert!((got[axis] - want[axis]).abs() < 1e-4, "draws {draws:?} guide {n}: tip {got:?}, want {want:?}");
            }
        }
    }
}

#[test]
fn a_head_moved_along_x_gives_the_same_guides_moved_by_as_much() {
    let g = params(24);
    for dx in [1.0_f32, -0.7] {
        let (mut a, mut b) =
            (Script { values: vec![0.37, 0.81, 0.52], drawn: 0 }, Script { values: vec![0.37, 0.81, 0.52], drawn: 0 });
        let (home, moved) = (compile_on(&Egg, &g, &mut a), compile_on(&Moved(dx), &g, &mut b));
        for (h, m) in home.clumps.iter().zip(&moved.clumps) {
            for (p, q) in h.rest.iter().zip(&m.rest) {
                assert!((*q - (*p + Vec3::X * dx)).abs().max_element() < 1e-5, "dx {dx}: {p} became {q}");
            }
        }
    }
}

#[test]
fn a_disabled_group_does_not_shift_later_draws() {
    let with_disabled = |disabled: bool| {
        let mut params = HairParams::default();
        params.groups.clear();
        let group = |enabled, generator| HairGroup { enabled, mirror: false, dynamics: Dynamics::default(), generator };
        if disabled {
            params.groups.insert(
                GroupId::new("a-off").unwrap(),
                group(false, Generator::Curtain(CurtainGen { count: 12, ..params_curtain() })),
            );
        }
        params.groups.insert(GroupId::new("b-curtain").unwrap(), group(true, Generator::Curtain(params_curtain())));
        let mut forked = Rng::new(9);
        let out =
            compile_groups(&params, &Dispatch, &Egg, &Palette::default(), Quality::Live, &mut Forked(&mut forked))
                .unwrap();
        out.into_iter().next().unwrap().group
    };
    assert_eq!(with_disabled(false), with_disabled(true));
}

fn params_curtain() -> CurtainGen {
    params(16)
}

#[test]
fn the_same_seed_gives_the_same_guides() {
    let g = params(30);
    assert_eq!(compile_forked(&g, 4), compile_forked(&g, 4));
    assert_ne!(compile_forked(&g, 4), compile_forked(&g, 5));
}

#[test]
fn degenerate_parameters_still_build_every_guide() {
    let g = CurtainGen { count: 8, envelope: Vec::new(), length_front: 0.0, length_back: 0.0, ..CurtainGen::default() };
    let group = compile_forked(&g, 1);
    assert_eq!(group.clumps.len(), 8);
    assert!(group.clumps.iter().all(|c| c.rest.len() == GUIDE_POINTS));
}

#[test]
fn a_non_finite_parameter_still_makes_every_clump() {
    // Pins the clump count only (the contract check that rejects such a group is #1775); the points are not promised.
    let nan = CurtainGen { count: 8, spread_deg: f64::NAN, ..CurtainGen::default() };
    assert_eq!(compile_forked(&nan, 1).clumps.len(), 8);
}
