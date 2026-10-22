//! Reusable trim features: piping, braid, buttons and rivets (#188), draped chains, medallion clasps, emblem decals and
//! lace edges (#189), placed on a garment's [`Surface`].
//!
//! A garment carries its trims as [`TrimSpec`] values in its parameters, and adds each one with [`add_trim`] after its
//! own surfaces. Each generator is a pure function of the surface and the spec that returns
//! [`MeshBuilder`](crate::kit::grid::MeshBuilder) pieces, skinned like the surface under them (a [`Rail`] carries the
//! surface's weights at each of its points); [`add_trim`] gives the pieces their materials and names them after the part.
//!
//! **Placing.** A trim is placed in the surface's own coordinates: `[t, phi]`, `t` along the region from 0 to 1 and
//! `phi` round it (0 at the front, `pi / 2` on the character's left). A [`Rail`] is a path of such points, lifted off the
//! surface by [`LIFT`] unless it says otherwise, so the trim sits on the garment wherever the garment goes.
//!
//! **Metal.** Gold and other metal trims use the renderer's `band-highlight` program through a [`MetalStyle`]: a hard
//! highlight band fixed in view space, as the prototype's gold buttons and brass have.

pub mod braid;
pub mod buttons;
pub mod chain;
pub mod clasp;
pub mod decal;
pub mod lace;
pub mod piping;

use omuzaki_char_api::{BuildCtx, BuildError, JointRef, MaterialClass, MaterialName, MaterialSpec};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DVec2, DVec3, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::{ParamValue, ProgramParams, Shading};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{GarmentError, GarmentOutput};
use super::loft::Surface;
use crate::kit::KitError;
use crate::kit::curve::{TAU, fraction};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};

/// How far a rail sits off the surface unless it says otherwise, metres (the robe's `LIFT`).
pub const LIFT: f64 = 0.0025;
/// The most a rail may be lifted, metres.
pub const MAX_LIFT: f64 = 0.02;
/// The most points a rail's path may have.
pub const MAX_RAIL_POINTS: usize = 64;
/// The most samples a rail may have.
pub const MAX_RAIL_SAMPLES: usize = 4096;
/// The prototype's rail step, metres: a sample about this often.
pub const RAIL_STEP: f64 = 0.004;
/// Chords each step of a rail's path is measured with (the prototype's).
const MEASURE_STEPS: usize = 10;
/// A rail shorter than this is no rail, metres.
pub const MIN_RAIL_LENGTH: f64 = 1e-4;
/// The smallest size of any trim feature (a width, a radius, a link), metres.
pub const MIN_FEATURE: f64 = 0.0002;
/// The largest size of any trim feature, metres.
pub const MAX_FEATURE: f64 = 0.2;
/// The name of the renderer's metal highlight program.
pub const METAL_PROGRAM: &str = "band-highlight";

/// Why a trim could not be built.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TrimError {
    /// The rail has no length along the surface: its points coincide.
    #[error("the rail has no length (under {MIN_RAIL_LENGTH} m along the surface): give it points that differ")]
    ZeroLengthRail,
    /// A number or choice the trim can't use.
    #[error("{what}: {message}")]
    Invalid {
        /// What was wrong.
        what: &'static str,
        /// Why, and what would work.
        message: String,
    },
    /// The surface refused a point.
    #[error(transparent)]
    Garment(#[from] GarmentError),
    /// The geometry kit refused the shape.
    #[error(transparent)]
    Kit(#[from] KitError),
}

impl TrimError {
    /// A [`TrimError::Invalid`].
    pub fn invalid(what: &'static str, message: impl Into<String>) -> Self {
        Self::Invalid { what, message: message.into() }
    }
}

/// Checks `value` is a feature size in [`MIN_FEATURE`]..=[`MAX_FEATURE`].
pub(crate) fn feature(c: &mut Check, name: &str, value: f64) {
    c.range(name, value, MIN_FEATURE, MAX_FEATURE);
}

/// Checks a surface position `[t, phi]`: `t` in `0..=1`, `phi` finite.
pub(crate) fn position(c: &mut Check, name: &str, at: [f64; 2]) {
    c.range(&format!("{name}/0"), at[0], 0.0, 1.0);
    if !at[1].is_finite() {
        c.fail(&format!("{name}/1"), format!("{} must be finite", at[1]));
    }
}

/// `spec` validated, or the first problem as a [`TrimError::Invalid`].
pub(crate) fn checked(spec: &impl Validate) -> Result<(), TrimError> {
    spec.validate().map_err(|e| TrimError::invalid("trim", e.to_string()))
}

// ------------------------------------------------------------------------------------------------------------ rails

/// A path on a garment's surface, as parameters store it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RailSpec {
    /// The path's points as `[t, phi]` on the surface. Each step between two points goes the short way round.
    pub points: Vec<[f64; 2]>,
    /// Whether the path returns from its last point to its first (a hem round the garment).
    #[serde(default)]
    pub closed: bool,
    /// How far the path sits off the surface, metres.
    #[serde(default = "default_lift")]
    pub lift: f64,
    /// About how far apart its samples are, metres (the prototype's rail step).
    #[serde(default = "default_step")]
    pub step: f64,
}

