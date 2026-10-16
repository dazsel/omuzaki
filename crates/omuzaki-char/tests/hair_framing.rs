//! The `framing` generator on a synthetic head: counts, the face-window keep-out, the random stream's order, and the
//! contract (`FramingGen` files parse unchanged; the lock heights validate). Everything is synthetic.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use omuzaki_char::hair::compile::{
    CompiledGroup, Dispatch, Forked, HairGenCtx, UnitStream, clumps_in, compile_groups, is_bilateral,
};
use omuzaki_char::hair::r#gen::framing::{self, HANGING_POINTS, TUCKED_POINTS};
use omuzaki_char_api::hair::FramingLocks;
use omuzaki_char_api::{
    Dynamics, FramingGen, Generator, GroupId, HairGroup, HairParams, HairSide, HeadSurface, Quality, Wave,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{Palette, Validate};

/// An ellipsoid head: centre `(0, 1.6, 0)`, semi-axes `(0.2, 0.22, 0.2)`, the scale the default outline is for.
#[derive(Debug)]
struct Egg;

impl HeadSurface for Egg {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 1.6, 0.0)
    }
    fn radius(&self, dir: Vec3) -> f32 {
        1.0 / (dir / Vec3::new(0.2, 0.22, 0.2)).length()
    }
}

/// The stream a test scripts: `values` cycle, and every draw is counted.
struct Script {
    values: Vec<f64>,
    drawn: usize,
}

impl Script {
    fn new(values: &[f64]) -> Self {
        Self { values: values.to_vec(), drawn: 0 }
    }
}

impl UnitStream for Script {
    fn next_unit(&mut self) -> f64 {
        let v = self.values[self.drawn % self.values.len()];
        self.drawn += 1;
        v
    }
}

/// A style that sways, so the two draws show in the points.
fn swaying(side: HairSide, count: u8, locks: bool) -> FramingGen {
    FramingGen {
        side,
        count,
        wave: Wave { amplitude: 0.016, ..Wave::default() },
        locks: locks.then(FramingLocks::new),
        ..FramingGen::default()
    }
}

fn compile_with(g: &FramingGen, stream: &mut dyn UnitStream) -> CompiledGroup {
    let (palette, id, dynamics) = (Palette::default(), GroupId::new("framing").unwrap(), Dynamics::default());
    let mut ctx = HairGenCtx::new(&Egg, &palette, Quality::Live, &id, &dynamics, stream);
    framing::compile(g, &mut ctx)
}

fn compile_forked(g: &FramingGen, seed: u64) -> CompiledGroup {
    let mut forked = Rng::new(seed);
    compile_with(g, &mut Forked(&mut forked))
}

fn gap(p: Vec3) -> f32 {
    let d = p - Egg.centre();
    d.length() - Egg.radius(d.normalize())
}

#[test]
fn clump_count_matches_compile_for_every_side_and_count() {
    for locks in [false, true] {
        for side in [HairSide::Left, HairSide::Right, HairSide::Both] {
            for count in 1..=8 {
                let g = swaying(side, count, locks);
                let group = compile_forked(&g, 4);
                assert_eq!(group.clumps.len(), framing::clump_count(&g), "{side:?} {count} locks {locks}");
                let per_side = usize::from(count);
                assert_eq!(group.clumps.len(), per_side * if side == HairSide::Both { 2 } else { 1 });
                assert!(group.clumps.iter().all(|c| c.rest.iter().all(|p| p.is_finite())));
            }
        }
    }
}

#[test]
fn the_left_tucks_all_but_its_first_lock_and_the_right_hangs_every_lock() {
    for locks in [false, true] {
        let g = swaying(HairSide::Both, 5, locks);
        let group = compile_forked(&g, 2);
        let lengths: Vec<usize> = group.clumps.iter().map(|c| c.rest.len()).collect();
        let expected = [HANGING_POINTS, TUCKED_POINTS, TUCKED_POINTS, TUCKED_POINTS, TUCKED_POINTS];
        assert_eq!(lengths[..5], expected, "left, locks {locks}");
        assert_eq!(lengths[5..], [HANGING_POINTS; 5], "right, locks {locks}");
    }
    assert_eq!((TUCKED_POINTS, HANGING_POINTS), (22, 24));
}

