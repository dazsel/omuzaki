//! The garment framework (#187): a synthetic tube garment through the conformance suite on every template and shape,
//! a deliberately tight one that the suite refuses, two stacked tubes that keep their spacing, envelopes, region
//! weights, rings, and the fabric tiles.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use std::sync::Arc;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::kit::grid::MeshBuilder;
use omuzaki_char::outfit::common::{
    ClothSpec, DEFAULT_EASE, Envelope, FabricStyle, Garment, GarmentCommon, GarmentOutput, LAYER_SHIRT, LAYER_VEST,
    PlaidBand, StripeDirection, enclosing_scale, fabric_tile,
};
use omuzaki_char::outfit::loft::{RegionWeights, SOFT_SQUARE, Surface, ring_at};
use omuzaki_char::outfit::trim::{TrimSpec, add_trim};
use omuzaki_char_api::{
    BodyFit, BodyRegion, BuildCtx, BuildError, BuildServices, FitFact, JointRef, PartCategory, PartKind, PartMeta,
    PartOutput,
};
use omuzaki_char_mock::TorsoFit;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DVec3, Vec2};
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_face_mock::MockRasterizer;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ------------------------------------------------------------------------------------------------- the tube garment

/// A plain tube round the torso: the least a garment can be.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
struct TubeParams {
    common: GarmentCommon,
    /// Where it starts and ends along the torso.
    span: [f64; 2],
    rings: usize,
    segs: usize,
    exponent: f64,
    /// How much closer than its ease it is built (a broken garment, for the suite to catch).
    sink: f64,
    /// Trims on it, for the evidence sheet.
    trims: Vec<TrimSpec>,
}

impl Default for TubeParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon::default(),
            span: [0.2, 0.55],
            rings: 24,
            segs: 48,
            exponent: 2.0,
            sink: 0.0,
            trims: Vec::new(),
        }
    }
}

impl Validate for TubeParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
    }
}

struct Tube;

impl PartKind for Tube {
    type Params = TubeParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-tube",
            summary: "a test tube garment",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn build(p: &TubeParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: omuzaki_char::outfit::common::GarmentError| ctx.fail(e.to_string());
        let surface = Surface::new(ctx.fit()?, BodyRegion::Torso, p.exponent, p.common.ease - p.sink).map_err(fail)?;
        let loft = surface.loft(p.span[0], p.span[1], p.rings, p.segs).map_err(fail)?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let mut builder = MeshBuilder::new();
        loft.add_to(&mut builder, out.tile()).map_err(fail)?;
        out.surface(ctx, "tube", builder)?;
        for (k, trim) in p.trims.iter().enumerate() {
            add_trim(&mut out, ctx, &surface, &format!("trim{k}"), trim)?;
        }
        out.finish(ctx, &p.common, &[BodyRegion::Torso], p.exponent)
    }
}

impl Garment for Tube {
    fn common(p: &TubeParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &TubeParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso]
    }

    fn open_borders(p: &TubeParams) -> usize {
        2 + p.trims.iter().map(TrimSpec::open_borders).sum::<usize>()
    }
}

omuzaki_char_api::register_part_kind!(Tube);

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

fn plaid() -> FabricStyle {
    FabricStyle::Plaid {
        ground: ColourRef::rgb(0x2a, 0x3a, 0x5a),
        period: 0.03,
        bands: vec![
            PlaidBand { colour: ColourRef::rgb(0xc8, 0x30, 0x30), offset: 0.004, width: 0.006 },
            PlaidBand { colour: ColourRef::rgb(0xf0, 0xe0, 0x90), offset: 0.027, width: 0.002 },
            PlaidBand { colour: ColourRef::rgb(0x10, 0x10, 0x10), offset: 0.026, width: 0.008 },
        ],
    }
}

// ------------------------------------------------------------------------------------------------------- the suite

#[test]
fn a_tube_garment_passes_the_suite_on_every_template_and_shape() {
    let rows = conformance::assert_conforms::<Tube>(&TubeParams::default(), &services());
    assert_eq!(rows.len(), 12, "4 templates x 3 shapes");
    for row in &rows {
        assert!((row.min_clearance_mm - DEFAULT_EASE * 1e3).abs() < 0.5, "{row:?}: a tube sits at its ease");
    }
}

#[test]
fn a_patterned_and_square_cut_tube_passes_too() {
    let params = TubeParams {
        common: GarmentCommon { ease: 0.006, fabric: plaid(), cloth: ClothSpec::default() },
        exponent: SOFT_SQUARE,
        // Wider and squarer: kept a little lower, clear of the arm roots.
        span: [0.2, 0.5],
        ..TubeParams::default()
    };
    conformance::assert_conforms::<Tube>(&params, &services());
}

