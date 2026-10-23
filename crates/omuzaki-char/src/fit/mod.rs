//! Silhouette fit of body proportions and shape against sheet views (CB-03).
//!
//! This module holds the objective: the CPU orthographic silhouette of the built character, aligned to a target mask
//! at sole and crown and across by its mean column, and scored by `IoU`. [`solver`] searches the body's parameters
//! with it.
//!
//! **Two `IoU`s.** The number reported is the `IoU` of the centre-sampled silhouette, sampled as the target is. The
//! cost is `1 -` the `IoU` of each pixel's coverage from 2 by 2 samples, plus #159's boundary-distance term (a
//! symmetric chamfer distance between the centre-sampled outlines, over the mask height), which keeps two bodies that
//! overlap the target equally apart when their outlines lie at different distances from it. The centre-sampled `IoU`
//! alone moves in whole-pixel steps, and a line search stalls on its plateaus; the coverage `IoU` changes smoothly.
//!
//! **Alignment.** Matching the model's mean column to the target's (rather than the box's middle, whose ends are
//! whole pixels) keeps the true body at an `IoU` of about 0.997 against its own 256-row silhouettes; the box alone left
//! it up to half a pixel off sideways, 0.981 on `standard` with mitten hands, below the recovery test's 0.985.
//!
//! Mask pixels are row-major from the top-left. A view is looked at orthographically with the character facing `+z`:
//! front shows body `x` across, side shows `z` across (forward to the right), back shows `-x`.

// Pixel and vertex arithmetic: the masks are at most MAX_SIDE wide, so every index is far inside the casts' ranges.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
#![allow(clippy::many_single_char_names, clippy::similar_names)] // geometry: u, v, x, y, w, h and their neighbours

use omuzaki_char_api::{BodyParams, BodyShape, MaterialName, Proportions};
use omuzaki_core::mesh::MeshData;

use crate::body::{hands, junction, mesh};
use crate::kit::KitError;
use crate::template::{Template, registry};

pub mod heatmap;
pub mod input;
pub mod ops;
pub mod solver;

/// The longest side of a mask, in pixels: the caller downsizes a larger sheet view first, so a fit stays cheap.
pub const MAX_SIDE: usize = 512;
/// How many of its 8 neighbours a target pixel must disagree with to count as noise ([`Silhouette::denoised`]).
const SPECK: usize = 7;
/// Weight of the boundary distance (mean chamfer distance over the mask height) against `1 - IoU` in the cost (#159).
const BOUNDARY_WEIGHT: f64 = 1.0;
/// Samples per pixel side for the coverage the cost reads: 2 by 2 makes the cost change smoothly with sub-pixel moves
/// of the outline, which the centre-sampled mask only shows in whole-pixel steps (plateaus a line search stalls on).
const SUPERSAMPLE: usize = 2;

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
    /// The caller's watch stopped the fit before it ended (`solver::fit_from`).
    #[error("the fit was stopped before it ended")]
    Stopped,
    /// A heatmap could not be encoded as a PNG.
    #[error("cannot encode the heatmap: {0}")]
    Encode(String),
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

    /// The mask with isolated specks and pinholes removed: a pixel that disagrees with at least [`SPECK`] of its 8
    /// neighbours (outside the mask counts as background) takes their value. A speck outside the figure would
    /// otherwise stretch its box and shift its mean column (and with them the alignment), and fill the boundary term.
    /// Thin features (a strand of hair, a finger a pixel or two wide) keep at least two neighbours like themselves, so
    /// they stay: a majority filter would erase them from the target while the model still draws them.
    fn denoised(&self) -> Self {
        let (w, h) = (self.width, self.height);
        let at = |x: isize, y: isize| {
            x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && self.on[y as usize * w + x as usize]
        };
        let on = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w).cast_signed(), (i / w).cast_signed());
                let here = self.on[i];
                let unlike = (-1..=1)
                    .flat_map(|dy| (-1..=1).map(move |dx| (dx, dy)))
                    .filter(|&(dx, dy)| (dx, dy) != (0, 0) && at(x + dx, y + dy) != here)
                    .count();
                if unlike >= SPECK { !here } else { here }
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

