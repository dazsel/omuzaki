//! The `straps` garment: flat bands that lie over the body along a path of points on it. A sash across the chest,
//! crossed straps over a bodysuit, a harness, suspenders, a bandolier and a belt worn high are all this one garment
//! with different paths.
//!
//! Each strap runs through its points (a region of the body, a fraction along it and an angle round it) in order,
//! straight between them and pressed onto whatever the strap lies on: every station along the way is pushed out to
//! the torso's or a leg's surface (the layers already worn, plus the ease) wherever it would sink in, then the path is
//! smoothed and pressed again. The band's cross-section is a flat, rounded strip `width` across and `thickness` deep,
//! its inner face on that surface, its long edges in `edge_colour`; both ends are closed. A strap set to `mirror` is
//! built twice, the second on the character's other side and one band's thickness further out, so two crossed straps
//! lie one over the other where they cross. Skinned by the bones of the region each station lies on.
//!
//! Layer: [`LAYER_STRAPS`], over a shirt, a bodysuit and arm warmers, under a vest.

use std::sync::Arc;

use omuzaki_char_api::{
    BodyFit, BodyRegion, BuildCtx, BuildError, JointRef, PartCategory, PartKind, PartMeta, PartOutput,
};
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::{Outline, Rim, Shading, Toon};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, LAYER_SHIRT};
use super::loft::{MIN_EXPONENT, Surface};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};

/// The layer straps are worn on: over the shirt, the bodysuit and arm warmers, under the vest.
pub const LAYER_STRAPS: i32 = LAYER_SHIRT + 4;
/// The spacing of the stations along a strap, metres.
pub const STATION_STEP: f64 = 0.008;
/// The furthest a strap's point may stand off the surface, metres.
pub const MAX_LIFT: f64 = 0.1;
/// The most straps one part may hold (a mirrored strap counts once).
pub const MAX_STRAPS: usize = 8;
/// The most points one strap may run through.
pub const MAX_POINTS: usize = 8;
/// The narrowest a strap may be, metres.
pub const MIN_WIDTH: f64 = 0.005;
/// The widest a strap may be, metres.
pub const MAX_WIDTH: f64 = 0.12;
/// The thinnest a strap may be, metres.
pub const MIN_THICKNESS: f64 = 0.0005;
/// The thickest a strap may be, metres.
pub const MAX_THICKNESS: f64 = 0.012;
/// How far apart two stacked bands stand, metres, on top of the lower one's thickness.
const STACK_GAP: f64 = 0.0005;
/// How many times a station is pressed onto the surfaces (each press can only move it outward).
const PRESSES: usize = 4;
/// How many smoothing passes the pressed path gets.
const SMOOTHING: usize = 4;
/// The cross-section of a band, `(across, up)`: across from -1 to 1 over the width, up from 0 (on the surface) to 1
/// (the band's top) over its thickness; a flat strip with rounded edges, counter-clockwise seen along the strap.
const CROSS: [(f64, f64); 14] = [
    (1.0, 0.5),
    (0.96, 0.9),
    (0.8, 1.0),
    (0.4, 1.0),
    (0.0, 1.0),
    (-0.4, 1.0),
    (-0.8, 1.0),
    (-0.96, 0.9),
    (-1.0, 0.5),
    (-0.96, 0.1),
    (-0.8, 0.0),
    (0.0, 0.0),
    (0.8, 0.0),
    (0.96, 0.1),
];
/// The width of the outline, metres at two metres.
const OUTLINE_WIDTH: f32 = 0.0015;
/// The bands' shading: the arm warmers' cloth.
const CLOTH_SHIFT: f32 = -0.12;
const CLOTH_SOFTNESS: f32 = 0.2;

/// The regions a strap's points lie on.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StrapRegion {
    /// The torso, from the hips (0) to the base of the neck (1).
    Torso,
    /// The character's left leg (+x), from the hip (0) to the ankle (1).
    LegLeft,
    /// The right leg.
    LegRight,
}

