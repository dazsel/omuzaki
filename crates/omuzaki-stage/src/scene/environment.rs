//! The scene's environment on the renderer (LK-08, design D3): the sky or HDRI a resolved scene owes, made when its
//! source changes and used by every frame with the scene's intensity, turn and toon ramp. Both stages hold one, as they
//! hold a ground.
//!
//! A sky is made from its colours. An HDRI's pixels come from the asset store, which the render thread never reads:
//! whoever resolves the scene and has the store loads them ([`ResolvedScene::load_hdri`]), so they travel with the
//! resolved scene and the render thread only uploads them.

use std::sync::Arc;

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{EnvironmentDesc, EnvironmentId, EnvironmentUse, Renderer};
use omuzaki_stage_api::{EnvironmentSource, SceneError};
use omuzaki_store_api::{ContentHash, Store};

use super::resolve::{ResolvedEnvironment, ResolvedScene};
use crate::error::StageError;

/// An HDRI's pixels, decoded once and shared by every copy of the resolved scene that carries them.
#[derive(Clone, Debug)]
pub struct HdriPixels {
    asset: ContentHash,
    desc: Arc<EnvironmentDesc>,
}

impl PartialEq for HdriPixels {
    /// The same asset is the same pixels: comparing them would read megabytes.
    fn eq(&self, other: &Self) -> bool {
        self.asset == other.asset
    }
}

impl HdriPixels {
    /// Decodes a Radiance `.hdr` file into the renderer's equirectangular layout (linear RGB, rows from straight up
    /// to straight down, as `.hdr` files are written).
    ///
    /// # Errors
    /// [`StageError::Scene`] at `lighting.environment.asset` for bytes that aren't a Radiance image, or pixels that
    /// aren't finite and non-negative.
    pub fn decode(asset: ContentHash, bytes: &[u8]) -> Result<Self, StageError> {
        let refuse = |why: String| StageError::Scene { source: SceneError::new("lighting.environment.asset", why) };
        let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Hdr)
            .map_err(|e| refuse(format!("the HDRI {asset} isn't a Radiance .hdr image: {e}")))?
            .into_rgb32f();
        let size = UVec2::new(image.width(), image.height());
        let desc = EnvironmentDesc::Hdri { size, rgb: image.into_raw() };
        desc.validate().map_err(|e| refuse(format!("the HDRI {asset}: {e}")))?;
        Ok(Self { asset, desc: Arc::new(desc) })
    }

    /// The asset the pixels came from.
    pub fn asset(&self) -> ContentHash {
        self.asset
    }
}

impl ResolvedScene {
    /// Loads the pixels of the scene's HDRI from `store`, unless they are already here; a sky or no environment needs
    /// nothing. Call it after [`resolve_into`](Self::resolve_into), off the render thread: it reads and decodes the file.
    ///
    /// # Errors
    /// [`StageError::Scene`] at `lighting.environment.asset` for an asset the store lacks or can't read, or one that
    /// isn't a Radiance image.
    pub fn load_hdri(&mut self, store: &dyn Store) -> Result<(), StageError> {
        let Some(asset) = self.environment.as_ref().and_then(ResolvedEnvironment::hdri) else { return Ok(()) };
        if self.hdri.as_ref().is_some_and(|h| h.asset == asset) {
            return Ok(());
        }
        let bytes = store.get(&asset).map_err(|e| StageError::Scene {
            source: SceneError::new("lighting.environment.asset", format!("the HDRI {asset} can't be read: {e}")),
        })?;
        self.hdri = Some(HdriPixels::decode(asset, &bytes)?);
        Ok(())
    }
}

/// The environment on a renderer, or none, and the source it was made from.
#[derive(Debug, Default)]
pub(crate) struct StageEnvironment {
    live: Option<(EnvironmentSource, EnvironmentId)>,
}

impl StageEnvironment {
    /// Makes the renderer's environment `wanted`'s source: the old one is released and a new one made, its pixels from
    /// `hdri` for an HDRI. Cold: a sky bakes in milliseconds, an HDRI in proportion to its size. On an error the stage
    /// has no environment.
    ///
    /// # Errors
    /// [`StageError::Scene`] at `lighting.environment.asset` when the HDRI's pixels weren't loaded
    /// ([`ResolvedScene::load_hdri`]); [`StageError::Render`] if the renderer refuses the environment.
    pub(crate) fn set(
        &mut self,
        renderer: &mut dyn Renderer,
        wanted: Option<EnvironmentSource>,
        hdri: Option<&HdriPixels>,
    ) -> Result<(), StageError> {
        self.release(renderer)?;
        let Some(source) = wanted else { return Ok(()) };
        let sky;
        let desc = match source {
            EnvironmentSource::Sky { zenith, horizon, ground } => {
                sky = EnvironmentDesc::Sky { zenith, horizon, ground };
                &sky
            }
            EnvironmentSource::Hdri { asset } => {
                let pixels = hdri.filter(|h| h.asset == asset).ok_or_else(|| StageError::Scene {
                    source: SceneError::new(
                        "lighting.environment.asset",
                        format!("the HDRI {asset} wasn't loaded from the asset store before the scene was set"),
                    ),
                })?;
                pixels.desc.as_ref()
            }
        };
        let id = renderer.create_environment(desc).map_err(|source| StageError::render("the environment", source))?;
        self.live = Some((source, id));
        Ok(())
    }

    /// How a frame uses the environment `wanted` asks for: `None` without one, or while the one on the renderer is
    /// another (the work is still owed). Hot path: a comparison.
    pub(crate) fn use_for(&self, wanted: Option<&ResolvedEnvironment>) -> Option<EnvironmentUse> {
        let (wanted, (source, id)) = wanted.zip(self.live.as_ref())?;
        (wanted.source == *source).then_some(EnvironmentUse {
            id: *id,
            intensity: wanted.intensity,
            rotation_deg: wanted.rotation_deg,
            toon_ramp: wanted.toon_ramp,
        })
    }

    /// Gives the environment back to `renderer`.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses the release; the stage forgets it either way.
    pub(crate) fn release(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        let Some((_, id)) = self.live.take() else { return Ok(()) };
        renderer.release(id.into()).map_err(|source| StageError::render("the environment", source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2 x 1 Radiance file: run-length encoding is only for rows of 8 to 32767 pixels, so these are flat RGBE.
    fn tiny_hdr() -> Vec<u8> {
        let mut bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 2\n".to_vec();
        // (1, 0.5, 0.25) and (2, 2, 2): mantissas over 256 times 2^(exponent - 128).
        bytes.extend_from_slice(&[128, 64, 32, 129, 128, 128, 128, 130]);
        bytes
    }

    #[test]
    fn a_radiance_file_decodes_to_linear_rows() {
        let asset = ContentHash::of(&tiny_hdr());
        let pixels = HdriPixels::decode(asset, &tiny_hdr()).unwrap();
        let EnvironmentDesc::Hdri { size, rgb } = pixels.desc.as_ref() else { panic!("not an HDRI") };
        assert_eq!(*size, UVec2::new(2, 1));
        assert_eq!(rgb, &[1.0, 0.5, 0.25, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn bytes_that_are_not_radiance_are_refused_at_the_asset() {
        let error = HdriPixels::decode(ContentHash::of(b"png"), b"\x89PNG\r\n").unwrap_err();
        assert!(matches!(error, StageError::Scene { ref source } if source.path == "lighting.environment.asset"));
    }
}