/// Fills the triangles into a `w` by `h` mask (both windings, pixel centres): for each row of pixel centres a triangle
/// crosses, the span between the two points where the row's centre line meets its edges.
fn raster(tris: &[&[Tri]], map: &Map, w: usize, h: usize) -> Vec<bool> {
    let mut on = vec![false; w * h];
    for tri in tris.iter().flat_map(|t| t.iter()) {
        let p = tri.map(|[u, v]| [(u - map.u0) * map.s, (map.v0 - v) * map.s]);
        let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]);
        if area == 0.0 {
            continue;
        }
        let (ymin, ymax) = (p[0][1].min(p[1][1]).min(p[2][1]), p[0][1].max(p[1][1]).max(p[2][1]));
        let y0 = (ymin - 0.5).ceil().max(0.0) as usize;
        let y1 = ((ymax - 0.5).floor().max(-1.0) + 1.0).min(h as f32) as usize;
        for y in y0..y1 {
            let cy = y as f32 + 0.5;
            let (mut xl, mut xr) = (f32::INFINITY, f32::NEG_INFINITY);
            for (a, b) in [(p[0], p[1]), (p[1], p[2]), (p[2], p[0])] {
                let (da, db) = (a[1] - cy, b[1] - cy);
                if da * db > 0.0 {
                    continue;
                }
                if (da - db).abs() < f32::MIN_POSITIVE {
                    // An edge along the row's centre line: both its ends are on the row.
                    (xl, xr) = (xl.min(a[0]).min(b[0]), xr.max(a[0]).max(b[0]));
                } else {
                    let x = a[0] + da / (da - db) * (b[0] - a[0]);
                    (xl, xr) = (xl.min(x), xr.max(x));
                }
            }
            if xl > xr {
                continue;
            }
            let x0 = (xl - 0.5).ceil().max(0.0) as usize;
            let x1 = ((xr - 0.5).floor().max(-1.0) + 1.0).min(w as f32) as usize;
            if x0 < x1 {
                on[y * w + x0..y * w + x1].fill(true);
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

/// The mean column of `mask`'s character pixels, at pixel centres (the mask is never blank once checked).
fn mean_column(mask: &Silhouette) -> f64 {
    let (mut sum, mut n) = (0.0f64, 0usize);
    for (i, _) in mask.on.iter().enumerate().filter(|(_, on)| **on) {
        sum += (i % mask.width) as f64 + 0.5;
        n += 1;
    }
    sum / n.max(1) as f64
}

/// A target view prepared once: its alignment, boundary and distance field, and the fixed parts' projection.
struct ViewObjective {
    view: FitView,
    mask: Silhouette,
    extent: (usize, usize, usize, usize),
    /// The target's mean column (pixel centres), what the model's own mean column is matched to.
    centroid: f64,
    boundary: Vec<usize>,
    field: Vec<f32>,
    fixed: Vec<Tri>,
}

/// One candidate against one view: what [`ViewObjective::score`] measures.
struct ViewScore {
    /// `IoU` of the centre-sampled model mask with the target: the number reported, sampled as the target is.
    iou: f64,
    /// `IoU` of the model's [`SUPERSAMPLE`]d pixel coverage with the target: what the cost reads, because it changes
    /// smoothly as the body moves by less than a pixel, where the centre-sampled one moves in steps.
    soft_iou: f64,
    /// The mean boundary distance (symmetric chamfer, centre-sampled masks) over the mask height: two bodies that
    /// overlap the target equally but whose outlines lie at different distances from it do not tie.
    edge: f64,
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

    /// The map for the body `tris`: crown and sole on the target's rows (as [`Self::align`]), then shifted across so
    /// the model's mean column, read from its coverage, falls on the target's. Matching the box's middle alone left
    /// the model up to half a pixel off sideways, since the target's extent is whole pixels: on a 256-row view that
    /// cost the true body itself up to 2% of `IoU`. `None` if there is nothing to draw.
    fn aligned_map(&self, body: &[Tri]) -> Option<Map> {
        let b = bounds(self.fixed.iter().chain(body))?;
        let mut map = self.align(b);
        let w = self.mask.width;
        let (mut sum, mut total) = (0.0f64, 0.0f64);
        for (i, &c) in self.coverage(body, &map).iter().enumerate() {
            sum += f64::from(c) * ((i % w) as f64 + 0.5);
            total += f64::from(c);
        }
        if total > 0.0 {
            map.u0 += ((sum / total - self.centroid) as f32) / map.s;
        }
        Some(map)
    }

    /// Each pixel's share covered by the body `tris` and the fixed parts under `map`, from [`SUPERSAMPLE`] by
    /// [`SUPERSAMPLE`] samples, as a count of covered samples.
    fn coverage(&self, body: &[Tri], map: &Map) -> Vec<u16> {
        let (w, h, k) = (self.mask.width, self.mask.height, SUPERSAMPLE);
        let fine = Map { s: map.s * k as f32, u0: map.u0, v0: map.v0 };
        let mut counts = vec![0u16; w * h];
        for (i, _) in raster(&[&self.fixed, body], &fine, w * k, h * k).iter().enumerate().filter(|(_, on)| **on) {
            let (y, x) = (i / (w * k), i % (w * k));
            counts[(y / k) * w + x / k] += 1;
        }
        counts
    }

    /// The pixels the body `tris` and the fixed parts cover, aligned to the target and centre-sampled as the target is;
    /// `None` if there is nothing to draw.
    fn model_pixels(&self, body: &[Tri]) -> Option<Vec<bool>> {
        let map = self.aligned_map(body)?;
        Some(raster(&[&self.fixed, body], &map, self.mask.width, self.mask.height))
    }

    /// The body `tris` plus the fixed parts against the target (see [`ViewScore`]); `None` if there is nothing to draw.
    fn score(&self, body: &[Tri]) -> Option<ViewScore> {
        let map = self.aligned_map(body)?;
        let samples = (SUPERSAMPLE * SUPERSAMPLE) as f64;
        let (mut inter, mut union) = (0.0f64, 0.0f64);
        for (&c, &t) in self.coverage(body, &map).iter().zip(&self.mask.on) {
            let c = f64::from(c) / samples;
            (inter, union) = if t { (inter + c, union + 1.0) } else { (inter, union + c) };
        }
        let (w, h) = (self.mask.width, self.mask.height);
        let on = raster(&[&self.fixed, body], &map, w, h);
        let (mut both, mut either) = (0usize, 0usize);
        for (&a, &t) in on.iter().zip(&self.mask.on) {
            both += usize::from(a && t);
            either += usize::from(a || t);
        }
        let own = Silhouette { width: w, height: h, on };
        let seeds = own.boundary();
        let mean =
            |of: &[usize], field: &[f32]| of.iter().map(|&i| f64::from(field[i])).sum::<f64>() / of.len().max(1) as f64;
        let near = distance(&seeds, w, h);
        let d = f64::midpoint(mean(&seeds, &self.field), mean(&self.boundary, &near));
        Some(ViewScore {
            iou: both as f64 / either.max(1) as f64,
            soft_iou: inter / union.max(f64::MIN_POSITIVE),
            edge: d / h as f64,
        })
    }
}

/// How well a candidate matches the targets.
#[derive(Clone, Debug)]
pub(crate) struct Score {
    /// What the search minimises: `1 -` the coverage `IoU` plus the boundary term, averaged over the views.
    pub cost: f64,
    /// Mean centre-sampled `IoU` over the views.
    pub iou: f64,
    /// `IoU` per view, in the order of the targets.
    pub per_view: Vec<f64>,
}

/// One view's body mask and target mask, the same size and aligned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewMasks {
    /// The view.
    pub view: FitView,
    /// The silhouette of the body (and the fixed and followed parts) of the parameters.
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

/// Builds the parts that move with the body: for a candidate's parameters, their meshes in metres, in the same space as
/// the body's. The fit calls it for every candidate, so a neck, tail or wings built on the body's joints and
/// dimensions follow the candidate instead of staying where the starting body put them.
pub type Follow<'a> = &'a dyn Fn(&FitParams) -> Result<Vec<MeshData>, FitError>;

/// Everything a fit evaluates a candidate against.
pub(crate) struct Objective<'a> {
    template: &'static dyn Template,
    views: Vec<ViewObjective>,
    /// The body every candidate is built from, with the candidate's proportions and shape: its hands stay as they are.
    base: BodyParams,
    /// The parts rebuilt for every candidate, drawn with its body.
    follow: Option<Follow<'a>>,
}

impl<'a> Objective<'a> {
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
            let mut tris = Vec::new();
            for m in fixed {
                tris.extend(project(t.view, m)?);
            }
            let centroid = mean_column(&mask);
            let boundary = mask.boundary();
            let field = distance(&boundary, mask.width, mask.height);
            views.push(ViewObjective {
                view: t.view,
                extent: mask.extent(),
                centroid,
                mask,
                boundary,
                field,
                fixed: tris,
            });
        }
        Ok(Self { template, views, base: BodyParams::default(), follow: None })
    }

    /// The same objective building every candidate with `body`'s hands (its proportions and shape are the candidate's).
    pub(crate) fn with_body(mut self, body: &BodyParams) -> Self {
        self.base = body.clone();
        self
    }

    /// The same objective drawing, with every candidate, the parts `follow` builds for it.
    pub(crate) fn with_follow(mut self, follow: Option<Follow<'a>>) -> Self {
        self.follow = follow;
        self
    }

    /// Whether parts are rebuilt with every candidate ([`with_follow`](Self::with_follow)).
    pub(crate) fn follows(&self) -> bool {
        self.follow.is_some()
    }

    /// The meshes of a candidate: its body, then the parts that follow it.
    fn candidate(&self, params: &FitParams) -> Result<Vec<MeshData>, FitError> {
        let mut meshes = body_geometry(self.template, &self.base, params)?;
        if let Some(follow) = self.follow {
            meshes.extend(follow(params)?);
        }
        Ok(meshes)
    }

    /// The targets' views, in order.
    pub(crate) fn views(&self) -> impl Iterator<Item = FitView> + '_ {
        self.views.iter().map(|v| v.view)
    }

    /// For each view, the body of `params` and the (denoised) target the fit compares it with, aligned and rasterised
    /// exactly as the score does: what the difference heatmap draws.
    pub(crate) fn masks(&self, params: &FitParams) -> Result<Vec<ViewMasks>, FitError> {
        let meshes = self.candidate(params)?;
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
        let meshes = self.candidate(params)?;
        let mut per_view = Vec::with_capacity(self.views.len());
        let mut cost = 0.0;
        for v in &self.views {
            let tris = meshes.iter().map(|m| project(v.view, m)).collect::<Result<Vec<_>, _>>()?.concat();
            let score = v.score(&tris).unwrap_or(ViewScore { iou: 0.0, soft_iou: 0.0, edge: 1.0 });
            cost += 1.0 - score.soft_iou + BOUNDARY_WEIGHT * score.edge;
            per_view.push(score.iou);
        }
        let n = per_view.len() as f64;
        Ok(Score { cost: cost / n, iou: per_view.iter().sum::<f64>() / n, per_view })
    }
}

/// The geometry of the body part `base` with the proportions and shape of `params`: what `Body::build` makes, without
/// the skin that a silhouette never reads, in metres.
fn body_geometry(template: &dyn Template, base: &BodyParams, params: &FitParams) -> Result<Vec<MeshData>, FitError> {
    let body = BodyParams { proportions: params.proportions.clone(), shape: params.shape.clone(), ..base.clone() };
    let layout = template.try_layout(&body.proportions, &body.shape, &body.hand)?;
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
    for m in body_geometry(template, &BodyParams::default(), params)?.iter().chain(fixed) {
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