impl StrapRegion {
    /// The body region.
    pub fn region(self) -> BodyRegion {
        match self {
            Self::Torso => BodyRegion::Torso,
            Self::LegLeft => BodyRegion::LegL,
            Self::LegRight => BodyRegion::LegR,
        }
    }

    /// The region on the character's other side.
    #[must_use]
    pub fn mirrored(self) -> Self {
        match self {
            Self::Torso => Self::Torso,
            Self::LegLeft => Self::LegRight,
            Self::LegRight => Self::LegLeft,
        }
    }
}

/// A point a strap runs through.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StrapPoint {
    /// The region it lies on.
    pub region: StrapRegion,
    /// How far along the region, `0..=1`.
    pub t: f64,
    /// Radians round the region from the front, turning toward the character's left: 0 the front, 1.57 the left side,
    /// -1.57 the right side, 3.14 the back.
    pub angle: f64,
    /// How far off the surface the strap passes here, metres: 0 lies on it; more holds it out, so a strap crossing
    /// the belly from the sides of the chest runs straight, standing off it, as a taut ribbon does.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub lift: f64,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

impl StrapPoint {
    /// A point on `region` at `t` and `angle`.
    pub const fn new(region: StrapRegion, t: f64, angle: f64) -> Self {
        Self { region, t, angle, lift: 0.0 }
    }

    /// The same point held `lift` metres off the surface.
    #[must_use]
    pub const fn lifted(self, lift: f64) -> Self {
        Self { lift, ..self }
    }

    /// The same point on the character's other side.
    #[must_use]
    pub fn mirrored(self) -> Self {
        Self { region: self.region.mirrored(), angle: -self.angle, ..self }
    }
}

/// One strap.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Strap {
    /// The points it runs through, in order: 2 to [`MAX_POINTS`].
    pub points: Vec<StrapPoint>,
    /// How wide it is, metres.
    pub width: f64,
    /// Also build it on the character's other side, lying over this one where they cross (crossed straps, a pair of
    /// suspenders).
    #[serde(default)]
    pub mirror: bool,
}

/// The parameters of the `straps` kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct StrapsParams {
    /// Ease over what the straps lie on, and the cloth (`common.fabric`'s colour is the bands').
    pub common: GarmentCommon,
    /// The straps: 1 to [`MAX_STRAPS`].
    pub straps: Vec<Strap>,
    /// How thick each band is, metres.
    pub thickness: f64,
    /// How much of each long edge is in `edge_colour`, as a fraction of the half width: 0 for none.
    pub edge: f64,
    /// The colour of the bands' long edges.
    pub edge_colour: ColourRef,
    /// The shadow colour of the bands.
    pub shade: ColourRef,
    /// The outline's colour.
    pub line: ColourRef,
}

impl Default for StrapsParams {
    /// A pair of straps crossed over the chest, from each shoulder to the other side of the hips.
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: 0.002,
                fabric: FabricStyle::Plain { colour: ColourRef::rgb(0x3a, 0x2c, 0x24) },
                ..GarmentCommon::default()
            },
            straps: vec![Strap {
                points: vec![
                    StrapPoint::new(StrapRegion::Torso, 0.9, 0.5),
                    StrapPoint::new(StrapRegion::Torso, 0.1, -0.6),
                ],
                width: 0.03,
                mirror: true,
            }],
            thickness: 0.003,
            edge: 0.0,
            edge_colour: ColourRef::rgb(0xc8, 0xa8, 0x60),
            shade: ColourRef::rgb(0x24, 0x1a, 0x16),
            line: ColourRef::rgb(0x14, 0x10, 0x0e),
        }
    }
}

