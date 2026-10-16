//! Silhouette fit of body proportions and shape against sheet views (CB-03).
//!
//! This module holds the objective: the CPU orthographic silhouette of the built character, aligned to a target mask
//! at sole and crown, scored by `IoU` and a boundary-distance term. [`solver`] searches the body's parameters with it.
//!
//! Mask pixels are row-major from the top-left. A view is looked at orthographically with the character facing `+z`:
//! front shows body `x` across, side shows `z` across (forward to the right), back shows `-x`.

// Pixel and vertex arithmetic: the masks are at most MAX_SIDE wide, so every index is far inside the casts' ranges.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
#![allow(clippy::many_single_char_names, clippy::similar_names)] // geometry: u, v, x, y, w, h and their neighbours

use omuzaki_char_api::{BodyParams, BodyShape, HandParams, MaterialName, Proportions};
use omuzaki_core::mesh::MeshData;

use crate::body::{hands, junction, mesh};
use crate::kit::KitError;
use crate::template::{Template, registry};

pub mod heatmap;
pub mod ops;
pub mod png;
pub mod solver;

/// The longest side of a mask, in pixels: the caller downsizes a larger sheet view first, so a fit stays cheap.
pub const MAX_SIDE: usize = 512;
/// Weight of the boundary distance (mean chamfer distance over the mask height) against `1 - IoU` in the cost.
const BOUNDARY_WEIGHT: f64 = 1.0;

/// Why a fit cannot run.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum FitError {
    /// No target masks were given.
    #[error("a fit needs at least one target view")]
    NoTargets,
    /// Two targets are of the same view.
    #[error("view {0:?} is given twice")]
    DuplicateView(FitView),
    /// A mask is empty, too large, mis-sized or blank.
    #[error("bad target mask: {0}")]
    BadMask(String),
    /// A fixed mesh has a non-finite position or an index out of range.
    #[error("bad fixed mesh: {0}")]
    BadMesh(String),
    /// The template id is not registered.
    #[error("there is no body template `{0}`")]
    UnknownTemplate(String),
    /// The search order is empty or names a parameter twice.
    #[error("the search order must name each parameter at most once, and at least one")]
    BadOrder,
    /// The body could not be built for some parameters.
    #[error("cannot build the body: {0}")]
    Build(String),
}

impl From<KitError> for FitError {
    fn from(e: KitError) -> Self {
        Self::Build(e.to_string())
    }
}

/// Which side of the character a target mask shows.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FitView {
    /// From the front: body `x` across.
    Front,
    /// From the side: `z` (forward) across.
    Side,
    /// From the back: `-x` across.
    Back,
}

impl FitView {
    /// Where a body-space point falls across the screen.
    fn across(self, p: [f32; 3]) -> f32 {
        match self {
            Self::Front => p[0],
            Self::Side => p[2],
            Self::Back => -p[0],
        }
    }
}

/// A binary mask: `true` is character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Silhouette {
    width: usize,
    height: usize,
    on: Vec<bool>,
}