#[test]
fn an_ease_that_is_too_small_fails_naming_the_vertex_region_and_depth() {
    // Built 3 mm closer than its 4 mm ease: 1 mm off the body, below half the ease.
    let tight = TubeParams { sink: 0.003, ..TubeParams::default() };
    let error = conformance::run::<Tube>(&tight, &services()).expect_err("a tight garment fails");
    for word in ["vertex", "region Torso", "depth", "below half the ease"] {
        assert!(error.contains(word), "the failure names the {word}: {error}");
    }
    // Built 6 mm in: through the body.
    let sunk = TubeParams { sink: 0.010, ..TubeParams::default() };
    let error = conformance::run::<Tube>(&sunk, &services()).expect_err("a garment inside the body fails");
    for word in ["vertex", "inside the body", "region", "depth"] {
        assert!(error.contains(word), "the failure names the {word}: {error}");
    }
}

#[test]
fn a_garment_that_lies_about_its_borders_fails_closure() {
    struct Liar;
    impl PartKind for Liar {
        type Params = TubeParams;
        fn meta() -> PartMeta {
            PartMeta { id: "test-liar", summary: "a tube", category: PartCategory::Garment, requires: &["body"] }
        }
        fn build(p: &TubeParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
            Tube::build(p, ctx)
        }
    }
    impl Garment for Liar {
        fn common(p: &TubeParams) -> &GarmentCommon {
            &p.common
        }
        fn regions(_: &TubeParams) -> Vec<BodyRegion> {
            vec![BodyRegion::Torso]
        }
        fn open_borders(_: &TubeParams) -> usize {
            0
        }
    }
    omuzaki_char_api::register_part_kind!(Liar);
    let error = conformance::run::<Liar>(&TubeParams::default(), &services()).expect_err("a tube has two borders");
    assert!(error.contains("2 open borders"), "{error}");
}

// ------------------------------------------------------------------------------------------------------ stacking

/// The closest point of triangle `abc` to `p` (Ericson, Real-Time Collision Detection 5.1.5).
fn closest_on_triangle(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> DVec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}

/// Where the ray from `o` along `d` first meets triangle `abc` (Möller-Trumbore), as a distance along the ray.
fn ray_hit(o: DVec3, d: DVec3, a: DVec3, b: DVec3, c: DVec3) -> Option<f64> {
    let (e1, e2) = (b - a, c - a);
    let h = d.cross(e2);
    let det = e1.dot(h);
    if det.abs() < 1e-14 {
        return None;
    }
    let s = o - a;
    let u = s.dot(h) / det;
    let q = s.cross(e1);
    let v = d.dot(q) / det;
    let t = e2.dot(q) / det;
    // A ray through a shared edge or vertex must not slip between the triangles either side.
    let slack = 1e-9;
    (u >= -slack && v >= -slack && u + v <= 1.0 + slack && t > 0.0).then_some(t)
}

fn triangles(output: &PartOutput) -> Vec<[DVec3; 3]> {
    let g = &output.meshes[0].geometry;
    g.indices.chunks_exact(3).map(|t| [0, 1, 2].map(|k| g.positions[t[k] as usize].as_dvec3())).collect()
}

#[test]
fn two_stacked_garments_keep_their_spacing_at_100_heights() {
    // The contract's example: a shirt 4 mm off the body and a vest 6 mm off the shirt.
    let shirt = TubeParams { span: [0.2, 0.55], ..TubeParams::default() };
    let vest = TubeParams {
        common: GarmentCommon { ease: 0.006, ..GarmentCommon::default() },
        span: [0.22, 0.53],
        ..TubeParams::default()
    };
    for template in conformance::TEMPLATES {
        for (label, shape) in conformance::shapes() {
            let def = conformance::character(
                template,
                &shape,
                &[
                    ("shirt", "test-tube", LAYER_SHIRT, serde_json::to_value(&shirt).unwrap()),
                    ("vest", "test-tube", LAYER_VEST, serde_json::to_value(&vest).unwrap()),
                ],
            );
            let stack = conformance::build(def, &services()).unwrap();
            let fits = &stack.published.get::<FitFact>().fits;
            assert_eq!(fits.len(), 3, "the body, then each garment's envelope");
            let body = fits[0].clone();
            let (inner, outer) = (triangles(stack.output("shirt")), triangles(stack.output("vest")));
            let (mut least, mut most, mut nearest) = (f64::INFINITY, 0.0_f64, f64::INFINITY);
            for k in 0..100 {
                let t = 0.23 + 0.29 * f64::from(k) / 99.0;
                let ring = ring_at(body.as_ref(), BodyRegion::Torso, t, 2.0, 0.0).unwrap();
                for a in 0..16 {
                    let phi = std::f64::consts::TAU * f64::from(a) / 16.0;
                    let dir = (ring.point(phi) - ring.centre).normalize();
                    let first = |mesh: &[[DVec3; 3]], what: &str| {
                        mesh.iter()
                            .filter_map(|[a, b, c]| {
                                Some((ray_hit(ring.centre, dir, *a, *b, *c)?, (*b - *a).cross(*c - *a)))
                            })
                            .min_by(|x, y| x.0.total_cmp(&y.0))
                            .unwrap_or_else(|| panic!("{template}/{label}: no {what} at t = {t}, phi = {phi}"))
                    };
                    let ((shirt_at, normal), (vest_at, _)) = (first(&inner, "shirt"), first(&outer, "vest"));
                    // The separation in this height's section plane: along the shirt's normal projected into it.
                    let in_plane = (normal - ring.axis * normal.dot(ring.axis)).normalize();
                    let gap = (vest_at - shirt_at) * dir.dot(in_plane).abs();
                    least = least.min(gap);
                    most = most.max(gap);
                    let p = ring.centre + dir * vest_at;
                    let surface = inner
                        .iter()
                        .map(|[a, b, c]| p.distance(closest_on_triangle(p, *a, *b, *c)))
                        .fold(f64::INFINITY, f64::min);
                    nearest = nearest.min(surface);
                }
            }
            println!(
                "{template}/{label}: vest over shirt {:.2}..{:.2} mm in each section, nearest {:.2} mm in 3D",
                least * 1e3,
                most * 1e3,
                nearest * 1e3
            );
            assert!(
                least >= 0.004 && most <= 0.006 + 1e-4 && nearest > 0.0,
                "{template}/{label}: the vest is {:.2}..{:.2} mm off the shirt, outside 4..6 mm",
                least * 1e3,
                most * 1e3
            );
        }
    }
}

