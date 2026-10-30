//! Materials: toon parameters, outlines, and the overlays the face painter composites through.
//!
//! The split follows what the project document can store. The *parameter* types ([`Toon`], [`Outline`], [`Mapping`]
//! and friends) hold only numbers and colours, so they serialise into the document and carry JSON Schemas for ops
//! (LK-01, LK-02, LK-03). A [`Material`] adds texture handles, which only exist at run time, so it does not serialise:
//! the stage builds one from the document's parameters and the textures it has uploaded.

use std::collections::BTreeMap;

use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::id::Slug;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::limits::{MAX_OVERLAYS, MAX_PROGRAM_PARAMS};
use crate::{Colour, Mtoon, RenderError, TextureId};

/// How a surface is lit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shading {
    /// Soft cel shading with a tinted shadow colour (LK-01): the look of the product.
    Toon(Toon),
    /// No lighting: backdrops, UI planes, sprites.
    Unlit,
    /// A named shading program for looks the toon model can't express (hair strands and the angel-ring highlight,
    /// glass, and so on). It still receives the material's `base`, `base_texture`, `overlays`, `alpha` and `outline`;
    /// the program adds its own shading, driven by `params` and by the mesh's [`extra`](crate::MeshData::extra)
    /// vertex attributes.
    ///
    /// **Registration contract** (§39 rule 2: nothing edits a central list). Programs live in `omuzaki-render`, one file
    /// each, and register themselves by name with a `register_shading_program!` macro, as ops do. A registration
    /// declares: the program's [`Slug`] name; its parameters (name, [`ParamValue`] default, and optional range); the
    /// extra vertex attributes it reads (name and components); and its shader code. Rules every renderer follows:
    /// - `create_material` and `update_material` check `params` against the declaration: an unknown name, a value of
    ///   the wrong type or one out of range is `Invalid`; a missing parameter takes its default.
    /// - A mesh that lacks an attribute the program reads supplies zeros, so older meshes still draw.
    /// - A program name the renderer doesn't know is **not** an error: the material is drawn as
    ///   [`Toon::default`] with the same base, textures and overlays, so projects keep rendering on a renderer
    ///   (such as the mock) that lacks the program. [`Capabilities::shading_programs`](crate::Capabilities) lists the
    ///   names a renderer does know.
    Program {
        /// The program's registered name.
        name: Slug,
        /// Its parameters, at most [`MAX_PROGRAM_PARAMS`].
        params: ProgramParams,
    },
}

/// A parameter of a shading program: a small closed set of types, so program parameters serialise into the project
/// document, carry a JSON Schema and can be edited by ops like any other look setting.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParamValue {
    /// A number.
    Float(f32),
    /// Two numbers.
    Vec2(#[schemars(with = "[f32; 2]")] Vec2),
    /// Three numbers.
    Vec3(#[schemars(with = "[f32; 3]")] Vec3),
    /// A colour (converted to linear light for the shader, like every other colour).
    Colour(Colour),
    /// An on/off switch.
    Bool(bool),
}

impl ParamValue {
    fn is_finite(&self) -> bool {
        match self {
            Self::Float(v) => v.is_finite(),
            Self::Vec2(v) => v.is_finite(),
            Self::Vec3(v) => v.is_finite(),
            Self::Colour(_) | Self::Bool(_) => true,
        }
    }
}

/// A shading program's parameters by name. A `BTreeMap`, so the order in project files and in hashes is stable.
pub type ProgramParams = BTreeMap<Slug, ParamValue>;

/// Soft cel shading, ported from the prototype's `toon.js`.
///
/// Per light, `t = smoothstep(shift - softness, shift + softness, N·L)` and the lit colour is
/// `light × mix(shade, 1, t) × base / π`. The shadow side is a *tinted multiplier* (`shade`), never a grey, which is
/// what makes anime shadows read warm. Light intensities follow the three.js convention (see [`Light`](crate::Light))
/// so the prototype's light rigs port number for number.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Toon {
    /// Multiplier applied on the shadow side, for example a pinker tint of the skin colour.
    pub shade: Colour,
    /// Where the terminator sits, `-1..=1`; negative lights more of the surface.
    pub shift: f32,
    /// Half-width of the soft edge between lit and shade, `0..=1` (0 is a hard edge).
    pub softness: f32,
    /// The rim light.
    pub rim: Rim,
    /// Light the surface gives off, added after shading.
    pub emissive: Colour,
    /// Skin treatment, for faces and bodies that must never sink into muddy shadow. `None` for everything else.
    pub skin: Option<Skin>,
}