impl Silhouette {
    /// A mask of `width` by `height` pixels, row-major from the top-left.
    ///
    /// # Errors
    /// [`FitError::BadMask`] if a side is 0 or over [`MAX_SIDE`], `on` is not `width * height` long, or no pixel is on.
    pub fn new(width: usize, height: usize, on: Vec<bool>) -> Result<Self, FitError> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(FitError::BadMask(format!("{width} x {height} is outside 1..={MAX_SIDE} on each side")));
        }
        if on.len() != width * height {
            return Err(FitError::BadMask(format!("{} pixels for {width} x {height}", on.len())));
        }
        if !on.contains(&true) {
            return Err(FitError::BadMask("the mask is blank".to_owned()));
        }
        Ok(Self { width, height, on })
    }

    /// Width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The pixels, row-major from the top-left.
    pub fn pixels(&self) -> &[bool] {
        &self.on
    }

    /// The mask with isolated specks removed: a pixel is character if most of its 3 by 3 neighbourhood is. A speck
    /// outside the figure would otherwise stretch its box (and with it the alignment) and fill the boundary term.
    fn denoised(&self) -> Self {
        let (w, h) = (self.width, self.height);
        let at = |x: isize, y: isize| {
            usize::from(
                x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && self.on[y as usize * w + x as usize],
            )
        };
        let on = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w).cast_signed(), (i / w).cast_signed());
                (-1..=1).map(|dy| (-1..=1).map(|dx| at(x + dx, y + dy)).sum::<usize>()).sum::<usize>() >= 5
            })
            .collect();
        Self { width: w, height: h, on }
    }

    /// Rows and columns that hold character: `(top, bottom + 1, left, right + 1)`.
    fn extent(&self) -> (usize, usize, usize, usize) {
        let (mut t, mut b, mut l, mut r) = (usize::MAX, 0, usize::MAX, 0);
        for i in (0..self.on.len()).filter(|&i| self.on[i]) {
            let (y, x) = (i / self.width, i % self.width);
            (t, b, l, r) = (t.min(y), b.max(y + 1), l.min(x), r.max(x + 1));
        }
        (t, b, l, r)
    }

    /// Character pixels with a background (or the edge) as a 4-neighbour.
    fn boundary(&self) -> Vec<usize> {
        let (w, h) = (self.width, self.height);
        (0..w * h)
            .filter(|&i| {
                let (y, x) = (i / w, i % w);
                self.on[i]
                    && (x == 0
                        || y == 0
                        || x + 1 == w
                        || y + 1 == h
                        || !self.on[i - 1]
                        || !self.on[i + 1]
                        || !self.on[i - w]
                        || !self.on[i + w])
            })
            .collect()
    }
}

/// A target mask and the view it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The view.
    pub view: FitView,
    /// The mask.
    pub mask: Silhouette,
}

/// The parameters a fit moves: the body part's proportions and shape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FitParams {
    /// Plain lengths and widths, `0.5..=2.0`.
    pub proportions: Proportions,
    /// The body-shape axes, `-1..=1`.
    pub shape: BodyShape,
}

/// A projected triangle, in metres across and up.
type Tri = [[f32; 2]; 3];

/// Projects `mesh` into `view`, failing closed on a non-finite position or a stray index.
fn project(view: FitView, mesh: &MeshData) -> Result<Vec<Tri>, FitError> {
    if !mesh.indices.len().is_multiple_of(3) {
        return Err(FitError::BadMesh(format!("{} indices is not whole triangles", mesh.indices.len())));
    }
    if mesh.positions.iter().any(|p| !p.is_finite()) {
        return Err(FitError::BadMesh("a position is not finite".to_owned()));
    }
    let at = |i: u32| {
        mesh.positions
            .get(i as usize)
            .map(|p| [view.across(p.to_array()), p.y])
            .ok_or_else(|| FitError::BadMesh(format!("index {i} is past the {} vertices", mesh.positions.len())))
    };
    mesh.indices.as_chunks::<3>().0.iter().map(|t| Ok([at(t[0])?, at(t[1])?, at(t[2])?])).collect()
}

/// How metres map to pixels: `x = (u - u0) * s`, `y = (v0 - v) * s`.
struct Map {
    s: f32,
    u0: f32,
    v0: f32,
}

/// `(u min, u max, v min, v max)` of `tris`, if any.
fn bounds<'a>(tris: impl IntoIterator<Item = &'a Tri>) -> Option<[f32; 4]> {
    tris.into_iter().flatten().fold(None, |b, p| {
        let [u0, u1, v0, v1] = b.unwrap_or([p[0], p[0], p[1], p[1]]);
        Some([u0.min(p[0]), u1.max(p[0]), v0.min(p[1]), v1.max(p[1])])
    })
}

