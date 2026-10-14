//! Giving a character's resources back to the renderer, in the one order that never fails with `InUse`.

use omuzaki_render_api::{MaterialId, MeshId, RenderError, Renderer, TextureId};

use crate::error::StageError;
use crate::upload::StageCharacter;

/// Releases `materials`, then `textures`, then `meshes`.
///
/// The order is the renderer's dependency rule (`Renderer` docs, "Lifetimes"): a texture a live material reads can't
/// be released, so every material goes first. It keeps going after a failure, because a half-released character is
/// worse than a report of the first problem, and returns that first error.
///
/// # Errors
/// The first [`RenderError`] any release returned (`UnknownHandle` for a handle released twice, `DeviceLost`).
pub(crate) fn release_in_order(
    renderer: &mut dyn Renderer,
    materials: impl IntoIterator<Item = MaterialId>,
    textures: impl IntoIterator<Item = TextureId>,
    meshes: impl IntoIterator<Item = MeshId>,
) -> Result<(), RenderError> {
    let mut first = None;
    let mut note = |result: Result<(), RenderError>| {
        if let Err(error) = result {
            first.get_or_insert(error);
        }
    };
    for material in materials {
        note(renderer.release(material.into()));
    }
    for texture in textures {
        note(renderer.release(texture.into()));
    }
    for mesh in meshes {
        note(renderer.release(mesh.into()));
    }
    first.map_or(Ok(()), Err)
}

impl StageCharacter {
    /// Releases everything this character put on `renderer`: materials, then the textures they read (procedural
    /// ones and the face layers), then meshes. Consuming `self` makes a second release impossible.
    ///
    /// Dropping a `StageCharacter` without calling this is a bug: the handles are `Copy` and the renderer can't tell
    /// they were forgotten. Debug builds (and so every test) panic on it; release builds log an error.
    ///
    /// # Errors
    /// [`StageError::Render`] with the first release the renderer refused. Everything else was still released.
    pub fn release(mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        self.released = true;
        release_in_order(
            renderer,
            self.materials.values().copied(),
            self.textures.values().copied().chain(self.face_layers.values().map(|layer| layer.texture)),
            self.meshes.iter().map(|mesh| mesh.mesh),
        )
        .map_err(|source| StageError::render("releasing the character", source))
    }
}

impl Drop for StageCharacter {
    fn drop(&mut self) {
        // A panic already in flight is the real failure; a second one would abort and hide it.
        let leaked = !self.released && !std::thread::panicking();
        if leaked {
            tracing::error!("a StageCharacter was dropped without release(): its renderer resources leak");
        }
        // Loud in tests and debug builds only: a release build logs and carries on rather than taking the app down.
        debug_assert!(!leaked, "a StageCharacter was dropped without release(): its renderer resources leak");
    }
}

/// Releases textures that were created but never became part of a [`StageCharacter`] (a failed upload or resize), and
/// logs instead of returning a failure: the caller is already reporting the error that made them useless.
pub(crate) fn discard_textures(renderer: &mut dyn Renderer, textures: impl IntoIterator<Item = TextureId>) {
    if let Err(error) = release_in_order(renderer, [], textures, []) {
        tracing::warn!(%error, "releasing textures of an abandoned upload failed");
    }
}