// ------------------------------------------------------------------------------------------------------ envelopes

fn torso() -> Arc<dyn BodyFit> {
    Arc::new(TorsoFit::new(&omuzaki_char_api::BodyParams::default()))
}

#[test]
fn envelopes_pass_joints_bind_and_uncovered_regions_through() {
    let inner = torso();
    let envelope = Envelope::new(inner.clone(), &[BodyRegion::Torso], 0.004, 2.0).unwrap();
    for bone in HumanoidBone::ALL {
        let joint = JointRef::Humanoid(bone);
        assert_eq!(envelope.joint(&joint), inner.joint(&joint), "{bone:?} passes through");
    }
    assert_eq!(envelope.bind(), inner.bind());
    assert!(envelope.envelope());
    for t in [0.0, 0.25, 0.5, 1.0] {
        let (a, b) = (inner.section(BodyRegion::Torso, t).unwrap(), envelope.section(BodyRegion::Torso, t).unwrap());
        assert_eq!((a.centre, a.axis), (b.centre, b.axis));
        assert!((b.radii - a.radii - Vec2::splat(0.004)).abs().max_element() < 1e-6, "ease on both radii");
        assert_eq!(envelope.section(BodyRegion::ArmL, t), inner.section(BodyRegion::ArmL, t), "uncovered");
    }
    let square = Envelope::new(inner.clone(), &[BodyRegion::Torso], 0.004, 4.0).unwrap();
    let (a, b) = (inner.section(BodyRegion::Torso, 0.5).unwrap(), square.section(BodyRegion::Torso, 0.5).unwrap());
    let scale = enclosing_scale(4.0) as f32;
    assert!(((a.radii + Vec2::splat(0.004)) * scale - b.radii).abs().max_element() < 1e-6, "a square cut is enclosed");
}

#[test]
fn an_envelope_encloses_its_square_cut_and_an_ellipse_needs_no_widening() {
    assert!((enclosing_scale(2.0) - 1.0).abs() < 1e-12);
    for e in [2.2, 3.0, 5.0, 8.0] {
        let ring = ring_at(torso().as_ref(), BodyRegion::Torso, 0.5, e, 0.0).unwrap();
        let k = enclosing_scale(e);
        let worst = (0..3600)
            .map(|a| {
                let q = ring.point(f64::from(a) * std::f64::consts::TAU / 3600.0) - ring.centre;
                ((q.dot(ring.across) / ring.radii.x).powi(2) + (q.dot(ring.through) / ring.radii.y).powi(2)).sqrt()
            })
            .fold(0.0, f64::max);
        assert!(worst <= k + 1e-9 && worst > k - 1e-3, "exponent {e}: reaches {worst}, scale {k}");
    }
}

#[test]
fn envelopes_refuse_bad_numbers() {
    let inner = torso();
    for (ease, exponent) in [(0.0, 2.0), (f64::NAN, 2.0), (0.5, 2.0), (0.004, 1.5), (0.004, 9.0), (0.004, f64::NAN)] {
        assert!(Envelope::new(inner.clone(), &[BodyRegion::Torso], ease, exponent).is_err(), "{ease} {exponent}");
    }
    assert!(Envelope::new(inner, &[], 0.004, 2.0).is_err(), "no regions");
}

// ---------------------------------------------------------------------------------------------- rings and weights

