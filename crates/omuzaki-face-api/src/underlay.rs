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
    /// A newer request for the same character has already changed what is shown, so this one must not.
    #[error("a newer underlay request for this character replaced this one")]
    Superseded,
    /// The app failed while showing the underlay.
    #[error("{0}")]
    Failed(String),
}

/// What `face.underlay.set` needs from the app, injected as a service. The stage implements it: it owns the head
/// material and the textures.
///
/// Jobs for one character can overlap, and the CPU lane runs several at once, so every change names a `ticket` that
/// orders the requests: a larger ticket is a newer request. The op passes its job id, which the job manager gives in the
/// order requests were admitted, not the order workers reach the app. The app keeps the largest ticket it has applied
/// for each character and refuses a change with a smaller one, deciding that in the same step as the change, so a slower,
/// older request can never replace what a newer one showed, nor set an opacity on pixels it did not build.
pub trait UnderlayTarget: Send + Sync {
    /// The head of `character` as it is on screen now.
    ///
    /// # Errors
    /// [`UnderlayTargetError::NotFound`] when the app shows no such character.
    fn head(&self, character: &str) -> Result<UnderlayHead, UnderlayTargetError>;

    /// The [`UnderlayImage::key`] of the underlay now shown for `character`, if any. Only a preview for planning: the
    /// answer can be out of date by the time a job acts, which is why changes carry a ticket.
    fn shown(&self, character: &str) -> Option<String>;

    /// Sets the strength of the underlay shown for `character`, `0..=1`, and nothing else (no new pixels), when no
    /// newer ticket has been applied **and** the shown underlay has this `key`, both decided in one step. A change
    /// that is made records `ticket` as the newest applied. Returns `false`, changing nothing, when nothing is shown or
    /// what is shown has another key: the caller then builds the pixels.
    ///
    /// # Errors
    /// [`UnderlayTargetError::Superseded`] when a newer ticket than `ticket` has been applied for `character`.
    fn set_opacity(&self, character: &str, ticket: u64, key: &str, opacity: f32) -> Result<bool, UnderlayTargetError>;

    /// Shows `image` under the Base layer of `character`'s face, replacing the one shown before, when no newer ticket
    /// has been applied, and records `ticket` as the newest applied.
    ///
    /// # Errors
    /// [`UnderlayTargetError::Superseded`] when a newer ticket than `ticket` has been applied for `character`;
    /// [`UnderlayTargetError::Failed`] when the app cannot show it.
    fn show(&self, character: &str, ticket: u64, image: UnderlayImage) -> Result<(), UnderlayTargetError>;
}
