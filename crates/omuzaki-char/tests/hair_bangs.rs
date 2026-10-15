//! The `bangs` hair generator on a synthetic head: counts, rest points, no random draws, and how close the width fit is
//! to the prototype's outline. Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use omuzaki_char::hair::compile::{Dispatch, GroupGenerator, HairGenCtx, UnitStream, compile_groups};
use omuzaki_char::hair::r#gen::bangs::{self, REST_POINTS};
use omuzaki_char_api::{
    BangLayer, BangsGen, Dynamics, Generator, GroupId, HairParams, HairRng, HeadSurface, Quality, Wisp,
};
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::Palette;

/// An ellipsoid head: centre `(0, 0.2, 0)`, semi-axes `(0.08, 0.11, 0.095)`.
#[derive(Debug)]
struct Egg;

impl HeadSurface for Egg {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 0.2, 0.0)
    }
    fn radius(&self, dir: Vec3) -> f32 {
        1.0 / (dir / Vec3::new(0.08, 0.11, 0.095)).length()
    }
}

/// A stream that counts its draws.
#[derive(Default)]
struct Counting(usize);

impl UnitStream for Counting {
    fn next_unit(&mut self) -> f64 {
        self.0 += 1;
        0.5
    }
}

fn layer(count: u8, tips: &[f64]) -> BangLayer {
    BangLayer { count, tip_heights: tips.to_vec(), ..BangLayer::default() }
}

fn compile(g: &BangsGen, rng: &mut dyn UnitStream) -> omuzaki_char::hair::compile::CompiledGroup {
    let (id, dynamics) = (GroupId::new("bangs").unwrap(), Dynamics::default());
    let palette = Palette::default();
    let mut ctx = HairGenCtx::new(&Egg, &palette, Quality::Live, &id, &dynamics, rng);
    bangs::compile(g, &mut ctx)
}

/// A style of `layers` layers (the last is the top layer) and `wisps` wisps.
fn style(layers: usize, wisps: usize) -> BangsGen {
    BangsGen {
        layers: (0..layers).map(|n| layer(3 + n as u8, &[0.0, -0.02, 0.01])).collect(),
        wisps: (0..wisps)
            .map(|n| Wisp { phi_deg: -10.0 + 7.0 * n as f64, tip_height: -0.03 - 0.01 * n as f64, width: 0.022 })
            .collect(),
        sweep_deg: 7.0,
    }
}

#[test]
fn the_count_is_the_layers_plus_the_wisps_and_matches_compile() {
    for layers in 0..=3 {
        for wisps in 0..=4 {
            let g = style(layers, wisps);
            let expected: usize = g.layers.iter().map(|l| usize::from(l.count)).sum::<usize>() + wisps;
            assert_eq!(bangs::clump_count(&g), expected);
            assert_eq!(compile(&g, &mut Counting::default()).clumps.len(), expected);
        }
    }
    let g = BangsGen::default();
    assert_eq!(bangs::clump_count(&g), 9);
    assert_eq!(compile(&g, &mut Counting::default()).clumps.len(), 9);
}

#[test]
fn the_dispatch_checks_the_count_and_builds_the_default_style() {
    let params = HairParams { rng: HairRng::Forked, ..HairParams::default() };
    let mut stream = Counting::default();
    let entries = compile_groups(&params, &Dispatch, &Egg, &Palette::default(), Quality::Live, &mut stream).unwrap();
    let bangs = entries.iter().find(|e| e.id.as_str() == "b-bangs").unwrap();
    assert_eq!(bangs.group.clumps.len(), Dispatch.clump_count(&Generator::Bangs(BangsGen::default())));
    assert_eq!(bangs.group.clumps.len(), 9);
}

#[test]
fn every_clump_has_22_points_all_outside_the_head() {
    let mut g = style(3, 4);
    g.sweep_deg = 25.0;
    g.layers[2].hook_deg = 12.0;
    for clump in compile(&g, &mut Counting::default()).clumps {
        assert_eq!(clump.rest.len(), REST_POINTS);
        for p in &clump.rest {
            let d = *p - Egg.centre();
            assert!(d.length() > Egg.radius(d.normalize()), "{p:?} is inside the head");
        }
    }
}

