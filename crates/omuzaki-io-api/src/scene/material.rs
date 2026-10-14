//! Materials, textures and images.
//!
//! Toon shading follows `VRMC_materials_mtoon` 1.0 field for field (`VRMC_materials_mtoon-1.0/README.md`), because
//! that is what VRM files contain and what other apps render. The renderer's own toon parameters are a different
//! model; the op layer converts (see `docs/design/io-api.md`, "Interfaces with `omuzaki-render-api`").
//! Colour factors are linear, as in glTF; colour textures (base colour, emissive, shade, matcap, rim) are sRGB-coded
//! images, data textures (normal, shading shift, masks) are linear.

use std::sync::Arc;

use omuzaki_core::glam::{Vec2, Vec3, Vec4};

use super::{ImageIx, Passthrough, TextureIx};

/// Immutable bytes that are cheap to clone, so cloning a 45 MB scene doesn't copy its textures.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blob(Arc<[u8]>);

impl Blob {
    /// Wraps `bytes`.
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self(bytes.into())
    }

    /// The bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// The length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True if there are no bytes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The two image formats glTF 2.0 core allows. VRM forbids extension formats for the thumbnail
/// (`VRMC_vrm-1.0/meta.md`, "thumbnailImage"), and we don't use them elsewhere.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ImageMime {
    /// `image/png`.
    Png,
    /// `image/jpeg`.
    Jpeg,
}

impl ImageMime {
    /// The media type string used in glTF `image.mimeType`.
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }
}

/// An encoded image, byte for byte as in the file. Decoding is the consumer's job; keeping the original bytes means
/// an unchanged texture round-trips bit-identically and hashes to the same `ContentHash`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EncodedImage {
    /// Display name.
    pub name: String,
    /// The encoding.
    pub mime: ImageMime,
    /// The encoded bytes.
    pub bytes: Blob,
}

impl EncodedImage {
    /// An image from encoded bytes.
    pub fn new(name: impl Into<String>, mime: ImageMime, bytes: Blob) -> Self {
        Self { name: name.into(), mime, bytes }
    }
}

/// Texture coordinate wrapping.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Wrap {
    /// Tile.
    #[default]
    Repeat,
    /// Tile, mirrored.
    MirroredRepeat,
    /// Clamp to the edge. Face-atlas flipbooks need this, or offsets bleed into the neighbour cell.
    ClampToEdge,
}

/// Texture filtering.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    /// Nearest texel (pixel art, flipbook cells).
    Nearest,
    /// Bilinear.
    #[default]
    Linear,
}

/// How a texture is sampled.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Sampler {
    /// Wrapping along U.
    pub wrap_u: Wrap,
    /// Wrapping along V.
    pub wrap_v: Wrap,
    /// Magnification filter.
    pub mag: Filter,
    /// Minification filter.
    pub min: Filter,
    /// Whether minification uses mipmaps.
    pub mipmaps: bool,
}

impl Default for Sampler {
    fn default() -> Self {
        Self { wrap_u: Wrap::Repeat, wrap_v: Wrap::Repeat, mag: Filter::Linear, min: Filter::Linear, mipmaps: true }
    }
}

/// An image plus how to sample it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Texture {
    /// The image.
    pub image: ImageIx,
    /// The sampler.
    pub sampler: Sampler,
}

/// A `KHR_texture_transform` offset and scale. VRM 1.0 recommends *not* using the extension's `rotation` and
/// `texCoord` (`VRMC_vrm-1.0/README.md`, "Obsolete properties of `KHR_texture_transform` with VRM1"), so they are not
/// representable: importers drop them with a warning.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct UvTransform {
    /// Added to the UV.
    pub offset: Vec2,
    /// Multiplies the UV.
    pub scale: Vec2,
}

impl Default for UvTransform {
    fn default() -> Self {
        Self { offset: Vec2::ZERO, scale: Vec2::ONE }
    }
}

/// A use of a texture by a material slot.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TexRef {
    /// The texture.
    pub texture: TextureIx,
    /// Which UV set (`TEXCOORD_n`) it reads. Only set 0 is stored in meshes, so anything else is a warning.
    pub uv_set: u8,
    /// The optional transform.
    pub transform: Option<UvTransform>,
}

impl TexRef {
    /// A plain reference: UV set 0, no transform.
    pub fn of(texture: TextureIx) -> Self {
        Self { texture, uv_set: 0, transform: None }
    }
}

