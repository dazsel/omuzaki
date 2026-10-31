//! The skin materials of the head part: the face (with the painted-face overlay), the ears and the neck; and the fur and
//! inner panel of animal ears.
//!
//! The numbers are the prototype's `toon()` options in `head.js`: a toned-down terminator (`shift` -0.28, `soft` 0.28) and
//! a faint rim so an anime face reads nearly flat-lit, a brightness floor (`HEAD_FLOOR`) higher than the body's so the
//! chin underside is never muddy, and no shadows on the face. Colours come from the head's palette slots
//! ([`HeadParams::skin`]) so head, body and ears always agree, and a palette change recolours them.

use omuzaki_char_api::{AnimalEars, BuildCtx, BuildError, FaceOverlay, HeadParams, MaterialName, MaterialSpec};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::{ColourRef, FaceLayerId};
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

/// Resolves a colour reference through the palette of a build ([`BuildCtx::colour`]) or of a recolour
/// (`RecolourCtx::colour`): the materials are made by the same functions either way, so the two cannot drift apart.
pub type Resolve<'a> = &'a dyn Fn(&ColourRef) -> Result<Colour, BuildError>;

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
fn toon(colour: Resolve<'_>, p: &HeadParams, shift: f32, softness: f32, rim_strength: f32) -> Result<Toon, BuildError> {
    Ok(Toon {
        shade: colour(&p.skin.shade)?,
        shift,
        softness,
        rim: Rim { colour: colour(&p.skin.rim)?, power: RIM_POWER, strength: rim_strength },
        emissive: Colour::BLACK,
        skin: Some(Skin { floor: HEAD_FLOOR, hue_keep: HUE_KEEP }),
    })
}

/// The painted face's overlays on the skin: the face window placed at `head_centre` (the head centre in bind space). The
/// overlay (sweat drop, gloom hatch) goes on the skin and on the hair, so it shows on whichever is in front. Geometry,
/// not colour: a recolour keeps the ones the build made.
pub fn face_overlays(p: &HeadParams, head_centre: Vec3) -> Vec<FaceOverlay> {
    [FaceLayerId::Base, FaceLayerId::Overlay]
        .map(|layer| FaceOverlay::of(&p.face_projection(), head_centre, layer))
        .to_vec()
}

/// The face (`skin-head`): lit flat, no shadow maps, the hair's own shadow under the fringe (#2528), a soft coloured
/// outline, and the painted face composited over it (`overlays`, from [`face_overlays`]).
///
/// # Errors
/// [`BuildError::Colour`] if a skin slot is missing from the palette.
pub fn skin_head(colour: Resolve<'_>, p: &HeadParams, overlays: Vec<FaceOverlay>) -> Result<MaterialSpec, BuildError> {
    let (base, line) = (colour(&p.skin.base)?, colour(&p.skin.line)?);
    Ok(MaterialSpec {
        base,
        base_texture: None,
        shading: Shading::Toon(toon(colour, p, -0.28, 0.28, 0.10)?),
        overlays,
        painted: None,
        alpha: AlphaMode::Opaque,
        order_independent: false,
        double_sided: false,
        vertex_colours: false,
        outline: Some(Outline { colour: lighten(line, base, FACE_LINE_LIGHTEN), width: HEAD_OUTLINE }),
        receives_shadows: false,
        hair_shadow: omuzaki_render_api::HairShadow::RECEIVES,
        uv_transform: omuzaki_render_api::UvTransform::IDENTITY,
    })
}

/// The ears (`skin-ear`): the skin with a little less shading, drawn on both sides because the shell is a thin sheet.
///
/// # Errors
/// [`BuildError::Colour`] if a skin slot is missing from the palette.
pub fn skin_ear(colour: Resolve<'_>, p: &HeadParams) -> Result<MaterialSpec, BuildError> {
    Ok(MaterialSpec {
        base: colour(&p.skin.base)?,
        base_texture: None,
        shading: Shading::Toon(Toon {
            skin: Some(Skin { floor: Skin::default().floor, hue_keep: HUE_KEEP }),
            ..toon(colour, p, -0.1, 0.2, 0.08)?
        }),
        overlays: Vec::new(),
        painted: None,
        alpha: AlphaMode::Opaque,
        order_independent: false,
        double_sided: true,
        vertex_colours: false,
        outline: Some(Outline { colour: colour(&p.skin.line)?, width: EAR_OUTLINE }),
        receives_shadows: true,
        hair_shadow: omuzaki_render_api::HairShadow::NONE,
        uv_transform: omuzaki_render_api::UvTransform::IDENTITY,
    })
}