/// Fills the triangles into a `w` by `h` mask (both windings, pixel centres).
fn raster(tris: &[&[Tri]], map: &Map, w: usize, h: usize) -> Vec<bool> {
    let mut on = vec![false; w * h];
    for tri in tris.iter().flat_map(|t| t.iter()) {
        let p = tri.map(|[u, v]| [(u - map.u0) * map.s, (map.v0 - v) * map.s]);
        let edge = |a: [f32; 2], b: [f32; 2], x: f32, y: f32| (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
        let area = edge(p[0], p[1], p[2][0], p[2][1]);
        if area == 0.0 {
            continue;
        }
        let lo = |f: fn(f32, f32) -> f32, k: usize| f(f(p[0][k], p[1][k]), p[2][k]);
        let x0 = (lo(f32::min, 0) - 0.5).ceil().max(0.0) as usize;
        let x1 = ((lo(f32::max, 0) - 0.5).floor().max(-1.0) + 1.0).min(w as f32) as usize;
        let y0 = (lo(f32::min, 1) - 0.5).ceil().max(0.0) as usize;
        let y1 = ((lo(f32::max, 1) - 0.5).floor().max(-1.0) + 1.0).min(h as f32) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = edge(p[1], p[2], cx, cy) / area;
                let w1 = edge(p[2], p[0], cx, cy) / area;
                if w0 >= 0.0 && w1 >= 0.0 && w0 + w1 <= 1.0 {
                    on[y * w + x] = true;
                }
            }
        }
    }
    on
}

/// Distance in pixels from each pixel to the nearest of `seeds` (a 1 / sqrt 2 chamfer, two passes).
fn distance(seeds: &[usize], w: usize, h: usize) -> Vec<f32> {
    let mut d = vec![f32::MAX / 4.0; w * h];
    for &i in seeds {
        d[i] = 0.0;
    }
    let relax = |d: &mut [f32], i: usize, j: usize, c: f32| d[i] = d[i].min(d[j] + c);
    let (w, h) = (w.cast_signed(), h.cast_signed());
    let step = |d: &mut [f32], x: isize, y: isize, dx: isize, dy: isize, c: f32| {
        let (nx, ny) = (x + dx, y + dy);
        if nx >= 0 && ny >= 0 && nx < w && ny < h {
            relax(d, (y * w + x) as usize, (ny * w + nx) as usize, c);
        }
    };
    for y in 0..h {
        for x in 0..w {
            for (dx, dy, c) in
                [(-1, 0, 1.0), (0, -1, 1.0), (-1, -1, std::f32::consts::SQRT_2), (1, -1, std::f32::consts::SQRT_2)]
            {
                step(&mut d, x, y, dx, dy, c);
            }
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            for (dx, dy, c) in
                [(1, 0, 1.0), (0, 1, 1.0), (1, 1, std::f32::consts::SQRT_2), (-1, 1, std::f32::consts::SQRT_2)]
            {
                step(&mut d, x, y, dx, dy, c);
            }
        }
    }
    d
}

/// A target view prepared once: its alignment, boundary and distance field, and the fixed parts' projection.
struct ViewObjective {
    view: FitView,
    mask: Silhouette,
    extent: (usize, usize, usize, usize),
    boundary: Vec<usize>,
    field: Vec<f32>,
    fixed: Vec<Tri>,
}

impl ViewObjective {
    /// The map that puts the character's crown and sole on the target's top and bottom row, and its box's middle on
    /// the target's, so absolute size and placement drop out.
    fn align(&self, b: [f32; 4]) -> Map {
        let (top, bottom, left, right) = self.extent;
        let s = (bottom - top) as f32 / (b[3] - b[2]).max(f32::EPSILON);
        let centre = (left + right) as f32 / 2.0;
        Map { s, u0: f32::midpoint(b[0], b[1]) - centre / s, v0: b[3] + top as f32 / s }
    }

    /// The pixels the body `tris` and the fixed parts cover, aligned to the target; `None` if there is nothing to draw.
    fn model_pixels(&self, body: &[Tri]) -> Option<Vec<bool>> {
        let b = bounds(self.fixed.iter().chain(body))?;
        Some(raster(&[&self.fixed, body], &self.align(b), self.mask.width, self.mask.height))
    }

    /// `(iou, boundary distance over height)` of the body `tris` plus the fixed parts against the target.
    fn score(&self, body: &[Tri]) -> (f64, f64) {
        let (w, h) = (self.mask.width, self.mask.height);
        let Some(on) = self.model_pixels(body) else { return (0.0, 1.0) };
        let (mut inter, mut union) = (0usize, 0usize);
        for (&a, &t) in on.iter().zip(&self.mask.on) {
            inter += usize::from(a && t);
            union += usize::from(a || t);
        }
        let iou = inter as f64 / union as f64;
        let own = Silhouette { width: w, height: h, on };
        let seeds = own.boundary();
        let mean =
            |of: &[usize], field: &[f32]| of.iter().map(|&i| f64::from(field[i])).sum::<f64>() / of.len().max(1) as f64;
        let near = distance(&seeds, w, h);
        let d = f64::midpoint(mean(&seeds, &self.field), mean(&self.boundary, &near));
        (iou, d / h as f64)
    }
}