/// How transparency is handled (glTF `alphaMode`).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub enum AlphaMode {
    /// Ignore alpha.
    #[default]
    Opaque,
    /// Cut out below `cutoff`.
    Mask {
        /// The alpha threshold.
        cutoff: f32,
    },
    /// Alpha blend.
    Blend,
}

/// A normal map and its strength.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NormalTexture {
    /// The map.
    pub texture: TexRef,
    /// Scales the normal's XY.
    pub scale: f32,
}

/// Metallic-roughness shading (glTF core).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Pbr {
    /// 0 dielectric, 1 metal.
    pub metallic: f32,
    /// 0 smooth, 1 rough.
    pub roughness: f32,
    /// Blue is metallic, green is roughness.
    pub metallic_roughness_texture: Option<TexRef>,
}

impl Default for Pbr {
    fn default() -> Self {
        // glTF's defaults are fully metallic and fully rough; cel characters never want that, but a file that omits
        // the block means exactly this, so the default stays spec-true.
        Self { metallic: 1.0, roughness: 1.0, metallic_roughness_texture: None }
    }
}

/// How outlines are drawn.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum OutlineMode {
    /// No outline.
    #[default]
    None,
    /// Width in metres, scaled with the world.
    WorldCoordinates,
    /// Width as a fraction of screen height.
    ScreenCoordinates,
}

/// A shading-shift texture and its contribution.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ShiftTexture {
    /// The map.
    pub texture: TexRef,
    /// How strongly it shifts the shade boundary.
    pub scale: f32,
}

/// `VRMC_materials_mtoon` 1.0 parameters. Defaults are the spec's, with one hazard: the README table gives
/// `shadeColorFactor` default `[0,0,0]` while the JSON schema says `[1,1,1]`. We follow the README prose and always
/// write explicit values on export, so a file we write never depends on either.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Toon {
    /// Colour of the shaded side (linear). Never grey: LK-01 wants a tinted shade.
    pub shade_color: Vec3,
    /// Multiplied into the shade colour.
    pub shade_multiply_texture: Option<TexRef>,
    /// Moves the shade boundary (−1..1).
    pub shading_shift: f32,
    /// Per-texel shift.
    pub shading_shift_texture: Option<ShiftTexture>,
    /// Sharpness of the boundary (0 soft, 1 hard).
    pub shading_toony: f32,
    /// How much ambient light is flattened (0..1).
    pub gi_equalization: f32,
    /// Multiplied into the matcap.
    pub matcap_color: Vec3,
    /// The matcap, if any.
    pub matcap_texture: Option<TexRef>,
    /// Rim light colour.
    pub rim_color: Vec3,
    /// Fresnel exponent of the rim.
    pub rim_fresnel_power: f32,
    /// Lifts the rim away from the silhouette.
    pub rim_lift: f32,
    /// Masks the rim.
    pub rim_multiply_texture: Option<TexRef>,
    /// How much scene lighting modulates the rim (0..1).
    pub rim_lighting_mix: f32,
    /// Outline mode.
    pub outline_mode: OutlineMode,
    /// Outline width (metres or screen fraction, by mode).
    pub outline_width: f32,
    /// Per-texel outline width factor.
    pub outline_width_texture: Option<TexRef>,
    /// Outline colour (linear).
    pub outline_color: Vec3,
    /// How much scene lighting modulates the outline (0..1).
    pub outline_lighting_mix: f32,
    /// UV scroll speed along U (per second).
    pub uv_scroll_x: f32,
    /// UV scroll speed along V (per second).
    pub uv_scroll_y: f32,
    /// UV rotation speed (radians per second).
    pub uv_rotation: f32,
    /// Masks the UV animation.
    pub uv_animation_mask: Option<TexRef>,
    /// For blended materials, whether depth is written.
    pub transparent_with_zwrite: bool,
    /// Render-order offset, −9..=9.
    pub render_queue_offset: i32,
}

impl Default for Toon {
    fn default() -> Self {
        Self {
            shade_color: Vec3::ZERO,
            shade_multiply_texture: None,
            shading_shift: 0.0,
            shading_shift_texture: None,
            shading_toony: 0.9,
            gi_equalization: 0.9,
            matcap_color: Vec3::ONE,
            matcap_texture: None,
            rim_color: Vec3::ZERO,
            rim_fresnel_power: 5.0,
            rim_lift: 0.0,
            rim_multiply_texture: None,
            rim_lighting_mix: 1.0,
            outline_mode: OutlineMode::None,
            outline_width: 0.0,
            outline_width_texture: None,
            outline_color: Vec3::ZERO,
            outline_lighting_mix: 1.0,
            uv_scroll_x: 0.0,
            uv_scroll_y: 0.0,
            uv_rotation: 0.0,
            uv_animation_mask: None,
            transparent_with_zwrite: false,
            render_queue_offset: 0,
        }
    }
}