impl Default for Toon {
    /// The prototype's defaults: a soft violet-grey shade and a faint blue rim.
    fn default() -> Self {
        Self {
            shade: Colour::hex(0x009a_8f9e),
            shift: 0.0,
            softness: 0.12,
            rim: Rim::default(),
            emissive: Colour::BLACK,
            skin: None,
        }
    }
}

/// A rim light: `colour × strength × (1 - N·V)^power`, added to the lit colour.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rim {
    /// Rim colour.
    pub colour: Colour,
    /// Falloff exponent, `> 0`; higher keeps the rim thinner.
    pub power: f32,
    /// Brightness; 0 turns the rim off.
    pub strength: f32,
}

impl Default for Rim {
    fn default() -> Self {
        Self { colour: Colour::hex(0x006d_74a8), power: 3.0, strength: 0.18 }
    }
}

/// Keeps skin readable under any light: the albedo's hue survives coloured light, and shadows never go below a floor.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Skin {
    /// Minimum brightness as a fraction of the albedo, `0..=1`.
    pub floor: f32,
    /// How strongly the lit colour is pulled back to the albedo's hue, `0..=1`.
    pub hue_keep: f32,
}

impl Default for Skin {
    fn default() -> Self {
        Self { floor: 0.42, hue_keep: 0.55 }
    }
}

/// An inverted-hull outline (LK-02): the mesh drawn again, pushed out along its smooth normals, back faces only.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Outline {
    /// Line colour: a darker shade of the surface it borders, never pure black.
    pub colour: Colour,
    /// Line width in metres at [`Outline::REFERENCE_DISTANCE`].
    pub width: f32,
}

impl Outline {
    /// The camera distance at which `width` is exact, in metres.
    pub const REFERENCE_DISTANCE: f32 = 2.0;
    /// Lines never get thinner than this fraction of `width`...
    pub const MIN_SCALE: f32 = 0.6;
    /// ...or thicker than this multiple of it, so they stay readable close up and don't swamp the model far away.
    pub const MAX_SCALE: f32 = 3.0;

    /// The line width in metres for a surface `view_depth` metres in front of the camera: it grows gently with
    /// distance (the prototype's `clamp(d / 2, 0.6, 3)`). One formula here keeps every renderer's lines identical.
    pub fn world_width(&self, view_depth: f32) -> f32 {
        self.width * (view_depth.max(0.1) / Self::REFERENCE_DISTANCE).clamp(Self::MIN_SCALE, Self::MAX_SCALE)
    }
}

/// What an [`Outline`]'s `width` measures (`MToon`'s `outlineWidthMode`, #647).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutlineWidthMode {
    /// Metres at [`Outline::REFERENCE_DISTANCE`], growing gently with distance ([`Outline::world_width`]): the kit's
    /// line, which stays readable as a character steps back.
    #[default]
    WorldByDistance,
    /// Metres at any distance: `MToon`'s `worldCoordinates`.
    World,
    /// A fraction of the target's height, the same on screen at any distance and through any lens: `MToon`'s
    /// `screenCoordinates`.
    Screen,
}