fn default_lift() -> f64 {
    LIFT
}

fn default_step() -> f64 {
    RAIL_STEP
}

impl RailSpec {
    /// A rail through `points`, lifted and sampled as the prototype's are.
    pub fn new(points: &[[f64; 2]], closed: bool) -> Self {
        Self { points: points.to_vec(), closed, lift: LIFT, step: RAIL_STEP }
    }
}

impl Validate for RailSpec {
    fn check(&self, c: &mut Check) {
        let need = if self.closed { 3 } else { 2 };
        if !(need..=MAX_RAIL_POINTS).contains(&self.points.len()) {
            c.fail("points", format!("{} points: a rail has {need} to {MAX_RAIL_POINTS}", self.points.len()));
        }
        for (k, p) in self.points.iter().enumerate() {
            position(c, &format!("points/{k}"), *p);
        }
        c.range("lift", self.lift, 0.0, MAX_LIFT);
        c.range("step", self.step, MIN_FEATURE, MAX_FEATURE);
    }
}

/// One sample of a [`Rail`]: a point lifted off the surface with its frame and skin.
#[derive(Clone, Debug, PartialEq)]
pub struct RailPoint {
    /// The point, `lift` off the surface.
    pub point: DVec3,
    /// The point on the surface under it.
    pub on_surface: DVec3,
    /// The surface's outward normal under it.
    pub normal: DVec3,
    /// The unit tangent along the rail, square to `normal`.
    pub tangent: DVec3,
    /// `tangent x normal`: across the rail, in the surface.
    pub side: DVec3,
    /// Where it is on the surface.
    pub t: f64,
    /// The angle round the surface.
    pub phi: f64,
    /// Its distance from the start along the surface points, metres (the strip's texture runs by it).
    pub distance: f64,
    /// The skin of the surface under it.
    pub skin: Vec<(JointRef, f32)>,
}

/// A path on a surface, densified as the prototype's `rail(S, pts, step)`: each step of the path is cut into
/// `max(2, ceil(length / step))` pieces uniform in `[t, phi]` (not in length), the joints between steps not repeated. A
/// closed rail's last sample repeats its first exactly, so a sweep along it wraps.
#[derive(Clone, Debug, PartialEq)]
pub struct Rail {
    points: Vec<RailPoint>,
    closed: bool,
    length: f64,
}

/// The step from `a` to `b` in `[t, phi]`, the short way round in `phi`.
fn step(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    let turn = std::f64::consts::TAU;
    let dphi = (b[1] - a[1] + std::f64::consts::PI).rem_euclid(turn) - std::f64::consts::PI;
    [b[0] - a[0], dphi]
}

/// `[t, phi]` a fraction `s` of the way from `a` to `b`.
fn mix(a: [f64; 2], b: [f64; 2], s: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s]
}

/// The path of `spec` in surface coordinates, each step unwrapped to go the short way round, closed if it says so.
fn unwrapped(spec: &RailSpec) -> Vec<[f64; 2]> {
    let mut path = vec![spec.points[0]];
    for &next in spec.points.iter().skip(1).chain(spec.closed.then_some(&spec.points[0])) {
        let last = path[path.len() - 1];
        let d = step(last, next);
        path.push([last[0] + d[0], last[1] + d[1]]);
    }
    path
}

/// The surface point at `at` (its `t` clamped to the surface).
fn surface_point(surface: &Surface, at: [f64; 2]) -> Result<DVec3, TrimError> {
    Ok(surface.point(at[0].clamp(0.0, 1.0), at[1])?)
}

/// The surface position `distance` metres from `at` in the direction `dir` (a unit vector in the surface's tangent
/// plane), to first order: the step split along the surface's two coordinate directions and divided by how fast the
/// surface moves along each.
///
/// # Errors
/// The surface's errors; [`TrimError::Invalid`] where the surface doesn't move along a coordinate.
pub fn param_offset(surface: &Surface, at: [f64; 2], dir: DVec3, distance: f64) -> Result<[f64; 2], TrimError> {
    let frame = surface.at(at[0].clamp(0.0, 1.0), at[1])?;
    let (per_t, per_phi) = rates(surface, at)?;
    Ok([at[0] + distance * dir.dot(frame.along) / per_t, at[1] + distance * dir.dot(frame.around) / per_phi])
}

