//! The `bangs` hair generator on a synthetic head: counts, rest points, no random draws, and how close the width fit is
//! to the prototype's outline. Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use omuzaki_char::hair::compile::{
    Dispatch, GroupGenerator, HairGenCtx, UnitStream, clumps_in, compile_groups, is_bilateral,
};
use omuzaki_char::hair::r#gen::bangs::{self, REST_POINTS};
use omuzaki_char::hair::tube::{OutwardFrame, TubeLook, TubeMesh};
use omuzaki_char_api::{
    BangLayer, BangsGen, Dynamics, Generator, GroupId, HairClump, HairGroup, HairParams, HairRng, HeadSurface, Quality,
    Wisp,
};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_face_api::{Palette, Validate};

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

/// The frame the tests draw tubes in when the character has no torso to read: the head's own centre and vertical axis.
fn head_frame(head: &dyn HeadSurface) -> OutwardFrame {
    let centre = head.centre();
    OutwardFrame { head: centre, body_axis: Vec2::new(centre.x, centre.z) }
}

/// The clumps of `g` on `head` as the part emits them: compiled, then lifted clear of the head in `frame`.
fn cleared_in(head: &dyn HeadSurface, frame: &OutwardFrame, g: &BangsGen) -> Vec<HairClump> {
    let (id, dynamics, palette) = (GroupId::new("bangs").unwrap(), Dynamics::default(), Palette::default());
    let mut stream = Counting::default();
    let mut ctx = HairGenCtx::new(head, &palette, Quality::Live, &id, &dynamics, &mut stream);
    let mut clumps = bangs::compile(g, &mut ctx).clumps;
    bangs::keep_clear(head, frame, &mut clumps).unwrap();
    clumps
}

/// [`cleared_in`] the head's own frame.
fn cleared(head: &dyn HeadSurface, g: &BangsGen) -> Vec<HairClump> {
    cleared_in(head, &head_frame(head), g)
}

