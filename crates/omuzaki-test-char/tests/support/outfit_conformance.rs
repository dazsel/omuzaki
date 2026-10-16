//! The garment conformance suite: builds any garment kind on every body template and shape and checks what every
//! garment must keep, whatever it looks like.
//!
//! A garment's test includes this file and calls [`run`] (or [`assert_conforms`]) with its parameters:
//!
//! ```ignore
//! #[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
//! mod conformance;
//! conformance::assert_conforms::<Shirt>(&ShirtParams::default(), &services);
//! ```
//!
//! On each of the 4 templates × 3 shapes it builds a body and the garment (layer [`LAYER_SHIRT`]) and checks:
//! - **clearance**: each vertex lies at least half the ease outside the fit it was built over, and nothing lies inside
//!   it; a failure names the vertex, the region, where along it and how deep;
//! - **fit**: every section of the body fit over the garment's regions lies within [`FIT_GAP`] of its region's own
//!   mesh, so a fit blown out round a neighbouring limb or a cap (and every garment over it) fails;
//! - **body mesh**: no garment triangle meets a triangle of the body's own mesh (a separating-axis test against every
//!   body triangle near it), so a body that stands outside its published fit is caught too, and so is a garment that
//!   reaches a part of the body it doesn't cover (an arm beside the torso);
//! - **body**: no vertex is inside a region of the body fit the garment covers, and no point of any triangle is either: each triangle is
//!   tested whole against convex hulls that contain the region (GJK, proving it clear with a separating plane or
//!   reporting it), so a crossing of any depth is caught, never sampled past. A hull spans two sections the fit itself
//!   reports, with a margin for the frames turning between them and for the fit's own curve bulging past the straight
//!   line between them (a body's profiles are cubic);
//! - **closure**: after welding coincident positions every edge is shared by at most two triangles wound opposite ways,
//!   and the boundary edges form exactly [`Garment::open_borders`] loops;
//! - **skin**: every mesh passes `PartMesh::validate` (weights normalised, joints listed) and every joint is in the rig;
//! - **names**: every material and texture is named after the part, and every reference between them resolves;
//! - **determinism**: a second build is byte-identical;
//! - **time**: the build takes at most [`MAX_BUILD_MS`] in a release build, and [`UNOPTIMISED_SLOWDOWN`] times that in
//!   the unoptimised test build CI runs.
//!
//! The geometry checks measure against the fit's analytic sections, not against the loft code under test: a vertex is
//! located on a region by finding where along the region's axis its section plane passes through the vertex.
//!
//! The file depends only on crates both `omuzaki-char` and `omuzaki-test-char` test with.

// A test helper: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(dead_code)] // each test binary uses the part of the suite it needs
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic on small counts
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use omuzaki_char::outfit::common::{Garment, LAYER_SHIRT};
use omuzaki_char::template::layout::section_basis;
use omuzaki_char_api::{
    BodyFit, BodyRegion, BodyShape, BuildError, BuildOptions, BuildServices, CharacterDef, FitFact, PartDef, PartId,
    PartKindId, PartMesh, PartOutput, Published, build_part, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{DVec2, DVec3};
use serde_json::{Value, json};

/// The body templates every garment is built on.
pub const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];
/// The longest a garment may take to build, milliseconds (asserted in release builds).
pub const MAX_BUILD_MS: f64 = 40.0;
/// How much slower an unoptimised test build of the garment code may be than [`MAX_BUILD_MS`] allows: CI's gates run
/// the test profile, so the budget is asserted there too, scaled by this (the trimmed test tube is about 10 ms in a
/// release build and about 15 times that unoptimised).
pub const UNOPTIMISED_SLOWDOWN: f64 = 25.0;

/// The build budget for this build of the suite, ms: [`MAX_BUILD_MS`], scaled by [`UNOPTIMISED_SLOWDOWN`] when debug
/// assertions are on (the test profile).
pub fn build_budget_ms() -> f64 {
    if cfg!(debug_assertions) { MAX_BUILD_MS * UNOPTIMISED_SLOWDOWN } else { MAX_BUILD_MS }
}
/// Every region of the body fit, checked for crossings.
pub const REGIONS: [BodyRegion; 8] = [
    BodyRegion::Torso,
    BodyRegion::Neck,
    BodyRegion::ArmL,
    BodyRegion::ArmR,
    BodyRegion::LegL,
    BodyRegion::LegR,
    BodyRegion::FootL,
    BodyRegion::FootR,
];
/// The part id the garment is built as.
pub const PART: &str = "garment";
/// The part id the body is built as.
pub const BODY: &str = "body";
/// Sections per region in the lookup table a vertex is located with, and the rows the crossing test's hulls start from.
pub const TABLE: usize = 256;
/// Positions closer than this weld into one vertex, metres.
const WELD: f64 = 1e-6;