/// How fast the surface moves at `at`, metres per unit of `t` and per radian of `phi`: central differences (one-sided
/// at the ends of the region).
///
/// # Errors
/// The surface's errors; [`TrimError::Invalid`] where the surface doesn't move along a coordinate.
pub fn rates(surface: &Surface, at: [f64; 2]) -> Result<(f64, f64), TrimError> {
    const H: f64 = 1e-4;
    let (t0, t1) = ((at[0] - H).max(0.0), (at[0] + H).min(1.0));
    let per_t = surface_point(surface, [t1, at[1]])?.distance(surface_point(surface, [t0, at[1]])?) / (t1 - t0);
    let per_phi =
        surface_point(surface, [at[0], at[1] + H])?.distance(surface_point(surface, [at[0], at[1] - H])?) / (2.0 * H);
    if !(per_t > 0.0 && per_phi > 0.0) {
        return Err(TrimError::invalid("surface", format!("it doesn't move at t = {}, phi = {}", at[0], at[1])));
    }
    Ok((per_t, per_phi))
}

/// The length on the surface of the step from `a` to `b`, from [`MEASURE_STEPS`] chords (the prototype's estimate).
fn step_length(surface: &Surface, a: [f64; 2], b: [f64; 2]) -> Result<f64, TrimError> {
    let mut length = 0.0;
    let mut previous = surface_point(surface, a)?;
    for i in 1..=MEASURE_STEPS {
        let next = surface_point(surface, mix(a, b, fraction(i, MEASURE_STEPS)))?;
        length += next.distance(previous);
        previous = next;
    }
    Ok(length)
}

