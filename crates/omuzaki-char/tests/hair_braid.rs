//! The braid-crown generator on a synthetic head: the strands stand outside the head, the count matches the dispatch's,
//! the `-x` side mirrors the `+x` side, and compiling draws no random numbers. Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::float_cmp, clippy::cast_precision_loss)] // exact values that were copied, not computed; indices are small

use omuzaki_char::hair::compile::{HairGenCtx, UnitStream, stream};
use omuzaki_char::hair::r#gen::braid;
use omuzaki_char_api::{BraidGen, Dynamics, GroupId, HairRng, HeadSurface, Quality};
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
fn the_mirror_side_matches_when_the_strands_have_no_sideways_swing() {
    // With `across` zero the binormal term vanishes, so the -x side is the +x side reflected in x, exactly.
    let g = BraidGen { across: 0.0, ..BraidGen::default() };
    let (clumps, _) = compile_with(&g);
    let per_side = usize::from(g.strands);
    for k in 0..per_side {
        let (pos, neg) = (&clumps[k].rest, &clumps[per_side + k].rest);
        for (p, q) in pos.iter().zip(neg) {
            assert!((p.x + q.x).abs() < 1e-6 && (p.y - q.y).abs() < 1e-6 && (p.z - q.z).abs() < 1e-6);
        }
    }
}
