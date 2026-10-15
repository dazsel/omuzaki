//! Why the stage could not do what it was asked. Every variant says what to fix.

use omuzaki_char_api::{MaterialError, MaterialName, PartId};
use omuzaki_render_api::RenderError;

/// A stage operation failed. Whatever the operation had created on the renderer before failing is released again, so
/// an error never leaves resources behind.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum StageError {
    /// The renderer refused a call (a bad mesh, no memory, a lost device).
    #[error("the renderer refused {what}: {source}")]
    Render {
        /// What the stage was doing (`mesh 'hair-curtain' of part 'hair'`).
        what: String,
        /// The renderer's answer.
        source: RenderError,
    },
    /// A material names a texture or face layer the stage has no handle for.
    #[error("material '{material}': {source}")]
    Material {
        /// The material.
        material: MaterialName,
        /// What is missing.
        source: MaterialError,
    },
    /// A mesh uses a material the character doesn't have.
    #[error("part '{part}': mesh '{mesh}' uses material '{material}', which the character doesn't define")]
    UnknownMaterial {
        /// The part.
        part: PartId,
        /// The mesh.
        mesh: String,
        /// The missing material.
        material: MaterialName,
    },
    /// The character passed in is not the one that was uploaded.
    #[error("the character differs from the uploaded one: {what} '{name}' was not uploaded")]
    NotUploaded {
        /// `material`.
        what: &'static str,
        /// Its name.
        name: String,
    },
    /// The character's rig can't be run: its inverse bind matrices or spring set don't fit its skeleton.
    #[error("the character's rig can't run: {reason}")]
    Rig {
        /// What is wrong, as the rig crates say it.
        reason: String,
    },
    /// The face system could not map, resolve or paint a frame.
    #[error("the face system failed: {reason}")]
    Face {
        /// What the face crates reported.
        reason: String,
    },
    /// A variant or palette change could not rebuild the character.
    #[error("the character could not be rebuilt: {reason}")]
    Rebuild {
        /// What the builder or the variant resolution reported.
        reason: String,
    },
    /// A face density that can't make a texture.
    #[error("face density {px_per_m} px/m is not usable: give a finite number above zero")]
    FaceDensity {
        /// The offending value.
        px_per_m: f32,
    },
    /// The character can't be framed: a landmark is missing or a number isn't usable.
    #[error("the character can't be framed: {reason}")]
    Framing {
        /// What is missing or unusable.
        reason: String,
    },
}

impl StageError {
    /// A [`StageError::Render`] for `what`, shorter at the many call sites that wrap a renderer error.
    pub(crate) fn render(what: impl Into<String>, source: RenderError) -> Self {
        Self::Render { what: what.into(), source }
    }
}