impl Rail {
    /// The rail `spec` describes on `surface`, densified at `step` metres ([`RAIL_STEP`] is the prototype's; its
    /// collar piping uses 0.006). Each sample is the surface point under it lifted by `spec.lift` along the normal.
    ///
    /// # Errors
    /// [`TrimError::ZeroLengthRail`] if the path has no length on the surface; [`TrimError::Invalid`] for a spec
    /// [`RailSpec`]'s validation refuses, a step that isn't a positive length, or a rail denser than
    /// [`MAX_RAIL_SAMPLES`]; the surface's errors for a point it can't place.
    pub fn from_polyline_on_surface(surface: &Surface, spec: &RailSpec, step: f64) -> Result<Self, TrimError> {
        checked(spec)?;
        if !(step.is_finite() && step > 0.0) {
            return Err(TrimError::invalid("rail", format!("a step of {step} m: use a positive length")));
        }
        let path = unwrapped(spec);
        let mut params: Vec<[f64; 2]> = Vec::new();
        for pair in path.windows(2) {
            let length = step_length(surface, pair[0], pair[1])?;
            if length < MIN_RAIL_LENGTH * 1e-3 {
                continue; // a repeated point: nothing to lay along
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // clamped
            let pieces = (length / step).ceil().clamp(2.0, MAX_RAIL_SAMPLES as f64) as usize;
            let first = usize::from(!params.is_empty());
            params.extend((first..=pieces).map(|i| mix(pair[0], pair[1], fraction(i, pieces))));
            if params.len() > MAX_RAIL_SAMPLES {
                return Err(TrimError::invalid(
                    "rail",
                    format!("more than {MAX_RAIL_SAMPLES} samples: use a longer step or a shorter path"),
                ));
            }
        }
        if params.len() < 2 {
            return Err(TrimError::ZeroLengthRail);
        }
        let mut points = Vec::with_capacity(params.len());
        let mut length = 0.0;
        for at in params {
            let frame = surface.at(at[0].clamp(0.0, 1.0), at[1])?;
            if let Some(previous) = points.last().map(|p: &RailPoint| p.on_surface) {
                length += frame.point.distance(previous);
            }
            points.push(RailPoint {
                point: frame.point + frame.normal * spec.lift,
                on_surface: frame.point,
                normal: frame.normal,
                tangent: DVec3::ZERO,
                side: DVec3::ZERO,
                t: at[0],
                phi: at[1],
                distance: length,
                skin: surface.weights().at(at[0].clamp(0.0, 1.0)),
            });
        }
        if !(length.is_finite() && length >= MIN_RAIL_LENGTH) {
            return Err(TrimError::ZeroLengthRail);
        }
        if spec.closed {
            // The last sample is the first again, a turn round: copy it, so the seam welds exactly.
            let first = points[0].clone();
            let last = points.len() - 1;
            points[last] = RailPoint { distance: length, ..first };
        }
        frame_points(&mut points, spec.closed)?;
        Ok(Self { points, closed: spec.closed, length })
    }

    /// The length of `spec`'s path on `surface` as the prototype estimates it before densifying, metres: for choosing a
    /// step from a count (a braid's lobes, a lace's scallops).
    ///
    /// # Errors
    /// As [`Rail::from_polyline_on_surface`].
    pub fn length_on(surface: &Surface, spec: &RailSpec) -> Result<f64, TrimError> {
        checked(spec)?;
        let path = unwrapped(spec);
        let mut length = 0.0;
        for pair in path.windows(2) {
            length += step_length(surface, pair[0], pair[1])?;
        }
        if length < MIN_RAIL_LENGTH {
            return Err(TrimError::ZeroLengthRail);
        }
        Ok(length)
    }

    /// The samples, from the start.
    pub fn points(&self) -> &[RailPoint] {
        &self.points
    }

    /// Whether the rail is a loop.
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// Its length along the surface, metres.
    pub fn length(&self) -> f64 {
        self.length
    }
}

/// Fills in each point's tangent (a central difference, one-sided at the ends of an open rail) and side.
fn frame_points(points: &mut [RailPoint], closed: bool) -> Result<(), TrimError> {
    let n = points.len();
    let last = n - 1;
    let mut tangents = Vec::with_capacity(n);
    for k in 0..n {
        let (before, after) = match (k, closed) {
            (0, true) => (last - 1, 1),
            (k, true) if k == last => (last - 1, 1),
            (0, false) => (0, 1),
            (k, false) if k == last => (last - 1, last),
            (k, _) => (k - 1, k + 1),
        };
        tangents.push(points[after].point - points[before].point);
    }
    for (p, d) in points.iter_mut().zip(tangents) {
        let tangent = (d - p.normal * d.dot(p.normal)).try_normalize();
        let Some(tangent) = tangent else {
            return Err(TrimError::invalid(
                "rail",
                format!("it doubles back on itself at t = {}, phi = {}", p.t, p.phi),
            ));
        };
        p.tangent = tangent;
        p.side = tangent.cross(p.normal);
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------------------ metal

/// How a metal trim catches the light: the parameters of the renderer's `band-highlight` program (defaults are the
/// prototype's gold).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct MetalStyle {
    /// The highlight's colour.
    pub highlight: ColourRef,
    /// The view-space direction of the band's centre, components in `-1..=1`.
    pub dir: [f32; 3],
    /// Where the band starts, as `dot(normal, dir)` in `0..=1`.
    pub edge0: f32,
    /// Where it is full, above `edge0`.
    pub edge1: f32,
    /// How far the highlight replaces the lit colour, `0..=1`.
    pub strength: f32,
}

impl Default for MetalStyle {
    fn default() -> Self {
        Self {
            highlight: ColourRef::rgb(0xff, 0xf2, 0xc8),
            dir: [-0.32, 0.58, 0.75],
            edge0: 0.86,
            edge1: 0.90,
            strength: 0.6,
        }
    }
}

impl Validate for MetalStyle {
    fn check(&self, c: &mut Check) {
        for (k, v) in self.dir.iter().enumerate() {
            c.range(&format!("dir/{k}"), f64::from(*v), -1.0, 1.0);
        }
        c.range("edge0", f64::from(self.edge0), 0.0, 1.0);
        c.range("edge1", f64::from(self.edge1), 0.0, 1.0);
        if self.edge1 <= self.edge0 {
            c.fail("edge1", format!("{} must be above edge0 ({})", self.edge1, self.edge0));
        }
        c.range("strength", f64::from(self.strength), 0.0, 1.0);
    }
}

impl MetalStyle {
    /// The material of a metal trim of colour `base`, its highlight `highlight` resolved.
    ///
    /// # Errors
    /// [`TrimError::Invalid`] if the program name isn't a valid slug (it is a constant, so never in practice).
    pub fn material(&self, base: Colour, highlight: Colour) -> Result<MaterialSpec, TrimError> {
        let name = Slug::new(METAL_PROGRAM).map_err(|e| TrimError::invalid("metal", e.to_string()))?;
        let mut params = ProgramParams::new();
        let mut put = |key: &str, value: ParamValue| -> Result<(), TrimError> {
            params.insert(Slug::new(key).map_err(|e| TrimError::invalid("metal", e.to_string()))?, value);
            Ok(())
        };
        put("dir", ParamValue::Vec3(Vec3::from(self.dir)))?;
        put("colour", ParamValue::Colour(highlight))?;
        put("edge0", ParamValue::Float(self.edge0))?;
        put("edge1", ParamValue::Float(self.edge1))?;
        put("strength", ParamValue::Float(self.strength))?;
        Ok(MaterialSpec { shading: Shading::Program { name, params }, ..MaterialSpec::toon(base) })
    }
}

/// What a trim is made of: a colour, metal when `metal` is set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Finish {
    /// The base colour.
    pub colour: ColourRef,
    /// Metal shading; `None` for cloth or plastic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metal: Option<MetalStyle>,
}

impl Default for Finish {
    /// The prototype's gold.
    fn default() -> Self {
        Self { colour: ColourRef::rgb(0xd4, 0xa8, 0x48), metal: Some(MetalStyle::default()) }
    }
}

impl Validate for Finish {
    fn check(&self, c: &mut Check) {
        if let Some(metal) = &self.metal {
            c.nested("metal", metal);
        }
    }
}

impl Finish {
    /// A matte finish of `colour`.
    pub fn cloth(colour: ColourRef) -> Self {
        Self { colour, metal: None }
    }
}

// ----------------------------------------------------------------------------------------------------------- shapes

/// A frame the prototype's primitives are placed in: an origin and three unit axes (local x, y and z). The primitives
/// follow three.js's conventions, so the prototype's construction ports line by line: spheres, cylinders and cones stand
/// along local y, a torus lies in the local xy plane round local z.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Basis {
    /// Where local `(0, 0, 0)` is.
    pub origin: DVec3,
    /// Local x.
    pub x: DVec3,
    /// Local y.
    pub y: DVec3,
    /// Local z.
    pub z: DVec3,
}