/// How a material's outline is drawn beyond its colour and width (#647): `MToon`'s width mode, width mask and lighting
/// mix. The default is the kit's line: world width by distance, no mask, the colour dimmed only by
/// `Scene::outline_light`.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct OutlineStyle {
    /// What `Outline::width` measures.
    pub width_mode: OutlineWidthMode,
    /// A per-texel multiplier of the width: the green channel of this texture at the mesh's UVs (through the
    /// material's `uv_transform`), in the texture's declared colour space (`MToon`'s `outlineWidthMultiplyTexture` is
    /// linear data). Black draws no line there.
    pub width_texture: Option<TextureId>,
    /// How much the lit surface tints the line, `0..=1` (`MToon`'s `outlineLightingMixFactor`): the line's colour is
    /// `Outline::colour` times a mix from `Scene::outline_light` (0) to the surface's own lit colour (1).
    pub lighting_mix: f32,
}

impl OutlineStyle {
    /// The kit's line ([`Default`]).
    pub const KIT: Self =
        Self { width_mode: OutlineWidthMode::WorldByDistance, width_texture: None, lighting_mix: 0.0 };
}

/// How a surface's alpha is used.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AlphaMode {
    /// Alpha is ignored.
    Opaque,
    /// Pixels with alpha below `cutoff` are discarded; the rest are opaque (hair cards, lashes, leaves).
    Mask {
        /// Threshold in `0..=1`.
        cutoff: f32,
    },
    /// Alpha blends over what is behind. Drawn after opaque surfaces, back to front, without writing depth.
    Blend,
}

/// Where an [`Overlay`]'s texture lands on a mesh.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mapping {
    /// The mesh's own texture coordinates.
    Uv,
    /// Projected straight along the mesh's local Z axis onto a rectangle of its local XY plane, like a slide
    /// projector pointed at the face. This is how the prototype puts the painted face on the head, and why a face
    /// layout can change (new profile, new proportions) without touching UVs.
    Planar {
        /// Lower-left corner of the rectangle, in the mesh's local XY (bind pose, before skinning).
        #[schemars(with = "[f32; 2]")]
        min: Vec2,
        /// Upper-right corner. The top row of the texture is at `max.y`.
        #[schemars(with = "[f32; 2]")]
        max: Vec2,
        /// Local Z where the projection starts to show (the back of the head stays bare)...
        fade_from: f32,
        /// ...and where it is fully shown. Equal values make a hard cut at that Z.
        fade_to: f32,
    },
}

/// A texture composited over the base colour *before* lighting, so a painted face is shaded like the skin around it.
///
/// The result colour is `mix(base, texture.rgb, texture.a × strength × mapping_weight)`. Several overlays apply in
/// order. Brows drawn over the bangs (CB-21) are a second overlay on the hair material using the same mapping.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Overlay {
    /// The texture, usually a dynamic one the face painter owns.
    pub texture: TextureId,
    /// Where it lands.
    pub mapping: Mapping,
    /// Overall opacity, `0..=1`.
    pub strength: f32,
}

/// A scale and offset applied to the UV of every UV-addressed texture of a [`Material`] (its `base_texture` and its
/// [`Mapping::Uv`] overlays): `uv' = uv × scale + offset`. It is how an atlas cell is picked without touching the
/// mesh, and the per-material half of `MToon`'s texture transform. [`Mapping::Planar`] overlays are not UV-addressed and
/// ignore it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct UvTransform {
    /// Multiplier on both axes (`ONE` leaves the UV alone).
    pub scale: Vec2,
    /// Added after scaling.
    pub offset: Vec2,
}

impl UvTransform {
    /// Leaves every UV as it is.
    pub const IDENTITY: Self = Self { scale: Vec2::ONE, offset: Vec2::ZERO };

    /// The transformed coordinate: the one definition every renderer uses.
    pub fn apply(&self, uv: Vec2) -> Vec2 {
        uv * self.scale + self.offset
    }

