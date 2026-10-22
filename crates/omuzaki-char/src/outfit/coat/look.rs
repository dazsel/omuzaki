//! How a coat looks: its colours and its cloth's toon material.
//!
//! The shell is one mesh whose outside, lining and rolled edges differ in colour, and the cloth simulation needs them
//! in one mesh: so a plain cloth's material is white and the vertex colours carry the colours. A patterned cloth keeps
//! its texture and the vertex colours shade it.

use omuzaki_char_api::{BuildCtx, BuildError, MaterialClass, MaterialSpec, TextureName};
use omuzaki_core::colour::Colour;
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::{Outline, Rim, Shading, Toon};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::shell::{Frame, Tones};
use super::trims::{self, FrontBand};
use crate::kit::grid::MeshBuilder;
use crate::outfit::common::{FabricStyle, GarmentCommon, GarmentOutput, fabric_tile};
use crate::outfit::trim::Finish;

/// The cloth's terminator shift and softness.
const SHIFT_SOFTNESS: (f32, f32) = (-0.1, 0.22);
/// The rim light's strength and power.
const RIM: (f32, f32) = (0.16, 2.6);
/// The outline's width, metres at two metres.
const OUTLINE: f32 = 0.0016;
/// The metal pieces' outline.
const METAL_LINE: f32 = 0.0009;

/// A coat's colours besides its cloth (`common.fabric`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct CoatColours {
    /// The lining.
    pub lining: ColourRef,
    /// The rolled edges, the lapels' rolls and the cords.
    pub trim: ColourRef,
    /// The cloth's shadow tint (multiplies the lit colour).
    pub shade: ColourRef,
    /// The cloth's rim light.
    pub rim: ColourRef,
    /// The outline.
    pub line: ColourRef,
}

impl Default for CoatColours {
    fn default() -> Self {
        Self {
            lining: ColourRef::rgb(0x8a, 0x84, 0x7c),
            trim: ColourRef::rgb(0x3a, 0x38, 0x40),
            shade: ColourRef::rgb(0x8c, 0x88, 0xa0),
            rim: ColourRef::rgb(0x9a, 0xa6, 0xc8),
            line: ColourRef::rgb(0x14, 0x12, 0x18),
        }
    }
}

impl Validate for CoatColours {
    fn check(&self, _: &mut Check) {}
}

/// The colour as linear RGB for a vertex.
fn linear(c: Colour) -> [f32; 3] {
    c.to_linear().to_array()
}

/// The tones of the shell for `common`'s cloth and `colours`: the outside's colour (white over a texture, which
/// carries its own), the lining's and the trim's.
///
/// # Errors
/// [`BuildError::Colour`] for a palette slot the palette lacks.
pub fn tones(ctx: &BuildCtx<'_>, common: &GarmentCommon, colours: &CoatColours) -> Result<Tones, BuildError> {
    let outer = match &common.fabric {
        FabricStyle::Plain { colour } => linear(ctx.colour(colour)?),
        _ => [1.0; 3],
    };
    Ok(Tones { outer, lining: linear(ctx.colour(&colours.lining)?), edge: linear(ctx.colour(&colours.trim)?) })
}

/// The cloth's material: white (or the fabric's texture, `<part>-fabric`, when `textured`) shaded by the vertex
/// colours, with the coat's toon.
///
/// # Errors
/// [`BuildError::Colour`] for a palette slot the palette lacks; [`BuildError::Part`] for a bad texture name.
pub fn cloth(ctx: &BuildCtx<'_>, colours: &CoatColours, textured: bool) -> Result<MaterialSpec, BuildError> {
    let mut spec = if textured {
        let name = format!("{}-fabric", ctx.part.as_str());
        let texture =
            name.parse::<TextureName>().map_err(|e| ctx.fail(format!("`{name}` isn't a texture name: {e}")))?;
        MaterialSpec { base_texture: Some(texture), ..MaterialSpec::toon(Colour::rgb(255, 255, 255)) }
    } else {
        MaterialSpec::toon(Colour::rgb(255, 255, 255))
    };
    spec.shading = Shading::Toon(Toon {
        shade: ctx.colour(&colours.shade)?,
        shift: SHIFT_SOFTNESS.0,
        softness: SHIFT_SOFTNESS.1,
        rim: Rim { colour: ctx.colour(&colours.rim)?, strength: RIM.0, power: RIM.1 },
        ..Toon::default()
    });
    spec.vertex_colours = true;
    spec.outline = Some(Outline { colour: ctx.colour(&colours.line)?, width: OUTLINE });
    Ok(spec)
}