#[test]
fn tip_heights_set_where_the_tips_end_and_the_last_repeats() {
    let mut g = BangsGen { layers: vec![layer(5, &[0.02, -0.04])], wisps: vec![], sweep_deg: 0.0 };
    g.layers[0].spread_deg = 0.0;
    let clumps = compile(&g, &mut Counting::default()).clumps;
    let tip_y = |i: usize| clumps[i].rest[REST_POINTS - 1].y - Egg.centre().y;
    assert!(tip_y(0) > tip_y(1), "the first lock is longer-set higher than the second");
    // Locks 2..5 repeat the last value, up to the jitter and the outer drop.
    assert!((tip_y(2) - tip_y(1)).abs() < 0.03);
}

#[test]
fn the_roots_start_near_the_top_and_the_tips_fall_toward_the_height() {
    let clumps = compile(&BangsGen::default(), &mut Counting::default()).clumps;
    for c in &clumps {
        assert!(c.rest[0].y > c.rest[REST_POINTS - 1].y, "root above tip");
    }
}

#[test]
fn compile_draws_no_random_numbers() {
    let mut stream = Counting::default();
    let _ = compile(&style(3, 4), &mut stream);
    assert_eq!(stream.0, 0);
}

#[test]
fn compile_is_deterministic_and_carries_the_group_settings() {
    let g = style(2, 2);
    let a = compile(&g, &mut Counting::default());
    assert_eq!(a, compile(&g, &mut Counting::default()));
    assert!(a.clumps.iter().all(|c| !c.simulate));
    assert!(a.geometry.is_tube());
}

/// The closest `Taper` against the prototype's outline: prints the largest difference per width and bounds it.
#[test]
fn the_width_fit_is_close_to_the_prototypes_outline() {
    for w in [0.072, 0.064, 0.050, 0.022] {
        let taper = bangs::taper(w);
        let worst = (0..=100)
            .map(|i| {
                let t = f64::from(i) / 100.0;
                (f64::from(taper.at(t as f32)) - bangs::outline(w, t)).abs()
            })
            .fold(0.0, f64::max);
        println!("width {w}: largest difference from the prototype's outline {worst:.5} m");
        assert!(worst < 0.01, "width {w}: {worst}");
    }
}

#[test]
fn non_finite_and_extreme_parameters_still_make_finite_clumps_outside_the_head() {
    let bad = BangLayer {
        count: 4,
        spread_deg: f64::NAN,
        tip_heights: vec![f64::NAN, f64::INFINITY, -1e9],
        width: f64::NEG_INFINITY,
        hook_deg: 1e9,
        root_offset: -1.0,
        tip_offset: f64::NAN,
        shade: f64::NAN,
    };
    let wisp = Wisp { phi_deg: f64::NAN, tip_height: f64::NAN, width: -3.0 };
    let g = BangsGen { layers: vec![bad.clone(), bad], wisps: vec![wisp], sweep_deg: f64::INFINITY };
    let group = compile(&g, &mut Counting::default());
    assert_eq!(group.clumps.len(), bangs::clump_count(&g));
    for clump in &group.clumps {
        assert!(clump.rest.iter().all(|p| p.is_finite()));
        assert!(clump.rest.iter().all(|p| (*p - Egg.centre()).length() >= Egg.radius((*p - Egg.centre()).normalize())));
    }
}

#[test]
fn layers_come_first_in_order_then_the_wisps() {
    let mut g = style(2, 1);
    g.layers[0].shade = 0.6;
    g.layers[1].shade = 1.0;
    let shades: Vec<f32> = compile(&g, &mut Counting::default()).clumps.iter().map(|c| c.shade).collect();
    // 3 under-layer clumps (0.6), 4 top-layer clumps (1.0 or 0.965), 1 wisp (0.98).
    assert!(shades[..3].iter().all(|s| (*s - 0.6).abs() < 1e-6), "{shades:?}");
    assert!(shades[3..7].iter().all(|s| *s > 0.95), "{shades:?}");
    assert!((shades[7] - 0.98).abs() < 1e-6, "{shades:?}");
}

#[test]
fn an_under_layer_is_mirror_symmetric_without_the_sweep() {
    let mut g = style(2, 0);
    g.sweep_deg = 0.0;
    g.layers[0] = layer(4, &[0.0]);
    let clumps = compile(&g, &mut Counting::default()).clumps;
    // The jitter is on the tip height only, and differs between mirrored locks, so compare the roots.
    for i in 0..2 {
        let (a, b) = (clumps[i].rest[0], clumps[3 - i].rest[0]);
        assert!(
            (a.x + b.x - 2.0 * Egg.centre().x).abs() < 1e-5 && (a.y - b.y).abs() < 1e-5 && (a.z - b.z).abs() < 1e-5
        );
    }
}