    fn is_finite(&self) -> bool {
        self.scale.is_finite() && self.offset.is_finite()
    }
}

impl Default for UvTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// A material's part in the hair shadow (#2528): the shadow hair casts on a face, which shadow maps miss because a fringe
/// hangs inside their bias.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct HairShadow {
    /// The surface is drawn into the hair shadow: hair.
    pub casts: bool,
    /// The surface turns to its shade tone where hair lies between it and a shadow-casting directional light: a face
    /// under its fringe.
    pub receives: bool,
}

impl HairShadow {
    /// Neither casts nor receives: the default.
    pub const NONE: Self = Self { casts: false, receives: false };
    /// Casts it, as hair does.
    pub const CASTS: Self = Self { casts: true, receives: false };
    /// Receives it, as a face does.
    pub const RECEIVES: Self = Self { casts: false, receives: true };
}

/// Everything about how one mesh's surface looks.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools, reason = "independent switches a renderer reads one at a time")]
pub struct Material {
    /// The surface colour (its alpha multiplies the texture's).
    pub base: Colour,
    /// A texture multiplied into `base`, read with the mesh's UVs.
    pub base_texture: Option<TextureId>,
    /// Lighting model.
    pub shading: Shading,
    /// Textures composited over the base, in order; at most [`MAX_OVERLAYS`].
    pub overlays: Vec<Overlay>,
    /// How alpha is used.
    pub alpha: AlphaMode,
    /// With [`AlphaMode::Blend`], blend without sorting (weighted blended order-independent transparency, #2507):
    /// many overlapping layers of one surface, such as hair tips or fur, blend smoothly with no sorting errors, at the
    /// cost of an approximate order where layers of very different colours overlap. A renderer that lacks it sorts the
    /// surface with the other blended ones. With [`AlphaMode::Mask`], the surface gets a soft edge (hair tips, #2529):
    /// the texels at or above the cutoff draw opaque, and those below it, instead of being cut, blend this way with
    /// their alpha scaled up to 1 at the cutoff. Ignored for an opaque surface.
    pub order_independent: bool,
    /// Draw both sides of every triangle (hair sheets, cloth). The back side is shaded with the flipped normal.
    pub double_sided: bool,
    /// Multiply the mesh's vertex colours into `base`.
    pub vertex_colours: bool,
    /// The outline drawn around meshes using this material, if any.
    pub outline: Option<Outline>,
    /// How that outline is drawn beyond its colour and width (`MToon`'s modes); ignored without an outline.
    pub outline_style: OutlineStyle,
    /// Whether shadow maps darken this surface. Faces turn it off: anime faces stay clean under hair shadows.
    pub receives_shadows: bool,
    /// The hair shadow (#2528): whether this surface casts it (hair) or receives it (a face). Both off by default.
    pub hair_shadow: HairShadow,
    /// Applied to the UV of every UV-addressed texture (`base_texture` and [`Mapping::Uv`] overlays).
    pub uv_transform: UvTransform,
    /// `MToon` 1.0's own lighting, for an imported model (LK-07): with toon shading, the surface is lit by the spec's
    /// model ([`Mtoon`]) in place of the toon ramp. Boxed, as only imported materials carry it.
    pub mtoon: Option<Box<Mtoon>>,
}

impl Material {
    /// An opaque toon material of `base` with the prototype's default shading and no outline.
    pub fn toon(base: Colour) -> Self {
        Self {
            base,
            base_texture: None,
            shading: Shading::Toon(Toon::default()),
            overlays: Vec::new(),
            alpha: AlphaMode::Opaque,
            order_independent: false,
            double_sided: false,
            vertex_colours: false,
            outline: None,
            outline_style: OutlineStyle::KIT,
            receives_shadows: true,
            hair_shadow: HairShadow::NONE,
            uv_transform: UvTransform::IDENTITY,
            mtoon: None,
        }
    }

