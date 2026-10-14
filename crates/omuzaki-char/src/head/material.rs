//! The skin materials of the head part: the face (with the painted-face overlay), the ears and the neck.
//!
//! The numbers are the prototype's `toon()` options in `head.js`: a toned-down terminator (`shift` -0.28, `soft` 0.28) and
//! a faint rim so an anime face reads nearly flat-lit, a brightness floor (`HEAD_FLOOR`) higher than the body's so the
//! chin underside is never muddy, and no shadows on the face. Colours come from the head's palette slots
//! ([`HeadParams::skin`]) so head, body and ears always agree, and a palette change recolours them.

use omuzaki_char_api::{BuildCtx, BuildError, FaceOverlay, HeadParams, MaterialName, MaterialSpec};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::FaceLayerId;
use omuzaki_render_api::{AlphaMode, Outline, Rim, Shading, Skin, Toon};

/// Anime faces read nearly flat-lit: the head's brightness floor, higher than the body's skin ([`Skin::default`]).
pub const HEAD_FLOOR: f32 = 0.66;
/// How much of the skin colour the lit side keeps under coloured light.
const HUE_KEEP: f32 = 0.55;
/// The face outline is the skin's line colour lightened this far toward the skin: the prototype draws the face's line
/// softer (`#d49a8e`) than the ears' (`#c98a80`), so the face is not framed in a hard line.
const FACE_LINE_LIGHTEN: f32 = 0.215;
/// Outline widths in metres at the renderer's reference distance, from the prototype.
const HEAD_OUTLINE: f32 = 0.0014;
const EAR_OUTLINE: f32 = 0.0016;
/// The rim power every skin material uses.
const RIM_POWER: f32 = 3.0;

/// `name` prefixed by the part id, the convention for material names (`MaterialName`).
///
/// # Errors
/// [`BuildError::Part`] if the result is not a valid slug (a bug in the caller's literal).
pub fn name(ctx: &BuildCtx<'_>, name: &str) -> Result<MaterialName, BuildError> {
    MaterialName::new(format!("{}-{name}", ctx.part)).map_err(|e| ctx.fail(format!("material name: {e}")))
}

/// `from` moved `t` of the way to `to`, per channel in sRGB (how an artist would lighten a line colour).
fn lighten(from: Colour, to: Colour, t: f32) -> Colour {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a blend of two bytes is a byte
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Colour::rgb(mix(from.r, to.r), mix(from.g, to.g), mix(from.b, to.b))
}

/// The toon shading shared by the skin materials, from the head's colours.
fn toon(ctx: &BuildCtx<'_>, p: &HeadParams, shift: f32, softness: f32, rim_strength: f32) -> Result<Toon, BuildError> {
    Ok(Toon {
        shade: ctx.colour(&p.skin.shade)?,
        shift,
        softness,
        rim: Rim { colour: ctx.colour(&p.skin.rim)?, power: RIM_POWER, strength: rim_strength },
        emissive: Colour::BLACK,
        skin: Some(Skin { floor: HEAD_FLOOR, hue_keep: HUE_KEEP }),
    })
}

/// The face (`skin-head`): lit flat, no shadows, a soft coloured outline, and the painted face composited over it.
/// `head_centre` is the head centre in bind space, where the face window is placed.
///
/// # Errors
/// [`BuildError::Colour`] if a skin slot is missing from the palette.
pub fn skin_head(ctx: &BuildCtx<'_>, p: &HeadParams, head_centre: Vec3) -> Result<MaterialSpec, BuildError> {
    let (base, line) = (ctx.colour(&p.skin.base)?, ctx.colour(&p.skin.line)?);
    Ok(MaterialSpec {
        base,
        base_texture: None,
        shading: Shading::Toon(toon(ctx, p, -0.28, 0.28, 0.10)?),
        overlays: vec![FaceOverlay::of(&p.face_projection(), head_centre, FaceLayerId::Base)],
        alpha: AlphaMode::Opaque,
        double_sided: false,
        vertex_colours: false,
        outline: Some(Outline { colour: lighten(line, base, FACE_LINE_LIGHTEN), width: HEAD_OUTLINE }),
        receives_shadows: false,
    })
}

/// The ears (`skin-ear`): the skin with a little less shading, drawn on both sides because the shell is a thin sheet.
///
/// # Errors
/// [`BuildError::Colour`] if a skin slot is missing from the palette.
pub fn skin_ear(ctx: &BuildCtx<'_>, p: &HeadParams) -> Result<MaterialSpec, BuildError> {
    Ok(MaterialSpec {
        base: ctx.colour(&p.skin.base)?,
        base_texture: None,
        shading: Shading::Toon(Toon {
            skin: Some(Skin { floor: Skin::default().floor, hue_keep: HUE_KEEP }),
            ..toon(ctx, p, -0.1, 0.2, 0.08)?
        }),
        overlays: Vec::new(),
        alpha: AlphaMode::Opaque,
        double_sided: true,
        vertex_colours: false,
        outline: Some(Outline { colour: ctx.colour(&p.skin.line)?, width: EAR_OUTLINE }),
        receives_shadows: true,
    })
}

/// The neck (`skin-neck`): skin without rim light or outline, mostly hidden by the collar and the chin.
///
/// # Errors
/// [`BuildError::Colour`] if a skin slot is missing from the palette.
pub fn skin_neck(ctx: &BuildCtx<'_>, p: &HeadParams) -> Result<MaterialSpec, BuildError> {
    let mut neck = MaterialSpec::toon(ctx.colour(&p.skin.base)?);
    neck.shading = Shading::Toon(Toon {
        shade: ctx.colour(&p.skin.shade)?,
        shift: 0.1,
        softness: 0.2,
        rim: Rim { strength: 0.0, ..Rim::default() },
        ..Toon::default()
    });
    Ok(neck)
}
