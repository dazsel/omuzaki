//! An image backdrop on the renderer (scenes D2, #930): the picture a resolved scene shows behind the cast, uploaded
//! as a texture when its asset changes and shown by every frame with the scene's fit and bars. Both stages hold one,
//! as they hold an environment.
//!
//! The picture's bytes come from the asset store, which the render thread never reads: whoever resolves the scene and
//! has the store decodes them ([`ResolvedScene::load_backdrop`]), so they travel with the resolved scene and the
//! render thread only uploads them. Until the texture is up, the frame shows the bars' gradient where the picture will
//! be.

use std::sync::Arc;

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{Background, Renderer, TextureDesc, TextureId};
use omuzaki_stage_api::SceneError;
use omuzaki_store_api::{ContentHash, Store};

use super::resolve::{ResolvedImage, ResolvedScene};
use crate::error::StageError;

/// A backdrop picture's pixels, decoded once and shared by every copy of the resolved scene that carries them.
#[derive(Clone, Debug)]
pub struct ImagePixels {
    asset: ContentHash,
    size: UVec2,
    /// Straight sRGB RGBA8, rows top to bottom.
    rgba: Arc<Vec<u8>>,
}

impl PartialEq for ImagePixels {
    /// The same asset is the same pixels: comparing them would read megabytes.
    fn eq(&self, other: &Self) -> bool {
        self.asset == other.asset
    }
}

impl ImagePixels {
    /// Decodes a PNG or JPEG into straight sRGB RGBA8.
    ///
    /// # Errors
    /// [`StageError::Scene`] at `scene.backdrop.asset` for bytes that aren't a PNG or a JPEG.
    pub fn decode(asset: ContentHash, bytes: &[u8]) -> Result<Self, StageError> {
        let image = image::load_from_memory(bytes).map_err(|e| StageError::Scene {
            source: SceneError::new(
                "scene.backdrop.asset",
                format!("the image {asset} can't be read as a PNG or JPEG: {e}"),
            ),
        })?;
        let rgba = image.into_rgba8();
        let size = UVec2::new(rgba.width(), rgba.height());
        Ok(Self { asset, size, rgba: Arc::new(rgba.into_raw()) })
    }

    /// The asset the pixels came from.
    pub fn asset(&self) -> ContentHash {
        self.asset
    }

    /// Width and height in pixels.
    pub fn size(&self) -> UVec2 {
        self.size
    }
}

impl ResolvedScene {
    /// Loads the pixels of the scene's backdrop picture from `store`, unless they are already here; any other backdrop
    /// needs nothing. Call it after [`resolve_into`](Self::resolve_into), off the render thread: it reads and decodes
    /// the file.
    ///
    /// # Errors
    /// [`StageError::Scene`] at `scene.backdrop.asset` for an asset the store lacks or can't read, or one that isn't a
    /// PNG or a JPEG.
    pub fn load_backdrop(&mut self, store: &dyn Store) -> Result<(), StageError> {
        let Some(asset) = self.backdrop_image.map(|i| i.asset) else { return Ok(()) };
        if self.image.as_ref().is_some_and(|i| i.asset == asset) {
            return Ok(());
        }
        let bytes = store.get(&asset).map_err(|e| StageError::Scene {
            source: SceneError::new("scene.backdrop.asset", format!("the image {asset} can't be read: {e}")),
        })?;
        self.image = Some(ImagePixels::decode(asset, &bytes)?);
        Ok(())
    }
}

/// The backdrop picture on a renderer, or none, and the asset it was made from.
#[derive(Debug, Default)]
pub(crate) struct StageBackdrop {
    live: Option<(ContentHash, TextureId)>,
}

impl StageBackdrop {
    /// Makes the renderer's backdrop texture `wanted`'s picture from `pixels`: the old one is released and the new one
    /// uploaded with its mips. Cold, in proportion to the picture's size. On an error the stage has no picture.
    ///
    /// # Errors
    /// [`StageError::Scene`] at `scene.backdrop.asset` when the pixels weren't loaded
    /// ([`ResolvedScene::load_backdrop`]); [`StageError::Render`] if the renderer refuses the texture (one larger than
    /// it can hold, for one).
    pub(crate) fn set(
        &mut self,
        renderer: &mut dyn Renderer,
        wanted: Option<ContentHash>,
        pixels: Option<&ImagePixels>,
    ) -> Result<(), StageError> {
        self.release(renderer)?;
        let Some(asset) = wanted else { return Ok(()) };
        let pixels = pixels.filter(|p| p.asset == asset).ok_or_else(|| StageError::Scene {
            source: SceneError::new(
                "scene.backdrop.asset",
                format!("the image {asset} wasn't loaded from the asset store before the scene was set"),
            ),
        })?;
        let id = renderer
            .create_texture(&TextureDesc::colour(pixels.size), Some(&pixels.rgba))
            .map_err(|source| StageError::render("the backdrop image", source))?;
        self.live = Some((asset, id));
        Ok(())
    }

    /// The background a frame shows for `wanted`: the picture over its bars once its texture is up, `None` without a
    /// picture or while the one on the renderer is another (the frame keeps the bars the resolve gave it). Hot path: a
    /// comparison.
    pub(crate) fn background_for(&self, wanted: Option<&ResolvedImage>) -> Option<Background> {
        let (wanted, (asset, texture)) = wanted.zip(self.live.as_ref())?;
        (wanted.asset == *asset).then_some(Background::Texture {
            texture: *texture,
            fit: wanted.fit,
            top: wanted.top,
            bottom: wanted.bottom,
        })
    }

    /// Gives the texture back to `renderer`.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses the release; the stage forgets it either way.
    pub(crate) fn release(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        let Some((_, id)) = self.live.take() else { return Ok(()) };
        renderer.release(id.into()).map_err(|source| StageError::render("the backdrop image", source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(size: UVec2, rgba: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_raw(size.x, size.y, rgba.to_vec())
            .unwrap()
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn a_png_decodes_to_straight_rgba_rows() {
        let rgba = [255, 0, 0, 255, 0, 255, 0, 128];
        let bytes = png(UVec2::new(2, 1), &rgba);
        let pixels = ImagePixels::decode(ContentHash::of(&bytes), &bytes).unwrap();
        assert_eq!(pixels.size(), UVec2::new(2, 1));
        assert_eq!(pixels.rgba.as_slice(), &rgba);
    }

    #[test]
    fn bytes_that_are_not_a_picture_are_refused_at_the_asset() {
        let error = ImagePixels::decode(ContentHash::of(b"text"), b"not a picture").unwrap_err();
        assert!(matches!(error, StageError::Scene { ref source } if source.path == "scene.backdrop.asset"));
    }
}
