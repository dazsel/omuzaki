//! Emblem decals: a picture laid on the garment as a sheet that follows the surface, just off it so the two never
//! z-fight.
//!
//! The picture is either an image from the asset store or a vector drawing, which is rasterised at the size the decal
//! needs (denser for an offline build), so it stays crisp close up. The sheet is a grid over the surface round its
//! anchor, each point placed by the surface itself at [`LIFT`] off it, so the decal bends with the garment.
//!
//! **Image assets** are raw RGBA: the 8 bytes [`IMAGE_MAGIC`], the width and the height as little-endian `u32`s, then
//! `width * height * 4` bytes of straight-alpha sRGB pixels, rows top to bottom, each side at most [`MAX_DECAL_PX`].

use omuzaki_char_api::{AnchorId, AnchorsFact, BuildCtx, BuildError, MaterialSpec, PartId, Quality, TextureData};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DVec2, DVec3, UVec2};
use omuzaki_face_api::{Check, DrawList, FaceRect, Validate};
use omuzaki_render_api::{AlphaMode, TextureDesc};
use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{TrimError, checked, feature, position, rates};
use crate::kit::curve::fraction;
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::outfit::common::GarmentOutput;
use crate::outfit::loft::Surface;

/// How far a decal sits off the surface, metres: enough that depth testing never mixes the two, and within half a
/// millimetre of it.
pub const LIFT: f64 = 0.0003;
/// The first bytes of a raw RGBA decal image.
pub const IMAGE_MAGIC: &[u8; 8] = b"OMZRGBA8";
/// The largest side of a decal's texture, pixels.
pub const MAX_DECAL_PX: u32 = 2048;
/// The density a drawn decal is rasterised at for a live build, pixels per metre (0.1 mm a pixel).
pub const LIVE_PX_PER_M: f64 = 10_000.0;
/// The density for an offline build.
pub const OFFLINE_PX_PER_M: f64 = 25_000.0;
/// Cells each way across a decal's sheet.
pub const CELLS: usize = 12;

/// Where a decal's picture comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecalSource {
    /// A raw RGBA image in the asset store (the module docs give the format).
    Image {
        /// The asset.
        hash: ContentHash,
    },
    /// A vector drawing.
    Drawing {
        /// The drawing, in metres.
        #[schemars(with = "serde_json::Value")]
        list: DrawList,
        /// The part of the drawing's plane the decal shows.
        extent: FaceRect,
    },
}

/// Where a decal is centred.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecalAnchor {
    /// A point of the surface.
    Surface {
        /// `[t, phi]`.
        at: [f64; 2],
    },
    /// The point of the surface nearest an anchor another part published.
    Anchor {
        /// The part that published it.
        part: PartId,
        /// Its name.
        anchor: AnchorId,
    },
}

/// A picture on the surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Decal {
    /// The picture.
    pub source: DecalSource,
    /// Where its centre is.
    pub anchor: DecalAnchor,
    /// Its width and height on the surface, metres.
    pub size: [f64; 2],
    /// A turn of the picture about the surface normal, degrees, counter-clockwise seen from outside.
    #[serde(default)]
    pub rotation: f64,
    /// How opaque it is, 0 to 1.
    #[serde(default = "opaque")]
    pub opacity: f64,
}

fn opaque() -> f64 {
    1.0
}

impl Validate for Decal {
    fn check(&self, c: &mut Check) {
        feature(c, "size/0", self.size[0]);
        feature(c, "size/1", self.size[1]);
        c.range("rotation", self.rotation, -360.0, 360.0);
        c.range("opacity", self.opacity, 0.0, 1.0);
        if let DecalAnchor::Surface { at } = &self.anchor {
            position(c, "anchor/at", *at);
        }
        if let DecalSource::Drawing { extent, .. } = &self.source
            && !(extent.width() > 0.0 && extent.height() > 0.0)
        {
            c.fail("source/extent", "the drawing's extent must have a positive width and height");
        }
    }
}