/// How well a candidate matches the targets.
#[derive(Clone, Debug)]
pub(crate) struct Score {
    /// What the search minimises: `1 - IoU` plus the boundary term, averaged over the views.
    pub cost: f64,
    /// Mean `IoU` over the views.
    pub iou: f64,
    /// `IoU` per view, in the order of the targets.
    pub per_view: Vec<f64>,
}

/// One view's body mask and target mask, the same size and aligned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewMasks {
    /// The view.
    pub view: FitView,
    /// The silhouette of the body (and the fixed parts) of the parameters.
    pub model: Silhouette,
    /// The target the fit compares it with (the denoised mask).
    pub target: Silhouette,
}

/// The body of `params` against each of `targets`, as the fit sees them (see [`ViewMasks`]).
///
/// # Errors
/// As [`solver::fit`]; also [`FitError::BadMesh`] if the body shows nothing in a view.
pub fn view_masks(
    template: &str,
    targets: &[Target],
    fixed: &[MeshData],
    params: &FitParams,
) -> Result<Vec<ViewMasks>, FitError> {
    Objective::new(template, targets, fixed)?.masks(params)
}

/// Everything a fit evaluates a candidate against.
pub(crate) struct Objective {
    template: &'static dyn Template,
    views: Vec<ViewObjective>,
}

impl Objective {
    /// Prepares `targets` and the projection of the parts that stay fixed.
    pub(crate) fn new(template: &str, targets: &[Target], fixed: &[MeshData]) -> Result<Self, FitError> {
        let template = registry::find(template).ok_or_else(|| FitError::UnknownTemplate(template.to_owned()))?;
        if targets.is_empty() {
            return Err(FitError::NoTargets);
        }
        let mut views: Vec<ViewObjective> = Vec::new();
        for t in targets {
            if views.iter().any(|v| v.view == t.view) {
                return Err(FitError::DuplicateView(t.view));
            }
            let mask = t.mask.denoised();
            if !mask.on.contains(&true) {
                return Err(FitError::BadMask("nothing but specks".to_owned()));
            }
            let boundary = mask.boundary();
            let field = distance(&boundary, mask.width, mask.height);
            let mut tris = Vec::new();
            for m in fixed {
                tris.extend(project(t.view, m)?);
            }
            views.push(ViewObjective { view: t.view, extent: mask.extent(), mask, boundary, field, fixed: tris });
        }
        Ok(Self { template, views })
    }

    /// The targets' views, in order.
    pub(crate) fn views(&self) -> impl Iterator<Item = FitView> + '_ {
        self.views.iter().map(|v| v.view)
    }

    /// For each view, the body of `params` and the (denoised) target the fit compares it with, aligned and rasterised
    /// exactly as the score does: what the difference heatmap draws.
    pub(crate) fn masks(&self, params: &FitParams) -> Result<Vec<ViewMasks>, FitError> {
        let meshes = body_geometry(self.template, params)?;
        self.views
            .iter()
            .map(|v| {
                let tris = meshes.iter().map(|m| project(v.view, m)).collect::<Result<Vec<_>, _>>()?.concat();
                let on = v.model_pixels(&tris).ok_or_else(|| FitError::BadMesh("nothing to draw".to_owned()))?;
                let model = Silhouette { width: v.mask.width, height: v.mask.height, on };
                Ok(ViewMasks { view: v.view, model, target: v.mask.clone() })
            })
            .collect()
    }

    /// Scores a body of `params`.
    pub(crate) fn evaluate(&self, params: &FitParams) -> Result<Score, FitError> {
        let meshes = body_geometry(self.template, params)?;
        let mut per_view = Vec::with_capacity(self.views.len());
        let mut cost = 0.0;
        for v in &self.views {
            let tris = meshes.iter().map(|m| project(v.view, m)).collect::<Result<Vec<_>, _>>()?.concat();
            let (iou, edge) = v.score(&tris);
            cost += 1.0 - iou + BOUNDARY_WEIGHT * edge;
            per_view.push(iou);
        }
        let n = per_view.len() as f64;
        Ok(Score { cost: cost / n, iou: per_view.iter().sum::<f64>() / n, per_view })
    }
}

