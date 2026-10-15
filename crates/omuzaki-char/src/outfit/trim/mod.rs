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
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::{ParamValue, ProgramParams, Shading};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{GarmentError, GarmentOutput};
use super::loft::Surface;
use crate::kit::KitError;
use crate::kit::curve::fraction;
use crate::kit::grid::MeshBuilder;

/// How far a rail sits off the surface unless it says otherwise, metres (the robe's `LIFT`).
pub const LIFT: f64 = 0.0025;
/// The most a rail may be lifted, metres.
pub const MAX_LIFT: f64 = 0.02;
/// The most points a rail's path may have.
pub const MAX_RAIL_POINTS: usize = 64;
/// The most samples a rail may be resampled to.
pub const MAX_RAIL_SAMPLES: usize = 2048;
/// Pieces each step of a rail's path is cut into to measure it along the surface.
const RAIL_PIECES: usize = 24;
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
}

fn default_lift() -> f64 {
    LIFT
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
    }
}

/// One sample of a [`Rail`]: a point lifted off the surface with its frame and skin.
#[derive(Clone, Debug, PartialEq)]
pub struct RailPoint {
    /// The point, `lift` off the surface.
    pub point: DVec3,
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
    /// Its distance along the rail from the start, metres.
    pub distance: f64,
    /// The skin of the surface under it.
    pub skin: Vec<(JointRef, f32)>,
}

/// A path on a surface, resampled evenly by its length along the surface. A closed rail's last sample repeats its first
/// exactly, so a sweep along it wraps.
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

impl Rail {
    /// The rail `spec` describes on `surface`, resampled to `samples` evenly spaced points (a closed rail gets one more,
    /// repeating the first). The port of the robe's `rail`: points projected onto the surface and lifted off it.
    ///
    /// # Errors
    /// [`TrimError::ZeroLengthRail`] if the path has no length; [`TrimError::Invalid`] for a spec [`RailSpec`]'s
    /// validation refuses or `samples` outside `2..=`[`MAX_RAIL_SAMPLES`]; the surface's errors for a point it can't
    /// place.
    pub fn from_polyline_on_surface(surface: &Surface, spec: &RailSpec, samples: usize) -> Result<Self, TrimError> {
        if !(2..=MAX_RAIL_SAMPLES).contains(&samples) {
            return Err(TrimError::invalid("rail", format!("{samples} samples: use 2 to {MAX_RAIL_SAMPLES}")));
        }
        let (dense, length) = measure(surface, spec)?;
        Self::resampled(surface, spec, &dense, length, samples)
    }

    /// The rail `spec` describes on `surface`, with a sample about every `step` metres (at least 2, at most
    /// [`MAX_RAIL_SAMPLES`]).
    ///
    /// # Errors
    /// As [`Rail::from_polyline_on_surface`]; [`TrimError::Invalid`] for a step that isn't a positive length.
    pub fn with_step(surface: &Surface, spec: &RailSpec, step: f64) -> Result<Self, TrimError> {
        if !(step.is_finite() && step > 0.0) {
            return Err(TrimError::invalid("rail", format!("a step of {step} m: use a positive length")));
        }
        let (dense, length) = measure(surface, spec)?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // clamped below
        let pieces = (length / step).ceil().min(MAX_RAIL_SAMPLES as f64) as usize;
        let samples = (pieces + 1).clamp(2, MAX_RAIL_SAMPLES);
        Self::resampled(surface, spec, &dense, length, samples)
    }