impl Validate for StrapsParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        if self.straps.is_empty() || self.straps.len() > MAX_STRAPS {
            c.fail("straps", format!("give 1 to {MAX_STRAPS} straps, not {}", self.straps.len()));
        }
        for (i, strap) in self.straps.iter().enumerate() {
            if strap.points.len() < 2 || strap.points.len() > MAX_POINTS {
                c.fail(
                    &format!("straps/{i}/points"),
                    format!("a strap runs through 2 to {MAX_POINTS} points, not {}", strap.points.len()),
                );
            }
            for (k, point) in strap.points.iter().enumerate() {
                c.range(&format!("straps/{i}/points/{k}/t"), point.t, 0.0, 1.0);
                c.range(
                    &format!("straps/{i}/points/{k}/angle"),
                    point.angle,
                    -std::f64::consts::PI,
                    std::f64::consts::PI,
                );
                c.range(&format!("straps/{i}/points/{k}/lift"), point.lift, 0.0, MAX_LIFT);
            }
            c.range(&format!("straps/{i}/width"), strap.width, MIN_WIDTH, MAX_WIDTH);
        }
        c.range("thickness", self.thickness, MIN_THICKNESS, MAX_THICKNESS);
        c.range("edge", self.edge, 0.0, 0.45);
    }
}

impl StrapsParams {
    /// How many bands the straps make: a mirrored strap makes two.
    pub fn bands(&self) -> usize {
        self.straps.iter().map(|s| if s.mirror { 2 } else { 1 }).sum()
    }
}

/// The `straps` kind.
pub struct Straps;

impl PartKind for Straps {
    type Params = StrapsParams;

    fn layer() -> Option<i32> {
        Some(LAYER_STRAPS)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "straps",
            summary: "Build flat bands that lie over the body along paths of points on it: a sash, crossed straps, a \
                      harness, suspenders or a bandolier, each band pressed onto what it lies on, with coloured edges.",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &StrapsParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let fit = ctx.fit()?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let base = match &p.common.fabric {
            FabricStyle::Plain { colour } => ctx.colour(colour)?.to_linear(),
            _ => omuzaki_core::glam::Vec3::ONE,
        };
        let edge = ctx.colour(&p.edge_colour)?.to_linear();
        let edge_tint = [ratio(edge.x, base.x), ratio(edge.y, base.y), ratio(edge.z, base.z)];
        let mut mesh = MeshBuilder::with_colours();
        let mut stack = 0usize;
        for strap in &p.straps {
            let mut paths = vec![strap.points.clone()];
            if strap.mirror {
                paths.push(strap.points.iter().map(|q| q.mirrored()).collect());
            }
            for points in paths {
                let offset = p.common.ease + stack_height(p, stack);
                let band = band(&fit, &points, strap.width, p, offset, edge_tint).map_err(fail)?;
                mesh.merge(&band).map_err(|e| fail(e.into()))?;
                stack += 1;
            }
        }
        out.surface(ctx, "bands", mesh)?;
        // What a garment over the straps clears: the ease and the whole stack.
        let envelope = GarmentCommon { ease: p.common.ease + stack_height(p, p.bands()), ..p.common.clone() };
        let mut output = out.finish(ctx, &envelope, &Self::regions(p), MIN_EXPONENT)?;
        let fabric = format!("{}-fabric", ctx.part);
        let (shade, line) = (ctx.colour(&p.shade)?, ctx.colour(&p.line)?);
        for (name, spec) in &mut output.materials {
            if name.as_str() == fabric {
                spec.shading = Shading::Toon(Toon {
                    shade,
                    shift: CLOTH_SHIFT,
                    softness: CLOTH_SOFTNESS,
                    rim: Rim { strength: 0.1, ..Rim::default() },
                    ..Toon::default()
                });
                spec.vertex_colours = true;
                spec.outline = Some(Outline { colour: line, width: OUTLINE_WIDTH });
            }
        }
        Ok(output)
    }
}

