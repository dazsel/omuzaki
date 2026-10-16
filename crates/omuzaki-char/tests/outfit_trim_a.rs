//! Trim library I (#188): piping, braid, buttons and rivets on a tube garment, through the conformance suite on every
//! template and shape; trims that follow the surface within 0.5 mm at 200 points; a zero-length rail that is an error;
//! the cross-sections, profiles and metal material.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)] // test arithmetic
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use std::collections::HashMap;
use std::sync::Arc;

use omuzaki_char::kit::grid::MeshBuilder;
use omuzaki_char::outfit::common::{Garment, GarmentCommon, GarmentOutput, LAYER_SHIRT};
use omuzaki_char::outfit::loft::Surface;
use omuzaki_char::outfit::trim::braid::Braid;
use omuzaki_char::outfit::trim::buttons::{self, Button, ButtonRow, ButtonShape, ButtonStyle};
use omuzaki_char::outfit::trim::piping::{self, Cord, Lip, Piping, VEST_PIPING};
use omuzaki_char::outfit::trim::{
    Finish, LIFT, METAL_PROGRAM, MetalStyle, RAIL_STEP, Rail, RailSpec, TrimError, TrimSpec, add_trim,
};
use omuzaki_char_api::{
    BodyFit, BodyRegion, BuildCtx, BuildError, BuildServices, JointRef, MaterialClass, PartCategory, PartKind, PartMeta,
    PartOutput,
};
use omuzaki_char_mock::TorsoFit;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_face_mock::MockRasterizer;
use omuzaki_render_api::Shading;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------------------------------- a trimmed tube garment

/// A tube round the torso with trims on it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TrimmedParams {
    common: GarmentCommon,
    trims: Vec<TrimSpec>,
}

impl Validate for TrimmedParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        for (k, trim) in self.trims.iter().enumerate() {
            c.nested(&format!("trims/{k}"), trim);
        }
    }
}

/// Where the tube runs along the torso, clear of the arm roots on every template.
const SPAN: (f64, f64) = (0.2, 0.55);

struct Trimmed;

impl PartKind for Trimmed {
    type Params = TrimmedParams;

    fn meta() -> PartMeta {
        PartMeta { id: "test-trimmed", summary: "a trimmed tube", category: PartCategory::Garment, requires: &["body"] }
    }

    fn build(p: &TrimmedParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: omuzaki_char::outfit::common::GarmentError| ctx.fail(e.to_string());
        let surface = Surface::new(ctx.fit()?, BodyRegion::Torso, 2.0, p.common.ease).map_err(fail)?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let mut builder = MeshBuilder::new();
        surface.loft(SPAN.0, SPAN.1, 24, 48).map_err(fail)?.add_to(&mut builder, out.tile()).map_err(fail)?;
        out.surface(ctx, "tube", builder)?;
        for (k, trim) in p.trims.iter().enumerate() {
            add_trim(&mut out, ctx, &surface, &format!("trim{k}"), trim)?;
        }
        out.finish(ctx, &p.common, &[BodyRegion::Torso], 2.0)
    }
}

impl Garment for Trimmed {
    fn common(p: &TrimmedParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &TrimmedParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso]
    }

    fn open_borders(p: &TrimmedParams) -> usize {
        2 + p.trims.iter().map(TrimSpec::open_borders).sum::<usize>()
    }
}

omuzaki_char_api::register_part_kind!(Trimmed);

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

fn rail(points: &[[f64; 2]], closed: bool) -> RailSpec {
    RailSpec::new(points, closed)
}

/// The front placket's rail: straight down the front.
fn placket() -> RailSpec {
    rail(&[[0.52, 0.0], [0.24, 0.0]], false)
}

/// A hem round the tube near its bottom.
fn hem() -> RailSpec {
    rail(&[[0.23, 0.0], [0.23, 2.1], [0.23, 4.2]], true)
}

fn gold_dome(radius: f64) -> ButtonStyle {
    ButtonStyle { radius, shank: 0.0015, shape: ButtonShape::Dome, finish: Finish::default() }
}