/// The sheet of a decal of `size` metres turned `rotation` degrees, centred at `[t, phi]` on `surface`: a grid placed by
/// the surface, [`LIFT`] off it, with UVs from the picture's top left.
///
/// # Errors
/// The surface's errors; [`TrimError::Invalid`] where the surface has no extent to lay the sheet on, or where the sheet
/// would reach past either end of the region.
pub fn sheet(surface: &Surface, at: [f64; 2], size: [f64; 2], rotation: f64) -> Result<MeshBuilder, TrimError> {
    // How far the surface moves per unit of t and of phi at the centre: the first-order map from the tangent plane.
    let (per_t, per_phi) = rates(surface, at)?;
    let (sin, cos) = rotation.to_radians().sin_cos();
    let local = |i: usize, j: usize| {
        let q = DVec2::new((fraction(j, CELLS) - 0.5) * size[0], (0.5 - fraction(i, CELLS)) * size[1]);
        DVec2::new(q.x * cos - q.y * sin, q.x * sin + q.y * cos)
    };
    let mut points = Vec::with_capacity((CELLS + 1) * (CELLS + 1));
    let mut rows = Vec::with_capacity((CELLS + 1) * (CELLS + 1));
    for i in 0..=CELLS {
        for j in 0..=CELLS {
            let q = local(i, j);
            let (t, phi) = (at[0] + q.y / per_t, at[1] + q.x / per_phi);
            // Clamping would fold the rows past the end onto it: a sheet must fit on the region.
            if !(0.0..=1.0).contains(&t) {
                return Err(TrimError::invalid(
                    "decal",
                    format!("its sheet reaches t = {t:.3}, past the end of the region: move it or make it smaller"),
                ));
            }
            let frame = surface.at(t, phi)?;
            points.push(frame.point + frame.normal * LIFT);
            rows.push(t);
        }
    }
    // The sheet is small beside its section: its ring's centre is inside it everywhere.
    let middle = surface.ring(at[0])?.centre;
    let grid = Grid::new(CELLS + 1, CELLS + 1, points)?;
    let inside = |_: usize, _: usize| middle;
    let uv = |i: usize, j: usize| DVec2::new(fraction(j, CELLS), fraction(i, CELLS));
    // Each vertex skinned as the garment is where it lies, so the sheet bends with the cloth across a joint.
    let weights = surface.weights();
    let per_vertex = |_: DVec3, i: usize, j: usize| weights.at(rows[i * (CELLS + 1) + j]);
    let options =
        GridOptions { inside: Some(&inside), uv: Some(&uv), skin: Skin::Per(&per_vertex), ..GridOptions::default() };
    let mut builder = MeshBuilder::new();
    builder.add_grid(&grid, &options)?;
    Ok(builder)
}

/// Reads a raw RGBA decal image (the module docs give the format).
///
/// # Errors
/// [`TrimError::Invalid`] for bytes that aren't one, or an image larger than [`MAX_DECAL_PX`] on a side.
pub fn read_image(bytes: &[u8]) -> Result<(UVec2, Vec<u8>), TrimError> {
    let bad = |m: String| TrimError::invalid("decal image", m);
    let header = IMAGE_MAGIC.len() + 8;
    if bytes.len() < header || &bytes[..IMAGE_MAGIC.len()] != IMAGE_MAGIC {
        return Err(bad("not a raw RGBA image: it must start with OMZRGBA8, the width and the height".to_owned()));
    }
    let word = |at: usize| {
        let mut w = [0_u8; 4];
        w.copy_from_slice(&bytes[at..at + 4]);
        u32::from_le_bytes(w)
    };
    let size = UVec2::new(word(IMAGE_MAGIC.len()), word(IMAGE_MAGIC.len() + 4));
    if size.x == 0 || size.y == 0 || size.x > MAX_DECAL_PX || size.y > MAX_DECAL_PX {
        return Err(bad(format!("{}x{} px: each side must be 1 to {MAX_DECAL_PX}", size.x, size.y)));
    }
    let pixels = usize::try_from(u64::from(size.x) * u64::from(size.y) * 4).map_err(|_| bad("too large".to_owned()))?;
    if bytes.len() != header + pixels {
        return Err(bad(format!(
            "{} bytes of pixels for {}x{} px (needs {pixels})",
            bytes.len() - header,
            size.x,
            size.y
        )));
    }
    Ok((size, bytes[header..].to_vec()))
}