/// The body shapes every garment is built on: the template's own, slim and straight, and broad and curvy.
pub fn shapes() -> [(&'static str, BodyShape); 3] {
    let slim = BodyShape { build: -1.0, curves: -1.0, shoulder_hip: -0.5, ..BodyShape::default() };
    let broad = BodyShape { build: 1.0, curves: 1.0, musculature: 1.0, shoulder_hip: 0.5, ..BodyShape::default() };
    [("default", BodyShape::default()), ("slim", slim), ("broad", broad)]
}

/// One row of the suite's table.
#[derive(Clone, Debug)]
pub struct Row {
    /// The template.
    pub template: &'static str,
    /// The shape's label.
    pub shape: &'static str,
    /// The least clearance of any vertex to the fit inside the garment, millimetres.
    pub min_clearance_mm: f64,
    /// The garment's ease, millimetres.
    pub ease_mm: f64,
    /// The fastest of three builds, milliseconds.
    pub build_ms: f64,
    /// Triangles in all the garment's meshes.
    pub triangles: usize,
}

/// The rows as a markdown table, for the PR's evidence.
pub fn table(rows: &[Row]) -> String {
    let mut out = String::from("| template | shape | min clearance (mm) | ease (mm) | build (ms) | triangles |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for r in rows {
        let _ = writeln!(
            out,
            "| {} | {} | {:.2} | {:.1} | {:.2} | {} |",
            r.template, r.shape, r.min_clearance_mm, r.ease_mm, r.build_ms, r.triangles
        );
    }
    out
}

/// A character built part by part: what each part output, and what was published before and after each.
pub struct Stack {
    /// The parts in build order with their outputs.
    pub outputs: Vec<(PartId, PartOutput)>,
    /// What was published before each part built, in the same order.
    pub before: Vec<Published>,
    /// Everything published at the end.
    pub published: Published,
    /// The character.
    pub def: CharacterDef,
    /// How long each part took to build, milliseconds.
    pub build_ms: Vec<f64>,
}

impl Stack {
    /// The output of `part`.
    pub fn output(&self, part: &str) -> &PartOutput {
        &self.outputs.iter().find(|(id, _)| id.as_str() == part).expect("the part was built").1
    }

    /// What was published before `part` built.
    pub fn before(&self, part: &str) -> &Published {
        let k = self.outputs.iter().position(|(id, _)| id.as_str() == part).expect("the part was built");
        &self.before[k]
    }
}

/// A body of `template` and `shape` with `garments` over it: `(part id, kind, layer, params)`.
pub fn character(template: &str, shape: &BodyShape, garments: &[(&str, &str, i32, Value)]) -> CharacterDef {
    let mut def = mock_character().expect("the mock character parses");
    def.template = template.parse().expect("a template id");
    def.parts.clear();
    let part = |kind: &str, layer: i32, params: Value| PartDef {
        kind: PartKindId::new(kind).expect("a kind id"),
        parent: None,
        enabled: true,
        layer,
        attach: None,
        params,
    };
    def.parts.insert(PartId::new(BODY).unwrap(), part("body", 0, json!({ "shape": shape })));
    for (id, kind, layer, params) in garments {
        def.parts.insert(PartId::new(*id).unwrap(), part(kind, *layer, params.clone()));
    }
    def
}

/// Builds `def` part by part in its build order.
///
/// # Errors
/// The first part's build error.
pub fn build(def: CharacterDef, services: &BuildServices) -> Result<Stack, BuildError> {
    let options = BuildOptions::default();
    let mut published = Published::default();
    let (mut outputs, mut before, mut build_ms) = (Vec::new(), Vec::new(), Vec::new());
    for id in plan(&def)? {
        let start = Instant::now();
        let output = build_part(&def, &id, &options, services, &published)?;
        build_ms.push(start.elapsed().as_secs_f64() * 1e3);
        before.push(published.clone());
        published.absorb(&id, output.publishes.clone())?;
        outputs.push((id, output));
    }
    Ok(Stack { outputs, before, published, def, build_ms })
}

/// Runs the suite for garment `K` with `params` on every template and shape. Returns the table rows, or the first
/// failure as a message naming the case and what broke.
///
/// # Errors
/// A message for the first check that fails.
pub fn run<K: Garment>(params: &K::Params, services: &BuildServices) -> Result<Vec<Row>, String> {
    let kind = K::meta().id;
    let value = serde_json::to_value(params).map_err(|e| format!("{kind}: parameters don't serialise: {e}"))?;
    let common = K::common(params);
    let ease = common.ease;
    let regions = K::regions(params);
    let mut rows = Vec::new();
    for template in TEMPLATES {
        for (label, shape) in shapes() {
            let case = format!("{kind} on {template}/{label}");
            let def = character(template, &shape, &[(PART, kind, LAYER_SHIRT, value.clone())]);
            let stack = build(def.clone(), services).map_err(|e| format!("{case}: the build failed: {e}"))?;
            let output = stack.output(PART);
            let before = stack.before(PART);
            let fits = &before.get::<FitFact>().fits;
            let body = fits.first().ok_or_else(|| format!("{case}: no body fit"))?;
            let inner = before.outer_fit().ok_or_else(|| format!("{case}: no fit under the garment"))?;
            let body_table = Tables::of(body.as_ref()).only(&regions);
            let body_pieces = body_pieces(body.as_ref(), &body_table);
            let inner_table = Tables::of(inner.as_ref());
            let min_clearance = check_clearance(output, &body_table, &body_pieces, &inner_table, &regions, ease)
                .map_err(|e| format!("{case}: {e}"))?;
            check_fit_tight(body.as_ref(), stack.output(BODY), &regions).map_err(|e| format!("{case}: {e}"))?;
            check_body_mesh(output, stack.output(BODY)).map_err(|e| format!("{case}: {e}"))?;
            check_closure(output, K::open_borders(params)).map_err(|e| format!("{case}: {e}"))?;
            check_skin(output, &stack.published).map_err(|e| format!("{case}: {e}"))?;
            check_names(output, PART).map_err(|e| format!("{case}: {e}"))?;
            let build_ms = check_repeat(&def, output, before, services).map_err(|e| format!("{case}: {e}"))?;
            let budget = build_budget_ms();
            if build_ms > budget {
                return Err(format!("{case}: the build took {build_ms:.1} ms, over the {budget} ms budget"));
            }
            rows.push(Row {
                template,
                shape: label,
                min_clearance_mm: min_clearance * 1e3,
                ease_mm: ease * 1e3,
                build_ms,
                triangles: output.meshes.iter().map(|m| m.geometry.indices.len() / 3).sum(),
            });
        }
    }
    Ok(rows)
}

/// Runs the suite and panics with the failure, or prints the table.
pub fn assert_conforms<K: Garment>(params: &K::Params, services: &BuildServices) -> Vec<Row> {
    let rows = run::<K>(params, services).unwrap_or_else(|e| panic!("conformance: {e}"));
    println!("{}", table(&rows));
    rows
}

// ------------------------------------------------------------------------------------------------------ locating

/// One region's sections, sampled evenly along it, with their planes' frames.
struct RegionTable {
    region: BodyRegion,
    rows: Vec<Slice>,
    /// A box round every section's ellipse: a point outside it is inside none of them.
    bounds: (DVec3, DVec3),
}

#[derive(Copy, Clone)]
struct Slice {
    t: f64,
    centre: DVec3,
    axis: DVec3,
    across: DVec3,
    through: DVec3,
    radii: DVec2,
}

/// The section of `region` at `t`, as the fit reports it, with its plane's frame.
fn slice(fit: &dyn BodyFit, region: BodyRegion, t: f64) -> Option<Slice> {
    let s = fit.section(region, t as f32)?;
    let (across, through) = section_basis(region, s.axis);
    Some(Slice {
        t,
        centre: s.centre.as_dvec3(),
        axis: s.axis.as_dvec3().normalize(),
        across: across.as_dvec3(),
        through: through.as_dvec3(),
        radii: s.radii.as_dvec2(),
    })
}

impl Slice {
    fn lerp(&self, other: &Self, s: f64) -> Self {
        let axis = self.axis.lerp(other.axis, s).normalize();
        Self {
            t: self.t + (other.t - self.t) * s,
            centre: self.centre.lerp(other.centre, s),
            axis,
            across: self.across.lerp(other.across, s).normalize(),
            through: self.through.lerp(other.through, s).normalize(),
            radii: self.radii.lerp(other.radii, s),
        }
    }

    /// The signed distance of `p` from the plane of this section, along its axis.
    fn height(&self, p: DVec3) -> f64 {
        (p - self.centre).dot(self.axis)
    }
}

/// Where a point sits on one region: the fraction along it, and its coordinates in the section's plane.
#[derive(Copy, Clone, Debug)]
struct Located {
    region: BodyRegion,
    t: f64,
    q: DVec2,
    radii: DVec2,
}

impl Located {
    /// How far outside the section's ellipse the point is (negative inside), metres.
    fn distance(&self) -> f64 {
        ellipse_distance(self.q, self.radii)
    }

    /// The point's ellipse level: below 1 inside.
    fn level(&self) -> f64 {
        (self.q / self.radii).length()
    }
}

/// The sections of every region of a fit.
pub struct Tables(Vec<RegionTable>);

impl Tables {
    /// These tables, of the `regions` only.
    #[must_use]
    pub fn only(self, regions: &[BodyRegion]) -> Self {
        Self(self.0.into_iter().filter(|t| regions.contains(&t.region)).collect())
    }

    /// The tables of `fit`'s regions.
    pub fn of(fit: &dyn BodyFit) -> Self {
        let tables = REGIONS
            .iter()
            .filter_map(|&region| {
                let rows =
                    (0..=TABLE).map(|k| slice(fit, region, k as f64 / TABLE as f64)).collect::<Option<Vec<_>>>()?;
                let reach = |s: &Slice| DVec3::splat(s.radii.max_element());
                let lo = rows.iter().map(|s| s.centre - reach(s)).reduce(DVec3::min)?;
                let hi = rows.iter().map(|s| s.centre + reach(s)).reduce(DVec3::max)?;
                Some(RegionTable { region, rows, bounds: (lo, hi) })
            })
            .collect();
        Self(tables)
    }

    fn region(&self, region: BodyRegion) -> Option<&RegionTable> {
        self.0.iter().find(|t| t.region == region)
    }
}

impl RegionTable {
    /// Where `p`'s section plane is, if it crosses the region: of the places along the axis whose plane passes through
    /// `p`, the one where `p` is deepest (lowest ellipse level).
    /// Whether `p` may be inside one of the region's sections.
    fn may_contain(&self, p: DVec3) -> bool {
        p.cmpge(self.bounds.0).all() && p.cmple(self.bounds.1).all()
    }

    fn locate(&self, p: DVec3) -> Option<Located> {
        let mut best: Option<Located> = None;
        for pair in self.rows.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (ha, hb) = (a.height(p), b.height(p));
            if ha.signum() == hb.signum() && ha != 0.0 {
                continue;
            }
            // On the first plane (a height of zero, of either sign), the point's section is that one; else bisect
            // within the interval, on the linearly interpolated section.
            let s = if ha == 0.0 {
                a
            } else {
                let (mut lo, mut hi) = (0.0, 1.0);
                for _ in 0..40 {
                    let mid = f64::midpoint(lo, hi);
                    if a.lerp(&b, mid).height(p).signum() == ha.signum() {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                a.lerp(&b, f64::midpoint(lo, hi))
            };
            let d = p - s.centre;
            let found = Located {
                region: self.region,
                t: s.t,
                q: DVec2::new(d.dot(s.across), d.dot(s.through)),
                radii: s.radii,
            };
            if best.is_none_or(|b| found.level() < b.level()) {
                best = Some(found);
            }
        }
        best
    }
}

/// The signed distance from `q` to the ellipse of semi-axes `r` (negative inside): the nearest of 720 points on the
/// ellipse, refined by golden-section search around it.
fn ellipse_distance(q: DVec2, r: DVec2) -> f64 {
    let at = |a: f64| DVec2::new(r.x * a.cos(), r.y * a.sin()).distance(q);
    let n = 720;
    let step = std::f64::consts::TAU / f64::from(n);
    let start = (0..n).map(|k| f64::from(k) * step).min_by(|a, b| at(*a).total_cmp(&at(*b))).unwrap_or(0.0);
    let (mut lo, mut hi) = (start - step, start + step);
    let g = 0.5 * (5.0_f64.sqrt() - 1.0);
    for _ in 0..60 {
        let (c, d) = (hi - g * (hi - lo), lo + g * (hi - lo));
        if at(c) < at(d) {
            hi = d;
        } else {
            lo = c;
        }
    }
    let dist = at(f64::midpoint(lo, hi));
    if (q / r).length() < 1.0 { -dist } else { dist }
}

// ------------------------------------------------------------------------------------------------------- checks

/// Every vertex is outside the regions of the body fit the garment covers (`body`, their tables) and no triangle reaches
/// them ([`triangle_reach`]); every vertex is at least half the ease outside the fit under the garment, measured on the
/// covered region it lies deepest in. Returns the least clearance, metres. The rest of the body is the body-mesh
/// check's ([`check_body_mesh`]).
fn check_clearance(
    output: &PartOutput,
    body: &Tables,
    pieces: &[(BodyRegion, Vec<Piece>)],
    inner: &Tables,
    covered: &[BodyRegion],
    ease: f64,
) -> Result<f64, String> {
    let mut least = f64::INFINITY;
    let mut located = 0_usize;
    for mesh in &output.meshes {
        let g = &mesh.geometry;
        let p = |i: u32| g.positions[i as usize].as_dvec3();
        for (k, v) in g.positions.iter().enumerate() {
            let v = v.as_dvec3();
            inside_body(body, v).map_or(Ok(()), |l| {
                Err(format!(
                    "vertex {k} of `{}` at {v} is inside the body: region {:?} at t = {:.3}, depth {:.2} mm",
                    mesh.name,
                    l.region,
                    l.t,
                    -l.distance() * 1e3
                ))
            })?;
            let on = covered
                .iter()
                .filter_map(|r| inner.region(*r)?.locate(v))
                .min_by(|a, b| a.level().total_cmp(&b.level()));
            if let Some(l) = on {
                located += 1;
                let clearance = l.distance();
                least = least.min(clearance);
                if clearance < 0.5 * ease {
                    return Err(format!(
                        "vertex {k} of `{}` at {v} is {:.2} mm from the fit under it (region {:?} at t = {:.3}, depth \
                         {:.2} mm below half the ease of {:.1} mm)",
                        mesh.name,
                        clearance * 1e3,
                        l.region,
                        l.t,
                        (0.5 * ease - clearance) * 1e3,
                        ease * 1e3
                    ));
                }
            }
        }
        for (n, tri) in g.indices.chunks_exact(3).enumerate() {
            let [a, b, c] = [p(tri[0]), p(tri[1]), p(tri[2])];
            if let Some(r) = triangle_reach(pieces, [a, b, c]) {
                return Err(format!(
                    "triangle {n} of `{}` reaches the body: region {:?} near t = {:.3}, within {:.3} mm of it",
                    mesh.name,
                    r.region,
                    r.t,
                    r.within * 1e3
                ));
            }
        }
    }
    if located == 0 {
        return Err("no vertex lies over the regions the garment says it covers".to_owned());
    }
    Ok(least)
}

// ----------------------------------------------------------------------------------------------- crossings

/// The most a sub-slab's radii change from one end to the other, metres: a slab between two table rows is split until
/// it is this even, so the convex hull the crossing test uses stays within half of this of the region itself.
const SUB_SLAB_STEP: f64 = 0.0002;
/// The most sub-slabs one slab is split into.
const MAX_SUB_SLABS: usize = 64;
/// The most GJK iterations before the test gives up proving a triangle clear (and reports it, to stay conservative).
const GJK_ITERATIONS: usize = 64;
/// How many times its estimate a sub-slab's margin allows for the fit straying from the straight lerp between its ends
/// ([`stray`]). The estimate is exact for a cubic; the factor covers the rest (a knot inside the sub-slab, a turning
/// frame, `f32` rounding), whose terms are smaller by orders of the sub-slab's length.
const STRAY_SAFETY: f64 = 2.0;

/// A triangle that reaches a region of the body: the region, about where along it, and how near the region's hull it
/// comes (0 when it passes into it), metres.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Reach {
    /// The region.
    pub region: BodyRegion,
    /// About where along it.
    pub t: f64,
    /// An upper bound on the distance between the triangle and the hull it was tested against, metres.
    pub within: f64,
}

/// A convex piece that contains one sub-slab of a region: the convex hull of its two end sections (filled ellipses),
/// and the margin within which the region between them may stand outside that hull.
///
/// The margin has two parts:
/// - **Turning frames.** The union over `s` of the filled ellipses of `lerp(e0, e1, s)` (centre and radii lerped, the
///   frames normalised lerps): with fixed frames each such point is the same convex combination of points of the two
///   end ellipses, and lies in their hull. The frames turn by at most the angle between the ends, so a point lies
///   within `rx·|A0 - A1| + ry·|B0 - B1|` of the hull (`A`, `B` the across and through directions, `rx`, `ry` the larger
///   radii).
/// - **The fit's own curve** ([`stray`]). The fit's sections are not that lerp: a body's profiles are cubic in `t`, so
///   between two sampled sections a waist or a chest bulges past the straight line between them. This part bounds how
///   far.
struct Piece {
    ends: [Slice; 2],
    margin: f64,
    bounds: (DVec3, DVec3),
}

impl Piece {
    fn new(e0: Slice, e1: Slice, stray: f64) -> Self {
        let r = e0.radii.max(e1.radii);
        let margin = r.x * e0.across.distance(e1.across) + r.y * e0.through.distance(e1.through) + stray;
        let extent = |e: &Slice| {
            let along = |k: usize| {
                let (a, b) = (e.across.to_array()[k] * e.radii.x, e.through.to_array()[k] * e.radii.y);
                a.hypot(b)
            };
            DVec3::new(along(0), along(1), along(2))
        };
        let (x0, x1) = (extent(&e0), extent(&e1));
        let lo = (e0.centre - x0).min(e1.centre - x1) - DVec3::splat(margin);
        let hi = (e0.centre + x0).max(e1.centre + x1) + DVec3::splat(margin);
        Self { ends: [e0, e1], margin, bounds: (lo, hi) }
    }

    /// The point of the hull farthest along `d`.
    fn support(&self, d: DVec3) -> DVec3 {
        let ellipse = |e: &Slice| {
            let (a, b) = (e.radii.x * d.dot(e.across), e.radii.y * d.dot(e.through));
            let n = a.hypot(b);
            if n > 0.0 { e.centre + e.across * (e.radii.x * a / n) + e.through * (e.radii.y * b / n) } else { e.centre }
        };
        let (p, q) = (ellipse(&self.ends[0]), ellipse(&self.ends[1]));
        if p.dot(d) >= q.dot(d) { p } else { q }
    }
}

/// A bound on `max |e(s)|` over `0..=1` for a function with `e(0) = e(1) = 0`, from its values at `s` = 1/4, 1/2 and
/// 3/4.
///
/// Such a cubic is `e(s) = s(1 - s)(α + βs)`: the quarter values give `α = 8e₁ - 8e₃/3` and `α + β = 8e₃ - 8e₁/3`, and
/// as `s(1 - s) ≤ 1/4` and `α + βs` is linear, `|e| ≤ max(|α|, |α + β|) / 4` everywhere. That is exact for the cubic
/// (and tight for a parabola). What the cubic misses shows at `s = 1/2`, where it predicts `2(e₁ + e₃)/3`; twice that
/// residual is added.
fn cubic_bound(e1: f64, e2: f64, e3: f64) -> f64 {
    let alpha = 8.0 * e1 - 8.0 * e3 / 3.0;
    let beta_end = 8.0 * e3 - 8.0 * e1 / 3.0;
    let residual = (e2 - 2.0 * (e1 + e3) / 3.0).abs();
    alpha.abs().max(beta_end.abs()) / 4.0 + 2.0 * residual
}

/// [`cubic_bound`] for each component of a vector, combined: a bound on the vector's length.
fn cubic_bound3(e1: DVec3, e2: DVec3, e3: DVec3) -> f64 {
    DVec3::new(cubic_bound(e1.x, e2.x, e3.x), cubic_bound(e1.y, e2.y, e3.y), cubic_bound(e1.z, e2.z, e3.z)).length()
}

/// How far a point of the fit's region between sections `e0` and `e1` may stand from the matching point of
/// `lerp(e0, e1, s)`, metres: the bound the piece's margin adds for the fit's own curve.
///
/// A point of a section is `centre + across·rx·u + through·ry·v` with `u² + v² ≤ 1`, so the fit's point and the lerp's
/// differ by at most `|Δcentre| + rx·|Δacross| + |Δrx| + ry·|Δthrough| + |Δry|`. Each difference is zero at both
/// ends and is bounded over the sub-slab by [`cubic_bound`] from the fit sampled at its quarters. `None` where the fit
/// has no section there.
fn stray(fit: &dyn BodyFit, region: BodyRegion, e0: &Slice, e1: &Slice) -> Option<f64> {
    let differences = [0.25, 0.5, 0.75]
        .into_iter()
        .map(|s| {
            let lerped = e0.lerp(e1, s);
            let real = slice(fit, region, lerped.t)?;
            Some((
                real.centre - lerped.centre,
                real.across - lerped.across,
                real.through - lerped.through,
                real.radii - lerped.radii,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let [(c1, a1, b1, r1), (c2, a2, b2, r2), (c3, a3, b3, r3)] = differences[..] else {
        return None;
    };
    let r = e0.radii.max(e1.radii);
    let bound = cubic_bound3(c1, c2, c3)
        + r.x * cubic_bound3(a1, a2, a3)
        + cubic_bound(r1.x, r2.x, r3.x)
        + r.y * cubic_bound3(b1, b2, b3)
        + cubic_bound(r1.y, r2.y, r3.y);
    Some(STRAY_SAFETY * bound)
}

/// The pieces of `table`, the region of `fit` it samples: every slab between two rows split until its radii change by
/// at most [`SUB_SLAB_STEP`], each sub-slab's ends sampled from the fit itself (not lerped between the rows), and its
/// margin grown by how far the fit strays from the lerp between them ([`stray`]). `None` where the fit has no section.
fn pieces(fit: &dyn BodyFit, table: &RegionTable) -> Option<Vec<Piece>> {
    let mut out = Vec::new();
    for pair in table.rows.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let change = (b.radii - a.radii).abs().max_element();
        #[allow(clippy::cast_sign_loss)] // a small positive ratio, clamped
        let k = ((change / SUB_SLAB_STEP).ceil() as usize).clamp(1, MAX_SUB_SLABS);
        let at = |i: usize| {
            if i == 0 {
                Some(a)
            } else if i == k {
                Some(b)
            } else {
                slice(fit, table.region, a.t + (b.t - a.t) * i as f64 / k as f64)
            }
        };
        let mut start = a;
        for i in 1..=k {
            let end = at(i)?;
            out.push(Piece::new(start, end, stray(fit, table.region, &start, &end)?));
            start = end;
        }
    }
    Some(out)
}

/// The point of triangle `abc` nearest `p` (Ericson, Real-Time Collision Detection 5.1.5).
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

/// The point of the simplex `points` (1 to 4 of them) nearest the origin, and the smallest face of it that holds that
/// point (the next simplex).
fn nearest_on_simplex(points: &[DVec3]) -> (DVec3, Vec<DVec3>) {
    let origin = DVec3::ZERO;
    match points.len() {
        1 => (points[0], points.to_vec()),
        2 => {
            let (a, b) = (points[0], points[1]);
            let ab = b - a;
            let s = if ab.length_squared() > 0.0 { (-a.dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            (a + ab * s, points.to_vec())
        }
        3 => (closest_on_triangle(origin, points[0], points[1], points[2]), points.to_vec()),
        _ => {
            let [a, b, c, d] = [points[0], points[1], points[2], points[3]];
            let faces = [[a, b, c], [a, b, d], [a, c, d], [b, c, d]];
            // Inside the tetrahedron: the origin is on the same side of each face as the opposite corner.
            let side = |[p, q, r]: [DVec3; 3], o: DVec3| {
                let n = (q - p).cross(r - p);
                (n.dot(origin - p), n.dot(o - p))
            };
            let inside = faces.iter().zip([d, c, b, a]).all(|(f, o)| {
                let (x, y) = side(*f, o);
                y != 0.0 && x * y >= 0.0
            });
            if inside {
                return (origin, points.to_vec());
            }
            faces
                .into_iter()
                .map(|[p, q, r]| (closest_on_triangle(origin, p, q, r), vec![p, q, r]))
                .min_by(|x, y| x.0.length_squared().total_cmp(&y.0.length_squared()))
                .unwrap_or((a, vec![a]))
        }
    }
}

/// Whether triangle `tri` comes within `piece.margin` of the piece's hull: GJK on the Minkowski difference. It proves
/// the triangle clear only with a separating plane (a lower bound on the distance above the margin); anything it can't
/// prove clear it reports, with an upper bound on the distance, so the test never misses a crossing.
fn reaches(tri: [DVec3; 3], piece: &Piece) -> Option<f64> {
    let support_tri = |d: DVec3| tri.into_iter().max_by(|x, y| x.dot(d).total_cmp(&y.dot(d))).unwrap_or(tri[0]);
    // A point of the difference `triangle - hull`, and the support of the difference in direction `-v`.
    let mut v = tri[0] - piece.ends[0].centre;
    let mut simplex = vec![v];
    for _ in 0..GJK_ITERATIONS {
        let distance = v.length();
        if distance <= piece.margin {
            return Some(distance);
        }
        let w = support_tri(-v) - piece.support(v);
        // Every point z of the difference has v·z ≥ v·w, so the distance is at least v·w / |v|.
        if v.dot(w) / distance > piece.margin {
            return None;
        }
        if distance * distance - v.dot(w) <= 1e-12 * distance * distance {
            return Some(distance); // converged at the margin: report it
        }
        simplex.push(w);
        (v, simplex) = nearest_on_simplex(&simplex);
    }
    Some(v.length())
}

/// Where triangle `abc` reaches a region of `body` (comes within the margin of a hull containing it), if anywhere.
fn triangle_reach(body: &[(BodyRegion, Vec<Piece>)], [a, b, c]: [DVec3; 3]) -> Option<Reach> {
    let (lo, hi) = (a.min(b).min(c), a.max(b).max(c));
    for (region, pieces) in body {
        for piece in pieces {
            if lo.cmpgt(piece.bounds.1).any() || hi.cmplt(piece.bounds.0).any() {
                continue;
            }
            if let Some(within) = reaches([a, b, c], piece) {
                let t = f64::midpoint(piece.ends[0].t, piece.ends[1].t);
                return Some(Reach { region: *region, t, within });
            }
        }
    }
    None
}

/// The convex pieces of every region of `tables`, the table of `fit`.
fn body_pieces(fit: &dyn BodyFit, tables: &Tables) -> Vec<(BodyRegion, Vec<Piece>)> {
    let pieces = |t: &RegionTable| pieces(fit, t).expect("a fit with sections at its rows has them between the rows");
    tables.0.iter().map(|t| (t.region, pieces(t))).collect()
}

/// How far `p` stands outside every region of `fit` (negative inside one), metres: the least, over the regions whose
/// section planes pass through it, of its distance outside that section's ellipse. `None` where no region's section
/// plane passes through it (past every region's ends).
pub fn outside_fit(tables: &Tables, p: DVec3) -> Option<f64> {
    tables.0.iter().filter_map(|t| t.locate(p)).map(|l| l.distance()).reduce(f64::min)
}

/// Where `p` lies on each region whose section plane passes through it: the region, the fraction along it and how far
/// outside that section's ellipse it is (negative inside), metres.
pub fn located(tables: &Tables, p: DVec3) -> Vec<(BodyRegion, f64, f64)> {
    tables.0.iter().filter_map(|t| t.locate(p)).map(|l| (l.region, l.t, l.distance())).collect()
}

/// Where `p` lies along `region`'s axis: before its first section's plane, between its planes, or past its last.
pub fn along(tables: &Tables, region: BodyRegion, p: DVec3) -> Option<std::cmp::Ordering> {
    let rows = &tables.region(region)?.rows;
    let (first, last) = (rows.first()?, rows.last()?);
    Some(if first.height(p) < 0.0 {
        std::cmp::Ordering::Less
    } else if last.height(p) > 0.0 {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Equal
    })
}

/// Whether `p` is inside any region of `fit`.
pub fn inside(fit: &dyn BodyFit, p: DVec3) -> bool {
    inside_body(&Tables::of(fit), p).is_some()
}

/// Where triangle `tri` reaches a region of `fit`, as the suite's crossing check sees it.
pub fn crossing(fit: &dyn BodyFit, tri: [DVec3; 3]) -> Option<Reach> {
    triangle_reach(&body_pieces(fit, &Tables::of(fit)), tri)
}

/// The region of the body `p` is inside, if any.
fn inside_body(body: &Tables, p: DVec3) -> Option<Located> {
    body.0.iter().filter(|t| t.may_contain(p)).filter_map(|t| t.locate(p)).find(|l| l.level() < 1.0)
}

// --------------------------------------------------------------------------------------------- the body's own mesh

/// The side of a cell of the grid the body's triangles are filed in, metres.
const MESH_CELL: f64 = 0.015;

/// The body's triangles filed by the grid cells their boxes overlap.
struct BodyMesh<'a> {
    triangles: Vec<([DVec3; 3], &'a str, usize)>,
    cells: HashMap<[i64; 3], Vec<usize>>,
}

/// The grid cell of `p`.
fn cell(p: DVec3) -> [i64; 3] {
    (p / MESH_CELL).floor().as_i64vec3().to_array()
}

impl<'a> BodyMesh<'a> {
    fn of(meshes: impl IntoIterator<Item = &'a PartMesh>) -> Self {
        let mut triangles = Vec::new();
        let mut cells: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
        for mesh in meshes {
            let g = &mesh.geometry;
            for (n, tri) in g.indices.chunks_exact(3).enumerate() {
                let t = [0, 1, 2].map(|k| g.positions[tri[k] as usize].as_dvec3());
                let (lo, hi) = (cell(t[0].min(t[1]).min(t[2])), cell(t[0].max(t[1]).max(t[2])));
                for x in lo[0]..=hi[0] {
                    for y in lo[1]..=hi[1] {
                        for z in lo[2]..=hi[2] {
                            cells.entry([x, y, z]).or_default().push(triangles.len());
                        }
                    }
                }
                triangles.push((t, mesh.name.as_str(), n));
            }
        }
        Self { triangles, cells }
    }

    /// The body triangles whose cells `tri`'s box overlaps, each once.
    fn near(&self, tri: &[DVec3; 3]) -> Vec<usize> {
        let (lo, hi) = (cell(tri[0].min(tri[1]).min(tri[2])), cell(tri[0].max(tri[1]).max(tri[2])));
        let mut out = Vec::new();
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    out.extend(self.cells.get(&[x, y, z]).into_iter().flatten().copied());
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// How far `p` lies from the nearest triangle, searching cells out to `limit`; `None` past it.
    fn distance(&self, p: DVec3, limit: f64) -> Option<f64> {
        let mut reach = MESH_CELL;
        loop {
            let r = DVec3::splat(reach);
            let nearest = self
                .near(&[p - r, p + r, p])
                .into_iter()
                .map(|i| {
                    let [a, b, c] = self.triangles[i].0;
                    closest_on_triangle(p, a, b, c).distance(p)
                })
                .fold(f64::INFINITY, f64::min);
            // A triangle beyond the searched box can lie no nearer than its half-width.
            if nearest <= reach {
                return Some(nearest);
            }
            if reach >= limit {
                return None;
            }
            reach = (reach * 2.0).min(limit);
        }
    }
}

/// Whether triangles `a` and `b` share a point: the separating-axis test on their two normals, the nine cross
/// products of their edges and, for triangles in one plane, each edge turned in its normal's plane. Triangles are
/// convex, so they are apart exactly when one of these axes separates them; touching counts as meeting.
fn triangles_meet(a: &[DVec3; 3], b: &[DVec3; 3]) -> bool {
    let edges = |t: &[DVec3; 3]| [t[1] - t[0], t[2] - t[1], t[0] - t[2]];
    let (ea, eb) = (edges(a), edges(b));
    let (na, nb) = (ea[0].cross(ea[1]), eb[0].cross(eb[1]));
    let mut axes = vec![na, nb];
    for u in ea {
        for v in eb {
            axes.push(u.cross(v));
        }
    }
    axes.extend(ea.iter().map(|e| na.cross(*e)));
    axes.extend(eb.iter().map(|e| nb.cross(*e)));
    let span = |t: &[DVec3; 3], axis: DVec3| {
        let d = t.map(|p| p.dot(axis));
        (d[0].min(d[1]).min(d[2]), d[0].max(d[1]).max(d[2]))
    };
    !axes.into_iter().filter(|axis| axis.length_squared() > 1e-30).any(|axis| {
        let ((a0, a1), (b0, b1)) = (span(a, axis), span(b, axis));
        a1 < b0 || b1 < a0
    })
}

/// No triangle of the garment meets a triangle of the body's own mesh. The fit checks measure against the published
/// fit; this measures against the body itself, so a body that stands outside its own fit is caught too.
///
/// # Errors
/// A message naming the first garment triangle that meets the body's mesh, and the body triangle it meets.
pub fn check_body_mesh(output: &PartOutput, body: &PartOutput) -> Result<(), String> {
    let mesh = BodyMesh::of(&body.meshes);
    for garment in &output.meshes {
        let g = &garment.geometry;
        for (n, tri) in g.indices.chunks_exact(3).enumerate() {
            let t = [0, 1, 2].map(|k| g.positions[tri[k] as usize].as_dvec3());
            if let Some(&(_, name, k)) =
                mesh.near(&t).into_iter().map(|i| &mesh.triangles[i]).find(|(b, ..)| triangles_meet(&t, b))
            {
                let centre = (t[0] + t[1] + t[2]) / 3.0;
                return Err(format!(
                    "triangle {n} of `{}` (centred {centre}) passes through the body's own mesh: `{name}` triangle {k}",
                    garment.name
                ));
            }
        }
    }
    Ok(())
}

// --------------------------------------------------------------------------------------------- the fit's tightness

/// How far a section of the body's fit may stand from its own region's mesh, metres. The mesh is lofted from the
/// layout's sections in rings a few centimetres apart, and the fit holds it and rounds its corners, so the two part by
/// a few millimetres: the template's own sections stand up to 3.3 mm off the mesh between its rings, and the fit up to
/// 6.2 mm where the hip's bridged rings end. A fit stretched round a neighbouring limb, or round a cap past a region's
/// end, stands centimetres off its region and fails.
pub const FIT_GAP: f64 = 0.008;

/// Places along each region where its section is measured against its mesh.
const GAP_RINGS: usize = 65;
/// Points round each section measured.
const GAP_POINTS: usize = 48;
/// How far the search for a section point's nearest triangle goes, metres: past it, the gap is reported as at least
/// this.
const GAP_SEARCH: f64 = 0.25;

/// Where a body fit stands farthest from its own region's mesh.
#[derive(Clone, Copy, Debug)]
pub struct Gap {
    pub region: BodyRegion,
    /// Where along the region, `0..=1`.
    pub t: f64,
    /// The point of the section, and how far it lies from the region's mesh, metres.
    pub point: DVec3,
    pub distance: f64,
}

/// Where the sections of `fit`'s `region` stand farthest from that region's own mesh in `body`: [`GAP_POINTS`] points
/// round the section at each of [`GAP_RINGS`] places along the region, each measured to the nearest triangle of the
/// region's mesh (no other region's: a section round two limbs stands off the one it belongs to). `None` when the body
/// has no mesh of the region or the fit no section of it.
pub fn fit_gap(fit: &dyn BodyFit, body: &PartOutput, region: BodyRegion) -> Option<Gap> {
    let name = omuzaki_char::body::mesh::name_of(region);
    let mesh = BodyMesh::of(body.meshes.iter().filter(|m| m.name == name));
    if mesh.triangles.is_empty() {
        return None;
    }
    let mut worst: Option<Gap> = None;
    for i in 0..GAP_RINGS {
        let t = i as f64 / (GAP_RINGS - 1) as f64;
        let slice = slice(fit, region, t)?;
        for j in 0..GAP_POINTS {
            let (sin, cos) = (std::f64::consts::TAU * j as f64 / GAP_POINTS as f64).sin_cos();
            let point = slice.centre + slice.across * (slice.radii.x * cos) + slice.through * (slice.radii.y * sin);
            let distance = mesh.distance(point, GAP_SEARCH).unwrap_or(GAP_SEARCH);
            if worst.is_none_or(|w| distance > w.distance) {
                worst = Some(Gap { region, t, point, distance });
            }
        }
    }
    worst
}

/// Every section of the body fit over `regions` lies within [`FIT_GAP`] of its region's own mesh: the fit is the
/// body's shape, not a hull round more than it.
///
/// # Errors
/// A message naming the region, where along it and how far off its mesh the section stands.
pub fn check_fit_tight(fit: &dyn BodyFit, body: &PartOutput, regions: &[BodyRegion]) -> Result<(), String> {
    for &region in regions {
        if let Some(gap) = fit_gap(fit, body, region)
            && gap.distance > FIT_GAP
        {
            return Err(format!(
                "the body fit's {region:?} section at t = {:.3} stands {:.1} mm off its own mesh at {} (at most {:.1} mm)",
                gap.t,
                gap.distance * 1e3,
                gap.point,
                FIT_GAP * 1e3
            ));
        }
    }
    Ok(())
}

/// Every edge is shared by at most two triangles, wound opposite ways, and the boundary forms `borders` loops in all.
fn check_closure(output: &PartOutput, borders: usize) -> Result<(), String> {
    let mut loops = 0;
    for mesh in &output.meshes {
        loops += boundary_loops(mesh)?;
    }
    if loops == borders {
        Ok(())
    } else {
        Err(format!("the meshes have {loops} open borders; the garment documents {borders}"))
    }
}

/// The number of boundary loops of `mesh` after welding, or why it isn't a manifold surface.
fn boundary_loops(mesh: &PartMesh) -> Result<usize, String> {
    let g = &mesh.geometry;
    let mut ids: HashMap<[i64; 3], usize> = HashMap::new();
    let weld: Vec<usize> = g
        .positions
        .iter()
        .map(|p| {
            let key = [p.x, p.y, p.z].map(|c| (f64::from(c) / WELD).round() as i64);
            let next = ids.len();
            *ids.entry(key).or_insert(next)
        })
        .collect();
    let mut edges: HashMap<(usize, usize), usize> = HashMap::new();
    for tri in g.indices.chunks_exact(3) {
        let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| weld[i as usize]);
        if a == b || b == c || c == a {
            continue;
        }
        for e in [(a, b), (b, c), (c, a)] {
            *edges.entry(e).or_default() += 1;
        }
    }
    let mut next: HashMap<usize, usize> = HashMap::new();
    for (&(a, b), &n) in &edges {
        if n > 1 {
            return Err(format!("`{}`: edge {a}->{b} is used {n} times the same way", mesh.name));
        }
        if !edges.contains_key(&(b, a)) && next.insert(a, b).is_some() {
            return Err(format!("`{}`: two open edges leave vertex {a}: the border is pinched", mesh.name));
        }
    }
    // Walk each border once.
    let mut seen = std::collections::HashSet::new();
    let mut starts: Vec<usize> = next.keys().copied().collect();
    starts.sort_unstable();
    let mut loops = 0;
    for start in starts {
        if !seen.insert(start) {
            continue;
        }
        loops += 1;
        let mut at = next[&start];
        while at != start {
            if !seen.insert(at) {
                return Err(format!("`{}`: an open border runs into another at vertex {at}", mesh.name));
            }
            at = *next.get(&at).ok_or_else(|| format!("`{}`: an open border ends at vertex {at}", mesh.name))?;
        }
    }
    Ok(loops)
}

/// Every mesh validates and every skin joint is in the rig.
fn check_skin(output: &PartOutput, published: &Published) -> Result<(), String> {
    for mesh in &output.meshes {
        mesh.validate().map_err(|e| format!("`{}` is invalid: {e}", mesh.name))?;
        if !mesh.geometry.is_skinned() {
            return Err(format!("`{}` is not skinned", mesh.name));
        }
        for joint in &mesh.joint_bones {
            if published.joint(joint).is_none() {
                return Err(format!("`{}` is skinned to {joint:?}, which the rig doesn't have", mesh.name));
            }
        }
        if mesh.geometry.normals.iter().any(|n| !(n.is_finite() && (n.length() - 1.0).abs() < 1e-3)) {
            return Err(format!("`{}` has a normal that isn't unit length", mesh.name));
        }
    }
    Ok(())
}

/// Every material and texture is named after `part`, and every reference resolves.
fn check_names(output: &PartOutput, part: &str) -> Result<(), String> {
    let prefixed = |name: &str| name == part || name.starts_with(&format!("{part}-"));
    for (name, spec) in &output.materials {
        if !prefixed(name.as_str()) {
            return Err(format!("material `{name}` isn't prefixed by the part id `{part}`"));
        }
        if let Some(texture) = &spec.base_texture
            && !output.textures.iter().any(|(t, _)| t == texture)
        {
            return Err(format!("material `{name}` uses texture `{texture}`, which the part doesn't have"));
        }
    }
    for (name, data) in &output.textures {
        if !prefixed(name.as_str()) {
            return Err(format!("texture `{name}` isn't prefixed by the part id `{part}`"));
        }
        data.validate().map_err(|e| format!("texture `{name}`: {e}"))?;
    }
    for mesh in &output.meshes {
        if !prefixed(&mesh.name) {
            return Err(format!("mesh `{}` isn't prefixed by the part id `{part}`", mesh.name));
        }
        if !output.materials.iter().any(|(m, _)| *m == mesh.material) {
            return Err(format!("mesh `{}` uses material `{}`, which the part doesn't have", mesh.name, mesh.material));
        }
    }
    Ok(())
}

/// Builds the garment three more times: each must equal `output` exactly. Returns the fastest build, milliseconds.
fn check_repeat(
    def: &CharacterDef,
    output: &PartOutput,
    before: &Published,
    services: &BuildServices,
) -> Result<f64, String> {
    let id = PartId::new(PART).unwrap();
    let mut fastest = f64::INFINITY;
    for _ in 0..3 {
        let start = Instant::now();
        let again = build_part(def, &id, &BuildOptions::default(), services, before)
            .map_err(|e| format!("the second build failed: {e}"))?;
        fastest = fastest.min(start.elapsed().as_secs_f64() * 1e3);
        if again.meshes != output.meshes {
            return Err("two builds made different meshes".to_owned());
        }
        if again.materials != output.materials || again.textures != output.textures {
            return Err("two builds made different materials or textures".to_owned());
        }
    }
    Ok(fastest)
}

/// The fit a garment published (its envelope), from what was published after it.
pub fn envelope_of(published: &Published, index: usize) -> Arc<dyn BodyFit> {
    published.get::<FitFact>().fits[index].clone()
}
