//! The garment framework (#187): a synthetic tube garment through the conformance suite on every template and shape,
//! a deliberately tight one that the suite refuses, two stacked tubes that keep their spacing, envelopes, region
//! weights, rings, and the fabric tiles.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use std::fmt::Write as _;
use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_char::kit::grid::MeshBuilder;
use omuzaki_char::outfit::common::{
    ClothSpec, DEFAULT_EASE, Envelope, FabricStyle, Garment, GarmentCommon, GarmentOutput, LAYER_SHIRT, LAYER_VEST,
    PlaidBand, StripeDirection, enclosing_scale, fabric_tile,
};
use omuzaki_char::outfit::loft::{RegionWeights, SOFT_SQUARE, Surface, ring_at};
use omuzaki_char::outfit::trim::{TrimSpec, add_trim};
use omuzaki_char_api::{
    BodyFit, BodyRegion, BodyShape, BuildCtx, BuildError, BuildServices, FitFact, JointRef, PartCategory, PartKind,
    PartMeta, PartOutput, Section,
};
use omuzaki_char_mock::TorsoFit;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DVec2, DVec3, Vec2, Vec3};
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
            span: [0.2, 0.48],
            // Rings about 3 to 6 mm apart (soft-sd to tall): close enough to follow the fit where it eases out to
            // hold the hips.
            rings: 48,
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
        // Wider and squarer: kept a little lower, clear of the arms (on soft-sd/broad they hang close by).
        span: [0.2, 0.44],
        ..TubeParams::default()
    };
    conformance::assert_conforms::<Tube>(&params, &services());
}

