//! Materials between the renderer's toon `Material` and the file's `MToon` fields, with a finding for everything the
//! renderer cannot draw.
//!
//! The character kit holds a [`MaterialSpec`] (the renderer's `Material` with named textures); a scene holds an
//! `io::Material` whose `Toon` mirrors `VRMC_materials_mtoon` 1.0. [`export_material`] goes one way,
//! [`import_material`] the other. Colours are 8-bit sRGB on the kit side and linear `f32` in the file
//! ([`super::colour`]); that conversion is exact for every byte, so a palette colour survives a trip, while an
//! imported file colour is rounded to the nearest byte.
//!
//! # Field table (toon materials)
//!
//! | Renderer | `MToon` / glTF | Mapping | Round trip |
//! |---|---|---|---|
//! | `base` | `baseColorFactor` | sRGB bytes to linear, alpha as is | exact |
//! | `base_texture` | `baseColorTexture` and `shadeMultiplyTexture` | the same texture in both slots (see `shade`) | exact |
//! | `shade` | `shadeColorFactor` | **`shade × base`**: the renderer's shade is a *multiplier* of the base, `MToon`'s is the absolute colour of the shaded side | exact for base channels above 0; see below |
//! | `shift` | `shadingShiftFactor` | negated: `MToon` adds the shift to `N·L`, the renderer moves the terminator to `N·L = shift` | exact |
//! | `softness` | `shadingToonyFactor` | `1 - softness`: `MToon`'s 1 is a hard edge, the renderer's 0 | exact on a 2^-k grid, else within 3e-8 |
//! | `rim.colour × rim.strength` | `parametricRimColorFactor` | one colour, capped at 1 | colour only; see below |
//! | `rim.power` | `parametricRimFresnelPowerFactor` | as is | exact |
//! | `emissive` | `emissiveFactor` | sRGB to linear | exact |
//! | `outline` | `outlineWidthMode = worldCoordinates`, `outlineWidthFactor`, `outlineColorFactor` | width in metres at 2 m, colour to linear | exact |
//! | `alpha`, `double_sided` | `alphaMode`, `alphaCutoff`, `doubleSided` | as is | exact |
//! | `Unlit` | `KHR_materials_unlit` | | exact |
//!
//! # What does not round trip
//!
//! - **`shade` where a base channel is 0.** The absolute shade colour is 0 there whatever the multiplier was, so an
//!   import cannot recover it and takes 1 (neutral, invisible). A shade brighter than the base in a channel
//!   (`MToon` allows it, a multiplier does not) is capped at 1 and reported.
//! - **`rim`.** The renderer has colour and strength, `MToon` one colour. Export writes `colour × strength`; import
//!   gives strength 1 and the colour rounded to a byte, so the *effective* rim matches to half a byte step, but a
//!   strength other than 1 does not come back. A product above 1 is capped and reported.
//! - **The alpha of `shade`, `rim`, `emissive` and outline colours** has no meaning in a file; imports are opaque.
//! - **File colours off the byte grid** are rounded when they become palette colours.
//!
//! # Findings
//!
//! Export (rules `material.*`): `program_dropped` (a shading program exports as the default toon look, which is also
//! what a renderer without the program draws; the kit's hair program instead exports as its mean strand colour and
//! toon, reported as `approximated`), `skin_dropped`, `vertex_colours_ignored` (`MToon` ignores them; the
//! mesh keeps its colours), `outline_dropped` (unlit materials), `not_exported` (face overlays, which the face baker
//! paints into the base texture first; shadow reception; the outline's distance scaling), `approximated` (a value
//! outside the file's range was capped), `texture_colour_space`. Import: `not_drawn` (the file's GI equalisation,
//! matcap, UV animation, render queue and the other fields the renderer has no counterpart for, one finding per
//! field), `approximated`, `texture_missing`. Locations are `<at>/<field>`.