#[test]
fn rings_are_tps_sections_with_outward_normals() {
    let fit = torso();
    let ring = ring_at(fit.as_ref(), BodyRegion::Torso, 0.5, 2.0, 0.01).unwrap();
    let section = fit.section(BodyRegion::Torso, 0.5).unwrap();
    assert!((ring.radii - (section.radii.as_dvec2() + 0.01)).abs().max_element() < 1e-9);
    let front = ring.point(0.0) - ring.centre;
    assert!((front.normalize() - ring.through).length() < 1e-9, "phi 0 is the front");
    let left = ring.point(std::f64::consts::FRAC_PI_2) - ring.centre;
    assert!(left.x > 0.0, "phi pi/2 is the character's left");
    for a in 0..64 {
        let phi = f64::from(a) * std::f64::consts::TAU / 64.0;
        let n = ring.normal(phi);
        assert!((n.length() - 1.0).abs() < 1e-9 && n.dot(ring.point(phi) - ring.centre) > 0.0, "outward at {phi}");
        assert!(n.dot(ring.axis).abs() < 1e-9, "in the section's plane");
    }
}

#[test]
fn rings_refuse_bad_input() {
    let fit = torso();
    assert!(ring_at(fit.as_ref(), BodyRegion::Torso, f64::NAN, 2.0, 0.0).is_err());
    // A fit's sections are defined on 0..=1 only: past the ends is refused, not clamped to the end ring.
    for t in [-1.0, -1e-9, 1.0 + 1e-9, 2.0] {
        assert!(ring_at(fit.as_ref(), BodyRegion::Torso, t, 2.0, 0.0).is_err(), "t = {t}");
    }
    for t in [0.0, 1.0] {
        assert_eq!(ring_at(fit.as_ref(), BodyRegion::Torso, t, 2.0, 0.0).unwrap().t, t);
    }
    assert!(ring_at(fit.as_ref(), BodyRegion::Torso, 0.5, 1.0, 0.0).is_err(), "rounder than an ellipse");
    assert!(ring_at(fit.as_ref(), BodyRegion::Torso, 0.5, 2.0, -1.0).is_err(), "collapsed");
    assert!(ring_at(fit.as_ref(), BodyRegion::ArmL, 0.5, 2.0, 0.0).is_err(), "the fit has no arms");
}

#[test]
fn region_weights_sum_to_one_and_hand_over_bone_by_bone() {
    let fit = torso();
    let weights = RegionWeights::of(fit.as_ref(), BodyRegion::Torso).unwrap();
    let chain = weights.chain();
    assert!(chain.len() >= 3, "{chain:?}");
    assert!(chain.windows(2).all(|w| w[0].1 <= w[1].1), "in order along the torso");
    for k in 0..=200 {
        let w = weights.at(f64::from(k) / 200.0);
        let total: f32 = w.iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1e-5 && w.len() <= 4, "{w:?}");
    }
    let bottom = weights.at(0.0);
    assert_eq!(bottom[0].0, chain[0].0, "the first bone owns the start");
    let (last, at) = &chain[chain.len() - 1];
    let share = |t: f64| weights.at(t).iter().find(|(j, _)| j == last).map_or(0.0, |(_, w)| *w);
    assert!(share(1.0) >= 0.5 - 1e-6, "the last bone owns at least half the end");
    assert!(share(*at - 0.2) < 1e-6, "and nothing well below its joint");
}

#[test]
fn surfaces_and_lofts_refuse_bad_sizes() {
    let surface = Surface::new(torso(), BodyRegion::Torso, 2.0, 0.004).unwrap();
    assert!(surface.loft(0.2, 0.8, 1, 16).is_err(), "one ring");
    assert!(surface.loft(0.2, 0.8, 8, 0).is_err(), "no segments");
    assert!(surface.loft(0.8, 0.2, 8, 16).is_err(), "falling");
    assert!(surface.loft(0.2, 1.5, 8, 16).is_err(), "past the end");
    assert!(surface.loft_span((0.2, 0.8), (0.0, 7.0), 8, 16).is_err(), "more than a turn");
    assert!(Surface::new(torso(), BodyRegion::Torso, 2.0, f64::INFINITY).is_err());
    let panel = surface.loft_span((0.2, 0.8), (-0.5, 0.5), 8, 16).unwrap();
    assert!(!panel.full_turn);
    let tube = surface.loft(0.2, 0.8, 8, 16).unwrap();
    assert!(tube.full_turn);
    assert_eq!(tube.grid.at(3, 0), tube.grid.at(3, 16), "the seam repeats exactly");
}

#[test]
fn a_surface_point_has_an_outward_frame_on_the_surface() {
    let surface = Surface::new(torso(), BodyRegion::Torso, SOFT_SQUARE, 0.004).unwrap();
    for (t, phi) in [(0.0, 0.0), (0.5, 1.0), (1.0, 4.0), (0.3, 2.5)] {
        let s = surface.at(t, phi).unwrap();
        assert_eq!(s.point, surface.point(t, phi).unwrap());
        assert!(s.normal.dot(s.along).abs() < 1e-6 && s.normal.dot(s.around).abs() < 1e-6);
        let ring = surface.ring(t).unwrap();
        assert!(s.normal.dot(s.point - ring.centre) > 0.0, "outward");
    }
}