/// A style of `layers` layers (the last is the top layer) and `wisps` wisps.
fn style(layers: usize, wisps: usize) -> BangsGen {
    BangsGen {
        layers: (0..layers).map(|n| layer(3 + n as u8, &[0.0, -0.02, 0.01])).collect(),
        wisps: (0..wisps)
            .map(|n| Wisp { phi_deg: -10.0 + 7.0 * n as f64, tip_height: -0.03 - 0.01 * n as f64, width: 0.022 })
            .collect(),
        sweep_deg: 7.0,
        ..BangsGen::default()
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
    for clump in cleared(&Egg, &g) {
        assert_eq!(clump.rest.len(), REST_POINTS);
        for p in &clump.rest {
            let d = *p - Egg.centre();
            assert!(d.length() > Egg.radius(d.normalize()), "{p:?} is inside the head");
        }
    }
}

#[test]
fn tip_heights_set_where_the_tips_end_and_the_last_repeats() {
    let mut g =
        BangsGen { layers: vec![layer(5, &[0.02, -0.04])], wisps: vec![], sweep_deg: 0.0, ..BangsGen::default() };
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
    let g =
        BangsGen { layers: vec![bad.clone(), bad], wisps: vec![wisp], sweep_deg: f64::INFINITY, ..BangsGen::default() };
    let clumps = cleared(&Egg, &g);
    assert_eq!(clumps.len(), bangs::clump_count(&g));
    for clump in &clumps {
        assert!(clump.rest.iter().all(|p| p.is_finite()));
        // A tip below the head wraps the lock round to the underside; the tube surface is checked for sane inputs below.
        assert!(clump.rest.iter().all(|p| (*p - Egg.centre()).length() > Egg.radius((*p - Egg.centre()).normalize())));
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

#[test]
fn a_mirrored_bangs_group_compiles_to_clump_count_clumps() {
    let mut params = HairParams::default();
    params.groups.clear();
    let group = HairGroup {
        enabled: true,
        mirror: true,
        dynamics: Dynamics::default(),
        generator: Generator::Bangs(style(2, 2)),
    };
    params.groups.insert(GroupId::new("b-bangs").unwrap(), group.clone());
    let entries =
        compile_groups(&params, &Dispatch, &Egg, &Palette::default(), Quality::Live, &mut Counting::default()).unwrap();
    assert!(is_bilateral(&group.generator), "the fringe already spans both sides");
    assert_eq!(entries[0].group.clumps.len(), bangs::clump_count(&style(2, 2)));
    assert_eq!(clumps_in(&group, &Dispatch), entries[0].group.clumps.len());
}

/// The smallest distance from the head to the lock's tube surface (negative: inside), sampled at every vertex and on a
/// barycentric grid of the spacing [`bangs::sample_spacing`] chooses for the head, over every triangle, as the generator measures it, in `frame`.
fn surface_gap(head: &dyn HeadSurface, frame: &OutwardFrame, clump: &HairClump) -> f32 {
    let centre = head.centre();
    let spacing = bangs::sample_spacing(head).unwrap() as f32;
    let gap = |p: Vec3| (p - centre).length() - head.radius((p - centre).normalize());
    let mut mesh = TubeMesh::new();
    let polar = vec![0.5; clump.rest.len()];
    let look = TubeLook { seed: 0.5, shade: 1.0, polar: &polar, tint: None, flip: 1.0 };
    mesh.push(&clump.rest, &clump.width, &clump.thickness, |p| frame.rest(p), &look).unwrap();
    let tube = mesh.finish().unwrap();
    let mut worst = f32::MAX;
    for tri in tube.indices.chunks(3) {
        let [p0, p1, p2] = [0, 1, 2].map(|k| tube.positions[tri[k] as usize]);
        let longest = (p1 - p0).length().max((p2 - p1).length()).max((p0 - p2).length());
        let steps = ((longest / spacing).ceil() as usize).max(1);
        for first in 0..=steps {
            for second in 0..=(steps - first) {
                let p = p0 + (p1 - p0) * (first as f32 / steps as f32) + (p2 - p0) * (second as f32 / steps as f32);
                worst = worst.min(gap(p));
            }
        }
    }
    worst
}

#[test]
fn the_tube_surface_stays_outside_the_head_for_the_defaults_and_for_offsets_of_0_and_1_mm() {
    let mut styles = vec![("defaults", BangsGen::default())];
    for (name, offset) in [("0 mm", 0.0), ("1 mm", 0.001)] {
        let mut g = style(3, 2);
        for l in &mut g.layers {
            l.root_offset = offset;
            l.tip_offset = offset;
        }
        styles.push((name, g));
    }
    for (name, g) in styles {
        for clump in cleared(&Egg, &g) {
            let gap = surface_gap(&Egg, &head_frame(&Egg), &clump);
            assert!(gap >= 0.0, "{name}: the tube surface is {gap} m inside the head");
        }
    }
}

/// A sphere of radius 0.11 centred at the origin: the deep review's head.
#[derive(Debug)]
struct Ball;

impl HeadSurface for Ball {
    fn centre(&self) -> Vec3 {
        Vec3::ZERO
    }
    fn radius(&self, _dir: Vec3) -> f32 {
        0.11
    }
}

/// A large sphere (radius 0.25, inside the cranium's accepted sizes): its lower half is well below the frame's blend.
#[derive(Debug)]
struct BigBall;

impl HeadSurface for BigBall {
    fn centre(&self) -> Vec3 {
        Vec3::ZERO
    }
    fn radius(&self, _dir: Vec3) -> f32 {
        0.25
    }
}

/// The wide under-layer that put the inside of a triangle into the ellipsoid between the old samples.
#[test]
fn the_inside_of_a_wide_triangle_clears_the_head() {
    let under = BangLayer {
        count: 2,
        spread_deg: 5.0,
        width: 0.3,
        hook_deg: -90.0,
        tip_heights: vec![0.1],
        root_offset: 0.0,
        tip_offset: 0.0,
        ..BangLayer::default()
    };
    let g =
        BangsGen { layers: vec![under, BangLayer::default()], wisps: vec![], sweep_deg: 90.0, ..BangsGen::default() };
    for (n, clump) in cleared(&Egg, &g).iter().enumerate() {
        let gap = surface_gap(&Egg, &head_frame(&Egg), clump);
        assert!(gap >= 0.0, "clump {n} is {gap} m inside");
    }
}

/// The tube mesh, not the rest points, is what must clear the head: across spreads, hook and sweep from -90 to 90
/// degrees, and the least and 1 mm offsets, on a sphere and on the ellipsoid.
#[test]
fn the_tube_mesh_clears_the_head_across_spread_hook_sweep_and_offset() {
    let heads: [(&str, &dyn HeadSurface); 2] = [("Egg", &Egg), ("Ball", &Ball)];
    for (name, head) in heads {
        for spread_deg in [5.0, 90.0] {
            for hook_deg in [-90.0, 0.0, 90.0] {
                for sweep_deg in [-90.0, 0.0, 90.0] {
                    for offset in [0.0, 0.001] {
                        let layer = |count: u8| BangLayer {
                            count,
                            spread_deg,
                            hook_deg,
                            root_offset: offset,
                            tip_offset: offset,
                            ..BangLayer::default()
                        };
                        let g = BangsGen {
                            layers: vec![layer(2), layer(3)],
                            wisps: vec![Wisp::default()],
                            sweep_deg,
                            ..BangsGen::default()
                        };
                        for (n, clump) in cleared(head, &g).iter().enumerate() {
                            let gap = surface_gap(head, &head_frame(head), clump);
                            assert!(
                                gap >= 0.0,
                                "{name}: clump {n} is {gap} m inside (spread {spread_deg}, hook {hook_deg}, sweep {sweep_deg}, offset {offset})"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn every_built_taper_bulge_and_shade_passes_the_contracts_validation() {
    let at_limits =
        |width: f64, shade: f64| BangLayer { count: 9, width, shade, tip_heights: vec![0.0], ..BangLayer::default() };
    for (width, shade) in [(0.3, 0.5), (0.3, 1.0), (0.001, 0.5), (0.064, 0.9)] {
        let wisp = Wisp { width, ..Wisp::default() };
        let g = BangsGen {
            layers: vec![at_limits(width, shade), at_limits(width, shade)],
            wisps: vec![wisp],
            ..BangsGen::default()
        };
        for clump in compile(&g, &mut Counting::default()).clumps {
            assert!(clump.width.validate().is_ok(), "w = {width}: {:?}", clump.width);
            assert!(clump.thickness.validate().is_ok(), "{:?}", clump.thickness);
            assert!((0.5..=1.0).contains(&clump.shade), "shade {} at {shade}", clump.shade);
        }
    }
}

/// A tip more than 78 mm below the head centre is where the drawn frame stops pointing away from the head's centre and
/// blends toward the body's axis, so the drawn tube differs from a radially oriented one. Measured with
/// [`head_frame`], the frame the part draws with when it reads no torso.
#[test]
fn a_tip_below_the_frames_transition_height_still_clears_the_head() {
    for (name, head) in [("Egg", &Egg as &dyn HeadSurface), ("Ball", &Ball), ("BigBall", &BigBall)] {
        for tip in [-0.085, -0.2] {
            for width in [0.3, 0.064] {
                for spread_deg in [5.0, 90.0] {
                    for hook_deg in [-90.0, 90.0] {
                        for sweep_deg in [-90.0, 90.0] {
                            let layer = |count: u8| BangLayer {
                                count,
                                spread_deg,
                                width,
                                hook_deg,
                                tip_heights: vec![tip],
                                root_offset: 0.0,
                                tip_offset: 0.0,
                                ..BangLayer::default()
                            };
                            let g = BangsGen {
                                layers: vec![layer(2), layer(3)],
                                wisps: vec![],
                                sweep_deg,
                                ..BangsGen::default()
                            };
                            for (n, clump) in cleared(head, &g).iter().enumerate() {
                                let gap = surface_gap(head, &head_frame(head), clump);
                                assert!(
                                    gap >= 0.0,
                                    "{name}: clump {n} is {gap} m inside (tip {tip}, width {width}, spread {spread_deg}, hook {hook_deg}, sweep {sweep_deg})"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The sharpest head the cranium's check accepts: 0.05 m semi-axes everywhere but a 0.4 m crown, whose top curves with a
/// radius near 6 mm.
fn sharpest_head() -> std::sync::Arc<dyn HeadSurface> {
    use omuzaki_char::head::mesh::{Resolution, build_head};
    use omuzaki_char_api::{Cranium, HeadParams};
    let cranium = Cranium {
        half_width: 0.05,
        half_depth_front: 0.05,
        half_depth_back: 0.05,
        half_height_up: 0.4,
        half_height_down_back: 0.05,
        half_height_down_front: 0.05,
        ..Cranium::default()
    };
    assert!(cranium.validate().is_ok(), "the sharpest cranium is one the check accepts");
    build_head(&HeadParams { cranium, ..HeadParams::default() }, Resolution::LIVE, None).unwrap().surface
}

#[test]
fn the_sampling_follows_the_head_and_the_sharpest_cranium_still_clears() {
    let ball = bangs::sample_spacing(&BigBall).unwrap();
    assert_eq!(ball, bangs::SAMPLE_SPACING, "a gentle head is sampled at the coarsest spacing");
    let sharp = sharpest_head();
    let spacing = bangs::sample_spacing(sharp.as_ref()).unwrap();
    println!("sharpest cranium: sample spacing {:.2} mm", spacing * 1000.0);
    assert!(spacing < 0.5 * bangs::SAMPLE_SPACING, "a 6 mm crown needs a finer grid: {spacing}");
    for g in [BangsGen::default(), style(3, 2)] {
        for (n, clump) in cleared(sharp.as_ref(), &g).iter().enumerate() {
            let gap = surface_gap(sharp.as_ref(), &head_frame(sharp.as_ref()), clump);
            assert!(gap >= 0.0, "clump {n} is {gap} m inside the sharpest cranium");
        }
    }
}

/// A head too sharp for the sampling: a ball of radius 5 mm.
#[derive(Debug)]
struct Needle;

impl HeadSurface for Needle {
    fn centre(&self) -> Vec3 {
        Vec3::ZERO
    }
    fn radius(&self, _dir: Vec3) -> f32 {
        0.005
    }
}

#[test]
fn a_head_too_sharp_to_sample_is_refused_by_name() {
    let (id, dynamics, palette) = (GroupId::new("bangs").unwrap(), Dynamics::default(), Palette::default());
    let mut stream = Counting::default();
    let mut ctx = HairGenCtx::new(&Needle, &palette, Quality::Live, &id, &dynamics, &mut stream);
    let mut clumps = bangs::compile(&BangsGen::default(), &mut ctx).clumps;
    let error = bangs::keep_clear(&Needle, &head_frame(&Needle), &mut clumps).unwrap_err();
    assert!(matches!(error, bangs::ClearanceError::SharpHead { .. }), "{error}");
    assert!(error.to_string().contains("curvature radius"), "{error}");
}

/// The default soft-sd layout: the torso's centre is off the head's in X and Z, so below 78 mm under the head centre
/// the drawn frame points away from the body's axis, not the head's.
#[test]
fn a_lock_cleared_in_the_drawn_frame_clears_there_but_not_in_the_heads_own() {
    let drawn = OutwardFrame { head: BigBall.centre(), body_axis: Vec2::new(0.12, -0.15) };
    let mut inside_in_head_frame = 0;
    for tip in [-0.1, -0.2] {
        for hook_deg in [-90.0, 90.0] {
            let layer = |count: u8| BangLayer {
                count,
                spread_deg: 90.0,
                width: 0.2,
                hook_deg,
                tip_heights: vec![tip],
                root_offset: 0.0,
                tip_offset: 0.0,
                ..BangLayer::default()
            };
            let g = BangsGen { layers: vec![layer(2), layer(3)], sweep_deg: 60.0, ..BangsGen::default() };
            for clump in cleared_in(&BigBall, &drawn, &g) {
                let gap = surface_gap(&BigBall, &drawn, &clump);
                assert!(gap >= 0.0, "tip {tip}, hook {hook_deg}: {gap} m inside in the drawn frame");
            }
            for clump in cleared(&BigBall, &g) {
                inside_in_head_frame += usize::from(surface_gap(&BigBall, &drawn, &clump) < 0.0);
            }
        }
    }
    assert!(inside_in_head_frame > 0, "the test never saw the head-frame pass fail, so it proves nothing");
}

#[test]
fn a_pinned_tip_radius_sets_the_tip_angle_and_the_head_radius_is_the_default() {
    let tip_y = |g: &BangsGen| compile(g, &mut Counting::default()).clumps[0].rest[REST_POINTS - 1].y;
    let base = BangsGen { layers: vec![layer(1, &[0.02])], ..BangsGen::default() };
    let crown = tip_y(&base);
    let pinned_to_crown = BangsGen { tip_radius: Some(f64::from(Egg.radius(Vec3::Y))), ..base.clone() };
    assert!((tip_y(&pinned_to_crown) - crown).abs() < 1e-6, "pinning the head's own radius changes nothing");
    let wider = BangsGen { tip_radius: Some(0.3), ..base.clone() };
    assert!(
        tip_y(&wider) < crown - 0.005,
        "a larger radius makes the same height a larger angle from the top: a lower tip"
    );
    let junk = BangsGen { tip_radius: Some(f64::NAN), ..base };
    assert!((tip_y(&junk) - crown).abs() < 1e-6, "a non-finite radius falls back to the head's");
}

#[test]
#[allow(clippy::type_complexity)] // a table of named edits
fn the_contracts_check_covers_every_bangs_value() {
    use omuzaki_char_api::hair::bangs_limits as limits;
    let bad = |edit: &dyn Fn(&mut BangsGen)| {
        let mut g = BangsGen::default();
        edit(&mut g);
        let mut params = HairParams::default();
        params.groups.get_mut(&GroupId::new("b-bangs").unwrap()).unwrap().generator = Generator::Bangs(g);
        params.validate().is_err()
    };
    assert!(!bad(&|_| {}));
    let nan = f64::NAN;
    let cases: [(&str, &dyn Fn(&mut BangsGen)); 17] = [
        ("no layers", &|g| g.layers.clear()),
        ("too many layers", &|g| g.layers = vec![BangLayer::default(); limits::LAYERS.1 + 1]),
        ("zero clumps", &|g| g.layers[0].count = 0),
        ("too many clumps", &|g| g.layers[0].count = limits::CLUMPS.1 + 1),
        ("no heights", &|g| g.layers[0].tip_heights.clear()),
        ("nan height", &|g| g.layers[0].tip_heights = vec![nan]),
        ("high height", &|g| g.layers[0].tip_heights = vec![limits::HEIGHT + 0.01]),
        ("nan width", &|g| g.layers[0].width = nan),
        ("wide width", &|g| g.layers[0].width = limits::WIDTH.1 + 0.01),
        ("hook", &|g| g.layers[0].hook_deg = limits::HOOK_DEG + 1.0),
        ("root offset", &|g| g.layers[0].root_offset = -0.01),
        ("tip offset", &|g| g.layers[0].tip_offset = limits::OFFSET.1 + 0.01),
        ("sweep", &|g| g.sweep_deg = nan),
        ("too many wisps", &|g| g.wisps = vec![Wisp::default(); limits::WISPS.1 + 1]),
        ("wisp angle", &|g| g.wisps = vec![Wisp { phi_deg: 91.0, ..Wisp::default() }]),
        ("wisp width", &|g| g.wisps = vec![Wisp { width: 0.0, ..Wisp::default() }]),
        ("tip radius", &|g| g.tip_radius = Some(0.01)),
    ];
    for (name, edit) in cases {
        assert!(bad(edit), "{name} passed the check");
    }
}
