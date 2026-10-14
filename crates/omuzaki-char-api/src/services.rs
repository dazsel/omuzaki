//! Build-time services: what a part needs while it builds but cannot reach by itself (decision G5, ADR 0012).
//!
//! An implementation crate may depend only on API crates, so `omuzaki-char` can neither call the face rasteriser in
//! `omuzaki-face` nor open the asset store. The app instead **injects** both as API types in [`BuildServices`], the
//! last argument of [`CharacterBuilder::build`](crate::CharacterBuilder::build), and a part reaches them through the
//! [`BuildCtx`]:
//! - [`BuildCtx::asset`] reads a stored asset by content hash (the head reads a traced radius map this way);
//! - [`BuildCtx::paint_texture`] draws a procedural texture (plaid, tie stripes, cloth weave, a robe's bands) with a
//!   fresh rasteriser from the [`RasterizerFactory`]. [`RecolourCtx::paint_texture`] is the same call, for repainting
//!   a texture when only the palette changed.
//!
//! Services are **not** part of [`BuildOptions`](crate::BuildOptions) and not part of a cache key: they change how a
//! build gets its inputs, never what the same inputs build (assets are content-addressed, the rasteriser is a
//! deterministic function of its draw list). Both are optional; a part that needs a missing one fails with a named
//! error instead of building something wrong.

use std::fmt;
use std::sync::Arc;

use omuzaki_core::glam::UVec2;
use omuzaki_face_api::{DrawList, FaceLayerId, FaceLayerSpec, FaceRasterizer, FaceRect, Raster};
use omuzaki_render_api::{AlphaEncoding, ColourSpace, TextureDesc};
use omuzaki_store_api::ContentHash;

use crate::error::BuildError;
use crate::geometry::TextureData;
use crate::ids::PartId;
use crate::kind::{BuildCtx, RecolourCtx};

/// Why an asset could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssetError {
    /// The source has no asset with this hash.
    #[error("asset {0} isn't in the store: check the hash in the part's parameters, or import the asset first")]
    NotFound(ContentHash),
    /// The build was given no asset source at all.
    #[error("this build has no asset reader: set `assets` in the BuildServices (the app wires the store in)")]
    Unavailable,
    /// The source has the asset but could not return it (an I/O error, a failed hash check).
    #[error("asset {hash} couldn't be read: {message}")]
    Unreadable {
        /// The asset.
        hash: ContentHash,
        /// What went wrong.
        message: String,
    },
}

/// Reads assets (traced maps, images, meshes) by content hash while a character builds.
///
/// The app implements it over the asset store (`omuzaki-store-api`); tests use an in-memory map. Reads are
/// content-addressed, so the same hash always returns the same bytes and a build stays a pure function of the character.
pub trait AssetSource: Send + Sync {
    /// The bytes of the asset named `hash`. A shared slice, so a cache can hand it out without copying.
    ///
    /// # Errors
    /// [`AssetError::NotFound`] if there is no such asset, [`AssetError::Unreadable`] if it exists but can't be read.
    fn get(&self, hash: &ContentHash) -> Result<Arc<[u8]>, AssetError>;
}

/// Makes a rasteriser. [`BuildCtx::paint_texture`] calls it once per texture, so rasterisers need no shared state and
/// a parallel builder never shares one between threads.
pub type RasterizerFactory = Arc<dyn Fn() -> Box<dyn FaceRasterizer> + Send + Sync>;

/// What the app injects into a build. Cheap to clone (shared handles); [`Default`] provides nothing.
#[derive(Clone, Default)]
pub struct BuildServices {
    /// Where [`BuildCtx::asset`] reads from.
    pub assets: Option<Arc<dyn AssetSource>>,
    /// Where [`BuildCtx::paint_texture`] gets its rasteriser.
    pub rasterizer: Option<RasterizerFactory>,
}

impl BuildServices {
    /// The same services, reading assets from `assets`.
    #[must_use]
    pub fn with_assets(mut self, assets: impl AssetSource + 'static) -> Self {
        self.assets = Some(Arc::new(assets));
        self
    }