#[test]
fn a_surface_whose_tangents_are_parallel_is_an_error_not_a_zero_normal() {
    // Every point on one line: moving along the region and round it both slide along the same direction.
    let surface =
        Surface::new(torso(), BodyRegion::Torso, 2.0, 0.004).unwrap().with_shape(|_, t, phi| DVec3::X * (t + phi));
    let error = surface.at(0.5, 1.0).expect_err("no normal");
    assert!(error.to_string().contains("parallel"), "{error}");
}

#[test]
fn projection_measures_along_the_ray_on_a_square_cut() {
    let surface = Surface::new(torso(), BodyRegion::Torso, 4.0, 0.0).unwrap();
    for (t, phi) in [(0.5, 0.3), (0.4, 0.8), (0.6, 2.2), (0.5, -1.3)] {
        let ring = surface.ring(t).unwrap();
        let on = ring.point(phi);
        let radius = on.distance(ring.centre);
        for k in [0.5, 0.9, 1.0, 1.2, 2.0] {
            // On the ray from the centre through the ring's point at `phi`, `k` of the way to it.
            let p = ring.centre + (on - ring.centre) * k;
            let at = surface.project(p).unwrap().expect("over the torso");
            assert!(
                (at.outside - (k - 1.0) * radius).abs() < 1e-6,
                "{t}, {phi}, {k}: {} for {}",
                at.outside,
                (k - 1.0) * radius
            );
            assert!(
                (ring.point(at.phi) - on).length() < 1e-6,
                "{t}, {phi}, {k}: the angle names the ring's point on the ray"
            );
        }
    }
}

#[test]
fn the_suite_finds_a_triangle_through_the_body_between_its_corners() {
    let fit = torso();
    let ring = ring_at(fit.as_ref(), BodyRegion::Torso, 0.5, 2.0, 0.0).unwrap();
    // A sliver: one edge runs straight through the torso, its corners and centroid all outside it.
    let tri = [ring.centre - ring.across * 0.5, ring.centre + ring.across * 0.5, ring.centre + ring.through * 1.0];
    assert!(tri.iter().all(|p| !conformance::inside(fit.as_ref(), *p)), "corners outside");
    assert!(!conformance::inside(fit.as_ref(), (tri[0] + tri[1] + tri[2]) / 3.0), "centroid outside");
    let hit = conformance::crossing(fit.as_ref(), tri).expect("the crossing is found");
    assert!(conformance::inside(fit.as_ref(), hit));
    // And a triangle wholly outside has no crossing.
    let clear = tri.map(|p| p + ring.through * 2.0);
    assert_eq!(conformance::crossing(fit.as_ref(), clear), None);
}

#[test]
fn the_build_budget_holds_in_the_test_profile_too() {
    let budget = conformance::build_budget_ms();
    if cfg!(debug_assertions) {
        assert_eq!(budget, conformance::MAX_BUILD_MS * conformance::UNOPTIMISED_SLOWDOWN);
    } else {
        assert_eq!(budget, conformance::MAX_BUILD_MS);
    }
}

// ---------------------------------------------------------------------------------------------- common settings

#[test]
fn common_settings_validate_and_round_trip() {
    let common = GarmentCommon::default();
    common.validate().unwrap();
    assert!(!common.cloth.enabled, "garments are skinned until cloth lands");
    let value = serde_json::to_value(&common).unwrap();
    assert_eq!(serde_json::from_value::<GarmentCommon>(value).unwrap(), common);
    let bad = |v: serde_json::Value| serde_json::from_value::<GarmentCommon>(v).map(|c| c.validate().is_err());
    assert!(bad(json!({ "ease": 0.0 })).unwrap(), "no ease");
    assert!(bad(json!({ "ease": 0.2 })).unwrap(), "too much ease");
    assert!(bad(json!({ "cloth": { "enabled": true } })).unwrap(), "cloth is off");
    assert!(
        bad(json!({ "fabric": { "kind": "stripes", "ground": "#000000", "stripe": "#ffffff", "period": 0.01,
        "width": 0.02, "direction": "vertical" } }))
        .unwrap()
    );
    assert!(
        bad(json!({ "fabric": { "kind": "weave", "warp": "#000000", "weft": "#ffffff", "thread": -1.0 } })).unwrap()
    );
    assert!(serde_json::from_value::<GarmentCommon>(json!({ "eaze": 0.004 })).is_err(), "unknown fields fail");
    let many = (0..9).map(|_| json!({ "colour": "#ffffff", "offset": 0.0, "width": 0.001 })).collect::<Vec<_>>();
    assert!(bad(json!({ "fabric": { "kind": "plaid", "ground": "#000000", "period": 0.02, "bands": many } })).unwrap());
}

// ------------------------------------------------------------------------------------------------------- textures