use omuzaki_char_api::{HAIR_PROGRAM, MaterialSpec, TextureName};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{self as io, Finding, Report};
use omuzaki_render_api::{self as render, ColourSpace, Outline, Rim};

use super::colour::{from_linear, to_linear};
use super::texture::slot_colour_space;

/// The slot a base colour texture occupies, which fixes its colour space.
const BASE_SLOT: &str = "baseColorTexture";

/// The vertical field of view that turns `MToon`'s screen-height outline widths into metres: the renderer's default
/// lens (`Camera::default`), the one the reference distance of [`Outline`] is stated for.
const ASSUMED_FOV_Y_DEG: f32 = 30.0;

/// How far a value may differ from a default and still count as the default (file floats pass through JSON).
const DEFAULT_TOLERANCE: f32 = 1e-6;

/// A texture as the scene has it: where it is and which colour space its pixels are in.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SceneTexture {
    /// The scene texture and how a material reads it.
    pub tex: io::TexRef,
    /// The colour space of the pixels the scene stored (`TextureDesc::colour_space` of the source texture).
    pub colour_space: ColourSpace,
}

/// The material names a texture the scene does not have.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("material '{material}' uses texture '{texture}', which the scene doesn't have")]
pub struct MissingTexture {
    /// The material's name.
    pub material: String,
    /// The texture it names.
    pub texture: TextureName,
}

/// Findings for one material: every location starts with the material's own.
struct Notes<'a> {
    at: &'a str,
    report: &'a mut Report,
}

impl Notes<'_> {
    fn push(&mut self, finding: Finding, field: &str) {
        self.report.push(finding.at(format!("{}/{field}", self.at)));
    }

    fn info(&mut self, rule: &str, field: &str, message: impl Into<String>) {
        self.push(Finding::info(rule, message), field);
    }

    fn warn(&mut self, rule: &str, field: &str, message: impl Into<String>) {
        self.push(Finding::warning(rule, message), field);
    }

    /// `value` limited to `lo..=hi`, with a finding when it had to move (a NaN becomes `lo`).
    fn limit(&mut self, field: &str, value: f32, lo: f32, hi: f32) -> f32 {
        if (lo..=hi).contains(&value) {
            return value;
        }
        let limited = if value.is_nan() { lo } else { value.clamp(lo, hi) };
        self.warn(
            "material.approximated",
            field,
            format!("{field} is {value}, outside {lo}..={hi}; {limited} was used"),
        );
        limited
    }
}

/// True if `value` is `default` to within [`DEFAULT_TOLERANCE`].
fn is_default(value: f32, default: f32) -> bool {
    (value - default).abs() <= DEFAULT_TOLERANCE
}