    /// The same services, painting textures with the rasterisers `factory` makes.
    #[must_use]
    pub fn with_rasterizer(mut self, factory: impl Fn() -> Box<dyn FaceRasterizer> + Send + Sync + 'static) -> Self {
        self.rasterizer = Some(Arc::new(factory));
        self
    }
}

impl fmt::Debug for BuildServices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuildServices")
            .field("assets", &self.assets.is_some())
            .field("rasterizer", &self.rasterizer.is_some())
            .finish()
    }
}

/// The services as one part reaches them. [`BuildCtx`] and [`RecolourCtx`] both delegate here, so a part's texture is
/// painted by the same code, with the same checks and errors, whether it is being built or recoloured.
struct PartServices<'a> {
    part: &'a PartId,
    services: &'a BuildServices,
}

impl PartServices<'_> {
    fn fail(&self, message: impl Into<String>) -> BuildError {
        BuildError::Part { part: self.part.clone(), message: message.into() }
    }

    fn asset(&self, hash: &ContentHash) -> Result<Arc<[u8]>, AssetError> {
        self.services.assets.as_ref().ok_or(AssetError::Unavailable)?.get(hash)
    }

    fn asset_or_fail(&self, hash: &ContentHash) -> Result<Arc<[u8]>, BuildError> {
        self.asset(hash).map_err(|error| BuildError::Asset { part: self.part.clone(), error })
    }

    fn paint_texture(
        &self,
        size_px: UVec2,
        rect: FaceRect,
        list: &DrawList,
        desc: TextureDesc,
    ) -> Result<TextureData, BuildError> {
        let factory = self
            .services
            .rasterizer
            .as_ref()
            .ok_or_else(|| BuildError::Missing { part: self.part.clone(), needs: "a 2D rasteriser" })?;
        let desc = TextureDesc { size: size_px, ..desc };
        desc.validate().map_err(|e| self.fail(format!("texture description: {e}")))?;
        if desc.colour_space != ColourSpace::Srgb {
            return Err(self.fail("a painted texture is sRGB colour: linear data can't come from the rasteriser"));
        }
        if !(rect.width() > 0.0 && rect.height() > 0.0) {
            return Err(self.fail("the texture rectangle must be non-empty"));
        }
        #[allow(clippy::cast_precision_loss)] // texture sizes are far below 2^24
        let px_per_m = size_px.x as f32 / rect.width();
        let spec = FaceLayerSpec::for_rect(rect, FaceLayerId::Base, px_per_m);
        if spec.size_px() != (size_px.x, size_px.y) {
            return Err(self.fail(format!(
                "a {}x{} px texture can't cover a {} x {} m rectangle with square pixels: match the aspect ratios",
                size_px.x,
                size_px.y,
                rect.width(),
                rect.height()
            )));
        }
        let mut raster = Raster::new(size_px.x, size_px.y);
        factory()
            .rasterize(&spec, list, px_per_m, &mut raster)
            .map_err(|e| self.fail(format!("painting a texture failed: {e}")))?;
        if (raster.width, raster.height) != (size_px.x, size_px.y) {
            return Err(self.fail(format!(
                "the rasteriser returned {}x{} px for a {}x{} px texture",
                raster.width, raster.height, size_px.x, size_px.y
            )));
        }
        let mut rgba8 = raster.pixels;
        if desc.alpha == AlphaEncoding::Straight {
            unpremultiply(&mut rgba8);
        }
        Ok(TextureData { desc, rgba8 })
    }
}