/// Every trim of this library, between them every option.
fn trims() -> Vec<TrimSpec> {
    vec![
        TrimSpec::Piping(strip(placket_side(), 0.006)),
        TrimSpec::Piping(Piping {
            lip: None,
            bevel: 0.5,
            ramp: 5,
            caps: true,
            ..strip(rail(&[[0.5, 2.0], [0.3, 2.2]], false), 0.008)
        }),
        TrimSpec::Piping(Piping { flip: true, ..strip(hem(), 0.008) }),
        TrimSpec::Cord(Cord {
            rail: rail(&[[0.5, 0.0], [0.5, 2.1], [0.5, 4.2]], true),
            radius: VEST_PIPING,
            segments: 6,
            finish: Finish::default(),
        }),
        TrimSpec::Cord(Cord {
            rail: rail(&[[0.5, -1.0], [0.3, -1.1]], false),
            radius: 0.0007,
            segments: 5,
            finish: Finish::default(),
        }),
        TrimSpec::Braid(Braid {
            rail: rail(&[[0.5, -0.4], [0.3, -0.5]], false),
            strands: 3,
            lobes: 4,
            strand_width: 0.002,
            finish: Finish::cloth(ColourRef::rgb(0xc8, 0x9a, 0x3c)),
        }),
        TrimSpec::Braid(Braid {
            rail: rail(&[[0.45, 0.0], [0.45, 2.1], [0.45, 4.2]], true),
            strands: 2,
            lobes: 40,
            strand_width: 0.0015,
            finish: Finish::default(),
        }),
        TrimSpec::Button(Button { at: [0.4, 1.2], style: gold_dome(0.006) }),
        TrimSpec::Button(Button {
            at: [0.4, 1.5],
            style: ButtonStyle { radius: 0.005, shank: 0.0, shape: ButtonShape::Flat, finish: Finish::default() },
        }),
        TrimSpec::ButtonRow(ButtonRow { rail: placket(), count: 4, spacing: Some(0.012), button: gold_dome(0.004) }),
        TrimSpec::ButtonRow(ButtonRow {
            rail: rail(&[[0.5, 2.6], [0.3, 2.6]], false),
            count: 4,
            spacing: None,
            button: ButtonStyle { radius: 0.003, shank: 0.0, shape: ButtonShape::Rivet, finish: Finish::default() },
        }),
    ]
}

/// A strip along `rail`, `width` wide, thin enough to sit on a tube with the default ease: its rolled edge and lip
/// reach under a millimetre under the cloth, which on a curvy bust's slope is more in its section.
fn strip(rail: RailSpec, width: f64) -> Piping {
    Piping {
        rail,
        width,
        thickness: 0.0006,
        bevel: 1.0,
        lip: Some(Lip { height: 0.0003, width: 0.002 }),
        bulge: 1.0,
        ramp: 0,
        caps: false,
        flip: false,
        tile: piping::TILE,
        finish: Finish::cloth(ColourRef::rgb(0xf2, 0xea, 0xd8)),
    }
}

/// A rail beside the placket, for a strip.
fn placket_side() -> RailSpec {
    rail(&[[0.52, 0.35], [0.24, 0.35]], false)
}

// ------------------------------------------------------------------------------------------------------- the suite

#[test]
fn every_trim_passes_the_garment_suite_on_every_template_and_shape() {
    let params = TrimmedParams { trims: trims(), ..TrimmedParams::default() };
    conformance::assert_conforms::<Trimmed>(&params, &services());
}

// ------------------------------------------------------------------------------------------ following the surface

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

/// The distance from `p` to the nearest of `triangles`.
fn distance_to(triangles: &[[DVec3; 3]], p: DVec3) -> f64 {
    triangles.iter().map(|[a, b, c]| p.distance(closest_on_triangle(p, *a, *b, *c))).fold(f64::INFINITY, f64::min)
}

fn triangles_of(output: &PartOutput, name: &str) -> Vec<[DVec3; 3]> {
    let mesh = output.meshes.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("no mesh {name}"));
    let g = &mesh.geometry;
    g.indices.chunks_exact(3).map(|t| [0, 1, 2].map(|k| g.positions[t[k] as usize].as_dvec3())).collect()
}