/// Paints `fabric` over `repeats` x `repeats` repeats with the mock rasteriser.
fn paint(fabric: &FabricStyle, repeats: u32) -> (u32, Vec<u8>) {
    let resolve = |c: &ColourRef| match c {
        ColourRef::Fixed(c) => Ok(*c),
        ColourRef::Slot(_) => Ok(Colour::rgb(0, 0, 0)),
    };
    let tile = fabric_tile(fabric, repeats, resolve, |m| panic!("{m}")).unwrap().expect("a patterned fabric");
    let services = services();
    let factory = services.rasterizer.as_ref().unwrap();
    let mut raster = omuzaki_face_api::Raster::new(tile.px, tile.px);
    let px_per_m = tile.px as f32 / tile.rect.width();
    let spec = omuzaki_face_api::FaceLayerSpec::for_rect(tile.rect, omuzaki_face_api::FaceLayerId::Base, px_per_m);
    factory().rasterize(&spec, &tile.list, px_per_m, &mut raster).unwrap();
    (tile.px, raster.pixels)
}

fn fabrics() -> Vec<(&'static str, FabricStyle)> {
    let (a, b) = (ColourRef::rgb(0x20, 0x30, 0x60), ColourRef::rgb(0xe0, 0xd0, 0xa0));
    let stripes = |direction| FabricStyle::Stripes {
        ground: a.clone(),
        stripe: b.clone(),
        period: 0.012,
        width: 0.004,
        direction,
    };
    vec![
        ("weave", FabricStyle::Weave { warp: a.clone(), weft: b.clone(), thread: 0.002 }),
        ("vertical stripes", stripes(StripeDirection::Vertical)),
        ("horizontal stripes", stripes(StripeDirection::Horizontal)),
        ("diagonal stripes", stripes(StripeDirection::Diagonal)),
        ("plaid", plaid()),
    ]
}

#[test]
fn fabric_tiles_are_deterministic_and_tile_seamlessly() {
    for (name, fabric) in fabrics() {
        let (px, one) = paint(&fabric, 1);
        assert_eq!(paint(&fabric, 1).1, one, "{name}: deterministic");
        // Two repeats each way: each quarter equals the single tile, so the tile's edges meet without a seam.
        let (px2, two) = paint(&fabric, 2);
        assert_eq!(px2, 2 * px, "{name}");
        let mut worst = 0_u8;
        for y in 0..px2 {
            for x in 0..px2 {
                for c in 0..4 {
                    let a = two[((y * px2 + x) * 4 + c) as usize];
                    let b = one[(((y % px) * px + x % px) * 4 + c) as usize];
                    worst = worst.max(a.abs_diff(b));
                }
            }
        }
        assert!(worst <= 1, "{name}: the 2x2 repeat differs from the tile by {worst} levels");
        // And the pattern shows: more than one colour.
        let first = &one[0..4];
        assert!(one.chunks_exact(4).any(|p| p != first), "{name}: a pattern, not a flat colour");
    }
}

#[test]
fn a_plain_fabric_needs_no_texture() {
    let plain = FabricStyle::default();
    assert!(fabric_tile(&plain, 1, |_| Ok(Colour::rgb(0, 0, 0)), |m| panic!("{m}")).unwrap().is_none());
}

#[test]
fn a_patterned_garment_without_a_rasteriser_names_what_is_missing() {
    let params =
        TubeParams { common: GarmentCommon { fabric: plaid(), ..GarmentCommon::default() }, ..TubeParams::default() };
    let def = conformance::character(
        "soft-sd",
        &omuzaki_char_api::BodyShape::default(),
        &[("tube", "test-tube", LAYER_SHIRT, serde_json::to_value(params).unwrap())],
    );
    let error = conformance::build(def, &BuildServices::default()).err().expect("no rasteriser");
    assert!(matches!(error, BuildError::Missing { .. }), "{error}");
}

// ------------------------------------------------------------------------------------------------------- evidence

/// A gold star in a 4 cm square, for a drawn decal.
fn star() -> omuzaki_face_api::DrawList {
    let mut star = omuzaki_face_api::DrawList::new();
    let points: Vec<omuzaki_core::glam::Vec2> = (0..10)
        .map(|k| {
            let a = std::f32::consts::TAU * k as f32 / 10.0;
            let r = if k % 2 == 0 { 0.019 } else { 0.008 };
            omuzaki_core::glam::Vec2::new(0.02 + r * a.sin(), 0.02 + r * a.cos())
        })
        .collect();
    let path = star.path().polyline(&points).close().finish();
    star.fill(path, omuzaki_face_api::Paint::Solid(Colour::rgb(0xe0, 0xc0, 0x40)));
    star
}