impl Garment for Straps {
    fn common(p: &StrapsParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &StrapsParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso, BodyRegion::LegL, BodyRegion::LegR]
    }

    /// None: every band is closed at both ends.
    fn open_borders(_: &StrapsParams) -> usize {
        0
    }
}

omuzaki_char_api::register_part_kind!(Straps);

/// How far out the `k`-th band of the stack starts, over the ease: the bands under it and their gaps.
#[allow(clippy::cast_precision_loss)] // a handful of bands
fn stack_height(p: &StrapsParams, k: usize) -> f64 {
    k as f64 * (p.thickness + STACK_GAP)
}

/// The vertex colour that turns `base` into `target`, held to a range a vertex colour can carry.
fn ratio(target: f32, base: f32) -> f32 {
    if base > 1e-4 { (target / base).clamp(0.0, 4.0) } else { 1.0 }
}

/// The surfaces a band lies on, `offset` out from the fit.
struct Ground {
    surfaces: Vec<Surface>,
}

/// A station of a band's path: where it is, the surface it lies on and where along that surface.
#[derive(Copy, Clone)]
struct Station {
    point: DVec3,
    surface: usize,
    t: f64,
    phi: f64,
}

impl Ground {
    fn new(fit: &Arc<dyn BodyFit>, offset: f64) -> Result<Self, GarmentError> {
        let surfaces = [BodyRegion::Torso, BodyRegion::LegL, BodyRegion::LegR]
            .into_iter()
            .map(|region| Surface::new(Arc::clone(fit), region, 2.0, offset))
            .collect::<Result<_, _>>()?;
        Ok(Self { surfaces })
    }

    fn index(region: BodyRegion) -> usize {
        match region {
            BodyRegion::LegL => 1,
            BodyRegion::LegR => 2,
            _ => 0,
        }
    }