/// The scene material for `spec`, named `name`. `textures` finds a named texture in the scene; `at` locates findings
/// (for example `materials/3`).
///
/// A toon material becomes an `MToon` one (the field table is in the module docs). A shading program becomes the
/// default toon look, since `MToon` cannot express it. Findings go to `report`.
///
/// # Errors
/// [`MissingTexture`] if the material names a texture `textures` does not know.
pub fn export_material(
    name: &str,
    spec: &MaterialSpec,
    textures: &dyn Fn(&TextureName) -> Option<SceneTexture>,
    at: &str,
    report: &mut Report,
) -> Result<io::Material, MissingTexture> {
    let mut notes = Notes { at, report };
    let base_texture = match &spec.base_texture {
        Some(texture) => Some(
            textures(texture).ok_or_else(|| MissingTexture { material: name.to_owned(), texture: texture.clone() })?,
        ),
        None => None,
    };
    if let Some(texture) = base_texture.filter(|t| t.colour_space != slot_colour_space(BASE_SLOT)) {
        let msg = format!("the base colour texture is {:?} but a base colour slot reads sRGB", texture.colour_space);
        notes.warn("material.texture_colour_space", "base_texture", msg);
    }
    let tex = base_texture.map(|t| t.tex);
    let mut base = to_linear(spec.base);

    let hair = match &spec.shading {
        render::Shading::Program { name, params } if name.as_str() == HAIR_PROGRAM => hair_look(params),
        _ => None,
    };
    let look = match (&spec.shading, hair) {
        (render::Shading::Toon(toon), _) => Some(*toon),
        (render::Shading::Program { .. }, Some((strand, toon))) => {
            base = (base.truncate() * strand).extend(base.w);
            let msg = "the hair program exports as MToon in its mean strand colour with its shading; the root-to-tip \
                       gradient, strand lines and angel ring aren't in the file";
            notes.warn("material.approximated", "shading", msg);
            Some(toon)
        }
        (render::Shading::Program { name: program, params }, None) => {
            let msg = format!(
                "shading program '{program}' ({} parameters) can't be written to a file; the default toon look was",
                params.len()
            );
            notes.warn("material.program_dropped", "shading", msg);
            Some(render::Toon::default())
        }
        (render::Shading::Unlit, _) => None,
    };
    let shading = if let Some(toon) = &look {
        io::Shading::Toon(Box::new(export_toon(toon, spec, base.truncate(), tex, &mut notes)))
    } else {
        if spec.outline.is_some() {
            notes.warn("material.outline_dropped", "outline", "an unlit material has no outline in a file");
        }
        io::Shading::Unlit
    };
    note_unexported(spec, look.is_some(), &mut notes);

    let mut material = io::Material::new(name, shading);
    material.base_color = base;
    material.base_color_texture = tex;
    material.emissive = look.map_or(Vec3::ZERO, |toon| toon.emissive.to_linear());
    material.alpha = match spec.alpha {
        render::AlphaMode::Opaque => io::AlphaMode::Opaque,
        render::AlphaMode::Mask { cutoff } => io::AlphaMode::Mask { cutoff },
        render::AlphaMode::Blend => io::AlphaMode::Blend,
    };
    material.double_sided = spec.double_sided;
    Ok(material)
}

/// The kit's hair program (`omuzaki-render` `program/hair.rs`) as a plain toon: the strand colour averaged over the
/// lock, in linear light, and the program's own toon base. `None` when the parameters lack a strand colour.
///
/// The shader runs `root` to `mid` over the first quarter of a lock (`smoothstep(0, 0.25)`) and then to `tip` over
/// `smoothstep(0.55, 1)`; averaged along the lock those weigh 0.125, 0.65 and 0.225. Clump edges darken by
/// `edge-darkening` over `smoothstep(0.55, 1)` of the width, 0.225 of it on average. The toon numbers are the program's
/// (`shift -0.05`, `softness 0.22`, a `#ffe2cc` rim at 0.12, power 3), with its `shade` parameter.
fn hair_look(params: &render::ProgramParams) -> Option<(Vec3, render::Toon)> {
    let colour = |name: &str| match params.iter().find(|(key, _)| key.as_str() == name)?.1 {
        render::ParamValue::Colour(colour) => Some(*colour),
        _ => None,
    };
    let float = |name: &str, default: f32| match params.iter().find(|(key, _)| key.as_str() == name).map(|(_, v)| v) {
        Some(render::ParamValue::Float(value)) => *value,
        _ => default,
    };
    let linear = |c: Colour| to_linear(c).truncate();
    let strand = linear(colour("root")?) * 0.125 + linear(colour("mid")?) * 0.65 + linear(colour("tip")?) * 0.225;
    let edges = 1.0 - float("edge-darkening", 0.14).clamp(0.0, 1.0) * 0.225;
    let toon = render::Toon {
        shade: colour("shade").unwrap_or(Colour::hex(0x00b0_8e88)),
        shift: -0.05,
        softness: 0.22,
        rim: Rim { colour: Colour::hex(0x00ff_e2cc), power: 3.0, strength: 0.12 },
        ..render::Toon::default()
    };
    Some((strand * edges, toon))
}