#[test]
fn trims_follow_the_garment_surface_within_half_a_millimetre_at_200_points() {
    let spec = strip(placket_side(), 0.006);
    let params = TrimmedParams { trims: vec![TrimSpec::Piping(spec.clone())], ..TrimmedParams::default() };
    let value = serde_json::to_value(&params).unwrap();
    for template in conformance::TEMPLATES {
        for (label, shape) in conformance::shapes() {
            let def = conformance::character(template, &shape, &[("coat", "test-trimmed", LAYER_SHIRT, value.clone())]);
            let stack = conformance::build(def, &services()).unwrap();
            let output = stack.output("coat");
            let garment = triangles_of(output, "coat-tube");
            // The rail, on the surface the garment was built from, at 200 points.
            let fit: Arc<dyn BodyFit> = stack.before("coat").outer_fit().unwrap().clone();
            let surface = Surface::new(fit, BodyRegion::Torso, 2.0, params.common.ease).unwrap();
            let length = Rail::length_on(&surface, &spec.rail).unwrap();
            let on = Rail::from_polyline_on_surface(&surface, &spec.rail, length / 199.5).unwrap();
            assert_eq!(on.points().len(), 201, "{template}/{label}: 200 pieces");
            let mut worst = 0.0_f64;
            for p in &on.points()[..200] {
                worst = worst.max((distance_to(&garment, p.point) - LIFT).abs());
            }
            // The strip's own face: the vertex at its free edge rides at the lift plus the face's margin.
            let piping = output.meshes.iter().find(|m| m.name == "coat-trim0").unwrap();
            let rung = piping.geometry.positions.len() / on_count(&surface, &spec);
            let face = rung - piping::FRACS.len();
            let mut face_worst = 0.0_f64;
            for k in (0..piping.geometry.positions.len() / rung).step_by(4) {
                let p = piping.geometry.positions[k * rung + face].as_dvec3();
                face_worst = face_worst.max((distance_to(&garment, p) - LIFT - piping::FACE_MARGIN).abs());
            }
            println!(
                "{template}/{label}: the rail is within {:.3} mm of its lift, the strip's face within {:.3} mm",
                worst * 1e3,
                face_worst * 1e3
            );
            assert!(worst <= 0.0005, "{template}/{label}: the rail strays {:.3} mm from the surface", worst * 1e3);
            assert!(face_worst <= 0.0005, "{template}/{label}: the strip strays {:.3} mm", face_worst * 1e3);
        }
    }
}

/// The rungs the strip `spec` has on `surface`.
fn on_count(surface: &Surface, spec: &Piping) -> usize {
    Rail::from_polyline_on_surface(surface, &spec.rail, spec.rail.step).unwrap().points().len()
}

fn torso_surface() -> Surface {
    let fit: Arc<dyn BodyFit> = Arc::new(TorsoFit::new(&omuzaki_char_api::BodyParams::default()));
    Surface::new(fit, BodyRegion::Torso, 2.0, 0.004).unwrap()
}

#[test]
fn a_zero_length_rail_is_an_error_not_a_panic() {
    let surface = torso_surface();
    let still = rail(&[[0.4, 0.3], [0.4, 0.3]], false);
    assert_eq!(Rail::from_polyline_on_surface(&surface, &still, RAIL_STEP), Err(TrimError::ZeroLengthRail));
    let spec = strip(still.clone(), 0.005);
    assert!(matches!(piping::build(&surface, &spec), Err(TrimError::ZeroLengthRail)));
    let cord = Cord { rail: still.clone(), radius: 0.001, segments: 6, finish: Finish::default() };
    assert!(matches!(piping::build_cord(&surface, &cord), Err(TrimError::ZeroLengthRail)));
    let row = ButtonRow { rail: still, count: 3, spacing: None, button: gold_dome(0.004) };
    assert!(matches!(buttons::build_row(&surface, &row), Err(TrimError::ZeroLengthRail)));
    // And through a build: the garment fails, naming the trim.
    let params = TrimmedParams { trims: vec![TrimSpec::Piping(spec)], ..TrimmedParams::default() };
    let def = conformance::character(
        "standard",
        &omuzaki_char_api::BodyShape::default(),
        &[("coat", "test-trimmed", LAYER_SHIRT, serde_json::to_value(params).unwrap())],
    );
    let error = conformance::build(def, &services()).err().expect("a zero-length rail fails the build");
    assert!(error.to_string().contains("trim `trim0`: the rail has no length"), "{error}");
}