#[test]
fn an_ease_that_is_too_small_fails_naming_the_vertex_region_and_depth() {
    // Built 3 mm closer than its 4 mm ease: 1 mm off the body, below half the ease.
    let tight = TubeParams { sink: 0.003, ..TubeParams::default() };
    let error = conformance::run::<Tube>(&tight, &services()).expect_err("a tight garment fails");
    // Round the hips the tube clears the thighs, and the nearest vertex may be over one of them.
    for word in ["vertex", "region ", "depth", "below half the ease"] {
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
    // Fine tubes, so the measure is of the envelopes and not of the tubes' facets.
    let shirt = TubeParams { span: [0.2, 0.48], rings: 64, segs: 96, ..TubeParams::default() };
    let vest = TubeParams {
        common: GarmentCommon { ease: 0.006, ..GarmentCommon::default() },
        span: [0.22, 0.46],
        rings: 64,
        segs: 96,
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
            let (mut least_at, mut most_at) = ((0.0, 0.0), (0.0, 0.0));
            for k in 0..100 {
                let t = 0.23 + 0.22 * f64::from(k) / 99.0;
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
                    // How close: in this height's section plane, along the shirt's normal projected into it (a nearer
                    // point of the shirt above or below, round the open end of the vest, is the tube's end, not its
                    // spacing).
                    let in_plane = (normal - ring.axis * normal.dot(ring.axis)).normalize();
                    let gap = (vest_at - shirt_at) * dir.dot(in_plane).abs();
                    if gap < least {
                        (least, least_at) = (gap, (t, phi));
                    }
                    // How far: from the vest to the nearest point of the shirt (along the ray, or the normal laid into
                    // the plane, it reads long where the cloth leans).
                    let p = ring.centre + dir * vest_at;
                    let apart = inner
                        .iter()
                        .map(|[a, b, c]| p.distance(closest_on_triangle(p, *a, *b, *c)))
                        .fold(f64::INFINITY, f64::min);
                    nearest = nearest.min(apart);
                    if apart > most {
                        (most, most_at) = (apart, (t, phi));
                    }
                }
            }
            println!(
                "{template}/{label}: vest over shirt {:.2}..{:.2} mm, nearest {:.2} mm in 3D",
                least * 1e3,
                most * 1e3,
                nearest * 1e3
            );
            // A little over 6 mm just above a thigh's dome: each garment clears the thighs by its own ease all round,
            // so the vest's 6 mm reaches a few millimetres higher over the dome's top than the shirt's 4 mm does.
            assert!(
                least >= 0.004 && most <= 0.006 + 5e-4 && nearest > 0.0,
                "{template}/{label}: the vest is {:.2}..{:.2} mm off the shirt (at t, phi = {least_at:.3?} and {most_at:.3?}), \
                 outside 4..6 mm",
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
    // A sliver: one edge cuts 4 mm into the front of the torso, its corners and centroid all outside it.
    let front = ring.centre + ring.through * (ring.radii.y - 0.004);
    let tri = [front - ring.across * 0.055, front + ring.across * 0.055, front + ring.through * 0.05];
    assert!(tri.iter().all(|p| !conformance::inside(fit.as_ref(), *p)), "corners outside");
    assert!(!conformance::inside(fit.as_ref(), (tri[0] + tri[1] + tri[2]) / 3.0), "centroid outside");
    let hit = conformance::crossing(fit.as_ref(), tri).expect("the crossing is found");
    assert_eq!(hit.region, BodyRegion::Torso);
    assert_eq!(hit.within, 0.0, "it passes into the region");
    // And a triangle wholly outside has no crossing.
    let clear = tri.map(|p| p + ring.through * 0.5);
    assert_eq!(conformance::crossing(fit.as_ref(), clear), None);
}

/// A body of one region: a straight cylinder of radius [`LIMB`] along y from 0 to 1 m, as a 5 cm limb.
#[derive(Debug)]
struct Limb;

/// The limb's radius, metres.
const LIMB: f32 = 0.05;

impl BodyFit for Limb {
    fn section(&self, region: BodyRegion, t: f32) -> Option<omuzaki_char_api::Section> {
        (region == BodyRegion::Torso).then(|| omuzaki_char_api::Section {
            centre: omuzaki_core::glam::Vec3::new(0.0, t, 0.0),
            axis: omuzaki_core::glam::Vec3::Y,
            radii: Vec2::splat(LIMB),
        })
    }

    fn joint(&self, joint: &omuzaki_char_api::JointRef) -> Option<omuzaki_core::glam::Vec3> {
        (*joint == omuzaki_char_api::JointRef::HIPS).then_some(omuzaki_core::glam::Vec3::ZERO)
    }
}

#[test]
fn the_suite_finds_a_triangle_grazing_a_limb_by_half_a_millimetre() {
    let (r, depth) = (f64::from(LIMB), 0.0005);
    // Flat in the plane x = r - depth: it dips into the limb along a strip 2 sqrt(2 r depth) ≈ 14 mm wide, round z = 0,
    // while its corners and centroid stay well clear of it on either side.
    let x = r - depth;
    let tri = [DVec3::new(x, 0.46, 0.05), DVec3::new(x, 0.54, 0.05), DVec3::new(x, 0.5, -0.05)];
    assert!(tri.iter().all(|p| !conformance::inside(&Limb, *p)), "corners outside");
    assert!(!conformance::inside(&Limb, (tri[0] + tri[1] + tri[2]) / 3.0), "the centroid is clear of the strip");
    let hit = conformance::crossing(&Limb, tri).expect("the graze is found");
    assert_eq!(hit.within, 0.0, "it passes into the limb");
    // Moved out by a millimetre it no longer touches.
    assert_eq!(conformance::crossing(&Limb, tri.map(|p| p + DVec3::X * 0.001)), None);
    // A metre-long triangle grazing the same way is found too: the test has no size limit.
    let huge = [DVec3::new(x, 0.0, 0.5), DVec3::new(x, 1.0, 0.5), DVec3::new(x, 0.5, -0.5)];
    assert!(conformance::crossing(&Limb, huge).is_some());
}

#[test]
fn the_suite_finds_a_triangle_grazing_a_limb_by_ten_micrometres() {
    let r = f64::from(LIMB);
    for depth in [1e-5, 1e-6] {
        // The strip it dips into is 2 sqrt(2 r depth) wide: 2 mm at 10 µm, 0.6 mm at 1 µm. The f32 sections round
        // the radius to about 4 nm, far below either.
        let x = r - depth;
        let tri = [DVec3::new(x, 0.3, 0.4), DVec3::new(x, 0.7, 0.4), DVec3::new(x, 0.5, -0.4)];
        assert!(tri.iter().all(|p| !conformance::inside(&Limb, *p)), "corners outside");
        assert!(conformance::crossing(&Limb, tri).is_some(), "a {} µm graze is found", depth * 1e6);
    }
    // 10 µm clear of the limb, the same triangle is proved clear.
    let x = r + 1e-5;
    let tri = [DVec3::new(x, 0.3, 0.4), DVec3::new(x, 0.7, 0.4), DVec3::new(x, 0.5, -0.4)];
    assert_eq!(conformance::crossing(&Limb, tri), None);
}

/// Where the standard body bulges most past the convex hull of the suite's table rows: the region, the direction, the
/// section there (centre, its support along the direction), the hull's support along it and the margin a hull of the
/// rows alone allows for its frames turning; `side` is the section's other axis, square to the direction.
struct Bulge {
    region: BodyRegion,
    t: f64,
    direction: DVec3,
    axis: DVec3,
    side: DVec3,
    centre: DVec3,
    surface: f64,
    hull: f64,
    turning: f64,
}

impl Bulge {
    /// How far the section reaches past the rows' hull and its turning margin.
    fn reach(&self) -> f64 {
        self.surface - self.hull - self.turning
    }
}

/// A section of `fit` at `t` as (centre, axis, across, through, radii), at full precision.
fn section64(fit: &dyn BodyFit, region: BodyRegion, t: f64) -> (DVec3, DVec3, DVec3, DVec3, DVec2) {
    let s = fit.section(region, t as f32).expect("a section");
    let (across, through) = omuzaki_char::template::layout::section_basis(region, s.axis);
    (s.centre.as_dvec3(), s.axis.as_dvec3().normalize(), across.as_dvec3(), through.as_dvec3(), s.radii.as_dvec2())
}

/// How far the ellipse of a section reaches along unit `d`.
fn support(section: (DVec3, DVec3, DVec3, DVec3, DVec2), d: DVec3) -> f64 {
    let (centre, _, across, through, r) = section;
    centre.dot(d) + (r.x * across.dot(d)).hypot(r.y * through.dot(d))
}

/// Of every slab between two table rows of the standard body, away from the ends of its region (where the regions
/// meet), the one where a section between the rows reaches furthest past both rows' ellipses and the margin a hull of
/// the rows allows for its turning frames (to either side, to the front or to the back): a cubic profile's bulge past
/// the chord, where nothing else would cover it.
fn widest_bulge(fit: &dyn BodyFit) -> Bulge {
    let rows = conformance::TABLE;
    let mut best: Option<Bulge> = None;
    for region in conformance::REGIONS {
        for k in rows / 8..rows - rows / 8 {
            let (t0, t1) = (k as f64 / rows as f64, (k + 1) as f64 / rows as f64);
            let (e0, e1) = (section64(fit, region, t0), section64(fit, region, t1));
            let r = e0.4.max(e1.4);
            let turning = r.x * e0.2.distance(e1.2) + r.y * e0.3.distance(e1.3);
            for i in 1..16 {
                let t = t0 + (t1 - t0) * f64::from(i) / 16.0;
                let real = section64(fit, region, t);
                let (centre, axis, across, through, _) = real;
                for (direction, side) in [(across, through), (-across, through), (through, across), (-through, across)]
                {
                    let (surface, hull) =
                        (support(real, direction), support(e0, direction).max(support(e1, direction)));
                    let found = Bulge { region, t, direction, axis, side, centre, surface, hull, turning };
                    if best.as_ref().is_none_or(|b| found.reach() > b.reach()) {
                        best = Some(found);
                    }
                }
            }
        }
    }
    best.expect("the body has regions")
}

#[test]
fn the_suite_finds_a_graze_where_the_body_bulges_past_its_table_rows() {
    // The body's profiles are cubic in `t`, so between two of the suite's table rows a section can reach past both
    // rows' ellipses, and past their convex hull with its margin for turning frames. A triangle grazing the body there,
    // by less than that bulge, is proved clear by any hull built on the rows alone; the crossing test must find it.
    // The template's own sections (the body publishes them widened to its mesh, in rows the suite's table holds).
    let params = omuzaki_char_api::BodyParams::default();
    let layout = omuzaki_char::template::registry::find("standard")
        .unwrap()
        .try_layout(&params.proportions, &params.shape, &params.hand)
        .unwrap();
    let fit: Arc<dyn BodyFit> = Arc::new(omuzaki_char::template::skeleton::TemplateFit::new(layout));
    let bulge = widest_bulge(fit.as_ref());
    let reach = bulge.reach();
    println!(
        "{:?} at t = {:.4}: the section reaches {:.2} µm past the rows' hull and its {:.3} µm turning margin",
        bulge.region,
        bulge.t,
        reach * 1e6,
        bulge.turning * 1e6
    );
    assert!(reach > 1e-6, "the standard body bulges past its rows somewhere, by {:.3} µm", reach * 1e6);
    // A flat triangle across the bulge, in the plane half the bulge inside the section's furthest point: it passes into
    // the body, and stays beyond the hull of the rows and its margin.
    let offset = bulge.surface - reach / 2.0;
    let on = bulge.centre + bulge.direction * (offset - bulge.centre.dot(bulge.direction));
    let tri = |at: DVec3| {
        let (along, side) = (bulge.axis * 0.004, bulge.side * 0.01);
        [at - along + side, at + along + side, at - side]
    };
    assert!(offset > bulge.hull + bulge.turning, "the triangle's plane is beyond the rows' hull and its margin");
    let hit = conformance::crossing(fit.as_ref(), tri(on)).expect("the graze between the rows is found");
    assert_eq!(hit.region, bulge.region);
    // A millimetre further out, the same triangle is proved clear.
    assert_eq!(conformance::crossing(fit.as_ref(), tri(on + bulge.direction * 0.001)), None);
}

#[test]
fn the_suite_finds_the_body_through_a_garment_built_over_the_template_alone() {
    use omuzaki_char::template::registry;
    use omuzaki_char::template::skeleton::TemplateFit;
    use omuzaki_char_api::{BodyParams, BuildOptions, PartId, Published, Publishes, build_part};
    // Before its fit held its mesh, the body published the template's own sections. The suite's tube over them passes
    // through the thighs' domes and the hips' bridge on soft-sd (what the evidence sheets showed), and the suite's check
    // against the body's own mesh says so; over the published fit the same tube clears the body.
    let template = "soft-sd";
    let shape = BodyShape::default();
    let tube = serde_json::to_value(TubeParams::default()).unwrap();
    let def = conformance::character(template, &shape, &[(conformance::PART, "test-tube", LAYER_SHIRT, tube)]);
    let stack = conformance::build(def.clone(), &services()).unwrap();
    let body = stack.output(conformance::BODY);
    conformance::check_body_mesh(stack.output(conformance::PART), body).expect("the tube clears the body");

    let params = BodyParams { shape: shape.clone(), ..BodyParams::default() };
    let layout =
        registry::find(template).unwrap().try_layout(&params.proportions, &params.shape, &params.hand).unwrap();
    let mut bare = Published::default();
    let fit: Arc<dyn BodyFit> = Arc::new(TemplateFit::new(layout));
    bare.absorb(&PartId::new(conformance::BODY).unwrap(), Publishes::default().with::<FitFact>(fit)).unwrap();
    let part = PartId::new(conformance::PART).unwrap();
    let over_template = build_part(&def, &part, &BuildOptions::default(), &services(), &bare).unwrap();
    let error = conformance::check_body_mesh(&over_template, body).expect_err("the body comes through");
    println!("{template}: over the template's own sections, {error}");
    assert!(error.contains("passes through the body's own mesh"), "{error}");
}

/// How far past a section a body vertex may lie and still count as held by it, metres: the f32 the fit and the mesh
/// are published in, and the table rows the widening is kept at.
const BODY_HELD: f64 = 2e-4;

/// The region a body mesh is of, by its name.
fn region_of(mesh: &str) -> Option<BodyRegion> {
    omuzaki_char::body::mesh::REGIONS.into_iter().find(|r| omuzaki_char::body::mesh::name_of(*r) == mesh)
}

#[test]
fn the_published_body_fit_holds_the_body_mesh() {
    use std::cmp::Ordering::Equal;
    // The body's junctions stand outside the template's sections (the hips' and ankles' bridged rings), so the body
    // publishes its fit widened to its mesh. On every template and shape, every vertex of a region within that region's
    // span lies in its section. Past its ends (the caps and roots) a region's mesh is no section's to hold.
    let mut widest = 0.0_f64;
    for template in conformance::TEMPLATES {
        for (label, shape) in conformance::shapes() {
            let def = conformance::character(template, &shape, &[]);
            let stack = conformance::build(def, &services()).expect("the body builds");
            let fit = stack.published.get::<FitFact>().fits.first().expect("a body fit").clone();
            let tables = conformance::Tables::of(fit.as_ref());
            let (mut worst, mut what) = (f64::NEG_INFINITY, String::new());
            for mesh in &stack.output(conformance::BODY).meshes {
                let Some(region) = region_of(&mesh.name) else { continue };
                for v in &mesh.geometry.positions {
                    let p = v.as_dvec3();
                    if conformance::along(&tables, region, p) != Some(Equal) {
                        continue;
                    }
                    let outside = conformance::located(&tables, p).into_iter().find(|(at, ..)| *at == region);
                    if let Some((.., d)) = outside
                        && d > worst
                    {
                        worst = d;
                        what = format!("`{}` vertex at {p} is {:.2} mm outside its section", mesh.name, d * 1e3);
                    }
                }
            }
            println!(
                "{template:9} {label:8} the farthest a body vertex lies outside its own section: {:.3} mm ({what})",
                worst * 1e3
            );
            assert!(worst <= BODY_HELD, "{template}/{label}: {what}");
            widest = widest.max(worst);
        }
    }
    println!("at most {:.3} mm on every template and shape", widest * 1e3);
}

#[test]
fn the_published_body_fit_keeps_to_the_body_mesh() {
    // The fit is the body's own shape: every section lies within the suite's gap of its own region's mesh, on every
    // template and shape. A section stretched round a neighbouring limb or a region's cap stands centimetres off.
    let mut rows = String::new();
    let mut widest = 0.0_f64;
    for template in conformance::TEMPLATES {
        for (label, shape) in conformance::shapes() {
            let def = conformance::character(template, &shape, &[]);
            let stack = conformance::build(def, &services()).expect("the body builds");
            let fit = stack.published.get::<FitFact>().fits.first().expect("a body fit").clone();
            let body = stack.output(conformance::BODY);
            let gaps: Vec<conformance::Gap> = omuzaki_char::body::mesh::REGIONS
                .into_iter()
                .filter_map(|region| conformance::fit_gap(fit.as_ref(), body, region))
                .collect();
            let cells: Vec<String> = gaps.iter().map(|g| format!("{:?} {:.1}", g.region, g.distance * 1e3)).collect();
            writeln!(rows, "{template:9} {label:8} {}", cells.join("  ")).unwrap();
            for gap in gaps {
                widest = widest.max(gap.distance);
                assert!(
                    gap.distance <= conformance::FIT_GAP,
                    "{template}/{label}: the {:?} section at t = {:.3} stands {:.1} mm off its mesh at {}",
                    gap.region,
                    gap.t,
                    gap.distance * 1e3,
                    gap.point
                );
            }
        }
    }
    println!("the farthest each region's sections stand from its mesh, mm:\n{rows}at most {:.1} mm", widest * 1e3);
}

/// A body fit whose sections of one region are blown out front to back, as a fit stretched round a neighbouring limb
/// is.
#[derive(Debug)]
struct Blown {
    fit: Arc<dyn BodyFit>,
    region: BodyRegion,
    by: f32,
}

impl BodyFit for Blown {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        let mut section = self.fit.section(region, t)?;
        if region == self.region {
            section.radii.y += self.by;
        }
        Some(section)
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        self.fit.joint(joint)
    }

    fn bind(&self) -> HumanoidPose {
        self.fit.bind()
    }
}

#[test]
fn the_suite_finds_a_body_fit_blown_past_the_body() {
    // A leg's sections a centimetre too deep front to back: the suite's fit check fails it, on every template.
    for template in conformance::TEMPLATES {
        let def = conformance::character(template, &conformance::shapes()[0].1, &[]);
        let stack = conformance::build(def, &services()).expect("the body builds");
        let fit = stack.published.get::<FitFact>().fits.first().expect("a body fit").clone();
        let body = stack.output(conformance::BODY);
        conformance::check_fit_tight(fit.as_ref(), body, &conformance::REGIONS)
            .expect("the body's own fit keeps to it");
        let blown = Blown { fit, region: BodyRegion::LegL, by: 0.01 };
        let error = conformance::check_fit_tight(&blown, body, &conformance::REGIONS).expect_err("a blown fit fails");
        println!("{template}: {error}");
        assert!(error.contains("LegL"), "{error}");
    }
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
    assert!(!common.cloth.enabled, "cloth is off until a garment turns it on");
    let value = serde_json::to_value(&common).unwrap();
    assert_eq!(serde_json::from_value::<GarmentCommon>(value).unwrap(), common);
    let bad = |v: serde_json::Value| serde_json::from_value::<GarmentCommon>(v).map(|c| c.validate().is_err());
    assert!(bad(json!({ "ease": 0.0 })).unwrap(), "no ease");
    assert!(bad(json!({ "ease": 0.2 })).unwrap(), "too much ease");
    assert!(!bad(json!({ "cloth": { "enabled": true, "preset": "heavy" } })).unwrap(), "cloth turns on");
    assert!(bad(json!({ "cloth": { "overrides": { "damping": -1.0 } } })).unwrap(), "a bad cloth override");
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
        ("damask", FabricStyle::Damask { ground: a.clone(), figure: b.clone(), period: 0.03 }),
        ("interlace", FabricStyle::Interlace { ground: a.clone(), strand: b.clone(), edge: a.clone(), period: 0.03 }),
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

/// Every trim of the library on a tube spanning `span` of the torso, placed by fractions of it; the braid repeats
/// `lobes` times down the front (as many as its strands have room for on that template's torso).
fn sheet_trims(span: [f64; 2], lobes: u32) -> Vec<TrimSpec> {
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
            lobes,
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

/// The characters the `garment-framework` evidence shows, per template: a shirt and a vest stacked on the body, and
/// every trim on a tube.
fn evidence_stacks() -> Vec<(&'static str, conformance::Stack, conformance::Stack)> {
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
    // Over the hips, where the thighs stand out of the pelvis, to below the arms (on soft-sd they hang close beside
    // the torso higher up).
    [("standard", [0.27, 0.55], 8), ("soft-sd", [0.3, 0.48], 3)]
        .into_iter()
        .map(|(template, span, lobes)| {
            let shirt = TubeParams { span, ..shirt.clone() };
            let vest = TubeParams { span: [span[0] + 0.03, span[1] - 0.04], ..vest.clone() };
            let trimmed = TubeParams { trims: sheet_trims(span, lobes), span, ..TubeParams::default() };
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
            (template, pair, sheet)
        })
        .collect()
}

#[test]
fn the_evidence_garments_clear_the_body_mesh() {
    // What the sheets show passes the suite's check against the body's own mesh: no skin through any band.
    for (template, pair, sheet) in evidence_stacks() {
        for (stack, part) in [(&pair, "shirt"), (&pair, "vest"), (&sheet, "coat")] {
            conformance::check_body_mesh(stack.output(part), stack.output(conformance::BODY))
                .unwrap_or_else(|e| panic!("{template} {part}: {e}"));
        }
    }
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
    for (template, pair, sheet) in evidence_stacks() {
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
