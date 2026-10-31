//! The braid-crown generator on a synthetic head: the strands stand outside the head, the count matches the dispatch's,
//! the `-x` side mirrors the `+x` side, and compiling draws no random numbers. Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // exact values that were copied, not computed; indices are small

use omuzaki_char::hair::compile::{Dispatch, HairGenCtx, UnitStream, compile_groups, is_bilateral, stream};
use omuzaki_char::hair::r#gen::braid;
use omuzaki_char_api::{
    BangsGen, BraidGen, CurtainGen, Dynamics, FramingGen, Generator, GroupId, HairGroup, HairParams, HairRng, HairSide,
    HeadSurface, Quality, TaperShape,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{Palette, Validate};

/// A sphere of radius `R` about `CENTRE`: the surface the braid is rooted on.
#[derive(Debug)]
struct Ball;

const CENTRE: Vec3 = Vec3::new(0.0, 0.2, 0.0);
const R: f32 = 0.1;

impl HeadSurface for Ball {
    fn centre(&self) -> Vec3 {
        CENTRE
    }
    fn radius(&self, _dir: Vec3) -> f32 {
        R
    }
}

/// A stream that counts its draws and always answers `0.5`.
#[derive(Default)]
struct Counting {
    draws: usize,
}

impl UnitStream for Counting {
    fn next_unit(&mut self) -> f64 {
        self.draws += 1;
        0.5
    }
}

/// The braid's clumps, compiled with `g` on `Ball`, and the stream they drew from.
fn compile_with(g: &BraidGen) -> (Vec<omuzaki_char_api::HairClump>, usize) {
    let (id, dynamics, palette) = (GroupId::new("braid").unwrap(), Dynamics::default(), Palette::default());
    let mut rng = Counting::default();
    let head = Ball;
    let mut ctx = HairGenCtx::new(&head, &palette, Quality::Live, &id, &dynamics, &mut rng);
    let group = braid::compile(g, &mut ctx);
    (group.clumps, rng.draws)
}

fn with_strands(strands: u8) -> BraidGen {
    BraidGen { strands, ..BraidGen::default() }
}

#[test]
fn the_count_matches_the_dispatch_for_every_strand_count() {
    for strands in 2..=5 {
        let g = with_strands(strands);
        let (clumps, _) = compile_with(&g);
        assert_eq!(braid::clump_count(&g), 2 * usize::from(strands));
        assert_eq!(clumps.len(), braid::clump_count(&g), "strands {strands}");
    }
}

#[test]
fn every_strand_has_eighty_points_and_stands_outside_the_head() {
    let (clumps, _) = compile_with(&BraidGen::default());
    for clump in &clumps {
        assert_eq!(clump.rest.len(), braid::REST_POINTS);
        for p in &clump.rest {
            let d = *p - CENTRE;
            assert!(d.length() > R, "rest point {p:?} is inside the head");
        }
    }
}

#[test]
fn the_shade_falls_by_two_hundredths_a_strand_and_the_width_is_the_prototypes_plateau() {
    let g = BraidGen::default();
    let (clumps, _) = compile_with(&g);
    for (n, clump) in clumps.iter().enumerate() {
        let k = n % usize::from(g.strands);
        assert!((f64::from(clump.shade) - (0.98 - 0.02 * k as f64)).abs() < 1e-6, "clump {n}");
        assert_eq!(clump.width.start, 0.5 * g.strand_width);
        assert_eq!(clump.width.mid, g.strand_width);
        assert_eq!(clump.width.tip, 0.4 * g.strand_width);
        assert_eq!(clump.width.shape, TaperShape::plateau(0.04, 0.08));
        assert_eq!(clump.thickness.base, g.strand_thickness);
        assert_eq!(clump.thickness.swell, 0.0);
    }
}

#[test]
fn compiling_draws_no_random_numbers() {
    let (_, draws) = compile_with(&BraidGen::default());
    assert_eq!(draws, 0);
}

#[test]
fn the_legacy_stream_is_not_advanced_by_the_braid() {
    let (id, dynamics, palette) = (GroupId::new("braid").unwrap(), Dynamics::default(), Palette::default());
    let mode = HairRng::LegacyLcg { seed: 7 };
    let mut forked = Rng::new(1);
    let mut used = stream(mode, &mut forked).unwrap();
    let head = Ball;
    let g = BraidGen::default();
    {
        let mut ctx = HairGenCtx::new(&head, &palette, Quality::Live, &id, &dynamics, &mut *used);
        braid::compile(&g, &mut ctx);
    }
    let mut fresh_rng = Rng::new(1);
    let mut fresh = stream(mode, &mut fresh_rng).unwrap();
    assert_eq!(used.next_unit(), fresh.next_unit());
}

#[test]
fn the_mirror_side_is_the_other_side_mirrored_with_the_sideways_swing_negated() {
    // The binormal `T × O` flips under a reflection in `x`, so the -x side's `across` term is the +x side's with its
    // sign reversed: the -x strands from `across = a` are the +x strands from `across = -a`, mirrored in `x`.
    let g = BraidGen::default();
    let (pos, _) = compile_with(&g);
    let (neg, _) = compile_with(&BraidGen { across: -g.across, ..g.clone() });
    let per_side = usize::from(g.strands);
    for k in 0..per_side {
        let (mine, theirs) = (&pos[per_side + k].rest, &neg[k].rest);
        for (q, p) in mine.iter().zip(theirs) {
            assert!((q.x + p.x).abs() < 1e-6 && (q.y - p.y).abs() < 1e-6 && (q.z - p.z).abs() < 1e-6, "strand {k}");
        }
    }
}

#[test]
fn a_mirrored_braid_group_makes_the_same_clumps_as_an_unmirrored_one() {
    // The braid makes both sides itself, so `mirror` must not add a second pair of strand sets.
    let g = BraidGen::default();
    assert!(is_bilateral(&Generator::Braid(g.clone())));
    let palette = Palette::default();
    let head = Ball;
    let clumps_of = |mirror: bool| {
        let group = HairGroup {
            enabled: true,
            mirror,
            dynamics: Dynamics::default(),
            generator: Generator::Braid(g.clone()),
            lock: omuzaki_char_api::LockShape::default(),
        };
        let params = HairParams { groups: [(GroupId::new("braid").unwrap(), group)].into(), ..HairParams::default() };
        let mut rng = Counting::default();
        let entries = compile_groups(&params, &Dispatch, &head, &palette, Quality::Live, &mut rng).unwrap();
        entries[0].group.clumps.len()
    };
    let (plain, mirrored) = (clumps_of(false), clumps_of(true));
    assert_eq!(plain, 2 * usize::from(g.strands));
    assert_eq!(mirrored, plain);
}

#[test]
fn the_generators_that_make_both_sides_are_bilateral() {
    // Braid, curtain and bangs port prototype code that makes both sides, so `mirror` must not add a second copy.
    assert!(is_bilateral(&Generator::Braid(BraidGen::default())));
    assert!(is_bilateral(&Generator::Curtain(CurtainGen::default())));
    assert!(is_bilateral(&Generator::Bangs(BangsGen::default())));
}

#[test]
fn a_framing_group_is_bilateral_only_when_it_makes_both_sides() {
    // The default framing is `Both`, so mirroring it would lay a second copy; a single side is mirrored as asked.
    let framing = |side| Generator::Framing(FramingGen { side, ..FramingGen::default() });
    assert!(is_bilateral(&framing(HairSide::Both)));
    assert!(!is_bilateral(&framing(HairSide::Left)));
    assert!(!is_bilateral(&framing(HairSide::Right)));
}

#[test]
fn the_width_ramps_match_the_prototypes_formula() {
    // `0.024 · (t > 0.92 ? lerp(1, 0.4, (t - 0.92) / 0.08) : 1) · (t < 0.04 ? lerp(0.5, 1, t / 0.04) : 1)`
    let g = BraidGen::default();
    let (clumps, _) = compile_with(&g);
    let width = clumps[0].width;
    let prototype = |t: f64| {
        let tip = if t > 0.92 { 1.0 + (0.4 - 1.0) * (t - 0.92) / 0.08 } else { 1.0 };
        let root = if t < 0.04 { 0.5 + 0.5 * t / 0.04 } else { 1.0 };
        0.024 * tip * root
    };
    for step in 0..=200 {
        let t = f64::from(step) / 200.0;
        let ours = f64::from(width.at(t as f32));
        assert!((ours - prototype(t)).abs() < 1e-6, "t {t}: {ours} against {}", prototype(t));
    }
}

#[test]
fn a_non_finite_parameter_keeps_the_clump_count_and_the_points_finite() {
    let g = BraidGen {
        across: f64::NAN,
        out: f64::INFINITY,
        lobes: f64::NAN,
        strand_width: f64::NAN,
        ..BraidGen::default()
    };
    let (clumps, _) = compile_with(&g);
    assert_eq!(clumps.len(), braid::clump_count(&g));
    for clump in &clumps {
        assert!(clump.rest.iter().all(|p| p.is_finite()), "a rest point is not finite");
        assert!(clump.width.at(0.5).is_finite());
    }
}

#[test]
fn each_out_of_range_braid_value_fails_naming_its_field() {
    let bad = |g: BraidGen| {
        let group = HairGroup {
            enabled: true,
            mirror: false,
            dynamics: Dynamics::default(),
            generator: Generator::Braid(g),
            lock: omuzaki_char_api::LockShape::default(),
        };
        let mut c = omuzaki_face_api::Check::new();
        group.check(&mut c);
        c.finish().err().map(|e| e.to_string()).unwrap_or_default()
    };
    assert_eq!(bad(BraidGen::default()), "");
    let d = BraidGen::default;
    for (field, g) in [
        ("path_from_deg", BraidGen { path_from_deg: f64::NAN, ..d() }),
        ("path_to_deg", BraidGen { path_to_deg: 400.0, ..d() }),
        ("height_front", BraidGen { height_front: 3.0, ..d() }),
        ("height_back", BraidGen { height_back: f64::INFINITY, ..d() }),
        ("across", BraidGen { across: -0.01, ..d() }),
        ("out", BraidGen { out: 1.0, ..d() }),
        ("strand_width", BraidGen { strand_width: 0.001, ..d() }),
        ("strand_thickness", BraidGen { strand_thickness: 0.0, ..d() }),
        ("height_radius", BraidGen { height_radius: Some(0.0), ..d() }),
        ("seat", BraidGen { seat: Some(0.2), ..d() }),
    ] {
        let text = bad(g);
        assert!(text.contains(&format!("generator/{field}:")), "{field}: {text}");
    }
}

#[test]
fn a_height_radius_turns_the_braid_lines_height_into_its_own_angle() {
    // A larger radius flattens the angle the same height gives, so the path sits at another height on the head.
    let plain = compile_with(&BraidGen::default()).0;
    let wide = compile_with(&BraidGen { height_radius: Some(0.5), ..BraidGen::default() }).0;
    assert_ne!(plain[0].rest, wide[0].rest);
    let same = compile_with(&BraidGen { height_radius: Some(f64::from(R)), ..BraidGen::default() }).0;
    assert_eq!(plain[0].rest, same[0].rest, "the head's own crown radius is the default");
}

/// A side-part braid over the crown: from the character's right temple, over the top, to behind the left ear.
fn over_the_top() -> BraidGen {
    BraidGen { path: vec![[70.0, -60.0], [20.0, -20.0], [25.0, 60.0], [75.0, 120.0]], ..BraidGen::default() }
}

#[test]
fn a_braid_on_its_own_path_follows_it_over_the_top_of_the_head_once() {
    let g = over_the_top();
    let (clumps, draws) = compile_with(&g);
    assert_eq!((clumps.len(), braid::clump_count(&g), draws), (3, 3, 0), "one braid, no mirrored side, no randomness");
    assert!(!is_bilateral(&Generator::Braid(g.clone())), "a mirror flag would make a second braid");
    for clump in &clumps {
        let first = clump.rest[0] - CENTRE;
        let middle = clump.rest[clump.rest.len() / 2] - CENTRE;
        let last = clump.rest[clump.rest.len() - 1] - CENTRE;
        assert!(first.x < -0.03 && last.x > 0.03, "it runs from the right side to the left: {first:?} {last:?}");
        assert!(middle.y > 0.07, "it passes over the top of the head: {middle:?}");
        for p in &clump.rest {
            assert!((*p - CENTRE).length() > R, "{p:?} is inside the head");
        }
    }
}

#[test]
fn its_ends_tuck_toward_the_head_and_its_middle_lies_at_its_lift() {
    let g = over_the_top();
    let (clumps, _) = compile_with(&g);
    // The path itself is not exposed, so read the strands' mean distance off the surface at the ends and the middle.
    let off = |i: usize| clumps.iter().map(|c| (c.rest[i] - CENTRE).length() - R).sum::<f32>() / clumps.len() as f32;
    let (end, middle) = (off(0), off(braid::REST_POINTS / 2));
    assert!(middle > end + 0.008, "the ends sink into the hair: end {end}, middle {middle}");
    assert!((middle - braid::PATH_LIFT as f32).abs() < 0.015, "the middle lies near its lift: {middle}");

    let untucked = BraidGen { tuck: Some(0.0), ..over_the_top() };
    let (clumps, _) = compile_with(&untucked);
    let off = |i: usize| clumps.iter().map(|c| (c.rest[i] - CENTRE).length() - R).sum::<f32>() / clumps.len() as f32;
    assert!((off(0) - off(braid::REST_POINTS / 2)).abs() < 0.008, "without a tuck the ends stand as high");
}

#[test]
fn the_crown_is_unchanged_without_a_path_and_a_one_point_path_is_refused() {
    let (crown, _) = compile_with(&BraidGen::default());
    let (explicit, _) = compile_with(&BraidGen { tuck: Some(0.0), lift: None, ..BraidGen::default() });
    assert_eq!(crown, explicit, "no path and no tuck is the prototype's crown");
    let text = |g: BraidGen| {
        let group = HairGroup {
            enabled: true,
            mirror: false,
            dynamics: Dynamics::default(),
            generator: Generator::Braid(g),
            lock: omuzaki_char_api::LockShape::default(),
        };
        let mut c = omuzaki_face_api::Check::new();
        group.check(&mut c);
        c.finish().err().map(|e| e.to_string()).unwrap_or_default()
    };
    assert_eq!(text(over_the_top()), "");
    assert!(text(BraidGen { path: vec![[30.0, 0.0]], ..BraidGen::default() }).contains("generator/path:"));
    assert!(text(BraidGen { path: vec![[30.0, 0.0], [200.0, 0.0]], ..BraidGen::default() }).contains("path/1/0"));
    assert!(text(BraidGen { tuck: Some(0.2), ..BraidGen::default() }).contains("generator/tuck:"));
}

#[test]
fn a_seated_braid_lies_on_the_hair_under_it_whatever_its_lift() {
    let g = BraidGen { seat: Some(0.004), tuck: Some(0.0), ..over_the_top() };
    let (high, _) = compile_with(&BraidGen { lift: Some(0.08), ..g.clone() });
    let (low, _) = compile_with(&BraidGen { lift: Some(0.01), ..g.clone() });
    // Hair under it: a shell 3 cm off the ball, as points.
    let shell: Vec<Vec3> = (0..=40)
        .flat_map(|i| (0..80).map(move |j| (i as f32 * 0.04, j as f32 * std::f32::consts::TAU / 80.0)))
        .map(|(theta, phi)| {
            CENTRE + Vec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos()) * (R + 0.03)
        })
        .collect();
    let off = |clumps: &[omuzaki_char_api::HairClump], i: usize| {
        clumps.iter().map(|c| (c.rest[i] - CENTRE).length() - R).sum::<f32>() / clumps.len() as f32
    };
    for mut clumps in [high, low] {
        braid::seat(&g, &Ball, &shell, &mut clumps);
        for i in [10, braid::REST_POINTS / 2, braid::REST_POINTS - 11] {
            let o = off(&clumps, i);
            assert!((o - 0.034).abs() < 0.003, "point {i} lies on the hair, 4 mm over its 3 cm: {o}");
        }
    }
    // Unset, it stays where its lift put it.
    let (lifted, _) = compile_with(&BraidGen { seat: None, lift: Some(0.08), ..g.clone() });
    let mut kept = lifted.clone();
    braid::seat(&BraidGen { seat: None, ..g }, &Ball, &shell, &mut kept);
    assert_eq!(kept, lifted);
}