#[test]
fn rails_are_densified_as_the_prototype_framed_and_skinned_and_refuse_bad_specs() {
    let surface = torso_surface();
    let open = Rail::from_polyline_on_surface(&surface, &placket(), RAIL_STEP).unwrap();
    // One step: max(2, ceil(length / step)) pieces, uniform in [t, phi].
    let pieces = (Rail::length_on(&surface, &placket()).unwrap() / RAIL_STEP).ceil().max(2.0) as usize;
    assert_eq!(open.points().len(), pieces + 1);
    let dt: Vec<f64> = open.points().windows(2).map(|w| w[1].t - w[0].t).collect();
    assert!(dt.iter().all(|d| (d - dt[0]).abs() < 1e-12), "uniform in t");
    assert!(open.points().windows(2).all(|w| w[0].point.distance(w[1].point) < RAIL_STEP * 1.1));
    for p in open.points() {
        assert!(p.tangent.dot(p.normal).abs() < 1e-9 && (p.side - p.tangent.cross(p.normal)).length() < 1e-9);
        assert!((p.point - p.on_surface - p.normal * LIFT).length() < 1e-12, "lifted along the normal");
        let total: f32 = p.skin.iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1e-5);
    }
    // Two steps: no point repeated at the joint.
    let bent = rail(&[[0.5, 0.0], [0.4, 0.2], [0.3, 0.0]], false);
    let bent_rail = Rail::from_polyline_on_surface(&surface, &bent, RAIL_STEP).unwrap();
    assert!(bent_rail.points().windows(2).all(|w| w[0].point.distance(w[1].point) > 1e-6));
    let closed = Rail::from_polyline_on_surface(&surface, &hem(), RAIL_STEP).unwrap();
    let last = closed.points().len() - 1;
    assert_eq!(closed.points()[0].point, closed.points()[last].point, "a loop repeats its first point");
    // Each step goes the short way round: a hem given as 0 -> 4.2 -> 0 doesn't double back.
    let ring = surface.ring(0.23).unwrap();
    let around = 2.0 * std::f64::consts::PI * ring.radii.x.max(ring.radii.y);
    assert!(closed.length() < around * 1.1, "{} for a ring of about {around}", closed.length());
    for bad in [
        rail(&[[0.4, 0.0]], false),
        rail(&[[0.4, 0.0], [1.5, 0.0]], false),
        rail(&[[0.4, 0.0], [0.5, f64::NAN]], false),
        rail(&[[0.4, 0.0], [0.5, 0.0]], true),
        RailSpec { lift: -0.001, ..placket() },
        RailSpec { step: 0.0, ..placket() },
    ] {
        let refused = Rail::from_polyline_on_surface(&surface, &bad, RAIL_STEP);
        assert!(matches!(refused, Err(TrimError::Invalid { .. })), "{bad:?}");
    }
    assert!(Rail::from_polyline_on_surface(&surface, &placket(), 0.0).is_err());
}

#[test]
fn a_strip_rung_is_lip_rolled_edge_and_bevelled_face() {
    let surface = torso_surface();
    let spec = strip(placket_side(), 0.02);
    let mesh = piping::build(&surface, &spec).unwrap();
    let rungs = on_count(&surface, &spec);
    let g = mesh.mesh();
    assert_eq!(g.positions.len(), rungs * 11, "lip 2, edge 3, face 6 a rung");
    let rail = Rail::from_polyline_on_surface(&surface, &spec.rail, spec.rail.step).unwrap();
    let k = rungs / 2;
    let p = &rail.points()[k];
    let height = |j: usize| (g.positions[k * 11 + j].as_dvec3() - p.on_surface).dot(p.normal);
    let lip = spec.lip.as_ref().unwrap();
    assert!((height(1) + spec.thickness + lip.height).abs() < 1e-6, "the lip's root under the cloth");
    assert!((height(5) - (LIFT + piping::FACE_MARGIN)).abs() < 1e-6, "the face at the free edge");
    let inner = surface.project(g.positions[k * 11 + 10].as_dvec3()).unwrap().unwrap().outside;
    assert!((inner - piping::FACE_MARGIN).abs() < 1e-4, "bevelled down to the cloth at the inner border: {inner}");
    // The rolled edge stands out past the free edge by the bulge.
    let out = (g.positions[k * 11 + 3].as_dvec3() - p.on_surface).dot(-p.side);
    assert!((out - piping::BULGE.0).abs() < 2e-4, "{out}");
    let without = piping::build(&surface, &Piping { lip: None, ..spec }).unwrap();
    assert_eq!(without.mesh().positions.len(), rungs * 9);
}