/// The neck (`skin-neck`): skin without rim light or outline, mostly hidden by the collar and the chin. It takes the
/// skin treatment the ears and the body take, so coloured light leaves it the skin's hue like them instead of painting a
/// saturated band between the chin and the chest.
///
/// # Errors
/// [`BuildError::Colour`] if a skin slot is missing from the palette.
pub fn skin_neck(colour: Resolve<'_>, p: &HeadParams) -> Result<MaterialSpec, BuildError> {
    let mut neck = MaterialSpec::toon(colour(&p.skin.base)?);
    neck.shading = Shading::Toon(Toon {
        shade: colour(&p.skin.shade)?,
        shift: 0.1,
        softness: 0.2,
        rim: Rim { strength: 0.0, ..Rim::default() },
        skin: Some(Skin { floor: Skin::default().floor, hue_keep: HUE_KEEP }),
        ..Toon::default()
    });
    Ok(neck)
}

/// The animal ears' colours while the palette has no `hair_mid` or `ear_inner` slot: the kit's hair brown and a soft
/// pink.
const FUR: Colour = Colour::rgb(0x8a, 0x5a, 0x44);
const INNER: Colour = Colour::rgb(0xf0, 0xa8, 0xa8);

/// `colour` from the palette; when it is still the default slot reference (`default`) and the palette has no such slot,
/// `fallback`. A slot the person chose must exist.
fn defaulted(
    resolve: Resolve<'_>,
    colour: &ColourRef,
    default: &ColourRef,
    fallback: Colour,
) -> Result<Colour, BuildError> {
    match resolve(colour) {
        Err(_) if colour == default => Ok(fallback),
        resolved => resolved,
    }
}

/// How far an animal ear's fur shade and line move from its colour toward black.
const FUR_SHADE: f32 = 0.15;
/// How far the lit fur moves from its colour toward white.
const FUR_LIFT: f32 = 0.12;
const FUR_LINE: f32 = 0.5;

/// An animal ear's fur (`fur-ear`): the fur colour ([`omuzaki_char_api::AnimalEars::fur`], the hair's mid colour by
/// default) in the hair's soft toon, with a darker line, so the ears read as part of the hair.
///
/// # Errors
/// [`BuildError::Colour`] if the fur colour's slot is missing from the palette.
pub fn fur_ear(colour: Resolve<'_>, p: &HeadParams) -> Result<MaterialSpec, BuildError> {
    let fur = defaulted(colour, &p.ears.animal.fur, &AnimalEars::default().fur, FUR)?;
    // The hair is drawn lighter than its mid colour where lit; the fur is lifted the same way so the two read as one.
    let mut spec = MaterialSpec::toon(lighten(fur, Colour::WHITE, FUR_LIFT));
    spec.shading = Shading::Toon(Toon {
        shade: lighten(fur, Colour::BLACK, FUR_SHADE),
        shift: 0.0,
        softness: 0.15,
        rim: Rim { colour: lighten(fur, Colour::WHITE, 0.5), power: RIM_POWER, strength: 0.05 },
        emissive: Colour::BLACK,
        skin: None,
    });
    spec.outline = Some(Outline { colour: lighten(fur, Colour::BLACK, FUR_LINE), width: EAR_OUTLINE });
    spec.double_sided = false;
    Ok(spec)
}

/// An animal ear's inner panel (`ear-inner`): the inner colour, softly shaded, no outline, seen from either side.
///
/// # Errors
/// [`BuildError::Colour`] if the inner colour's slot is missing from the palette.
pub fn ear_inner(colour: Resolve<'_>, p: &HeadParams) -> Result<MaterialSpec, BuildError> {
    let inner = defaulted(colour, &p.ears.animal.inner, &AnimalEars::default().inner, INNER)?;
    let mut spec = MaterialSpec::toon(inner);
    spec.shading = Shading::Toon(Toon {
        shade: lighten(inner, Colour::BLACK, FUR_SHADE),
        shift: 0.1,
        softness: 0.25,
        rim: Rim { strength: 0.0, ..Rim::default() },
        emissive: Colour::BLACK,
        skin: None,
    });
    spec.outline = None;
    spec.double_sided = true;
    Ok(spec)
}