/// The `MToon` parameters of a renderer toon. Fields the renderer lacks keep the `MToon` defaults, which are written
/// out in full so the file depends on no reader's idea of a default.
#[allow(clippy::field_reassign_with_default)] // `io::Toon` is `#[non_exhaustive]`: only assignment can fill it from here
fn export_toon(
    toon: &render::Toon,
    spec: &MaterialSpec,
    base: Vec3,
    base_texture: Option<io::TexRef>,
    notes: &mut Notes<'_>,
) -> io::Toon {
    let mut out = io::Toon::default();
    // The shaded side is `shade × base × texture` in the renderer; `MToon` has `shadeColorFactor × shadeMultiplyTexture`,
    // so folding the base colour into the factor and sharing the base texture gives the same pixels.
    out.shade_color = toon.shade.to_linear() * base;
    out.shade_multiply_texture = base_texture;
    out.shading_shift = 0.0 - notes.limit("shading/shading_shift", toon.shift, -1.0, 1.0);
    out.shading_toony = 1.0 - notes.limit("shading/shading_toony", toon.softness, 0.0, 1.0);

    let rim = toon.rim.colour.to_linear() * toon.rim.strength.max(0.0);
    out.rim_color = rim.min(Vec3::ONE);
    if out.rim_color != rim {
        let msg = "rim colour × strength is above 1 in a channel; MToon colour factors stop at 1";
        notes.warn("material.approximated", "shading/rim_color", msg);
    }
    out.rim_fresnel_power = notes.limit("shading/rim_fresnel_power", toon.rim.power, 0.0, f32::INFINITY);

    if let Some(line) = spec.outline {
        out.outline_mode = io::OutlineMode::WorldCoordinates;
        out.outline_width = notes.limit("outline/width", line.width, 0.0, f32::INFINITY);
        out.outline_color = line.colour.to_linear();
        let msg = "the renderer widens outlines gently with camera distance; a world-coordinate outline keeps the width \
                   it has at 2 m";
        notes.info("material.not_exported", "outline", msg);
    }
    if toon.skin.is_some() {
        notes.warn("material.skin_dropped", "shading/skin", "MToon has no skin treatment (shadow floor, hue keeping)");
    }
    out
}

/// Findings for the parts of a material no file field holds, whichever shading it has.
fn note_unexported(spec: &MaterialSpec, toon_look: bool, notes: &mut Notes<'_>) {
    if !spec.overlays.is_empty() {
        let msg = format!(
            "{} face overlay(s) aren't converted; the face baker paints them into the base texture before export",
            spec.overlays.len()
        );
        notes.info("material.not_exported", "overlays", msg);
    }
    if spec.vertex_colours && toon_look {
        let msg = "MToon ignores vertex colours; the mesh keeps them but other viewers won't multiply them in";
        notes.warn("material.vertex_colours_ignored", "vertex_colours", msg);
    }
    if !spec.receives_shadows {
        notes.info("material.not_exported", "receives_shadows", "files have no switch for shadow reception");
    }
}