#[test]
fn the_gold_button_is_the_prototypes_and_rows_space_their_buttons() {
    let (foot, normal) = (DVec3::new(0.0, 1.0, 0.1), DVec3::Z);
    let skin = [(omuzaki_char_api::JointRef::HIPS, 1.0)];
    let mut builder = MeshBuilder::new();
    let gold = ButtonStyle { shank: 0.0, ..gold_dome(buttons::GOLD_RADIUS) };
    buttons::button(&mut builder, foot, normal, &gold, &skin).unwrap();
    let points: Vec<DVec3> = builder.mesh().positions.iter().map(|p| p.as_dvec3() - foot).collect();
    let height = points.iter().map(|p| p.dot(normal)).fold(f64::MIN, f64::max);
    let base = points.iter().map(|p| p.dot(normal)).fold(f64::MAX, f64::min);
    let widest = points.iter().map(|p| (*p - normal * p.dot(normal)).length()).fold(0.0, f64::max);
    // The tip's top: 1 mm standoff, 2.1 mm up, half its 1.2 mm height; the disc's foot: 1 mm less half its 2 mm.
    assert!((height - (0.0010 + 0.0021 + 0.0006)).abs() < 1e-6, "{height}");
    assert!(base.abs() < 1e-6, "{base}");
    assert!((widest - (0.0061 + 0.00095)).abs() < 1e-4, "the rim is the widest part: {widest}");
    let mut small = MeshBuilder::new();
    let half = ButtonStyle { radius: buttons::GOLD_RADIUS / 2.0, ..gold };
    buttons::button(&mut small, foot, normal, &half, &skin).unwrap();
    assert_eq!(small.vertex_count(), builder.vertex_count(), "scaled, not retessellated");

    let spaced = buttons::row_distances(0.2, 5, Some(0.03)).unwrap();
    assert_eq!(spaced.len(), 5);
    assert!((spaced[0] - 0.04).abs() < 1e-12 && (spaced[4] - 0.16).abs() < 1e-12, "centred: {spaced:?}");
    assert_eq!(buttons::row_distances(0.2, 1, None).unwrap(), vec![0.1]);
    assert_eq!(buttons::row_distances(0.2, 3, None).unwrap(), vec![0.0, 0.1, 0.2]);
    assert!(buttons::row_distances(0.1, 5, Some(0.03)).is_err(), "too far apart to fit");
}

#[test]
fn a_braid_has_a_closed_strand_per_strand_above_its_rail() {
    let surface = torso_surface();
    let spec = Braid {
        rail: rail(&[[0.5, -0.4], [0.3, -0.5]], false),
        strands: 4,
        lobes: 2,
        strand_width: 0.002,
        finish: Finish::default(),
    };
    let built = omuzaki_char::outfit::trim::braid::build(&surface, &spec).unwrap();
    assert!(built.triangle_count() > 0);
    for bad in [
        Braid { strands: 1, ..spec.clone() },
        Braid { lobes: 0, ..spec.clone() },
        Braid { strand_width: 0.0, ..spec.clone() },
        // Lobes too short for four strands to pass each other on this 4 to 5 cm rail.
        Braid { lobes: 12, ..spec },
    ] {
        assert!(omuzaki_char::outfit::trim::braid::build(&surface, &bad).is_err());
    }
}

#[test]
fn metal_uses_the_band_highlight_program_and_is_declared_metal() {
    let metal = MetalStyle::default();
    metal.validate().unwrap();
    let spec = metal.material(Colour::rgb(0xd4, 0xa8, 0x48), Colour::rgb(0xff, 0xf2, 0xc8)).unwrap();
    let Shading::Program { name, params } = &spec.shading else { panic!("a program") };
    assert_eq!(name.as_str(), METAL_PROGRAM);
    let keys: Vec<&str> = params.keys().map(omuzaki_core::id::Slug::as_str).collect();
    assert_eq!(keys, ["colour", "dir", "edge0", "edge1", "strength"]);
    spec.validate().unwrap();
    assert!(MetalStyle { edge0: 0.9, edge1: 0.8, ..MetalStyle::default() }.validate().is_err());
    assert!(MetalStyle { dir: [2.0, 0.0, 0.0], ..MetalStyle::default() }.validate().is_err());

    let params = TrimmedParams {
        trims: vec![TrimSpec::Button(Button { at: [0.4, 0.0], style: gold_dome(0.006) })],
        ..TrimmedParams::default()
    };
    let def = conformance::character(
        "soft-sd",
        &omuzaki_char_api::BodyShape::default(),
        &[("coat", "test-trimmed", LAYER_SHIRT, serde_json::to_value(params).unwrap())],
    );
    let stack = conformance::build(def, &services()).unwrap();
    let output = stack.output("coat");
    assert_eq!(output.material_classes, vec![("coat-trim0".parse().unwrap(), MaterialClass::Metal)]);
}