#[test]
fn the_right_side_is_on_negative_x_and_the_left_on_positive() {
    let group = compile_forked(&swaying(HairSide::Both, 3, true), 1);
    let (left, right) = group.clumps.split_at(3);
    assert!(left.iter().all(|c| c.rest[0].x > Egg.centre().x));
    assert!(right.iter().all(|c| c.rest[0].x < Egg.centre().x));
}

#[test]
fn no_lock_passes_through_the_face_window() {
    // The eyes' rectangle in front of the face, head-local, with a 3 mm keep-out from the head's surface.
    let in_eyes = |p: Vec3| {
        let d = p - Egg.centre();
        d.x.abs() < 0.11 && (-0.07..0.03).contains(&d.y) && d.z > 0.0
    };
    let styles = [
        ("default", FramingGen::default()),
        ("swaying, no heights", swaying(HairSide::Both, 5, false)),
        ("swaying, with heights", swaying(HairSide::Both, 5, true)),
    ];
    for (name, g) in styles {
        let mut seen = 0;
        for (n, c) in compile_forked(&g, 6).clumps.iter().enumerate() {
            // Test the points and the middle of every segment.
            let samples = c.rest.iter().copied().chain(c.rest.windows(2).map(|w| (w[0] + w[1]) * 0.5));
            for p in samples.filter(|p| in_eyes(*p)) {
                seen += 1;
                assert!(gap(p) >= 0.003, "{name} lock {n}: gap {}", gap(p));
            }
        }
        assert!(seen > 0, "the check must see points in front of the eyes ({name})");
    }
}

#[test]
fn each_lock_draws_wv_then_wa_before_the_next_lock() {
    let values: Vec<f64> = (0..40).map(|k| f64::from(k).mul_add(0.0231, 0.07)).collect();
    for locks in [false, true] {
        let both = swaying(HairSide::Both, 2, locks);
        let mut script = Script::new(&values);
        let all = compile_with(&both, &mut script);
        assert_eq!(script.drawn, 8, "two numbers per lock, four locks (locks {locks})");
        // The right side's first lock draws numbers 4 and 5, after the left side's two locks.
        let mut tail_only = Script::new(&values[4..6]);
        let alone = compile_with(&swaying(HairSide::Right, 1, locks), &mut tail_only);
        assert_eq!(tail_only.drawn, 2);
        assert_eq!(all.clumps[2].rest, alone.clumps[0].rest, "locks {locks}");
        // Swapping `wv` and `wa` changes the lock.
        let swapped = [values[5], values[4]];
        let other = compile_with(&swaying(HairSide::Right, 1, locks), &mut Script::new(&swapped));
        assert_ne!(other.clumps[0].rest, alone.clumps[0].rest, "a swapped draw must fail (locks {locks})");
    }
}

#[test]
fn adding_a_disabled_group_does_not_shift_later_draws() {
    let build = |disabled: bool| {
        let mut params = HairParams::default();
        params.groups.clear();
        let group = |enabled, g: FramingGen| HairGroup {
            enabled,
            mirror: false,
            dynamics: Dynamics::default(),
            generator: Generator::Framing(g),
        };
        if disabled {
            params.groups.insert(GroupId::new("a-off").unwrap(), group(false, swaying(HairSide::Both, 4, true)));
        }
        params.groups.insert(GroupId::new("b-on").unwrap(), group(true, swaying(HairSide::Both, 3, true)));
        let mut forked = Rng::new(9);
        let out =
            compile_groups(&params, &Dispatch, &Egg, &Palette::default(), Quality::Live, &mut Forked(&mut forked))
                .unwrap();
        out.into_iter().next().unwrap().group
    };
    assert_eq!(build(false), build(true));
}

#[test]
fn the_same_seed_gives_the_same_locks_and_another_seed_another() {
    let g = swaying(HairSide::Both, 4, true);
    assert_eq!(compile_forked(&g, 11), compile_forked(&g, 11));
    assert_ne!(compile_forked(&g, 11), compile_forked(&g, 12));
}

