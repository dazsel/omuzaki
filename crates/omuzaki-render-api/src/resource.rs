//! Data that lives on the GPU for many frames: meshes, textures, render targets, and the images read back from them.
//!
//! Everything here is plain CPU-side data. A renderer copies it to the GPU at creation; the caller keeps its own copy
//! (the document store holds the source) because after a device loss the renderer re-uploads from the caller.

use omuzaki_core::glam::UVec2;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Colour, RenderError};

/// Limits every renderer guarantees. Raising one is compatible; lowering one is a breaking change.
pub mod limits {
    /// Lights in one [`Scene`](crate::Scene).
    pub const MAX_LIGHTS: usize = 8;
    /// Skinning matrices in one [`Scene`](crate::Scene) (all skinned instances together).
    pub const MAX_JOINTS: usize = 2048;
    /// Morph weights in one [`Scene`](crate::Scene) (all morphed instances together).
    pub const MAX_MORPH_WEIGHTS: usize = 4096;
    /// Overlays on one [`Material`](crate::Material).
    pub const MAX_OVERLAYS: usize = 4;
    /// The largest texture or target side every renderer supports; `Capabilities::max_texture_size` can be higher.
    pub const GUARANTEED_TEXTURE_SIZE: u32 = 4096;
    /// Parameters on one shading program ([`Shading::Program`](crate::Shading::Program)).
    pub const MAX_PROGRAM_PARAMS: usize = 32;
    /// The most samples per pixel [`Quality::samples`](crate::Quality::samples) may ask for.
    pub const MAX_SAMPLES: u8 = 16;
}

/// How colour channels relate to alpha in stored pixels (textures, readbacks).
///
/// With `Premultiplied`, the stored colour numbers are the straight ones multiplied by alpha, whatever the colour
/// space (so sRGB-encoded bytes are scaled by alpha, which is what OBS, Spout and NDI expect, BR-01). Renderers must
/// filter and mip-map premultiplied data correctly; with straight data they premultiply internally. Either way the
/// result has no dark or white fringes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlphaEncoding {
    /// Colour is independent of alpha (PNG files, canvases, `#rrggbbaa`).
    #[default]
    Straight,
    /// Colour is already multiplied by alpha (GPU painters, compositing; each live output converts to what it carries,
    /// see `docs/design/render-api.md`, "The rendered frame").
    Premultiplied,
}

/// Whether a texture's colour channels are sRGB encoded or linear data.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColourSpace {
    /// Colour pictures (albedo, face paint): decoded to linear light before shading.
    #[default]
    Srgb,
    /// Data (masks, ramps): used as stored.
    Linear,
}

/// Whether a texture can change after creation.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextureUsage {
    /// Fixed at creation. The renderer may place it wherever is fastest.
    #[default]
    Static,
    /// Repainted while the app runs, for example the face atlas ([`Renderer::paint_texture`](crate::Renderer)).
    Dynamic,
}

/// How a texture is read between texels.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    /// Smooth (the default; mip-mapped when the texture has mips).
    #[default]
    Linear,
    /// Hard texels: pixel-art style (LK-06) and masks.
    Nearest,
}

/// What happens outside `0..1`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Wrap {
    /// Edge texels repeat outward.
    #[default]
    Clamp,
    /// The texture tiles.
    Repeat,
}

/// How a texture is sampled.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Sampler {
    /// Between texels.
    pub filter: Filter,
    /// Outside `0..1`.
    pub wrap: Wrap,
    /// Anisotropic filtering taps, `1..=16` (1 = off). Sharpens faces seen at a slant.
    pub anisotropy: u8,
}

impl Default for Sampler {
    fn default() -> Self {
        Self { filter: Filter::Linear, wrap: Wrap::Clamp, anisotropy: 1 }
    }
}

/// A texture's shape and meaning. Pixels are always RGBA, 8 bits per channel, rows top to bottom.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct TextureDesc {
    /// Width and height in pixels, each `1..=Capabilities::max_texture_size`.
    #[schemars(with = "[u32; 2]")]
    pub size: UVec2,
    /// sRGB colour or linear data.
    pub colour_space: ColourSpace,
    /// Straight or premultiplied alpha.
    pub alpha: AlphaEncoding,
    /// Whether the renderer builds (and, for dynamic textures, rebuilds) a mip chain.
    pub mips: bool,
    /// Static or repaintable.
    pub usage: TextureUsage,
    /// How it is read.
    pub sampler: Sampler,
}

impl TextureDesc {
    /// A static sRGB colour texture with straight alpha, mip-mapped: what a loaded PNG usually wants.
    pub fn colour(size: UVec2) -> Self {
        Self {
            size,
            colour_space: ColourSpace::Srgb,
            alpha: AlphaEncoding::Straight,
            mips: true,
            usage: TextureUsage::Static,
            sampler: Sampler::default(),
        }
    }

    /// The same texture, repaintable.
    #[must_use]
    pub fn dynamic(mut self) -> Self {
        self.usage = TextureUsage::Dynamic;
        self
    }

    /// The bytes of one full level-0 image: `width * height * 4`.
    pub fn byte_len(&self) -> usize {
        rgba_len(self.size)
    }