    /// An opaque unlit material of `base`.
    pub fn unlit(base: Colour) -> Self {
        Self { shading: Shading::Unlit, ..Self::toon(base) }
    }

    /// Every texture this material reads. A renderer refuses to release one of them while the material is alive.
    pub fn textures(&self) -> impl Iterator<Item = TextureId> + '_ {
        self.base_texture
            .into_iter()
            .chain(self.overlays.iter().map(|o| o.texture))
            .chain(self.outline_style.width_texture)
            .chain(self.mtoon.iter().flat_map(|m| m.textures().copied()))
    }

    /// Checks the rules every renderer shares (ranges and counts; handles are checked by the renderer).
    ///
    /// # Errors
    /// [`RenderError::Invalid`] or `LimitExceeded` naming the first broken rule.
    pub fn validate(&self) -> Result<(), RenderError> {
        let bad = |reason: &str| Err(RenderError::invalid("material", reason));
        if self.overlays.len() > MAX_OVERLAYS {
            return Err(RenderError::LimitExceeded { what: "overlays on one material", limit: MAX_OVERLAYS as u64 });
        }
        if let Shading::Toon(t) = &self.shading {
            if !(-1.0..=1.0).contains(&t.shift) || !(0.0..=1.0).contains(&t.softness) {
                return bad("toon shift must be in -1..=1 and softness in 0..=1");
            }
            if !(t.rim.power > 0.0 && t.rim.power.is_finite() && t.rim.strength >= 0.0 && t.rim.strength.is_finite()) {
                return bad("rim power must be > 0 and strength >= 0");
            }
            if let Some(s) = t.skin
                && !((0.0..=1.0).contains(&s.floor) && (0.0..=1.0).contains(&s.hue_keep))
            {
                return bad("skin floor and hue_keep must be in 0..=1");
            }
        }
        if let Shading::Program { params, .. } = &self.shading {
            if params.len() > MAX_PROGRAM_PARAMS {
                return Err(RenderError::LimitExceeded {
                    what: "parameters on one shading program",
                    limit: MAX_PROGRAM_PARAMS as u64,
                });
            }
            if !params.values().all(ParamValue::is_finite) {
                return bad("shading program parameters must be finite");
            }
        }
        if let AlphaMode::Mask { cutoff } = self.alpha
            && !(0.0..=1.0).contains(&cutoff)
        {
            return bad("alpha cutoff must be in 0..=1");
        }
        if self.overlays.iter().any(|o| !(0.0..=1.0).contains(&o.strength)) {
            return bad("overlay strength must be in 0..=1");
        }
        for o in &self.overlays {
            if let Mapping::Planar { min, max, fade_from, fade_to } = o.mapping {
                if !(max.x > min.x && max.y > min.y && min.is_finite() && max.is_finite()) {
                    return bad("a planar mapping needs max > min on both axes");
                }
                if !(fade_from.is_finite() && fade_to.is_finite() && fade_to >= fade_from) {
                    return bad("a planar mapping needs fade_to >= fade_from");
                }
            }
        }
        if self.outline.is_some_and(|o| !(o.width >= 0.0 && o.width.is_finite())) {
            return bad("outline width must be >= 0");
        }
        if !(0.0..=1.0).contains(&self.outline_style.lighting_mix) {
            return bad("outline lighting_mix must be in 0..=1");
        }
        if !self.uv_transform.is_finite() {
            return bad("the uv transform must be finite");
        }
        if let Some(mtoon) = &self.mtoon {
            mtoon.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Break = fn(&mut Material);

    fn tex(i: u32) -> TextureId {
        TextureId::from_parts(i, 1)
    }

    #[test]
    fn the_uv_transform_defaults_to_identity_and_scales_before_it_offsets() {
        assert_eq!(UvTransform::default(), UvTransform::IDENTITY);
        assert_eq!(Material::toon(Colour::WHITE).uv_transform, UvTransform::IDENTITY);
        let uv = Vec2::new(0.25, 0.5);
        assert_eq!(UvTransform::IDENTITY.apply(uv), uv);
        let atlas = UvTransform { scale: Vec2::new(0.5, 1.0), offset: Vec2::new(0.5, 0.0) };
        assert_eq!(atlas.apply(uv), Vec2::new(0.625, 0.5));
    }

    #[test]
    fn a_non_finite_uv_transform_is_invalid() {
        let mut m = Material::toon(Colour::WHITE);
        assert_eq!(m.validate(), Ok(()));
        for bad in [
            UvTransform { scale: Vec2::new(f32::NAN, 1.0), offset: Vec2::ZERO },
            UvTransform { scale: Vec2::ONE, offset: Vec2::new(0.0, f32::INFINITY) },
        ] {
            m.uv_transform = bad;
            assert!(matches!(m.validate(), Err(RenderError::Invalid { .. })), "{bad:?}");
        }
    }

    #[test]
    fn defaults_are_the_prototypes() {
        let t = Toon::default();
        assert_eq!((t.shade, t.shift, t.softness), (Colour::hex(0x009a_8f9e), 0.0, 0.12));
        assert_eq!((t.rim.power, t.rim.strength), (3.0, 0.18));
        assert_eq!(Skin::default(), Skin { floor: 0.42, hue_keep: 0.55 });
        assert_eq!(Material::toon(Colour::WHITE).validate(), Ok(()));
    }

    #[test]
    fn outline_width_scales_with_distance_inside_the_clamp() {
        let o = Outline { colour: Colour::BLACK, width: 0.01 };
        assert!((o.world_width(2.0) - 0.01).abs() < 1e-9, "exact at the reference distance");
        assert!((o.world_width(0.2) - 0.006).abs() < 1e-9, "never thinner than 0.6x");
        assert!((o.world_width(40.0) - 0.03).abs() < 1e-9, "never thicker than 3x");
        assert!((o.world_width(4.0) - 0.02).abs() < 1e-9, "linear in between");
        assert!(o.world_width(-1.0) > 0.0, "a surface behind the camera plane still gets a sane width");
    }

    #[test]
    fn textures_lists_the_base_and_every_overlay() {
        let mut m = Material::toon(Colour::WHITE);
        m.base_texture = Some(tex(1));
        m.overlays.push(Overlay { texture: tex(2), mapping: Mapping::Uv, strength: 1.0 });
        assert_eq!(m.textures().collect::<Vec<_>>(), vec![tex(1), tex(2)]);
    }

    #[test]
    fn validation_catches_each_kind_of_bad_value() {
        let planar = |max_x: f32, fade_to: f32| Overlay {
            texture: tex(1),
            mapping: Mapping::Planar { min: Vec2::ZERO, max: Vec2::new(max_x, 1.0), fade_from: 0.0, fade_to },
            strength: 1.0,
        };
        let cases: [(&str, Break); 5] = [
            ("shift", |m| m.shading = Shading::Toon(Toon { shift: 2.0, ..Toon::default() })),
            ("cutoff", |m| m.alpha = AlphaMode::Mask { cutoff: 1.5 }),
            ("outline", |m| m.outline = Some(Outline { colour: Colour::BLACK, width: -1.0 })),
            ("strength", |m| m.overlays.push(Overlay { texture: tex(1), mapping: Mapping::Uv, strength: 2.0 })),
            ("skin", |m| {
                m.shading = Shading::Toon(Toon { skin: Some(Skin { floor: 2.0, hue_keep: 0.0 }), ..Toon::default() });
            }),
        ];
        for (needle, break_it) in cases {
            let mut m = Material::toon(Colour::WHITE);
            break_it(&mut m);
            let err = m.validate().unwrap_err().to_string();
            assert!(err.contains(needle), "expected {needle:?} in {err:?}");
        }
        for overlay in [planar(0.0, 1.0), planar(1.0, -1.0)] {
            let mut m = Material::toon(Colour::WHITE);
            m.overlays.push(overlay);
            assert!(m.validate().is_err());
        }
        let mut crowded = Material::toon(Colour::WHITE);
        crowded.overlays = vec![planar(1.0, 1.0); MAX_OVERLAYS + 1];
        assert!(matches!(crowded.validate(), Err(RenderError::LimitExceeded { .. })));
    }

    #[test]
    fn parameter_types_round_trip_through_json() {
        let toon = Shading::Toon(Toon { skin: Some(Skin::default()), ..Toon::default() });
        let json = serde_json::to_value(&toon).unwrap();
        assert_eq!(json["kind"], "toon");
        assert_eq!(json["shade"], "#9a8f9e");
        assert_eq!(serde_json::from_value::<Shading>(json).unwrap(), toon);

        let planar = Mapping::Planar {
            min: Vec2::new(-0.125, 0.5),
            max: Vec2::new(0.125, 0.75),
            fade_from: 0.0,
            fade_to: 0.0625,
        };
        let json = serde_json::to_value(planar).unwrap();
        assert_eq!(json["min"], serde_json::json!([-0.125, 0.5]));
        assert_eq!(serde_json::from_value::<Mapping>(json).unwrap(), planar);
    }

    fn program(params: &[(&str, ParamValue)]) -> Shading {
        Shading::Program {
            name: Slug::new("hair").unwrap(),
            params: params.iter().map(|(k, v)| (Slug::new(*k).unwrap(), *v)).collect(),
        }
    }

    #[test]
    fn program_parameters_round_trip_and_stay_sorted() {
        let shading = program(&[
            ("ring-colour", ParamValue::Colour(Colour::hex(0x00ff_e2cc))),
            ("ring-width", ParamValue::Float(0.25)),
            ("strand-dir", ParamValue::Vec3(Vec3::new(0.0, -1.0, 0.0))),
            ("tip-fade", ParamValue::Vec2(Vec2::new(0.5, 1.0))),
            ("two-tone", ParamValue::Bool(true)),
        ]);
        let json = serde_json::to_value(&shading).unwrap();
        assert_eq!(json["kind"], "program");
        assert_eq!(json["name"], "hair");
        assert_eq!(json["params"]["ring-width"], serde_json::json!({ "float": 0.25 }));
        assert_eq!(json["params"]["ring-colour"], serde_json::json!({ "colour": "#ffe2cc" }));
        let keys: Vec<_> = json["params"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["ring-colour", "ring-width", "strand-dir", "tip-fade", "two-tone"], "serde_json sorts maps");
        assert_eq!(serde_json::from_value::<Shading>(json).unwrap(), shading);
    }

    #[test]
    fn program_materials_validate_their_parameters() {
        let mut m = Material::toon(Colour::WHITE);
        m.shading = program(&[("width", ParamValue::Float(1.0))]);
        assert_eq!(m.validate(), Ok(()));
        m.shading = program(&[("width", ParamValue::Float(f32::NAN))]);
        assert!(m.validate().is_err());
        m.shading = Shading::Program {
            name: Slug::new("hair").unwrap(),
            params: (0..=MAX_PROGRAM_PARAMS)
                .map(|i| (Slug::new(format!("p{i}")).unwrap(), ParamValue::Bool(true)))
                .collect(),
        };
        assert!(matches!(m.validate(), Err(RenderError::LimitExceeded { .. })));
    }

    #[test]
    fn schemas_generate_for_every_parameter_type() {
        for schema in [
            schemars::schema_for!(Shading),
            schemars::schema_for!(ParamValue),
            schemars::schema_for!(Outline),
            schemars::schema_for!(AlphaMode),
            schemars::schema_for!(Mapping),
        ] {
            assert!(serde_json::to_value(schema).is_ok());
        }
    }
}