/// The geometry of the body part for `params`: what `Body::build` makes, without the skin that a silhouette never reads.
fn body_geometry(template: &dyn Template, params: &FitParams) -> Result<Vec<MeshData>, FitError> {
    let hand = HandParams::default();
    let layout = template.try_layout(&params.proportions, &params.shape, &hand)?;
    let detail = mesh::Detail::LIVE;
    let mut rings = mesh::REGIONS
        .iter()
        .map(|&r| mesh::sample_rings(&layout, r, detail.rows(r)).map(|rs| (r, rs)))
        .collect::<Result<Vec<_>, _>>()?;
    junction::shape(&mut rings, (detail.arm_rows / 3).max(3), (detail.leg_rows / 3).max(6));
    let mut out = Vec::new();
    for (region, rings) in rings {
        let (start, end) = mesh::caps_of(region, &rings, detail.cap_rings);
        let (grid, centres) = mesh::ring_grid(&rings, start, end, detail.around)?;
        out.push(mesh::surface(&grid, &centres)?);
    }
    // The build scales the body to the character's stature before it builds anything else, so the parts that stay
    // fixed are in metres: the candidate body goes to metres too, or the two would not fit together.
    let scale = template.stature(&layout).map_or(1.0, |s| s.scale);
    let material = MaterialName::new("body-skin").map_err(|e| FitError::Build(e.to_string()))?;
    let body = BodyParams {
        proportions: params.proportions.clone(),
        shape: params.shape.clone(),
        hand,
        ..BodyParams::default()
    };
    out.extend(hands::build(&body, &layout, &material)?.into_iter().map(|m| m.geometry));
    for m in &mut out {
        m.positions.iter_mut().for_each(|p| *p *= scale);
    }
    Ok(out)
}

/// A synthetic target: the silhouette of the body of `params` and the `fixed` parts in `view`, `height` pixels tall
/// with a 4 px margin. For tests and tools that need a target of a known truth.
///
/// # Errors
/// [`FitError::BadMask`] if `height` is not in `9..=`[`MAX_SIDE`] (checked before the body is built) or the figure is
/// wider than [`MAX_SIDE`] pixels (checked once its extent is known), either way before the mask is allocated;
/// otherwise [`FitError`] if the template is unknown, the body cannot be built or a mesh
/// is bad.
pub fn render_target(
    template: &str,
    params: &FitParams,
    fixed: &[MeshData],
    view: FitView,
    height: usize,
) -> Result<Silhouette, FitError> {
    const MARGIN: f32 = 4.0;
    if height <= 2 * MARGIN as usize || height > MAX_SIDE {
        return Err(FitError::BadMask(format!("{height} rows is outside {}..={MAX_SIDE}", 2 * MARGIN as usize + 1)));
    }
    let template = registry::find(template).ok_or_else(|| FitError::UnknownTemplate(template.to_owned()))?;
    let mut tris = Vec::new();
    for m in body_geometry(template, params)?.iter().chain(fixed) {
        tris.extend(project(view, m)?);
    }
    let b = bounds(&tris).ok_or_else(|| FitError::BadMesh("nothing to draw".to_owned()))?;
    let s = (height as f32 - 2.0 * MARGIN) / (b[3] - b[2]).max(f32::EPSILON);
    let wide = ((b[1] - b[0]) * s + 2.0 * MARGIN).ceil();
    if !(wide >= 1.0 && wide <= MAX_SIDE as f32) {
        return Err(FitError::BadMask(format!("the figure is {wide} px wide, over {MAX_SIDE}")));
    }
    let width = wide as usize;
    let map = Map { s, u0: b[0] - MARGIN / s, v0: b[3] + MARGIN / s };
    Silhouette::new(width, height, raster(&[&tris], &map, width, height))
}
