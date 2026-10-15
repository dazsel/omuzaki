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
//! - **body**: no vertex and no triangle (its centroid) is inside any region of the body fit;
//! - **closure**: after welding coincident positions every edge is shared by at most two triangles wound opposite ways,
//!   and the boundary edges form exactly [`Garment::open_borders`] loops;
//! - **skin**: every mesh passes `PartMesh::validate` (weights normalised, joints listed) and every joint is in the rig;
//! - **names**: every material and texture is named after the part, and every reference between them resolves;
//! - **determinism**: a second build is byte-identical;
//! - **time**: the build takes at most [`MAX_BUILD_MS`] (asserted in release builds only; measured always).
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
/// Sections per region in the lookup table a vertex is located with.
const TABLE: usize = 256;
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
    def.parts.insert(PartId::new("body").unwrap(), part("body", 0, json!({ "shape": shape })));
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
            let body_table = Tables::of(body.as_ref());
            let inner_table = Tables::of(inner.as_ref());
            let min_clearance = check_clearance(output, &body_table, &inner_table, &regions, ease)
                .map_err(|e| format!("{case}: {e}"))?;
            check_closure(output, K::open_borders(params)).map_err(|e| format!("{case}: {e}"))?;
            check_skin(output, &stack.published).map_err(|e| format!("{case}: {e}"))?;
            check_names(output, PART).map_err(|e| format!("{case}: {e}"))?;
            let build_ms = check_repeat(&def, output, before, services).map_err(|e| format!("{case}: {e}"))?;
            if cfg!(not(debug_assertions)) && build_ms > MAX_BUILD_MS {
                return Err(format!("{case}: the build took {build_ms:.1} ms, over the {MAX_BUILD_MS} ms budget"));
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
struct Tables(Vec<RegionTable>);

impl Tables {
    fn of(fit: &dyn BodyFit) -> Self {
        let tables = REGIONS
            .iter()
            .filter_map(|&region| {
                let rows = (0..=TABLE)
                    .map(|k| {
                        let t = k as f64 / TABLE as f64;
                        let s = fit.section(region, t as f32)?;
                        let axis = s.axis.as_dvec3().normalize();
                        let (across, through) = section_basis(region, s.axis);
                        Some(Slice {
                            t,
                            centre: s.centre.as_dvec3(),
                            axis,
                            across: across.as_dvec3(),
                            through: through.as_dvec3(),
                            radii: s.radii.as_dvec2(),
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
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
            // Bisect within the interval, on the linearly interpolated section.
            let (mut lo, mut hi) = (0.0, 1.0);
            for _ in 0..40 {
                let mid = f64::midpoint(lo, hi);
                if a.lerp(&b, mid).height(p).signum() == ha.signum() {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let s = a.lerp(&b, f64::midpoint(lo, hi));
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

/// Every vertex and triangle centroid is outside the body; every vertex is at least half the ease outside the
/// fit under the garment, measured on the covered region it lies deepest in. Returns the least clearance, metres.
fn check_clearance(
    output: &PartOutput,
    body: &Tables,
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
            let probe = (a + b + c) / 3.0;
            if let Some(l) = inside_body(body, probe) {
                return Err(format!(
                    "triangle {n} of `{}` crosses the body: region {:?} at t = {:.3}, depth {:.2} mm at {probe}",
                    mesh.name,
                    l.region,
                    l.t,
                    -l.distance() * 1e3
                ));
            }
        }
    }
    if located == 0 {
        return Err("no vertex lies over the regions the garment says it covers".to_owned());
    }
    Ok(least)
}

/// The region of the body `p` is inside, if any.
fn inside_body(body: &Tables, p: DVec3) -> Option<Located> {
    body.0.iter().filter(|t| t.may_contain(p)).filter_map(|t| t.locate(p)).find(|l| l.level() < 1.0)
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
