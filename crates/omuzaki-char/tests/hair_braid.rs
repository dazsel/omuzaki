//! The braid-crown generator on a synthetic head: the strands stand outside the head, the count matches the dispatch's,
//! the `-x` side mirrors the `+x` side, and compiling draws no random numbers. Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::float_cmp, clippy::cast_precision_loss)] // exact values that were copied, not computed; indices are small

use omuzaki_char::hair::compile::{Dispatch, HairGenCtx, UnitStream, compile_groups, is_bilateral, stream};
use omuzaki_char::hair::r#gen::braid;
use omuzaki_char_api::{
    BangsGen, BraidGen, CurtainGen, Dynamics, FramingGen, Generator, GroupId, HairGroup, HairParams, HairRng, HairSide,
    HeadSurface, Quality,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::Palette;

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
fn the_shade_falls_by_two_hundredths_a_strand_and_the_width_is_the_strand_width() {
    let g = BraidGen::default();
    let (clumps, _) = compile_with(&g);
    for (n, clump) in clumps.iter().enumerate() {
        let k = n % usize::from(g.strands);
        assert!((f64::from(clump.shade) - (0.98 - 0.02 * k as f64)).abs() < 1e-6, "clump {n}");
        assert_eq!(clump.width.start, g.strand_width);
        assert_eq!(clump.width.tip, 0.4 * g.strand_width);
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
        let group =
            HairGroup { enabled: true, mirror, dynamics: Dynamics::default(), generator: Generator::Braid(g.clone()) };
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