#[test]
fn trim_specs_round_trip_and_reject_unknown_fields() {
    for trim in trims() {
        let value = serde_json::to_value(&trim).unwrap();
        assert_eq!(serde_json::from_value::<TrimSpec>(value.clone()).unwrap(), trim);
        trim.validate().unwrap();
    }
    let typo = serde_json::json!({ "kind": "piping", "rail": { "points": [[0.3, 0.0], [0.4, 0.0]] }, "width": 0.004,
        "thickness": 0.002, "thicknes": 0.002 });
    assert!(serde_json::from_value::<TrimSpec>(typo).is_err());
    let schema = schemars::schema_for!(TrimmedParams);
    assert!(serde_json::to_string(&schema).unwrap().contains("button_row"));
}

// ------------------------------------------------------------------------------------- skin across a hand-over

/// Each vertex's skin as a map from joint to weight, read back through the builder's bone list.
fn skins(builder: &MeshBuilder) -> Vec<HashMap<JointRef, f32>> {
    let m = builder.mesh();
    m.joints
        .iter()
        .zip(&m.weights)
        .map(|(j, w)| {
            let mut skin = HashMap::new();
            for (joint, weight) in j.iter().zip(w).filter(|(_, w)| **w > 0.0) {
                *skin.entry(builder.bones()[usize::from(*joint)].clone()).or_insert(0.0) += *weight;
            }
            skin
        })
        .collect()
}

/// The largest difference of any joint's weight between two skins.
fn skin_difference(a: &HashMap<JointRef, f32>, b: &HashMap<JointRef, f32>) -> f32 {
    a.keys().chain(b.keys()).map(|j| (a.get(j).unwrap_or(&0.0) - b.get(j).unwrap_or(&0.0)).abs()).fold(0.0, f32::max)
}

/// A blend as a map, as [`skins`] reads it back.
fn as_map(blend: Vec<(JointRef, f32)>) -> HashMap<JointRef, f32> {
    let mut skin = HashMap::new();
    for (joint, w) in blend.into_iter().filter(|(_, w)| *w > 0.0) {
        *skin.entry(joint).or_insert(0.0) += w;
    }
    skin
}

/// Vertices per rung of a strip with a lip, and the first face vertex in a rung (after the lip's 2 and the edge's 3).
const RUNG: usize = 11;
const FIRST_FACE: usize = 5;