/// The kit material for scene material `material`. `textures` names the kit texture a scene texture became; `at`
/// locates findings (for example `materials/3`).
///
/// An `MToon` material becomes a toon material (field table in the module docs); an unlit one stays unlit; a PBR one
/// becomes the default toon look, since the renderer has no metallic-roughness shading. Face overlays are empty (the
/// face system owns them) and shadows are received. Whatever the renderer cannot draw is reported, once per field.
pub fn import_material(
    material: &io::Material,
    textures: &dyn Fn(&io::TexRef) -> Option<TextureName>,
    at: &str,
    report: &mut Report,
) -> MaterialSpec {
    let mut notes = Notes { at, report };
    let mut spec = MaterialSpec::toon(from_linear(material.base_color));
    spec.base_texture = material
        .base_color_texture
        .as_ref()
        .and_then(|t| import_texture(t, "base_color_texture", textures, &mut notes));
    spec.alpha = match material.alpha {
        io::AlphaMode::Opaque => render::AlphaMode::Opaque,
        io::AlphaMode::Mask { cutoff } => render::AlphaMode::Mask { cutoff },
        io::AlphaMode::Blend => render::AlphaMode::Blend,
    };
    spec.double_sided = material.double_sided;
    if material.emissive_texture.is_some() {
        notes.info(
            "material.not_drawn",
            "emissive_texture",
            "emissive textures aren't drawn; only the emissive factor is kept",
        );
    }
    if material.normal_texture.is_some() {
        notes.info("material.not_drawn", "normal_texture", "normal maps aren't drawn by the toon renderer");
    }

    let emissive = import_emissive(material, &mut notes);
    match &material.shading {
        io::Shading::Toon(toon) => {
            let (mut look, outline) = import_toon(toon, material, &mut notes);
            look.emissive = emissive;
            spec.shading = render::Shading::Toon(look);
            spec.outline = outline;
        }
        io::Shading::Unlit => spec.shading = render::Shading::Unlit,
        io::Shading::Pbr(_) => {
            let msg = "metallic-roughness shading isn't drawn; the material imports as the default toon look";
            notes.info("material.not_drawn", "shading/pbr", msg);
            spec.shading = render::Shading::Toon(render::Toon { emissive, ..render::Toon::default() });
        }
    }
    spec
}

/// The kit texture behind a scene texture reference, with findings for what a name cannot carry.
fn import_texture(
    texture: &io::TexRef,
    field: &str,
    textures: &dyn Fn(&io::TexRef) -> Option<TextureName>,
    notes: &mut Notes<'_>,
) -> Option<TextureName> {
    if texture.uv_set != 0 || texture.transform.is_some_and(|t| t != io::UvTransform::default()) {
        notes.info(
            "material.not_drawn",
            field,
            "texture UV sets and transforms aren't kept; the mesh's own UVs are used",
        );
    }
    let name = textures(texture);
    if name.is_none() {
        notes.warn(
            "material.texture_missing",
            field,
            "the scene texture has no counterpart in the character; it was dropped",
        );
    }
    name
}

/// The emissive factor as a palette colour, capped at 1 (values above need `KHR_materials_emissive_strength`).
fn import_emissive(material: &io::Material, notes: &mut Notes<'_>) -> Colour {
    if material.emissive.max_element() > 1.0 + DEFAULT_TOLERANCE {
        notes.warn("material.approximated", "emissive", "emissive above 1 is capped at 1");
    }
    from_linear(material.emissive.extend(1.0))
}