/// Every trim of the library on a tube spanning `span` of the torso, placed by fractions of it.
fn sheet_trims(span: [f64; 2]) -> Vec<TrimSpec> {
    use omuzaki_char::outfit::trim::braid::Braid;
    use omuzaki_char::outfit::trim::buttons::{ButtonRow, ButtonShape, ButtonStyle};
    use omuzaki_char::outfit::trim::chain::Chain;
    use omuzaki_char::outfit::trim::chain::LINK;
    use omuzaki_char::outfit::trim::clasp::{Clasp, ClaspStyle};
    use omuzaki_char::outfit::trim::decal::{Decal, DecalAnchor, DecalSource};
    use omuzaki_char::outfit::trim::lace::{Lace, LacePattern};
    use omuzaki_char::outfit::trim::piping::{Cord, Lip, Piping, VEST_PIPING};
    use omuzaki_char::outfit::trim::{Finish, RailSpec};
    let t = |u: f64| span[0] + (span[1] - span[0]) * u;
    let rail = RailSpec::new;
    let strip = |rail: RailSpec, width, colour| Piping {
        rail,
        width,
        thickness: 0.0008,
        bevel: 1.0,
        lip: Some(Lip { height: 0.0004, width: 0.002 }),
        bulge: 1.0,
        ramp: 5,
        caps: true,
        flip: false,
        tile: 0.1,
        finish: Finish::cloth(colour),
    };
    let gold = |radius, shape| ButtonStyle { radius, shank: 0.001, shape, finish: Finish::default() };
    let star = star();
    vec![
        TrimSpec::Piping(strip(
            rail(&[[t(0.93), 0.25], [t(0.07), 0.25]], false),
            0.008,
            ColourRef::rgb(0xf2, 0xea, 0xd8),
        )),
        TrimSpec::Cord(Cord {
            rail: rail(&[[t(0.93), 0.0], [t(0.93), 2.1], [t(0.93), 4.2]], true),
            radius: VEST_PIPING,
            segments: 6,
            finish: Finish::cloth(ColourRef::rgb(0x8a, 0x1e, 0x2c)),
        }),
        TrimSpec::Braid(Braid {
            rail: rail(&[[t(0.82), -0.3], [t(0.11), -0.35]], false),
            strands: 3,
            lobes: 10,
            strand_width: 0.002,
            finish: Finish::cloth(ColourRef::rgb(0xc8, 0x9a, 0x3c)),
        }),
        TrimSpec::ButtonRow(ButtonRow {
            rail: rail(&[[t(0.82), 0.0], [t(0.14), 0.0]], false),
            count: 4,
            spacing: None,
            button: gold(0.004, ButtonShape::Dome),
        }),
        TrimSpec::ButtonRow(ButtonRow {
            rail: rail(&[[t(0.82), -0.7], [t(0.11), -0.7]], false),
            count: 3,
            spacing: None,
            button: gold(0.0025, ButtonShape::Rivet),
        }),
        TrimSpec::Chain(Chain {
            from: [t(0.64), -0.55],
            to: [t(0.64), 0.55],
            sag: 0.012,
            bow: 0.004,
            link_size: LINK.0,
            link_count: None,
            pendant: true,
            finish: Finish::default(),
        }),
        TrimSpec::Clasp(Clasp {
            at: [t(0.71), 0.6],
            style: ClaspStyle::Medallion,
            size: 0.018,
            jewel: Some(ColourRef::rgb(0x30, 0x60, 0xd0)),
            finish: Finish::default(),
        }),
        TrimSpec::Clasp(Clasp {
            at: [t(0.71), -0.6],
            style: ClaspStyle::Cross,
            size: 0.018,
            jewel: None,
            finish: Finish::default(),
        }),
        TrimSpec::Decal(Decal {
            source: DecalSource::Drawing {
                list: star,
                extent: omuzaki_face_api::FaceRect { x0: 0.0, x1: 0.04, y0: 0.0, y1: 0.04 },
            },
            anchor: DecalAnchor::Surface { at: [t(0.39), 0.9] },
            size: [0.025, 0.025],
            rotation: 10.0,
            opacity: 1.0,
        }),
        TrimSpec::Lace(Lace {
            rail: rail(&[[t(0.04), 0.0], [t(0.04), 2.1], [t(0.04), 4.2]], true),
            scallop_radius: 0.005,
            scallop_count: 48,
            pattern: LacePattern::Picot,
            colour: ColourRef::rgb(0xfa, 0xf6, 0xee),
            flip: false,
        }),
    ]
}

/// Writes `image` as a binary PPM.
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn write_ppm(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) {
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    for px in rgba.chunks_exact(4) {
        ppm.extend(&px[..3]);
    }
    std::fs::write(path, ppm).unwrap();
}