    fn resampled(
        surface: &Surface,
        spec: &RailSpec,
        dense: &[Measured],
        length: f64,
        samples: usize,
    ) -> Result<Self, TrimError> {
        let count = if spec.closed { samples } else { samples - 1 };
        let mut points = Vec::with_capacity(count + 1);
        let mut cursor = 0;
        for k in 0..=count {
            let want = length * fraction(k, count);
            while cursor + 2 < dense.len() && dense[cursor + 1].1 < want {
                cursor += 1;
            }
            let ((a, da), (b, db)) = (dense[cursor], dense[cursor + 1]);
            let s = if db > da { ((want - da) / (db - da)).clamp(0.0, 1.0) } else { 0.0 };
            let at = [a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s];
            let frame = surface.at(at[0].clamp(0.0, 1.0), at[1])?;
            points.push(RailPoint {
                point: frame.point + frame.normal * spec.lift,
                normal: frame.normal,
                tangent: DVec3::ZERO,
                side: DVec3::ZERO,
                t: at[0],
                phi: at[1],
                distance: want,
                skin: surface.weights().at(at[0]),
            });
        }
        if spec.closed {
            // The last sample is the first again: copy it, so the seam welds exactly.
            let first = points[0].clone();
            let last = points.len() - 1;
            points[last] = RailPoint { distance: length, ..first };
        }
        frame_points(&mut points, spec.closed)?;
        Ok(Self { points, closed: spec.closed, length })
    }

    /// The samples, from the start.
    pub fn points(&self) -> &[RailPoint] {
        &self.points
    }

    /// Whether the rail is a loop.
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// Its length along the lifted path, metres.
    pub fn length(&self) -> f64 {
        self.length
    }
}

/// A point of a rail's path in surface coordinates `[t, phi]`, and the length along the rail up to it.
type Measured = ([f64; 2], f64);

/// The path of `spec` measured on `surface`: its unwrapped `[t, phi]` points, finely cut, each with the length along the
/// lifted path up to it; and the whole length.
fn measure(surface: &Surface, spec: &RailSpec) -> Result<(Vec<Measured>, f64), TrimError> {
    checked(spec)?;
    // The path in surface coordinates, unwrapped so each step goes the short way round.
    let mut path = vec![spec.points[0]];
    let ends = spec.points.iter().skip(1).chain(spec.closed.then_some(&spec.points[0]));
    for &next in ends {
        let last = path[path.len() - 1];
        let d = step(last, next);
        path.push([last[0] + d[0], last[1] + d[1]]);
    }
    let mut dense: Vec<Measured> = Vec::with_capacity((path.len() - 1) * RAIL_PIECES + 1);
    let mut previous: Option<DVec3> = None;
    let mut length = 0.0;
    for pair in path.windows(2) {
        let pieces = if dense.is_empty() { 0..=RAIL_PIECES } else { 1..=RAIL_PIECES };
        for k in pieces {
            let s = fraction(k, RAIL_PIECES);
            let at = [pair[0][0] + (pair[1][0] - pair[0][0]) * s, pair[0][1] + (pair[1][1] - pair[0][1]) * s];
            let p = lifted(surface, at, spec.lift)?;
            if let Some(q) = previous {
                length += p.distance(q);
            }
            previous = Some(p);
            dense.push((at, length));
        }
    }
    if !(length.is_finite() && length >= MIN_RAIL_LENGTH) {
        return Err(TrimError::ZeroLengthRail);
    }
    Ok((dense, length))
}

/// The surface point at `at`, lifted along its normal.
fn lifted(surface: &Surface, at: [f64; 2], lift: f64) -> Result<DVec3, TrimError> {
    let frame = surface.at(at[0].clamp(0.0, 1.0), at[1])?;
    Ok(frame.point + frame.normal * lift)
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

// ------------------------------------------------------------------------------------------------------------ specs

/// One trim on a garment, as its parameters store it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrimSpec {
    /// A raised strip along a rail.
    Piping(piping::Piping),
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
    /// The open borders the trim's meshes have: a lace edge is a strip (one border round an open one, two for a loop),
    /// a decal a sheet; every other trim is closed.
    pub fn open_borders(&self) -> usize {
        match self {
            Self::Lace(lace) => {
                if lace.rail.closed {
                    2
                } else {
                    1
                }
            }
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
            add_finished(out, ctx, &format!("{name}-jewel"), pieces.jewel, &Finish::cloth(s.jewel.clone()))
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