impl Basis {
    /// The point at local `l`.
    pub fn place(&self, l: DVec3) -> DVec3 {
        self.origin + self.x * l.x + self.y * l.y + self.z * l.z
    }

    /// The same axes at local `l`.
    #[must_use]
    pub fn at(&self, l: DVec3) -> Self {
        Self { origin: self.place(l), ..*self }
    }

    /// The axes turned by `angle` about local y (x toward -z), as three.js's `rotation.y`.
    #[must_use]
    pub fn turned_y(&self, angle: f64) -> Self {
        let (s, c) = angle.sin_cos();
        Self { x: self.x * c - self.z * s, z: self.x * s + self.z * c, ..*self }
    }

    /// The axes turned by `angle` about local z (x toward y), as three.js's `rotation.z`.
    #[must_use]
    pub fn turned_z(&self, angle: f64) -> Self {
        let (s, c) = angle.sin_cos();
        Self { x: self.x * c + self.y * s, y: self.y * c - self.x * s, ..*self }
    }

    /// A basis at `origin` whose local y is `up` (unit), the other two axes square to it.
    pub fn standing(origin: DVec3, up: DVec3) -> Self {
        let x = up.any_orthonormal_vector();
        Self { origin, x, y: up, z: x.cross(up) }
    }
}

/// Segment counts as the prototype's robe sphere helper reduces them: `max(6, ws - 4)` round and `max(4, hs - 3)` from
/// pole to pole.
pub fn robe_segments(ws: usize, hs: usize) -> (usize, usize) {
    (ws.saturating_sub(4).max(6), hs.saturating_sub(3).max(4))
}

/// Adds the grid `points(i, j)` (`rows` by `cols`, wrapped as asked) round `inside`, skinned with `skin`.
fn add_closed(
    builder: &mut MeshBuilder,
    (rows, cols): (usize, usize),
    (wrap_i, wrap_j): (bool, bool),
    points: impl FnMut(usize, usize) -> DVec3,
    inside: &dyn Fn(usize, usize) -> DVec3,
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    let grid = Grid::from_fn(rows, cols, points)?;
    let options = GridOptions {
        wrap_i,
        wrap_j,
        inside: Some(inside),
        skin: Skin::Blend(skin.to_vec()),
        ..GridOptions::default()
    };
    builder.add_grid(&grid, &options)?;
    Ok(())
}

/// A sphere of radius `r` at the basis's origin, scaled by `scale` along its local axes, `segments` round and pole to
/// pole (three.js's `SphereGeometry(r, ws, hs)`); `upto` cuts it at a polar angle (`pi` for the whole sphere, `pi / 2`
/// for the upper hemisphere, closed with a flat disc).
///
/// # Errors
/// The kit's errors for a degenerate shape.
pub fn sphere(
    builder: &mut MeshBuilder,
    basis: &Basis,
    r: f64,
    scale: DVec3,
    (ws, hs): (usize, usize),
    upto: f64,
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    let local = |i: usize, j: usize| {
        let (theta, phi) = (upto * fraction(i, hs), TAU * fraction(j % ws, ws));
        DVec3::new(-r * phi.cos() * theta.sin(), r * theta.cos(), r * phi.sin() * theta.sin()) * scale
    };
    add_closed(builder, (hs + 1, ws + 1), (false, true), |i, j| basis.place(local(i, j)), &|_, _| basis.origin, skin)?;
    if upto < std::f64::consts::PI - 1e-9 {
        let rim: Vec<DVec3> = (0..ws).map(|j| basis.place(local(hs, j))).collect();
        disc(builder, rim, -basis.y, skin)?;
    }
    Ok(())
}