#[test]
fn width_shade_and_dynamics_follow_the_lock_index() {
    let g = FramingGen { side: HairSide::Left, count: 4, ..FramingGen::default() };
    for (i, c) in compile_forked(&g, 1).clumps.iter().enumerate() {
        let i = i as f64;
        assert!((c.width.start - (g.width.start + 0.004 * i)).abs() < 1e-12);
        assert_eq!((c.width.mid, c.width.tip, c.width.tip_at), (g.width.mid, g.width.tip, g.width.tip_at));
        assert_eq!(c.thickness, g.thickness);
        assert!((f64::from(c.shade) - (1.0 - 0.025 * i)).abs() < 1e-6);
        assert_eq!(c.stiffness, Dynamics::default().stiffness);
    }
}

#[test]
fn a_flat_wave_leaves_the_tail_unswayed_but_still_draws() {
    let g = FramingGen { side: HairSide::Right, count: 2, ..FramingGen::default() };
    let mut script = Script::new(&[0.3, 0.6]);
    let a = compile_with(&g, &mut script);
    assert_eq!(script.drawn, 4);
    let b = compile_with(&g, &mut Script::new(&[0.9, 0.1]));
    assert_eq!(a, b, "no amplitude, so the draws change nothing");
}

#[test]
fn a_non_finite_parameter_still_makes_every_lock() {
    let mut g = swaying(HairSide::Both, 3, true);
    g.edge[1][1] = f64::NAN;
    let group = compile_forked(&g, 1);
    assert_eq!(group.clumps.len(), framing::clump_count(&g));
    assert!(group.clumps.iter().all(|c| c.rest.len() >= TUCKED_POINTS && c.rest.iter().all(|p| p.is_finite())));
}

#[test]
fn the_mirror_rules_follow_the_side() {
    let group = |side| HairGroup {
        enabled: true,
        mirror: true,
        dynamics: Dynamics::default(),
        generator: Generator::Framing(swaying(side, 3, false)),
    };
    assert!(is_bilateral(&group(HairSide::Both).generator));
    assert_eq!(clumps_in(&group(HairSide::Both), &Dispatch), 6);
    assert_eq!(clumps_in(&group(HairSide::Left), &Dispatch), 6);
}

// ---------------------------------------------------------------------------------------------------------- the contract