/// The material of metal pieces (buttons, clasps, aglets) in `finish`, and its class.
///
/// # Errors
/// [`BuildError::Colour`] for a palette slot the palette lacks; [`BuildError::Part`] for a metal the library refuses.
pub fn metal(ctx: &BuildCtx<'_>, finish: &Finish, line: Colour) -> Result<(MaterialSpec, MaterialClass), BuildError> {
    let base = ctx.colour(&finish.colour)?;
    let (mut spec, class) = match &finish.metal {
        Some(metal) => {
            let highlight = ctx.colour(&metal.highlight)?;
            (metal.material(base, highlight).map_err(|e| ctx.fail(format!("metal: {e}")))?, MaterialClass::Metal)
        }
        None => (MaterialSpec::toon(base), MaterialClass::Cloth),
    };
    spec.outline = Some(Outline { colour: line, width: METAL_LINE });
    Ok((spec, class))
}

/// Adds a band `width` wide along both front edges of an open front (`<part>-band`), from the hem or from height
/// `from` up, in its own material: `fabric`
/// painted with the band's width as the repeat, laid with its length along the band (`<part>-band` texture), or plain.
///
/// # Errors
/// [`BuildError::Part`] for a pattern that paints nothing or a mesh the kit refuses; [`BuildError::Colour`].
pub fn front_band(
    ctx: &BuildCtx<'_>,
    out: &mut GarmentOutput,
    frame: &Frame,
    (width, from): (f64, Option<f64>),
    fabric: &FabricStyle,
    colours: &CoatColours,
    tones: &Tones,
) -> Result<(), BuildError> {
    let fabric = with_period(fabric, width);
    let tile = fabric_tile(&fabric, 1, |c| ctx.colour(c), |m| ctx.fail(m))?;
    let mut spec = cloth(ctx, colours, false)?;
    let (top, textured) = match (&fabric, tile) {
        (FabricStyle::Plain { colour }, _) => (ctx.colour(colour)?.to_linear().to_array(), false),
        (_, Some(tile)) => {
            let data = tile.paint(|size, rect, list, desc| ctx.paint_texture(size, rect, list, desc))?;
            spec.base_texture = Some(out.texture(ctx, "band", data)?);
            ([1.0; 3], true)
        }
        (_, None) => return Err(ctx.fail("the band's pattern painted no tile")),
    };
    let band = FrontBand { width, colours: (top, tones.lining, tones.edge), textured, from };
    let mut mesh = MeshBuilder::with_colours();
    trims::front_band(&mut mesh, frame, &band).map_err(|e| ctx.fail(e.to_string()))?;
    let material = out.material(ctx, "band", spec)?;
    out.mesh(ctx, "band", mesh, material)
}

/// `fabric` with its repeat set to `period` (a band's pattern repeats once per width along it).
fn with_period(fabric: &FabricStyle, period: f64) -> FabricStyle {
    let mut fabric = fabric.clone();
    match &mut fabric {
        FabricStyle::Interlace { period: p, .. } | FabricStyle::Damask { period: p, .. } => *p = period,
        FabricStyle::Plain { .. }
        | FabricStyle::Weave { .. }
        | FabricStyle::Stripes { .. }
        | FabricStyle::Plaid { .. } => {}
    }
    fabric
}