#[test]
fn a_band_across_a_hand_over_is_skinned_like_the_cloth_under_each_vertex() {
    let surface = torso_surface();
    // A band 6 cm wide from a rail round the front, and the same band laid diagonally: each spans a hand-over between
    // two torso bones, so its free edge and its inner border sit on differently weighted cloth.
    for rail_spec in [rail(&[[0.55, -0.6], [0.55, 0.6]], false), rail(&[[0.62, -0.6], [0.42, 0.6]], false)] {
        let spec = Piping { caps: true, ..strip(rail_spec, 0.06) };
        let built = piping::build(&surface, &spec).unwrap();
        let per_vertex = skins(&built);
        let rail = Rail::from_polyline_on_surface(&surface, &spec.rail, spec.rail.step).unwrap();
        let mut spread = 0.0_f32;
        for (i, p) in rail.points().iter().enumerate() {
            let a = [p.t, p.phi];
            let b = omuzaki_char::outfit::trim::param_offset(&surface, a, p.side, spec.width).unwrap();
            // Each face vertex is skinned as the cloth at its own place across the band, as the garment under it is.
            for (k, f) in piping::FRACS.iter().enumerate() {
                let t = (a[0] + (b[0] - a[0]) * f).clamp(0.0, 1.0);
                let expected = as_map(surface.weights().at(t));
                let got = &per_vertex[i * RUNG + FIRST_FACE + k];
                assert!(skin_difference(got, &expected) < 1e-4, "rung {i}, face vertex {k}: {got:?} for {expected:?}");
            }
            // The rolled edge is on the free edge, at the rail.
            let edge = as_map(surface.weights().at(p.t.clamp(0.0, 1.0)));
            assert!(skin_difference(&per_vertex[i * RUNG + 3], &edge) < 1e-4, "rung {i}'s rolled edge");
            spread = spread.max(skin_difference(&per_vertex[i * RUNG + 3], &per_vertex[i * RUNG + RUNG - 1]));
        }
        println!("{:?}: the inner border's weights differ from the free edge's by up to {spread:.3}", spec.rail.points);
        assert!(spread > 0.2, "the band crosses a hand-over (the free edge's weights would be wrong at its inner border)");

        // The end caps are skinned vertex by vertex too: each cap vertex on a rung vertex carries that vertex's skin,
        // so a bent band doesn't open at its ends. And every cap triangle faces out of the strip's end.
        let m = built.mesh();
        let strip_vertices = rail.points().len() * RUNG;
        let strip_triangles = (rail.points().len() - 1) * (RUNG - 1) * 2;
        let ends = [(0, -rail.points()[0].tangent), (rail.points().len() - 1, rail.points().last().unwrap().tangent)];
        let cap_vertices = (m.positions.len() - strip_vertices) / 2;
        let cap_triangles = (m.indices.len() / 3 - strip_triangles) / 2;
        assert_eq!(cap_vertices, RUNG + 1, "a cap is the rung plus the closing edge's midpoint");
        for (c, (row, outward)) in ends.into_iter().enumerate() {
            let first = strip_vertices + c * cap_vertices;
            let mut matched = 0;
            for v in first..first + cap_vertices {
                let on = (0..RUNG).find(|&k| m.positions[v] == m.positions[row * RUNG + k]);
                if let Some(k) = on {
                    matched += 1;
                    assert!(skin_difference(&per_vertex[v], &per_vertex[row * RUNG + k]) < 1e-6, "cap {c}, vertex {k}");
                }
            }
            assert_eq!(matched, RUNG, "every rung vertex is on the cap");
            let tris = &m.indices[(strip_triangles + c * cap_triangles) * 3..(strip_triangles + (c + 1) * cap_triangles) * 3];
            let facing: Vec<f64> = tris
                .chunks_exact(3)
                .map(|tri| {
                    let [a, b, d] = [0, 1, 2].map(|k| m.positions[tri[k] as usize].as_dvec3());
                    (b - a).cross(d - a).normalize_or_zero().dot(outward)
                })
                .collect();
            assert!(facing.iter().all(|f| *f > 0.0), "cap {c}: a triangle folds back into the strip: {facing:?}");
        }
    }
}

// ------------------------------------------------------------------------------------------ braids don't touch

/// Each strand's centre line: the centre of each of its rings ([`braid`]'s strands are equal blocks of rings of
/// `ROUND + 1` vertices, the last repeating the first).
fn strand_centres(built: &MeshBuilder, strands: usize) -> Vec<Vec<DVec3>> {
    const ROUND: usize = 8;
    let positions = &built.mesh().positions;
    let per = positions.len() / strands;
    (0..strands)
        .map(|s| {
            positions[s * per..(s + 1) * per]
                .chunks_exact(ROUND + 1)
                .map(|ring| ring[..ROUND].iter().map(|p| p.as_dvec3()).sum::<DVec3>() / ROUND as f64)
                .collect()
        })
        .collect()
}

/// The least distance between segments `p0 p1` and `q0 q1` (Ericson 5.1.9).
fn segment_distance(p0: DVec3, p1: DVec3, q0: DVec3, q1: DVec3) -> f64 {
    let (d1, d2, r) = (p1 - p0, q1 - q0, p0 - q0);
    let (a, e, f) = (d1.length_squared(), d2.length_squared(), d2.dot(r));
    let (s, t) = if a <= 1e-30 && e <= 1e-30 {
        (0.0, 0.0)
    } else if a <= 1e-30 {
        (0.0, (f / e).clamp(0.0, 1.0))
    } else {
        let c = d1.dot(r);
        if e <= 1e-30 {
            ((-c / a).clamp(0.0, 1.0), 0.0)
        } else {
            let b = d1.dot(d2);
            let denom = a * e - b * b;
            let mut s = if denom > 0.0 { ((b * f - c * e) / denom).clamp(0.0, 1.0) } else { 0.0 };
            let mut t = (b * s + f) / e;
            if t < 0.0 {
                t = 0.0;
                s = (-c / a).clamp(0.0, 1.0);
            } else if t > 1.0 {
                t = 1.0;
                s = ((b - c) / a).clamp(0.0, 1.0);
            }
            (s, t)
        }
    };
    (p0 + d1 * s).distance(q0 + d2 * t)
}

