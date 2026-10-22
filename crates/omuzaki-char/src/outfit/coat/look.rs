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

use super::shell::Tones;
use crate::outfit::common::{FabricStyle, GarmentCommon};
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