#[test]
fn existing_files_parse_and_round_trip_unchanged() {
    let default = serde_json::to_value(FramingGen::default()).unwrap();
    assert!(default.get("locks").is_none(), "no locks are written when there are none");
    let back: FramingGen = serde_json::from_value(default.clone()).unwrap();
    assert_eq!(back, FramingGen::default());
    assert_eq!(serde_json::to_value(&back).unwrap(), default);
    let partial: FramingGen = serde_json::from_str(r#"{"side":"left","count":2}"#).unwrap();
    assert_eq!(partial.locks, None);
    assert_eq!(partial.count, 2);
}

#[test]
fn lock_heights_round_trip() {
    let g = swaying(HairSide::Left, 3, true);
    let json = serde_json::to_string(&g).unwrap();
    assert_eq!(serde_json::from_str::<FramingGen>(&json).unwrap(), g);
    assert!(serde_json::from_str::<FramingGen>(r#"{"locks":{"nonsense":1}}"#).is_err());
    assert_eq!(FramingLocks::default(), FramingLocks::new());
}

/// The paths validation names for `g`.
fn failures(g: &FramingGen) -> String {
    let group = HairGroup {
        enabled: true,
        mirror: false,
        dynamics: Dynamics::default(),
        generator: Generator::Framing(g.clone()),
    };
    let mut c = omuzaki_face_api::Check::new();
    group.check(&mut c);
    c.finish().map_or_else(|e| e.to_string(), |()| String::new())
}

#[test]
fn the_defaults_validate() {
    assert_eq!(failures(&FramingGen::default()), "");
    assert_eq!(failures(&swaying(HairSide::Both, 5, true)), "");
}

#[test]
fn each_out_of_range_lock_value_fails_naming_its_field() {
    type Edit = fn(&mut FramingLocks);
    let cases: [(&str, Edit); 18] = [
        ("generator/locks/top", |l| l.top = 9.0),
        ("generator/locks/cheek", |l| l.cheek = f64::NAN),
        ("generator/locks/tuck_a", |l| l.tuck_a = -9.0),
        ("generator/locks/tuck_b", |l| l.tuck_b = 9.0),
        ("generator/locks/tuck_max_x", |l| l.tuck_max_x = -0.1),
        ("generator/locks/upper", |l| l.upper = vec![0.1]),
        ("generator/locks/upper", |l| l.upper = vec![0.0, 0.1]),
        ("generator/locks/lower/1", |l| l.lower[1] = f64::INFINITY),
        ("generator/locks/end", |l| l.end.clear()),
        ("generator/locks/end/0", |l| l.end[0] = 5.0),
        ("generator/locks/ear_top/y", |l| l.ear_top[1] = f64::NAN),
        ("generator/locks/ear_back/z", |l| l.ear_back[2] = 9.0),
        ("generator/locks/drop", |l| l.drop.clear()),
        ("generator/locks/drop/2/x", |l| l.drop[2][0] = f64::NAN),
        ("generator/locks/jaw_z/1", |l| l.jaw_z[1] = f64::NAN),
        ("generator/locks/chest_z/0", |l| l.chest_z[0] = 9.0),
        ("generator/locks/front_radius", |l| l.front_radius = Some(0.0)),
        ("generator/locks/front_radius", |l| l.front_radius = Some(f64::NAN)),
    ];
    for (field, edit) in cases {
        let mut locks = FramingLocks::new();
        edit(&mut locks);
        let g = FramingGen { locks: Some(locks), ..FramingGen::default() };
        assert!(failures(&g).contains(field), "{field}: {}", failures(&g));
    }
}

#[test]
fn values_accepted_before_the_locks_still_validate() {
    let mut g = FramingGen { spread: 0.21, ..FramingGen::default() };
    g.edge[2] = [f64::NAN, 5.0];
    g.edge.push([9.0, -9.0]);
    assert_eq!(failures(&g), "", "only the new `locks` field is bounded");
}

// ------------------------------------------------------------------------------------------ independent coordinates

/// Where the last point of lock `i` of a side must be, from the prototype's formulas worked out by hand with
/// `wv = u0 * 6.28`, `wa = 0.012 + 0.008 * u1`, on `Egg` with `wave = { amplitude: 0.016, frequency: 2, phase: 0 }`.
#[allow(clippy::approx_constant)] // the prototype's literal 6.28
fn wave_args(u0: f64, u1: f64) -> (f64, f64) {
    (u0 * 6.28, 0.012 + 0.008 * u1)
}

#[test]
fn a_hanging_lock_ends_where_the_formulas_put_it() {
    let (u0, u1) = (0.2, 0.9);
    let (wv, wa) = wave_args(u0, u1);
    let g = swaying(HairSide::Right, 1, true);
    let tip = compile_with(&g, &mut Script::new(&[u0, u1])).clumps[0].rest[HANGING_POINTS - 1];
    // t = 1: x = x0 + 0.03 + sin(tau 2 + wv) wa, z = 0.085 + 0.008 cos(tau 2 + wv), y = end[0]; x0 = (the outline's last x) + 0.008.
    let arg = std::f64::consts::TAU * 2.0 + wv;
    let edge_end = g.edge.last().unwrap()[1];
    let want =
        Vec3::new(-(edge_end + 0.008 + 0.03 + arg.sin() * wa) as f32, 1.6 - 0.4, (0.085 + 0.008 * arg.cos()) as f32);
    assert!(tip.distance(want) < 1e-4, "{tip} vs {want}");
    // The depth swing is the fixed 8 mm: another amplitude moves x but not z.
    let flat = FramingGen { wave: Wave { amplitude: 0.0, ..g.wave }, ..g.clone() };
    let tip0 = compile_with(&flat, &mut Script::new(&[u0, u1])).clumps[0].rest[HANGING_POINTS - 1];
    assert!((tip0.z - want.z).abs() < 1e-4, "z {} vs {}", tip0.z, want.z);
    // Swapped draws land elsewhere.
    let swapped = compile_with(&g, &mut Script::new(&[u1, u0])).clumps[0].rest[HANGING_POINTS - 1];
    assert!(swapped.distance(want) > 1e-3);
}

#[test]
fn a_tucked_lock_ends_where_the_formulas_put_it() {
    // Left, lock 1 draws numbers 2 and 3 (lock 0 takes 0 and 1).
    let (u0, u1) = (0.35, 0.6);
    let (wv, wa) = wave_args(u0, u1);
    let g = swaying(HairSide::Left, 2, true);
    let tip = compile_with(&g, &mut Script::new(&[0.9, 0.9, u0, u1])).clumps[1].rest[TUCKED_POINTS - 1];
    // The last drop point [0.16, -0.3, -0.12] of lock 1: y rises 0.02; the wave's phase is on the bind-space height.
    let y = -0.3 + 0.02;
    let x = 0.16 + 0.012 + ((1.6 + y) * 40.0 + wv).sin() * wa;
    let want = Vec3::new(x as f32, (1.6 + y) as f32, (-0.12 - 0.01) as f32);
    assert!(tip.distance(want) < 1e-4, "{tip} vs {want}");
    let swapped = compile_with(&g, &mut Script::new(&[0.9, 0.9, u1, u0])).clumps[1].rest[TUCKED_POINTS - 1];
    assert!(swapped.distance(want) > 1e-3);
}

#[test]
fn the_tip_curl_moves_the_tail() {
    for locks in [false, true] {
        for (side, count) in [
            (HairSide::Left, 1),
            (HairSide::Left, 2),
            (HairSide::Right, 1),
            (HairSide::Right, 2),
            (HairSide::Both, 1),
            (HairSide::Both, 3),
        ] {
            let plain = swaying(side, count, locks);
            let curled = FramingGen { wave: Wave { curl: 0.1, ..plain.wave }, ..plain.clone() };
            let (a, b) = (compile_forked(&plain, 3), compile_forked(&curled, 3));
            assert_ne!(a, b, "{side:?} x{count} locks {locks}: curl must change the geometry");
            for (p, q) in a.clumps.iter().zip(&b.clumps) {
                // The root is not moved.
                assert!(p.rest[0].distance(q.rest[0]) < 1e-6);
            }
        }
    }
}

#[test]
fn the_jaw_and_chest_depths_place_the_lock_ends() {
    let (u0, u1) = (0.2, 0.9);
    // The cheek lock (left, index 0) ends at the second jaw point; a hanging lock's depth runs from the cheek's to the
    // chest's. Both are head-local, so moving them moves the end by the same amount.
    let base = FramingGen { side: HairSide::Left, count: 1, locks: Some(FramingLocks::new()), ..FramingGen::default() };
    let mut moved = FramingLocks::new();
    moved.jaw_z = [0.06, 0.09];
    let shifted = FramingGen { locks: Some(moved), ..base.clone() };
    let end = |g: &FramingGen| compile_with(g, &mut Script::new(&[u0, u1])).clumps[0].rest[HANGING_POINTS - 1];
    assert!((end(&shifted).z - end(&base).z - 0.02).abs() < 1e-5, "{} vs {}", end(&shifted).z, end(&base).z);
    assert!((end(&base).z - 0.07).abs() < 1e-5, "the default jaw depth is the old literal");

    let hang = FramingGen { side: HairSide::Right, ..base.clone() };
    let mut deeper = FramingLocks::new();
    deeper.chest_z = [0.055, 0.105];
    let deeper = FramingGen { locks: Some(deeper), ..hang.clone() };
    let flat = |g: &FramingGen| FramingGen { wave: Wave { amplitude: 0.0, ..g.wave }, ..g.clone() };
    assert!((end(&flat(&deeper)).z - end(&flat(&hang)).z - 0.02).abs() < 1e-5);
}

#[test]
fn a_front_radius_makes_the_projection_the_prototypes() {
    // A point far out to the side and low is outside the front disc: the prototype gives it no depth, so it lies on
    // the side of the head at the centre's depth; without a front radius it slides in along x and keeps its height.
    let mut locks = FramingLocks::new();
    locks.lower = vec![-0.2];
    locks.front_radius = Some(0.2);
    let exact = FramingGen { side: HairSide::Left, count: 1, locks: Some(locks.clone()), ..FramingGen::default() };
    let mut plain = locks;
    plain.front_radius = None;
    let sliding = FramingGen { locks: Some(plain), ..exact.clone() };
    let (a, b) = (compile_forked(&exact, 1), compile_forked(&sliding, 1));
    assert_ne!(a.clumps[0].rest, b.clumps[0].rest, "the front radius changes the lock");
    // With no front radius the lock is what it was before the field existed.
    assert_eq!(b.clumps[0].rest, compile_forked(&sliding, 1).clumps[0].rest);
}