/// The renderer's toon and outline for `MToon` parameters `t` of `material`.
fn import_toon(t: &io::Toon, material: &io::Material, notes: &mut Notes<'_>) -> (render::Toon, Option<Outline>) {
    let defaults = io::Toon::default();
    let base = material.base_color.truncate();
    let toon = render::Toon {
        shade: import_shade(t, base, notes),
        shift: 0.0 - notes.limit("shading/shading_shift", t.shading_shift, -1.0, 1.0),
        softness: 1.0 - notes.limit("shading/shading_toony", t.shading_toony, 0.0, 1.0),
        rim: Rim {
            colour: from_linear(t.rim_color.extend(1.0)),
            // A power of 0 is a rim of uniform strength; the renderer wants a positive one, and the smallest positive
            // power is uniform for every practical purpose.
            power: t.rim_fresnel_power.max(f32::MIN_POSITIVE),
            strength: 1.0,
        },
        emissive: Colour::BLACK,
        skin: None,
    };

    let uv_animation = !is_default(t.uv_scroll_x, 0.0)
        || !is_default(t.uv_scroll_y, 0.0)
        || !is_default(t.uv_rotation, 0.0)
        || t.uv_animation_mask.is_some();
    let not_drawn = [
        ("gi_equalization", !is_default(t.gi_equalization, defaults.gi_equalization), "GI equalisation"),
        ("matcap_texture", t.matcap_texture.is_some(), "the matcap"),
        ("uv_animation", uv_animation, "UV animation"),
        ("render_queue_offset", t.render_queue_offset != 0, "the render queue offset"),
        ("transparent_with_zwrite", t.transparent_with_zwrite, "depth writes for blended surfaces"),
        ("shading_shift_texture", t.shading_shift_texture.is_some(), "the shading shift texture"),
        ("rim_lift", !is_default(t.rim_lift, 0.0), "the rim lift"),
        ("rim_lighting_mix", !is_default(t.rim_lighting_mix, defaults.rim_lighting_mix), "the rim lighting mix"),
        ("rim_multiply_texture", t.rim_multiply_texture.is_some(), "the rim mask"),
        ("outline_width_texture", t.outline_width_texture.is_some(), "the outline width mask"),
        (
            "outline_lighting_mix",
            t.outline_mode != io::OutlineMode::None
                && !is_default(t.outline_lighting_mix, defaults.outline_lighting_mix),
            "the outline lighting mix",
        ),
        (
            "shade_multiply_texture",
            t.shade_multiply_texture.is_some() && t.shade_multiply_texture != material.base_color_texture,
            "a shade texture other than the base colour texture",
        ),
    ];
    for (field, present, what) in not_drawn {
        if present {
            notes.info(
                "material.not_drawn",
                &format!("shading/{field}"),
                format!("{what} isn't drawn by the renderer and was dropped"),
            );
        }
    }
    (toon, import_outline(t, notes))
}

/// The renderer's shade multiplier for `MToon`'s absolute shade colour: `shade / base`, per channel.
fn import_shade(t: &io::Toon, base: Vec3, notes: &mut Notes<'_>) -> Colour {
    // Where the base channel is 0 the shaded side is 0 whatever the multiplier, so any value looks the same; 1 is the
    // neutral choice.
    let ratio = Vec3::from_array(std::array::from_fn(|i| if base[i] > 0.0 { t.shade_color[i] / base[i] } else { 1.0 }));
    if ratio.max_element() > 1.0 + 1e-4 {
        let msg = "the shade colour is brighter than the base colour in a channel; the multiplier is capped at 1";
        notes.warn("material.approximated", "shading/shade_color", msg);
    }
    from_linear(ratio.extend(1.0))
}

/// The outline of an `MToon` material, if it has one.
fn import_outline(t: &io::Toon, notes: &mut Notes<'_>) -> Option<Outline> {
    if t.outline_mode == io::OutlineMode::None {
        return None;
    }
    let colour = from_linear(t.outline_color.extend(1.0));
    let width = notes.limit("shading/outline_width", t.outline_width, 0.0, f32::INFINITY);
    match t.outline_mode {
        io::OutlineMode::None | io::OutlineMode::WorldCoordinates => Some(Outline { colour, width }),
        io::OutlineMode::ScreenCoordinates => {
            // A fraction of the screen height, turned into metres at the distance the renderer's width is stated for.
            let screen_height = 2.0 * Outline::REFERENCE_DISTANCE * (ASSUMED_FOV_Y_DEG.to_radians() / 2.0).tan();
            let msg = "the outline width is a fraction of the screen height; it was converted to metres at 2 m with a 30 degree lens";
            notes.warn("material.approximated", "shading/outline_mode", msg);
            Some(Outline { colour, width: width * screen_height })
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_render_api::{Camera, Projection};

    use super::*;

    #[test]
    fn the_screen_outline_lens_is_the_renderers_default_one() {
        let Projection::Perspective { fov_y_deg, .. } = Camera::default().projection else {
            panic!("the default camera is a perspective one");
        };
        assert!(is_default(fov_y_deg, ASSUMED_FOV_Y_DEG), "the renderer's lens is {fov_y_deg}");
    }
}