/// The least distance between two polylines.
fn polyline_distance(p: &[DVec3], q: &[DVec3]) -> f64 {
    let mut least = f64::INFINITY;
    for a in p.windows(2) {
        let (lo, hi) = (a[0].min(a[1]), a[0].max(a[1]));
        for b in q.windows(2) {
            // Segments whose boxes are further apart than the best so far can't beat it.
            let gap = (b[0].min(b[1]) - hi).max(lo - b[0].max(b[1])).max(DVec3::ZERO);
            if gap.length() < least {
                least = least.min(segment_distance(a[0], a[1], b[0], b[1]));
            }
        }
    }
    least
}

#[test]
fn braid_strands_never_touch_each_other() {
    use omuzaki_char::outfit::trim::braid;
    let surface = torso_surface();
    let open = rail(&[[0.5, -0.4], [0.3, -0.5]], false);
    let closed = rail(&[[0.45, 0.0], [0.45, 2.1], [0.45, 4.2]], true);
    // The closed two-strand fixture (whose strands used to meet where they crossed), a closed three-strand braid, and
    // two to six strands on an open rail, from loose to as dense as the strands allow.
    let mut cases = vec![(closed.clone(), 2, 40, 0.0015), (closed, 3, 40, 0.0015)];
    for strands in braid::MIN_STRANDS..=braid::MAX_STRANDS {
        for lobes in [2, 6, 12] {
            cases.push((open.clone(), strands, lobes, 0.002));
        }
    }
    for (rail_spec, strands, lobes, strand_width) in cases {
        let spec = Braid { rail: rail_spec, strands, lobes, strand_width, finish: Finish::default() };
        let built = match braid::build(&surface, &spec) {
            Ok(built) => built,
            Err(e) => {
                // Refused only where its lobes leave a strand no more than the gap along the rail.
                let lobe = Rail::length_on(&surface, &spec.rail).unwrap() / f64::from(lobes);
                assert!(lobe / f64::from(strands) <= braid::GAP * strand_width * 1.02, "{strands} x {lobes}: {e}");
                assert!(e.to_string().contains("lobes"), "{e}");
                continue;
            }
        };
        let centres = strand_centres(&built, strands as usize);
        let mut least = f64::INFINITY;
        for j in 0..centres.len() {
            for k in j + 1..centres.len() {
                least = least.min(polyline_distance(&centres[j], &centres[k]));
            }
        }
        // Two round strands of this width touch when their centre lines are a width apart.
        println!("{strands} strands, {lobes} lobes: centre lines {:.3} widths apart at least", least / strand_width);
        assert!(least >= strand_width, "{strands} strands, {lobes} lobes: {:.3} mm apart", least * 1e3);
    }
}

#[test]
fn the_winding_radius_is_the_least_that_keeps_strands_apart() {
    use omuzaki_char::outfit::trim::braid::{pair_distance, winding_radius};
    let gap = 0.002;
    // On a long lobe the strands barely lean, so they need a circle whose chord between neighbours is the gap.
    for n in 2..=6_u32 {
        let r = winding_radius(n, 10.0, gap).unwrap();
        let chord = gap / (2.0 * (std::f64::consts::PI / f64::from(n)).sin());
        assert!((r - chord).abs() < 1e-3 * chord, "{n}: {r} for {chord}");
        // Every pair clears the gap, and a slightly smaller circle doesn't.
        assert!((1..n).all(|d| pair_distance(r, 10.0, d, n) >= gap));
        assert!((1..n).any(|d| pair_distance(r * 0.99, 10.0, d, n) < gap));
    }
    // A lobe that leaves each strand no more than the gap along the rail has no radius at all.
    assert_eq!(winding_radius(3, 3.0 * gap, gap), None);
    assert!(winding_radius(3, 3.3 * gap, gap).is_some());
}