/// Renders the parts of `stack` named `parts` through the mock toon renderer from `azimuth_deg` round `centre`.
fn render(
    stack: &conformance::Stack,
    parts: &[&str],
    centre: omuzaki_core::glam::Vec3,
    distance: f32,
    azimuth_deg: f32,
) -> omuzaki_render_api::Image {
    use omuzaki_core::glam::{Mat4, UVec2, Vec3};
    use omuzaki_render_api::{
        AlphaEncoding, Background, Camera, Instance, Light, Post, Quality, Renderer, RendererExt, Scene, Tonemap,
    };
    let mut renderer = omuzaki_render_mock::MockRenderer::new();
    let eye = centre + Mat4::from_rotation_y(azimuth_deg.to_radians()).transform_vector3(Vec3::new(0.0, 0.0, distance));
    let mut scene = Scene::new(Camera::look_at(eye, centre, Vec3::Y, Camera::default().projection));
    scene.background = Background::Solid { colour: Colour::hex(0x001e_1c24) };
    scene.lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00f1_eef8), ground: Colour::hex(0x004a_3e36), intensity: 0.75 },
        Light::Directional {
            to_light: Vec3::new(1.6, 3.0, 2.4),
            colour: Colour::hex(0x00ff_f6ec),
            intensity: 1.7,
            casts_shadow: false,
        },
    ];
    scene.post = Post { exposure: 1.0, tonemap: Tonemap::None, ..Post::default() };
    for (id, output) in &stack.outputs {
        if !parts.contains(&id.as_str()) {
            continue;
        }
        let textures: std::collections::HashMap<_, _> = output
            .textures
            .iter()
            .map(|(name, data)| (name.clone(), renderer.create_texture(&data.desc, Some(&data.rgba8)).unwrap()))
            .collect();
        for mesh in &output.meshes {
            let handle = renderer.create_mesh(&mesh.geometry).unwrap();
            let spec = &output.materials.iter().find(|(m, _)| *m == mesh.material).unwrap().1;
            let material = spec.to_material(&|name| textures.get(name).copied(), &|_| None).unwrap();
            let material = renderer.create_material(&material).unwrap();
            scene.instances.push(Instance::new(handle, material, Mat4::IDENTITY));
        }
    }
    let quality = Quality { samples: 4, ..Quality::offline() };
    renderer.render_image(UVec2::splat(512), AlphaEncoding::Straight, &scene, &quality).unwrap()
}

/// The evidence of the `garment-framework` case, written when `OMUZAKI_GARMENT_PPM=<directory>` is set: a shirt and a
/// vest stacked on the body, and every trim on a tube, front and three-quarter, on `standard` and `soft-sd`; and each
/// patterned fabric's tile at a 2 x 2 repeat.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_garment_framework_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_GARMENT_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let shirt =
        TubeParams { common: GarmentCommon { fabric: plaid(), ..GarmentCommon::default() }, ..TubeParams::default() };
    let vest = TubeParams {
        common: GarmentCommon {
            ease: 0.006,
            fabric: FabricStyle::Weave {
                warp: ColourRef::rgb(0x3a, 0x30, 0x2a),
                weft: ColourRef::rgb(0x5a, 0x4a, 0x3a),
                thread: 0.0015,
            },
            ..GarmentCommon::default()
        },
        ..TubeParams::default()
    };
    // Above the thigh caps, and on soft-sd above the hands, which stand out of the torso's fit below it.
    for (template, span) in [("standard", [0.27, 0.55]), ("soft-sd", [0.38, 0.56])] {
        let shirt = TubeParams { span, ..shirt.clone() };
        let vest = TubeParams { span: [span[0] + 0.03, span[1] - 0.04], ..vest.clone() };
        let trimmed = TubeParams { trims: sheet_trims(span), span, ..TubeParams::default() };
        let shape = omuzaki_char_api::BodyShape::default();
        let pair = conformance::build(
            conformance::character(
                template,
                &shape,
                &[
                    ("shirt", "test-tube", LAYER_SHIRT, serde_json::to_value(&shirt).unwrap()),
                    ("vest", "test-tube", LAYER_VEST, serde_json::to_value(&vest).unwrap()),
                ],
            ),
            &services(),
        )
        .unwrap();
        let sheet = conformance::build(
            conformance::character(
                template,
                &shape,
                &[("coat", "test-tube", LAYER_SHIRT, serde_json::to_value(&trimmed).unwrap())],
            ),
            &services(),
        )
        .unwrap();
        let body = pair.published.get::<FitFact>().fits[0].clone();
        let (low, high) =
            (body.section(BodyRegion::Torso, 0.15).unwrap(), body.section(BodyRegion::Torso, 0.6).unwrap());
        let centre = (low.centre + high.centre) / 2.0;
        let distance = 3.6 * (high.centre - low.centre).length();
        for (view, azimuth) in [("front", 0.0_f32), ("three-quarter", 35.0)] {
            for (name, stack, parts) in
                [("pair", &pair, &["body", "shirt", "vest"][..]), ("trims", &sheet, &["body", "coat"][..])]
            {
                let image = render(stack, parts, centre, distance, azimuth);
                write_ppm(
                    &directory.join(format!("{template}-{name}-{view}.ppm")),
                    image.size.x,
                    image.size.y,
                    &image.rgba8,
                );
            }
        }
    }
    for (name, fabric) in fabrics() {
        let (px, pixels) = paint(&fabric, 2);
        write_ppm(&directory.join(format!("tile-{}.ppm", name.replace(' ', "-"))), px, px, &pixels);
    }
}