    /// Checks the rules every renderer shares.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for a zero side or an anisotropy outside `1..=16`.
    pub fn validate(&self) -> Result<(), RenderError> {
        if self.size.x == 0 || self.size.y == 0 {
            return Err(RenderError::invalid(
                "texture",
                format!("size {}x{} has a zero side", self.size.x, self.size.y),
            ));
        }
        if !(1..=16).contains(&self.sampler.anisotropy) {
            return Err(RenderError::invalid(
                "texture",
                format!("anisotropy {} isn't in 1..=16", self.sampler.anisotropy),
            ));
        }
        Ok(())
    }
}

/// The bytes of a tightly packed RGBA8 image of `size`.
pub fn rgba_len(size: UVec2) -> usize {
    size.x as usize * size.y as usize * 4
}

/// A rectangle of pixels, origin top-left.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct PixelRect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl PixelRect {
    /// The whole of an image of `size`.
    pub fn full(size: UVec2) -> Self {
        Self { x: 0, y: 0, width: size.x, height: size.y }
    }

    /// True if the rectangle lies inside an image of `size`.
    pub fn fits(&self, size: UVec2) -> bool {
        u64::from(self.x) + u64::from(self.width) <= u64::from(size.x)
            && u64::from(self.y) + u64::from(self.height) <= u64::from(size.y)
    }

    /// The bytes of its pixels as tightly packed RGBA8.
    pub fn byte_len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

/// What a render target is for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct TargetDesc {
    /// Output size in pixels, each `1..=Capabilities::max_texture_size`.
    #[schemars(with = "[u32; 2]")]
    pub size: UVec2,
    /// How alpha is stored in what [`read_target`](crate::Renderer::read_target) returns (and, for GPU sharing, in the
    /// texture handed to the output). Irrelevant when the background is opaque.
    pub alpha: AlphaEncoding,
}

/// A finished frame on the CPU: RGBA8, sRGB-encoded colour, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width and height in pixels.
    pub size: UVec2,
    /// How alpha relates to colour in `rgba8`.
    pub alpha: AlphaEncoding,
    /// `size.x * size.y * 4` bytes.
    pub rgba8: Vec<u8>,
}

impl Image {
    /// Wraps pixel data.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] if `rgba8` isn't exactly `width * height * 4` bytes.
    pub fn new(size: UVec2, alpha: AlphaEncoding, rgba8: Vec<u8>) -> Result<Self, RenderError> {
        if rgba8.len() != rgba_len(size) {
            let reason =
                format!("{} bytes for a {}x{} RGBA image (needs {})", rgba8.len(), size.x, size.y, rgba_len(size));
            return Err(RenderError::invalid("image", reason));
        }
        Ok(Self { size, alpha, rgba8 })
    }

    /// The pixel at `(x, y)`, or `None` outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.size.x || y >= self.size.y {
            return None;
        }
        let i = (y as usize * self.size.x as usize + x as usize) * 4;
        self.rgba8.get(i..i + 4).map(|p| [p[0], p[1], p[2], p[3]])
    }

    /// The pixel as a [`Colour`] (colour as stored; check [`Image::alpha`] before treating it as straight).
    pub fn colour(&self, x: u32, y: u32) -> Option<Colour> {
        self.pixel(x, y).map(|[r, g, b, a]| Colour::rgba(r, g, b, a))
    }
}

/// How much a renderer holds, for leak tests and the RAM/VRAM budgets (§10).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResourceStats {
    /// Live meshes.
    pub meshes: u32,
    /// Live textures.
    pub textures: u32,
    /// Live materials.
    pub materials: u32,
    /// Live render targets.
    pub targets: u32,
    /// Live environments.
    pub environments: u32,
    /// Estimated GPU memory held by all of the above, in bytes. Released resources leave the counts at once but may
    /// stay in this figure until the frames still using them finish.
    pub gpu_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_desc_checks_size_and_anisotropy() {
        let ok = TextureDesc::colour(UVec2::new(4, 2));
        assert_eq!((ok.validate(), ok.byte_len()), (Ok(()), 32));
        assert!(TextureDesc::colour(UVec2::new(0, 2)).validate().is_err());
        let mut aniso = ok;
        aniso.sampler.anisotropy = 17;
        assert!(aniso.validate().is_err());
    }

    #[test]
    fn rects_know_if_they_fit() {
        let size = UVec2::new(8, 8);
        assert!(PixelRect::full(size).fits(size));
        assert!(!PixelRect { x: 4, y: 0, width: 5, height: 1 }.fits(size));
        assert!(!PixelRect { x: u32::MAX, y: 0, width: 2, height: 1 }.fits(size), "no overflow wrap-around");
    }

    #[test]
    fn images_check_their_length_and_index_pixels() {
        assert!(Image::new(UVec2::new(2, 1), AlphaEncoding::Straight, vec![0; 7]).is_err());
        let img = Image::new(UVec2::new(2, 1), AlphaEncoding::Straight, vec![1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        assert_eq!(img.pixel(1, 0), Some([5, 6, 7, 8]));
        assert_eq!(img.pixel(2, 0), None);
        assert_eq!(img.colour(0, 0), Some(Colour::rgba(1, 2, 3, 4)));
    }
}