/// A torus at the basis's origin in its local xy plane: ring radius `big`, tube radius `r`, `radial` segments round the
/// tube and `tubular` round the ring (three.js's `TorusGeometry(big, r, radial, tubular)`), scaled by `scale`.
///
/// # Errors
/// The kit's errors for a degenerate shape.
#[allow(clippy::many_single_char_names)] // the torus's own names: ring angle u, tube angle v
pub fn torus(
    builder: &mut MeshBuilder,
    basis: &Basis,
    (big, r): (f64, f64),
    (radial, tubular): (usize, usize),
    scale: DVec3,
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    let local = |i: usize, j: usize| {
        let (u, v) = (TAU * fraction(i % tubular, tubular), TAU * fraction(j % radial, radial));
        DVec3::new((big + r * v.cos()) * u.cos(), (big + r * v.cos()) * u.sin(), r * v.sin()) * scale
    };
    let inside = |i: usize, _: usize| {
        let u = TAU * fraction(i % tubular, tubular);
        basis.place(DVec3::new(big * u.cos(), big * u.sin(), 0.0) * scale)
    };
    add_closed(builder, (tubular + 1, radial + 1), (true, true), |i, j| basis.place(local(i, j)), &inside, skin)
}

/// A capped cylinder along local y centred on the origin, radius `top` at `+height / 2` and `bottom` at `-height / 2`,
/// `radial` segments round (three.js's `CylinderGeometry`; a `top` of 0 is a cone).
///
/// # Errors
/// The kit's errors for a degenerate shape.
pub fn cylinder(
    builder: &mut MeshBuilder,
    basis: &Basis,
    (top, bottom): (f64, f64),
    height: f64,
    radial: usize,
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    // Rows from the bottom cap's centre out to its rim, up the side, and in across the top cap: a closed lathe. A cone
    // ends at its apex.
    let mut rows = vec![(-0.5 * height, 0.0), (-0.5 * height, bottom), (0.5 * height, top)];
    if top > 0.0 {
        rows.push((0.5 * height, 0.0));
    }
    let local = |i: usize, j: usize| {
        let (y, r) = rows[i];
        let a = TAU * fraction(j % radial, radial);
        DVec3::new(r * a.sin(), y, r * a.cos())
    };
    add_closed(
        builder,
        (rows.len(), radial + 1),
        (false, true),
        |i, j| basis.place(local(i, j)),
        &|_, _| basis.origin,
        skin,
    )
}

/// A box of `size` centred on the origin, along the local axes.
///
/// # Errors
/// [`TrimError::Invalid`] for a size that isn't positive; the kit's errors for a degenerate shape.
pub fn cuboid(
    builder: &mut MeshBuilder,
    basis: &Basis,
    size: DVec3,
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    if !(size.is_finite() && size.min_element() > 0.0) {
        return Err(TrimError::invalid("box", format!("its size {size} must be positive")));
    }
    let units = [basis.x, basis.y, basis.z];
    let axes = [units[0] * (size.x / 2.0), units[1] * (size.y / 2.0), units[2] * (size.z / 2.0)];
    for (k, sign) in [(0, 1.0), (0, -1.0), (1, 1.0), (1, -1.0), (2, 1.0), (2, -1.0)] {
        let (u, v) = (axes[(k + 1) % 3], axes[(k + 2) % 3]);
        let centre = basis.origin + axes[k] * sign;
        let ring = vec![centre - u - v, centre + u - v, centre + u + v, centre - u + v];
        disc(builder, ring, units[k] * sign, skin)?;
    }
    Ok(())
}

/// The bevel of an extruded outline: how far its edge is rounded back (`thickness`) and out (`size`), in `segments`
/// steps (three.js's `bevelThickness`, `bevelSize` and `bevelSegments`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Bevel {
    /// How far the rounding runs back from each face.
    pub thickness: f64,
    /// How far the outline grows at the middle.
    pub size: f64,
    /// Steps of the rounding.
    pub segments: usize,
}

/// The longest a bevel's corner offset grows, in bevel sizes: three.js's cap on a sharp corner's mitre (`sqrt(2)`).
const MAX_MITRE: f64 = std::f64::consts::SQRT_2;