/// The shading model of a material.
#[derive(Clone, Debug, PartialEq)]
pub enum Shading {
    /// glTF metallic-roughness.
    Pbr(Pbr),
    /// `KHR_materials_unlit`: base colour as is.
    Unlit,
    /// `MToon`. Boxed because it is much larger than the others.
    Toon(Box<Toon>),
}

/// A material. For toon materials `base_color` and `base_color_texture` are the lit colour, as in the `MToon` spec.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Material {
    /// Display name. Material names are what 0.x expressions refer to, so keep them unique when exporting 0.x.
    pub name: String,
    /// Linear RGBA factor.
    pub base_color: Vec4,
    /// Base colour texture (sRGB).
    pub base_color_texture: Option<TexRef>,
    /// Linear emissive factor (values above 1 need `KHR_materials_emissive_strength`).
    pub emissive: Vec3,
    /// Emissive texture (sRGB).
    pub emissive_texture: Option<TexRef>,
    /// Normal map.
    pub normal_texture: Option<NormalTexture>,
    /// Transparency handling.
    pub alpha: AlphaMode,
    /// Render both faces.
    pub double_sided: bool,
    /// How it is shaded.
    pub shading: Shading,
    /// Unknown extensions and extras on this material, carried through unchanged.
    pub passthrough: Passthrough,
}

impl Material {
    /// A named white material with the given shading.
    pub fn new(name: impl Into<String>, shading: Shading) -> Self {
        Self {
            name: name.into(),
            base_color: Vec4::ONE,
            base_color_texture: None,
            emissive: Vec3::ZERO,
            emissive_texture: None,
            normal_texture: None,
            alpha: AlphaMode::Opaque,
            double_sided: false,
            shading,
            passthrough: Passthrough::default(),
        }
    }
}

impl Material {
    /// Every texture slot in use, with its glTF-style slot name, for index checks and for exporters that collect
    /// the textures a material needs.
    pub fn texture_refs(&self) -> Vec<(&'static str, &TexRef)> {
        fn add<'a>(out: &mut Vec<(&'static str, &'a TexRef)>, slot: &'static str, t: Option<&'a TexRef>) {
            out.extend(t.map(|t| (slot, t)));
        }
        let mut out = Vec::new();
        add(&mut out, "baseColorTexture", self.base_color_texture.as_ref());
        add(&mut out, "emissiveTexture", self.emissive_texture.as_ref());
        match &self.shading {
            Shading::Pbr(p) => add(&mut out, "metallicRoughnessTexture", p.metallic_roughness_texture.as_ref()),
            Shading::Unlit => {}
            Shading::Toon(t) => {
                add(&mut out, "shadeMultiplyTexture", t.shade_multiply_texture.as_ref());
                add(&mut out, "matcapTexture", t.matcap_texture.as_ref());
                add(&mut out, "rimMultiplyTexture", t.rim_multiply_texture.as_ref());
                add(&mut out, "outlineWidthMultiplyTexture", t.outline_width_texture.as_ref());
                add(&mut out, "uvAnimationMaskTexture", t.uv_animation_mask.as_ref());
                out.extend(t.shading_shift_texture.as_ref().map(|s| ("shadingShiftTexture", &s.texture)));
            }
        }
        out.extend(self.normal_texture.as_ref().map(|n| ("normalTexture", &n.texture)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the defaults to the `MToon` 1.0 spec tables, so a careless edit shows up as a test failure.
    #[test]
    fn toon_defaults_match_the_mtoon_spec() {
        let t = Toon::default();
        assert_eq!((t.shading_toony, t.gi_equalization, t.rim_fresnel_power), (0.9, 0.9, 5.0));
        assert_eq!((t.rim_lighting_mix, t.outline_lighting_mix, t.outline_mode), (1.0, 1.0, OutlineMode::None));
        assert_eq!((t.matcap_color, t.shade_color, t.rim_color), (Vec3::ONE, Vec3::ZERO, Vec3::ZERO));
        assert_eq!(t.render_queue_offset, 0);
    }

    #[test]
    fn blob_clones_share_storage() {
        let a = Blob::new(vec![1u8, 2, 3]);
        let b = a.clone();
        assert_eq!(a, b);
        assert_eq!((a.len(), a.as_slice()), (3, &[1u8, 2, 3][..]));
        assert!(Blob::default().is_empty());
    }
}