    /// `p` pushed out of every surface it lies inside, with the surface it ends nearest. Moves only outward.
    fn press(&self, p: DVec3) -> Result<Station, GarmentError> {
        let mut point = p;
        for _ in 0..PRESSES {
            let mut moved = false;
            for surface in &self.surfaces {
                if let Some(at) = surface.project(point)?
                    && at.outside < 0.0
                {
                    point = surface.point(at.t, at.phi)?;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        let mut best: Option<(f64, Station)> = None;
        for (k, surface) in self.surfaces.iter().enumerate() {
            if let Some(at) = surface.project(point)?
                && best.is_none_or(|(d, _)| at.outside.abs() < d)
            {
                best = Some((at.outside.abs(), Station { point, surface: k, t: at.t, phi: at.phi }));
            }
        }
        best.map(|(_, s)| s)
            .ok_or_else(|| GarmentError::invalid("straps", format!("the point {point} lies over no part of the body")))
    }

    /// The outward normal of the surface at `station`.
    fn normal(&self, station: &Station) -> Result<DVec3, GarmentError> {
        Ok(self.surfaces[station.surface].ring(station.t)?.normal(station.phi))
    }

    fn skin(&self, station: &Station) -> Vec<(JointRef, f32)> {
        self.surfaces[station.surface].weights().at(station.t)
    }
}

/// One band through `points`, `offset` out from the fit.
#[allow(clippy::many_single_char_names)] // a frame's `t`, `n` and `b`
fn band(
    fit: &Arc<dyn BodyFit>,
    points: &[StrapPoint],
    width: f64,
    p: &StrapsParams,
    offset: f64,
    edge_tint: [f32; 3],
) -> Result<MeshBuilder, GarmentError> {
    let ground = Ground::new(fit, offset)?;
    let anchors = points
        .iter()
        .map(|q| {
            let surface = &ground.surfaces[Ground::index(q.region.region())];
            Ok(surface.point(q.t, q.angle)? + surface.ring(q.t)?.normal(q.angle) * q.lift)
        })
        .collect::<Result<Vec<_>, GarmentError>>()?;
    // Straight between the points, a station every step.
    let mut path = vec![anchors[0]];
    for pair in anchors.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // a short count
        let n = ((a.distance(b) / STATION_STEP).ceil() as usize).max(1);
        #[allow(clippy::cast_precision_loss)]
        path.extend((1..=n).map(|k| a.lerp(b, k as f64 / n as f64)));
    }
    if path.len() < 3 {
        return Err(GarmentError::invalid("straps", "a strap's points are less than two stations apart"));
    }
    let mut stations = path.iter().map(|q| ground.press(*q)).collect::<Result<Vec<_>, _>>()?;
    for _ in 0..SMOOTHING {
        let smoothed: Vec<DVec3> = (0..stations.len())
            .map(|i| {
                if i == 0 || i + 1 == stations.len() {
                    stations[i].point
                } else {
                    stations[i].point * 0.5 + (stations[i - 1].point + stations[i + 1].point) * 0.25
                }
            })
            .collect();
        stations = smoothed.iter().map(|q| ground.press(*q)).collect::<Result<Vec<_>, _>>()?;
    }
    let half = 0.5 * width;
    let mut rings = Vec::with_capacity(stations.len());
    let mut centres = Vec::with_capacity(stations.len());
    let mut tangents = Vec::with_capacity(stations.len());
    for i in 0..stations.len() {
        let (prev, next) = (stations[i.saturating_sub(1)].point, stations[(i + 1).min(stations.len() - 1)].point);
        let t = (next - prev)
            .try_normalize()
            .ok_or_else(|| GarmentError::invalid("straps", "two of a strap's stations coincide"))?;
        let n = ground.normal(&stations[i])?;
        let n = (n - t * n.dot(t)).try_normalize().unwrap_or(n);
        let b = n.cross(t);
        let origin = stations[i].point;
        let mut ring: Vec<DVec3> = CROSS
            .iter()
            .map(|&(u, v)| {
                let q = origin + b * (u * half) + n * (v * p.thickness);
                // Over a hollow (between the breasts, round the crotch) the band's edge would dip in: press it out.
                ground.press(q).map_or(q, |s| s.point)
            })
            .collect();
        ring.push(ring[0]);
        rings.push(ring);
        centres.push(origin + n * (0.5 * p.thickness));
        tangents.push(t);
    }
    let grid = Grid::from_rows(&rings)?;
    let skins: Vec<Vec<(JointRef, f32)>> = stations.iter().map(|s| ground.skin(s)).collect();
    let skin = |_: DVec3, i: usize, _: usize| skins[i].clone();
    let inside = |i: usize, _: usize| centres[i];
    let edge_from = 1.0 - p.edge;
    let colour = |_: usize, j: usize, _: DVec3| {
        let (u, _) = CROSS[j % CROSS.len()];
        if p.edge > 0.0 && u.abs() >= edge_from - 1e-9 {
            [edge_tint[0], edge_tint[1], edge_tint[2], 1.0]
        } else {
            [1.0; 4]
        }
    };
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        colour: Some(&colour),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    let mut mesh = MeshBuilder::with_colours();
    mesh.add_grid(&grid, &options)?;
    // The ends, closed flat: each a polygon over the end ring, facing out along the strap.
    let last = rings.len() - 1;
    for (i, out) in [(0, -tangents[0]), (last, tangents[last])] {
        let mut cap: Vec<DVec3> = rings[i][..CROSS.len()].to_vec();
        // Which way the ring turns (Newell's normal), so the fan faces out.
        let turn: DVec3 = (0..cap.len()).map(|k| cap[k].cross(cap[(k + 1) % cap.len()])).sum();
        if turn.dot(out) < 0.0 {
            cap.reverse();
        }
        let colour = [1.0, 1.0, 1.0, 1.0];
        mesh.add_polygon(&cap, out, colour, &skins[i])?;
    }
    Ok(mesh)
}