/// The flat `outline` (local x, y; counter-clockwise) extruded along local z from 0 to `depth`, its edges bevelled as
/// three.js's `ExtrudeGeometry` does: the faces are the outline at `-bevel.thickness` and `depth + bevel.thickness`, and
/// between them the outline grows by up to `bevel.size` (each edge moved out by that much, corners mitred, a mitre
/// never longer than [`MAX_MITRE`] sizes). The faces
/// are filled as the convex pieces `faces` lists (index lists into `outline`), so a concave outline closes too.
///
/// # Errors
/// [`TrimError::Invalid`] for fewer than 3 outline points, a point that isn't finite or one repeated in a row; the
/// kit's errors for a degenerate shape.
pub fn extrude(
    builder: &mut MeshBuilder,
    basis: &Basis,
    outline: &[DVec2],
    faces: &[&[usize]],
    depth: f64,
    bevel: Bevel,
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    let n = outline.len();
    if n < 3 {
        return Err(TrimError::invalid("outline", format!("{n} points: an outline needs 3")));
    }
    if (0..n).any(|k| outline[k].distance(outline[(k + 1) % n]) <= 0.0 || !outline[k].is_finite()) {
        return Err(TrimError::invalid("outline", "its points must be finite and each differ from the next"));
    }
    // Each corner's mitre: the offset that moves both its edges out by 1, no longer than three.js lets it grow (its
    // `getBevelVec` shortens a sharp corner's mitre to that length, "to prevent crazy spikes").
    let mitres: Vec<DVec2> = (0..n)
        .map(|k| {
            let (a, b, c) = (outline[(k + n - 1) % n], outline[k], outline[(k + 1) % n]);
            // Each edge has length (checked above), so its outward normal is a unit vector.
            let out = |d: DVec2| DVec2::new(d.y, -d.x).normalize();
            let (n1, n2) = (out(b - a), out(c - b));
            let mitre = (n1 + n2) / (1.0 + n1.dot(n2)).max(1e-6);
            mitre.clamp_length_max(MAX_MITRE)
        })
        .collect();
    // The layers from the back face to the front: the bevel's steps, then the same steps mirrored at the front.
    let quarter = std::f64::consts::FRAC_PI_2;
    let mut layers: Vec<(f64, f64)> = (0..=bevel.segments)
        .map(|b| {
            let t = fraction(b, bevel.segments.max(1));
            (bevel.size * (t * quarter).sin(), -bevel.thickness * (t * quarter).cos())
        })
        .collect();
    let back: Vec<(f64, f64)> = layers.iter().rev().map(|&(grow, z)| (grow, depth - z)).collect();
    layers.extend(back);
    layers.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12);
    let at = |layer: usize, k: usize| {
        let (grow, z) = layers[layer];
        let p = outline[k % n] + mitres[k % n] * grow;
        basis.place(DVec3::new(p.x, p.y, z))
    };
    let centre = outline.iter().copied().sum::<DVec2>() / f64::from(u32::try_from(n).unwrap_or(u32::MAX));
    let middle = basis.place(DVec3::new(centre.x, centre.y, depth / 2.0));
    add_closed(builder, (layers.len(), n + 1), (false, true), at, &|_, _| middle, skin)?;
    let last = layers.len() - 1;
    for piece in faces {
        disc(builder, piece.iter().map(|&k| at(0, k)).collect(), -basis.z, skin)?;
        disc(builder, piece.iter().map(|&k| at(last, k)).collect(), basis.z, skin)?;
    }
    Ok(())
}

/// Closes a flat convex ring with a polygon facing `outward`, wound to match.
fn disc(
    builder: &mut MeshBuilder,
    mut ring: Vec<DVec3>,
    outward: DVec3,
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    // Newell's normal of the ring as given: reverse the ring if it faces the other way.
    let mut newell = DVec3::ZERO;
    for (k, p) in ring.iter().enumerate() {
        newell += p.cross(ring[(k + 1) % ring.len()]);
    }
    if newell.dot(outward) < 0.0 {
        ring.reverse();
    }
    builder.add_polygon(&ring, outward, [1.0; 4], skin)?;
    Ok(())
}

// ------------------------------------------------------------------------------------------------------------ specs

/// One trim on a garment, as its parameters store it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrimSpec {
    /// A thick-edged strip along a rail.
    Piping(piping::Piping),
    /// A round cord along a rail.
    Cord(piping::Cord),
    /// Strands braided along a rail.
    Braid(braid::Braid),
    /// One button or rivet.
    Button(buttons::Button),
    /// Buttons spaced along a rail.
    ButtonRow(buttons::ButtonRow),
    /// A chain hanging between two points.
    Chain(chain::Chain),
    /// A medallion clasp.
    Clasp(clasp::Clasp),
    /// A picture on the surface.
    Decal(decal::Decal),
    /// A scalloped lace edge along a rail.
    Lace(lace::Lace),
}

