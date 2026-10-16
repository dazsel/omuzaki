//! What `face.underlay.set` (CB-25) needs from the app that shows the head: the head's placement, and a place to put
//! the sheet's pixels. The app (the stage) implements [`UnderlayTarget`] and registers it as a service; the op lives in
//! `omuzaki-face` and builds the pixels, so neither crate names the other's types.
//!
//! The pixels travel as plain data ([`UnderlayImage`]): a size, premultiplied RGBA8, an opacity and a placement. A
//! character is named by its id as text, because the character crate depends on this one.

use omuzaki_core::glam::{UVec2, Vec3};

use crate::{FaceProjection, PlanarWindow};

/// The head an underlay lands on, as the app knows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnderlayHead {
    /// Where the painted face layers land on the head.
    pub projection: FaceProjection,
    /// The head centre in bind space.
    pub head_centre: Vec3,
    /// The face layers' density, pixels per metre: the underlay is built at the Base layer's resolution.
    pub px_per_m: f32,
    /// The face rectangle of the character's locked sheet as `[x, y, width, height]` in sheet pixels, when the app has
    /// one.
    pub sheet_face: Option<[u32; 4]>,
}

/// The pixels of one underlay and where they land: everything the app needs to make the texture and the overlay.
#[derive(Clone, Debug, PartialEq)]
pub struct UnderlayImage {
    /// Identifies what the pixels were built from (the sheet, the face rectangle, the alignment, the head's placement
    /// and density). Equal keys mean equal pixels, so the app reports the key it shows ([`UnderlayTarget::shown`]) and
    /// the op skips the rebuild when only the opacity changed.
    pub key: String,
    /// Width and height in pixels.
    pub size: UVec2,
    /// Premultiplied sRGB RGBA8, rows top to bottom, `4 * size.x * size.y` bytes.
    pub rgba: Vec<u8>,
    /// The overlay strength, `0..=1`; zero hides the underlay.
    pub opacity: f32,
    /// Where the underlay lands on the head: the Base layer's window.
    pub placement: PlanarWindow,
}

/// Why the app could not do what the op asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnderlayTargetError {
    /// The app shows no such character, or has nothing shown for it.
    #[error("{0}")]
    NotFound(String),
    /// The app failed while showing the underlay.
    #[error("{0}")]
    Failed(String),
}

/// What `face.underlay.set` needs from the app, injected as a service. The stage implements it: it owns the head
/// material and the textures.
pub trait UnderlayTarget: Send + Sync {
    /// The head of `character` as it is on screen now.
    ///
    /// # Errors
    /// [`UnderlayTargetError::NotFound`] when the app shows no such character.
    fn head(&self, character: &str) -> Result<UnderlayHead, UnderlayTargetError>;

    /// The [`UnderlayImage::key`] of the underlay now shown for `character`, if any.
    fn shown(&self, character: &str) -> Option<String>;

    /// Shows `image` under the Base layer of `character`'s face, replacing the one shown before.
    ///
    /// # Errors
    /// [`UnderlayTargetError`] when the app cannot show it.
    fn show(&self, character: &str, image: UnderlayImage) -> Result<(), UnderlayTargetError>;

    /// Sets the strength of the underlay shown for `character`, `0..=1`, and nothing else: a material update, with no
    /// new pixels.
    ///
    /// # Errors
    /// [`UnderlayTargetError::NotFound`] when nothing is shown for `character`.
    fn set_opacity(&self, character: &str, opacity: f32) -> Result<(), UnderlayTargetError>;
}
