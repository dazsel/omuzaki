//! Stepping the part runtimes (hair, cloth, tails) with a pose source over the skin matrices, and re-uploading the
//! dynamic meshes whose revision changed.

use omuzaki_char_api::{BuiltCharacter, RuntimeSlot, StepCtx};
use omuzaki_render_api::{MeshId, Renderer};

use crate::error::StageError;
use crate::upload::MeshLookup;

/// One mesh a runtime rewrites, and the revision the renderer last saw of it.
struct DynamicTarget {
    /// Index into the character's runtime slots.
    slot: usize,
    /// Index among that runtime's dynamic meshes.
    index: usize,
    mesh: MeshId,
    seen: u64,
    /// Send on the next upload whatever the revision says (see [`DynamicMeshes::resend_all`]).
    stale: bool,
}

/// Which runtime meshes go to which renderer meshes, found once at construction so a frame only walks a short list.
pub(crate) struct DynamicMeshes {
    targets: Vec<DynamicTarget>,
}

impl DynamicMeshes {
    /// Pairs every dynamic mesh of every runtime with its uploaded mesh. The renderer already holds the geometry the
    /// runtimes started with (a runtime's initial state is what its part built), so their current revisions count as
    /// seen: nothing is re-uploaded until a step changes a mesh.
    ///
    /// # Errors
    /// [`StageError::NotUploaded`] for a runtime mesh that was not uploaded, or was uploaded static (it could not take
    /// `update_vertices`).
    pub(crate) fn new(built: &BuiltCharacter, uploaded: &impl MeshLookup) -> Result<Self, StageError> {
        let mut targets = Vec::new();
        for (slot, entry) in built.runtimes.iter().enumerate() {
            for index in 0..entry.runtime.dynamic_count() {
                let Some(view) = entry.runtime.dynamic_mesh(index) else { continue };
                let target = uploaded.mesh(&entry.part, view.mesh).filter(|mesh| mesh.dynamic).ok_or_else(|| {
                    StageError::NotUploaded { what: "dynamic mesh", name: format!("{}#{}", entry.part, view.mesh) }
                })?;
                targets.push(DynamicTarget { slot, index, mesh: target.mesh, seen: view.revision, stale: false });
            }
        }
        Ok(Self { targets })
    }

    /// Makes the next [`upload`](Self::upload) resend every mesh, whatever the renderer last saw.
    pub(crate) fn resend_all(&mut self) {
        for target in &mut self.targets {
            target.stale = true;
        }
    }

    /// Steps every runtime by `ctx`. Hot path: whatever the runtimes cost, nothing more.
    pub(crate) fn step(slots: &mut [RuntimeSlot], ctx: &StepCtx<'_>) {
        for slot in slots {
            slot.runtime.step(ctx);
        }
    }

    /// Sends the meshes whose revision moved since the last call to `renderer`, and returns how many that was.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses an update. The mesh stays unseen, so the next frame retries it.
    pub(crate) fn upload(&mut self, slots: &[RuntimeSlot], renderer: &mut dyn Renderer) -> Result<u32, StageError> {
        let mut sent = 0;
        for target in &mut self.targets {
            let Some(view) = slots[target.slot].runtime.dynamic_mesh(target.index) else { continue };
            if view.revision == target.seen && !target.stale {
                continue;
            }
            renderer
                .update_vertices(target.mesh, view.positions, view.normals)
                .map_err(|source| StageError::render("a dynamic mesh's new vertices", source))?;
            target.seen = view.revision;
            target.stale = false;
            sent += 1;
        }
        Ok(sent)
    }
}
