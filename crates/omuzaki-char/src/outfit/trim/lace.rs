//! Lace edges: a strip along a rail, cut into scallops by an alpha-masked texture painted from a procedural pattern.
//!
//! The strip lies in the surface's tangent plane, hanging off the rail to its `side` (down, for a hem rail that runs
//! round the garment in the direction of growing `phi`), so it continues past a garment's edge. Its texture is one
//! scallop, repeated a whole number of times along the rail: a heading band along the rail and a half-disc below it
//! filled with the pattern's threads. Everything not drawn is clear and is cut away by the material's alpha mask.

use omuzaki_char_api::{BuildCtx, BuildError, MaterialSpec};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DVec2, DVec3, UVec2, Vec2};
use omuzaki_face_api::{Check, Cmd, ColourRef, DrawList, FaceRect, Paint, Validate};
use omuzaki_render_api::{AlphaMode, Filter, Sampler, TextureDesc, Wrap};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Rail, RailSpec, TrimError, checked, feature};
use crate::kit::curve::fraction;
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::outfit::common::GarmentOutput;
use crate::outfit::loft::Surface;

/// The most scallops along a lace edge.
pub const MAX_SCALLOPS: u32 = 512;
/// The width of the lace's texture tile (one scallop), pixels.
pub const TILE_PX: u32 = 128;
/// The strip's depth off its rail, in scallop radii: the scallop's disc plus the heading band.
pub const DEPTH: f64 = 1.2;
/// The alpha below which the lace is cut away.
pub const CUTOFF: f32 = 0.5;
/// Samples along the rail per scallop, at least.
const PER_SCALLOP: f64 = 4.0;

/// The threads filling each scallop.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LacePattern {
    /// A diagonal net.
    #[default]
    Net,
    /// A ring of eyelets with spokes.
    Eyelet,
    /// Small loops (picots) round the scallop's edge, over a net.
    Picot,
}

/// A scalloped lace edge along a rail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lace {
    /// Where its heading runs.
    pub rail: RailSpec,
    /// The radius of a scallop, metres; the strip is [`DEPTH`] of it deep.
    pub scallop_radius: f64,
    /// How many scallops along the rail, 1 to [`MAX_SCALLOPS`].
    pub scallop_count: u32,
    /// The threads in each scallop.
    #[serde(default)]
    pub pattern: LacePattern,
    /// The thread's colour.
    #[serde(default = "white")]
    pub colour: ColourRef,
    /// Hang the strip off the other side of the rail.
    #[serde(default)]
    pub flip: bool,
}

fn white() -> ColourRef {
    ColourRef::rgb(0xfa, 0xf6, 0xee)
}

impl Validate for Lace {
    fn check(&self, c: &mut Check) {
        c.nested("rail", &self.rail);
        feature(c, "scallop_radius", self.scallop_radius);
        if !(1..=MAX_SCALLOPS).contains(&self.scallop_count) {
            c.fail("scallop_count", format!("{}: a lace edge has 1 to {MAX_SCALLOPS} scallops", self.scallop_count));
        }
    }
}