impl Validate for TrimSpec {
    fn check(&self, c: &mut Check) {
        match self {
            Self::Piping(s) => s.check(c),
            Self::Cord(s) => s.check(c),
            Self::Braid(s) => s.check(c),
            Self::Button(s) => s.check(c),
            Self::ButtonRow(s) => s.check(c),
            Self::Chain(s) => s.check(c),
            Self::Clasp(s) => s.check(c),
            Self::Decal(s) => s.check(c),
            Self::Lace(s) => s.check(c),
        }
    }
}

impl TrimSpec {
    /// The open borders the trim's meshes have: a strip or a lace edge is a sheet (one border round it along an open
    /// rail, two along a loop), a decal a sheet; every other trim is closed.
    pub fn open_borders(&self) -> usize {
        let sheet = |rail: &RailSpec| if rail.closed { 2 } else { 1 };
        match self {
            Self::Piping(piping) => sheet(&piping.rail),
            Self::Lace(lace) => sheet(&lace.rail),
            Self::Decal(_) => 1,
            _ => 0,
        }
    }
}

/// Builds `spec` on `surface` into `out`, as meshes and materials named `<part>-<name>` (and `<part>-<name>-<piece>`
/// for a trim of several materials).
///
/// # Errors
/// [`BuildError::Part`] naming the trim for anything the generator refuses (a zero-length rail among them),
/// [`BuildError::Colour`] for a palette slot the palette lacks, and the build's errors for a decal or lace texture
/// that can't be painted or read.
pub fn add_trim(
    out: &mut GarmentOutput,
    ctx: &BuildCtx<'_>,
    surface: &Surface,
    name: &str,
    spec: &TrimSpec,
) -> Result<(), BuildError> {
    let fail = |e: TrimError| ctx.fail(format!("trim `{name}`: {e}"));
    match spec {
        TrimSpec::Piping(s) => {
            let mesh = piping::build(surface, s).map_err(fail)?;
            add_finished(out, ctx, name, mesh, &s.finish)
        }
        TrimSpec::Cord(s) => {
            let mesh = piping::build_cord(surface, s).map_err(fail)?;
            add_finished(out, ctx, name, mesh, &s.finish)
        }
        TrimSpec::Braid(s) => {
            let mesh = braid::build(surface, s).map_err(fail)?;
            add_finished(out, ctx, name, mesh, &s.finish)
        }
        TrimSpec::Button(s) => {
            let mesh = buttons::build(surface, s).map_err(fail)?;
            add_finished(out, ctx, name, mesh, &s.style.finish)
        }
        TrimSpec::ButtonRow(s) => {
            let mesh = buttons::build_row(surface, s).map_err(fail)?;
            add_finished(out, ctx, name, mesh, &s.button.finish)
        }
        TrimSpec::Chain(s) => {
            let mesh = chain::build(surface, s).map_err(fail)?;
            add_finished(out, ctx, name, mesh, &s.finish)
        }
        TrimSpec::Clasp(s) => {
            let pieces = clasp::build(surface, s).map_err(fail)?;
            add_finished(out, ctx, name, pieces.metal, &s.finish)?;
            match &s.jewel {
                Some(colour) => {
                    add_finished(out, ctx, &format!("{name}-jewel"), pieces.jewel, &Finish::cloth(colour.clone()))
                }
                None => Ok(()),
            }
        }
        TrimSpec::Decal(s) => decal::add(out, ctx, surface, name, s),
        TrimSpec::Lace(s) => lace::add(out, ctx, surface, name, s),
    }
}

/// Adds `mesh` as `<part>-<name>` in a material of `finish`, declared metal when it is.
fn add_finished(
    out: &mut GarmentOutput,
    ctx: &BuildCtx<'_>,
    name: &str,
    mesh: MeshBuilder,
    finish: &Finish,
) -> Result<(), BuildError> {
    let base = ctx.colour(&finish.colour)?;
    let (spec, class) = match &finish.metal {
        Some(metal) => {
            let highlight = ctx.colour(&metal.highlight)?;
            let spec = metal.material(base, highlight).map_err(|e| ctx.fail(format!("trim `{name}`: {e}")))?;
            (spec, MaterialClass::Metal)
        }
        None => (MaterialSpec::toon(base), MaterialClass::Cloth),
    };
    let material: MaterialName = out.material(ctx, name, spec)?;
    out.classify(&material, class);
    out.mesh(ctx, name, mesh, material)
}