/// The pixel size a drawing of `extent` is rasterised at for `quality`, its aspect kept.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // positive sizes, clamped
fn drawing_px(extent: &FaceRect, size: [f64; 2], quality: Quality) -> UVec2 {
    let density = match quality {
        Quality::Live => LIVE_PX_PER_M,
        Quality::Offline => OFFLINE_PX_PER_M,
    };
    let width = (size[0] * density).round().clamp(1.0, f64::from(MAX_DECAL_PX));
    let height = (width * f64::from(extent.height()) / f64::from(extent.width())).round();
    if height > f64::from(MAX_DECAL_PX) {
        let width = (width * f64::from(MAX_DECAL_PX) / height).round().max(1.0);
        return UVec2::new(width as u32, MAX_DECAL_PX);
    }
    UVec2::new(width as u32, height.max(1.0) as u32)
}

/// Where the decal is centred on `surface`.
fn anchor_at(ctx: &BuildCtx<'_>, surface: &Surface, anchor: &DecalAnchor) -> Result<[f64; 2], BuildError> {
    match anchor {
        DecalAnchor::Surface { at } => Ok(*at),
        DecalAnchor::Anchor { part, anchor } => {
            let published = ctx.published.get::<AnchorsFact>();
            let found = published
                .get(&(part.clone(), anchor.clone()))
                .ok_or_else(|| ctx.fail(format!("no part published the anchor `{part}`/`{anchor}` for the decal")))?;
            let at = surface
                .project(found.position.as_dvec3())
                .map_err(|e| ctx.fail(e.to_string()))?
                .ok_or_else(|| ctx.fail(format!("the anchor `{part}`/`{anchor}` isn't over the decal's surface")))?;
            Ok([at.t, at.phi])
        }
    }
}

/// Adds the decal `spec` describes on `surface` to `out`: its sheet, its texture and a blended material, all named
/// `<part>-<name>`.
///
/// # Errors
/// [`BuildError::Asset`] for an image the build can't read (no asset source among them: a store image needs one);
/// [`BuildError::Missing`] for a drawing when the build has no rasteriser; [`BuildError::Part`] for a spec that fails
/// validation, an image that isn't a raw RGBA image, or an anchor nobody published.
pub fn add(
    out: &mut GarmentOutput,
    ctx: &BuildCtx<'_>,
    surface: &Surface,
    name: &str,
    spec: &Decal,
) -> Result<(), BuildError> {
    let fail = |e: TrimError| ctx.fail(format!("trim `{name}`: {e}"));
    checked(spec).map_err(fail)?;
    let texture = match &spec.source {
        DecalSource::Image { hash } => {
            let bytes = ctx.asset_or_fail(hash)?;
            let (size, rgba8) = read_image(&bytes).map_err(fail)?;
            TextureData { desc: TextureDesc::colour(size), rgba8 }
        }
        DecalSource::Drawing { list, extent } => {
            let size = drawing_px(extent, spec.size, ctx.quality);
            ctx.paint_texture(size, *extent, list, TextureDesc::colour(size))?
        }
    };
    let at = anchor_at(ctx, surface, &spec.anchor)?;
    let mesh = sheet(surface, at, spec.size, spec.rotation).map_err(fail)?;
    let texture = out.texture(ctx, name, texture)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // opacity is in 0..=1
    let alpha = (spec.opacity * 255.0).round() as u8;
    let material = MaterialSpec {
        base_texture: Some(texture),
        alpha: AlphaMode::Blend,
        outline: None,
        ..MaterialSpec::toon(Colour::rgba(255, 255, 255, alpha))
    };
    let material = out.material(ctx, name, material)?;
    out.mesh(ctx, name, mesh, material)
}