/// The strip of a lace edge along `rail`: two points per rail sample, the rail and `depth` off it to `side` (or the
/// other side when `flip`), with `u` counting `repeats` tiles along it and `v` running from the rail (0) out (1).
///
/// # Errors
/// The kit's errors for a degenerate strip.
pub fn strip(rail: &Rail, depth: f64, repeats: f64, flip: bool) -> Result<MeshBuilder, TrimError> {
    let points = rail.points();
    let sign = if flip { -1.0 } else { 1.0 };
    let grid =
        Grid::from_fn(points.len(), 2, |i, j| points[i].point + points[i].side * (sign * depth * fraction(j, 1)))?;
    let inside = |i: usize, _: usize| points[i].point - points[i].normal;
    let length = rail.length();
    let uv = |i: usize, j: usize| DVec2::new(repeats * points[i].distance / length, fraction(j, 1));
    let skin = |_: DVec3, i: usize, _: usize| points[i].skin.clone();
    let options = GridOptions {
        wrap_i: rail.closed(),
        inside: Some(&inside),
        uv: Some(&uv),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    let mut builder = MeshBuilder::new();
    builder.add_grid(&grid, &options)?;
    Ok(builder)
}

/// One scallop of `pattern` in `colour` on a clear ground, in a plane of scallop radius 1: `x` from 0 to 2 across the
/// tile, `y` from 0 (the scallops' points) up to [`DEPTH`] (the rail). Returns the drawing and the rectangle it covers.
pub fn scallop(pattern: LacePattern, colour: Colour) -> (DrawList, FaceRect) {
    let mut list = DrawList::new();
    let paint = Paint::Solid(colour);
    #[allow(clippy::cast_possible_truncation)] // a small constant; the drawing plane is f32
    let depth = DEPTH as f32;
    let thread = 0.08;
    let v = Vec2::new;
    // The heading band along the rail.
    let band = list.path().polyline(&[v(0.0, 1.0), v(2.0, 1.0), v(2.0, depth), v(0.0, depth)]).close().finish();
    list.fill(band, paint);
    // The scallop: a disc under the band, its edge stroked, the pattern clipped inside it.
    let disc = list.path().ellipse(v(1.0, 1.0), 0.97, 0.97).finish();
    list.stroke(disc, paint, thread * 1.5);
    list.push(Cmd::PushClip(disc));
    match pattern {
        LacePattern::Net | LacePattern::Picot => net(&mut list, paint, thread),
        LacePattern::Eyelet => {
            for k in 0_u8..5 {
                let a = std::f32::consts::PI * (1.1 + 0.2 * f32::from(k));
                let c = v(1.0 + 0.6 * a.cos(), 1.0 + 0.6 * a.sin());
                let ring = list.path().ellipse(c, 0.14, 0.14).finish();
                list.stroke(ring, paint, thread);
                let spoke =
                    list.path().polyline(&[v(1.0, 1.0), v(1.0 + 0.97 * a.cos(), 1.0 + 0.97 * a.sin())]).finish();
                list.stroke(spoke, paint, thread * 0.7);
            }
        }
    }
    list.push(Cmd::PopClip);
    if pattern == LacePattern::Picot {
        for k in 0_u8..7 {
            let a = std::f32::consts::PI * (1.08 + 0.14 * f32::from(k));
            let dot = list.path().ellipse(v(1.0 + 0.86 * a.cos(), 1.0 + 0.86 * a.sin()), 0.07, 0.07).finish();
            list.fill(dot, paint);
        }
    }
    (list, FaceRect { x0: 0.0, x1: 2.0, y0: 0.0, y1: depth })
}

/// Diagonal threads both ways across the tile.
fn net(list: &mut DrawList, paint: Paint, thread: f32) {
    let step = 0.25_f32;
    for k in -8_i16..=16 {
        let x = step * f32::from(k);
        let up = list.path().polyline(&[Vec2::new(x, 0.0), Vec2::new(x + 2.0, 2.0)]).finish();
        list.stroke(up, paint, thread * 0.6);
        let down = list.path().polyline(&[Vec2::new(x, 2.0), Vec2::new(x + 2.0, 0.0)]).finish();
        list.stroke(down, paint, thread * 0.6);
    }
}

/// The texture of one scallop: [`TILE_PX`] wide, repeating along the rail, straight alpha for the mask.
pub fn texture_desc() -> TextureDesc {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small positive constant
    let height = (f64::from(TILE_PX) * DEPTH / 2.0).round() as u32;
    let mut desc = TextureDesc::colour(UVec2::new(TILE_PX, height));
    desc.sampler = Sampler { filter: Filter::Linear, wrap: Wrap::Repeat, anisotropy: 4 };
    desc
}

/// Adds the lace edge `spec` describes on `surface` to `out`: its strip, its scallop texture and an alpha-masked,
/// double-sided material, all named `<part>-<name>`.
///
/// # Errors
/// [`BuildError::Missing`] when the build has no rasteriser; [`BuildError::Colour`] for a palette slot the palette
/// lacks; [`BuildError::Part`] for a spec that fails validation or a rail without length.
pub fn add(
    out: &mut GarmentOutput,
    ctx: &BuildCtx<'_>,
    surface: &Surface,
    name: &str,
    spec: &Lace,
) -> Result<(), BuildError> {
    let fail = |e: TrimError| ctx.fail(format!("trim `{name}`: {e}"));
    checked(spec).map_err(fail)?;
    let length = Rail::length_on(surface, &spec.rail).map_err(fail)?;
    let step = (length / (f64::from(spec.scallop_count) * PER_SCALLOP)).min(spec.rail.step);
    let rail = Rail::from_polyline_on_surface(surface, &spec.rail, step).map_err(fail)?;
    let mesh = strip(&rail, DEPTH * spec.scallop_radius, f64::from(spec.scallop_count), spec.flip).map_err(fail)?;
    let (list, rect) = scallop(spec.pattern, ctx.colour(&spec.colour)?);
    let desc = texture_desc();
    let texture = ctx.paint_texture(desc.size, rect, &list, desc)?;
    let texture = out.texture(ctx, name, texture)?;
    let material = MaterialSpec {
        base_texture: Some(texture),
        alpha: AlphaMode::Mask { cutoff: CUTOFF },
        double_sided: true,
        outline: None,
        ..MaterialSpec::toon(Colour::rgb(255, 255, 255))
    };
    let material = out.material(ctx, name, material)?;
    out.mesh(ctx, name, mesh, material)
}