impl BuildCtx<'_> {
    fn part_services(&self) -> PartServices<'_> {
        PartServices { part: self.part, services: self.services }
    }

    /// The bytes of the stored asset `hash`.
    ///
    /// # Errors
    /// [`AssetError::Unavailable`] if the build has no asset source, [`AssetError::NotFound`] if the source lacks the
    /// asset. A part that wants a [`BuildError`] uses [`BuildCtx::asset_or_fail`], which keeps the error typed.
    pub fn asset(&self, hash: &ContentHash) -> Result<Arc<[u8]>, AssetError> {
        self.part_services().asset(hash)
    }

    /// The bytes of the stored asset `hash`, or a [`BuildError::Asset`] naming this part. A part writes
    /// `let bytes = ctx.asset_or_fail(&hash)?;`.
    ///
    /// # Errors
    /// [`BuildError::Asset`] carrying the [`AssetError`] that [`BuildCtx::asset`] returns.
    pub fn asset_or_fail(&self, hash: &ContentHash) -> Result<Arc<[u8]>, BuildError> {
        self.part_services().asset_or_fail(hash)
    }

    /// Paints `list` into a `size_px` texture. `rect` is the part of the drawing plane (metres, y up) that the texture
    /// covers, so a stripe 2 mm wide is the same 2 mm at any `size_px`; row 0 of the texture is the top of `rect`.
    /// `desc` says how the texture is read (wrap mode, mips, filtering); its `size` is replaced by `size_px`.
    ///
    /// The rasteriser is a fresh one from the build's [`RasterizerFactory`]. Its premultiplied sRGB output is stored
    /// as `desc.alpha` asks, so the same `list` gives byte-identical pixels on every build.
    ///
    /// # Errors
    /// [`BuildError::Missing`] (`a 2D rasteriser`) if the build has none; [`BuildError::Part`] if `rect` is empty or
    /// its aspect differs from `size_px` (pixels must be square), if `desc` is not an sRGB colour texture, or if the
    /// rasteriser rejects the list.
    pub fn paint_texture(
        &self,
        size_px: UVec2,
        rect: FaceRect,
        list: &DrawList,
        desc: TextureDesc,
    ) -> Result<TextureData, BuildError> {
        self.part_services().paint_texture(size_px, rect, list, desc)
    }
}

impl RecolourCtx<'_> {
    fn part_services(&self) -> PartServices<'_> {
        PartServices { part: self.part, services: self.services }
    }

    /// The bytes of the stored asset `hash`, or a [`BuildError::Asset`] naming this part (as
    /// [`BuildCtx::asset_or_fail`]).
    ///
    /// # Errors
    /// [`BuildError::Asset`] carrying the [`AssetError`].
    pub fn asset_or_fail(&self, hash: &ContentHash) -> Result<Arc<[u8]>, BuildError> {
        self.part_services().asset_or_fail(hash)
    }

    /// Repaints a texture: the same call as [`BuildCtx::paint_texture`], with the same rasteriser and the same checks,
    /// so a list painted at the same size gives the same pixels as in the build.
    ///
    /// # Errors
    /// As [`BuildCtx::paint_texture`].
    pub fn paint_texture(
        &self,
        size_px: UVec2,
        rect: FaceRect,
        list: &DrawList,
        desc: TextureDesc,
    ) -> Result<TextureData, BuildError> {
        self.part_services().paint_texture(size_px, rect, list, desc)
    }
}

/// Premultiplied to straight alpha, in integers (round to nearest), so the result is identical on every platform.
fn unpremultiply(rgba8: &mut [u8]) {
    for [r, g, b, a] in rgba8.as_chunks_mut::<4>().0 {
        let a = u32::from(*a);
        if a == 0 || a == 255 {
            continue;
        }
        for c in [r, g, b] {
            *c = u8::try_from(((u32::from(*c) * 255 + a / 2) / a).min(255)).unwrap_or(u8::MAX);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpremultiplying_rounds_and_leaves_opaque_and_clear_pixels_alone() {
        let mut px = [100, 50, 0, 200, 9, 8, 7, 255, 0, 0, 0, 0, 255, 128, 1, 128];
        unpremultiply(&mut px);
        assert_eq!(px, [128, 64, 0, 200, 9, 8, 7, 255, 0, 0, 0, 0, 255, 255, 2, 128]);
    }

    #[test]
    fn services_debug_shows_what_is_wired_without_printing_handles() {
        let none = BuildServices::default();
        assert_eq!(format!("{none:?}"), "BuildServices { assets: false, rasterizer: false }");
    }
}
