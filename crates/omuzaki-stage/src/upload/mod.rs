//! Putting a [`BuiltCharacter`] on a renderer: its meshes, procedural textures, face-layer textures and materials, and
//! the bookkeeping that lets the frame loop find them again.
//!
//! Everything is generic over `dyn Renderer`: the stage never learns which backend it drives (spec v3 §39 rule 1).

use std::collections::BTreeMap;

use omuzaki_char_api::{BuiltCharacter, BuiltMesh, MaterialName, MaterialSpec, PartId, TextureName};
use omuzaki_face_api::{DEFAULT_PX_PER_M, FaceLayerId, FaceLayerSpec};
use omuzaki_render_api::{Material, MaterialId, MeshId, MeshOptions, MeshUsage, Renderer, TextureId};

use self::release::{discard_textures, release_in_order};
use crate::error::StageError;

// Private: the release order is an upload detail, so it lives here and not in the crate's public module list.
pub(crate) mod release;

/// How to upload.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct UploadOptions {
    /// Face texture density, pixels per metre of the base layer (other layers scale from it). It is the runtime
    /// quality knob: the frame-budget scheduler lowers it under load with [`StageCharacter::resize_face`].
    pub face_px_per_m: f32,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self { face_px_per_m: DEFAULT_PX_PER_M }
    }
}

/// One mesh on the renderer, with what the frame loop needs to draw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadedMesh {
    /// The part that made it.
    pub part: PartId,
    /// Its index in that part's own mesh list (what a runtime's dynamic mesh refers to).
    pub local: u16,
    /// The renderer's handle.
    pub mesh: MeshId,
    /// The handle of the material the mesh uses, resolved at upload.
    pub material: MaterialId,
    /// True if a part runtime rewrites the vertices every step, so the frame loop must re-upload them when they change.
    pub dynamic: bool,
}

/// One painted face layer on the renderer.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FaceLayerTexture {
    /// The dynamic texture the face painter fills.
    pub texture: TextureId,
    /// The layer's rectangle and density: what a painter needs to rasterise into `texture`.
    pub spec: FaceLayerSpec,
}

/// A character's resources on a renderer, by name and by handle.
///
/// Created with [`upload`](StageCharacter::upload), given back with [`release`](StageCharacter::release). The handles
/// are plain `Copy` ids, so the renderer cannot notice a forgotten character: dropping one without releasing it panics
/// in debug builds, which makes every test that leaks fail.
#[derive(Debug)]
pub struct StageCharacter {
    /// Build order, so `meshes()` matches `BuiltCharacter::meshes`.
    pub(crate) meshes: Vec<UploadedMesh>,
    /// Part, then local mesh index, to a position in `meshes`.
    mesh_index: BTreeMap<PartId, BTreeMap<u16, usize>>,
    pub(crate) materials: BTreeMap<MaterialName, MaterialId>,
    pub(crate) textures: BTreeMap<TextureName, TextureId>,
    pub(crate) face_layers: BTreeMap<FaceLayerId, FaceLayerTexture>,
    face_px_per_m: f32,
    /// Set by `release`; checked by `Drop` (see `release.rs`).
    pub(crate) released: bool,
}

/// The renderer material for `spec`, with the handles uploaded so far.
fn material_of(
    name: &MaterialName,
    spec: &MaterialSpec,
    textures: &BTreeMap<TextureName, TextureId>,
    layers: &BTreeMap<FaceLayerId, FaceLayerTexture>,
) -> Result<Material, StageError> {
    spec.to_material(&|texture| textures.get(texture).copied(), &|layer| layers.get(&layer).map(|l| l.texture))
        .map_err(|source| StageError::Material { material: name.clone(), source })
}

fn check_density(px_per_m: f32) -> Result<(), StageError> {
    if px_per_m.is_finite() && px_per_m > 0.0 { Ok(()) } else { Err(StageError::FaceDensity { px_per_m }) }
}

/// Creates one empty dynamic texture per face layer the character composites, at `px_per_m`. On failure it releases
/// the ones it made, so the caller has nothing to undo.
fn create_face_layers(
    renderer: &mut dyn Renderer,
    built: &BuiltCharacter,
    px_per_m: f32,
) -> Result<BTreeMap<FaceLayerId, FaceLayerTexture>, StageError> {
    let mut layers = BTreeMap::new();
    for (spec, desc) in built.face_textures(px_per_m) {
        match renderer.create_texture(&desc, None) {
            Ok(texture) => {
                layers.insert(spec.id, FaceLayerTexture { texture, spec });
            }
            Err(source) => {
                discard_textures(renderer, layers.values().map(|layer| layer.texture));
                return Err(StageError::render(format!("the {:?} face layer texture", spec.id), source));
            }
        }
    }
    Ok(layers)
}

/// One material that must point at a different set of face textures.
struct Repoint {
    name: MaterialName,
    id: MaterialId,
    old: Material,
    new: Material,
}

impl StageCharacter {
    fn empty(face_px_per_m: f32) -> Self {
        Self {
            meshes: Vec::new(),
            mesh_index: BTreeMap::new(),
            materials: BTreeMap::new(),
            textures: BTreeMap::new(),
            face_layers: BTreeMap::new(),
            face_px_per_m,
            released: false,
        }
    }

    /// Uploads `built`: its procedural textures, one dynamic texture per composited face layer, every material, then
    /// every mesh (materials exist before the meshes that name them, textures before the materials that read them).
    ///
    /// All or nothing: if any step fails, what was already created is released and the error is returned.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses a resource, [`StageError::Material`] or
    /// [`StageError::UnknownMaterial`] for a character whose names don't resolve (a built character never has this),
    /// [`StageError::FaceDensity`] for an unusable `options.face_px_per_m`.
    pub fn upload(
        renderer: &mut dyn Renderer,
        built: &BuiltCharacter,
        options: UploadOptions,
    ) -> Result<Self, StageError> {
        check_density(options.face_px_per_m)?;
        let mut stage = Self::empty(options.face_px_per_m);
        if let Err(error) = stage.fill(renderer, built) {
            if let Err(cleanup) = stage.release(renderer) {
                tracing::warn!(%cleanup, "releasing a partly uploaded character failed too");
            }
            return Err(error);
        }
        Ok(stage)
    }

    /// The creation steps of [`upload`](Self::upload); a failure leaves `self` holding exactly what was created.
    fn fill(&mut self, renderer: &mut dyn Renderer, built: &BuiltCharacter) -> Result<(), StageError> {
        for (name, data) in &built.textures {
            let id = renderer
                .create_texture(&data.desc, Some(&data.rgba8))
                .map_err(|source| StageError::render(format!("texture '{name}'"), source))?;
            self.textures.insert(name.clone(), id);
        }
        self.face_layers = create_face_layers(renderer, built, self.face_px_per_m)?;
        for (name, spec) in &built.materials {
            let material = material_of(name, spec, &self.textures, &self.face_layers)?;
            let id = renderer
                .create_material(&material)
                .map_err(|source| StageError::render(format!("material '{name}'"), source))?;
            self.materials.insert(name.clone(), id);
        }
        for built_mesh in &built.meshes {
            let (part, mesh) = (&built_mesh.part, &built_mesh.mesh);
            let material = *self.materials.get(&mesh.material).ok_or_else(|| StageError::UnknownMaterial {
                part: part.clone(),
                mesh: mesh.name.clone(),
                material: mesh.material.clone(),
            })?;
            // A mesh a runtime rewrites must be created `Dynamic`, or the renderer refuses `update_vertices` for it.
            let usage = if mesh.dynamic { MeshUsage::Dynamic } else { MeshUsage::Static };
            let id = renderer
                .create_mesh_with(&mesh.geometry, &MeshOptions { usage, morphs: None })
                .map_err(|source| StageError::render(format!("mesh '{}' of part '{part}'", mesh.name), source))?;
            self.mesh_index.entry(part.clone()).or_default().insert(built_mesh.local, self.meshes.len());
            self.meshes.push(UploadedMesh {
                part: part.clone(),
                local: built_mesh.local,
                mesh: id,
                material,
                dynamic: mesh.dynamic,
            });
        }
        Ok(())
    }

    /// Every uploaded mesh, in build order (the order of `BuiltCharacter::meshes`).
    pub fn meshes(&self) -> &[UploadedMesh] {
        &self.meshes
    }

    /// The `local`th mesh of `part`, as `BuiltMesh::part` and `BuiltMesh::local` name it.
    pub fn mesh(&self, part: &PartId, local: u16) -> Option<&UploadedMesh> {
        self.mesh_index.get(part)?.get(&local).map(|&index| &self.meshes[index])
    }

    /// The handle of the material called `name`.
    pub fn material(&self, name: &MaterialName) -> Option<MaterialId> {
        self.materials.get(name).copied()
    }

    /// Updates the materials called `names` in place to their specs in `built`: what a look edit changes (LK-06), with
    /// no mesh or texture created. Every material is converted before the renderer is called, so a spec that doesn't
    /// resolve changes nothing; a renderer refusal part way leaves the earlier ones updated, for the caller to restore.
    /// Returns how many were updated.
    ///
    /// # Errors
    /// [`StageError::NotUploaded`] for a name the character has no material or handle for, [`StageError::Material`] for a spec that names a texture the stage lacks, [`StageError::Render`] if the renderer
    /// refuses an update.
    pub(crate) fn update_materials(
        &self,
        renderer: &mut dyn Renderer,
        built: &BuiltCharacter,
        names: &[MaterialName],
    ) -> Result<u32, StageError> {
        let mut updates = Vec::with_capacity(names.len());
        for name in names {
            let unknown = || StageError::NotUploaded { what: "material", name: name.as_str().to_owned() };
            let (Some(spec), Some(id)) = (built.materials.get(name), self.material(name)) else { return Err(unknown()) };
            updates.push((id, name, material_of(name, spec, &self.textures, &self.face_layers)?));
        }
        for (id, name, material) in &updates {
            renderer
                .update_material(*id, material)
                .map_err(|source| StageError::render(format!("updating material '{name}'"), source))?;
        }
        Ok(u32::try_from(updates.len()).unwrap_or(u32::MAX))
    }

    /// The handle of the procedural texture called `name`.
    pub fn texture(&self, name: &TextureName) -> Option<TextureId> {
        self.textures.get(name).copied()
    }

    /// The texture and spec of one face layer, or `None` if no material of the character composites it.
    pub fn face_layer(&self, layer: FaceLayerId) -> Option<&FaceLayerTexture> {
        self.face_layers.get(&layer)
    }

    /// Every face layer texture, in [`FaceLayerId`] order.
    pub fn face_layers(&self) -> impl Iterator<Item = &FaceLayerTexture> {
        self.face_layers.values()
    }

    /// The base face density the layers are at now, in pixels per metre.
    pub fn face_px_per_m(&self) -> f32 {
        self.face_px_per_m
    }

    /// Changes the face texture density: creates new layer textures, points every material that composites a face
    /// layer at them with `update_material`, then releases the old textures (the renderer's documented safe order, so
    /// no frame ever names a dead texture).
    ///
    /// The new textures start empty: the caller repaints every layer before the next frame (their
    /// [`spec`](FaceLayerTexture::spec)s have the new density). `built` must be the character that was uploaded.
    /// A no-op if the density is unchanged.
    ///
    /// All or nothing: on an error the old textures and materials are as they were and the new textures are released.
    ///
    /// # Errors
    /// [`StageError::FaceDensity`] for an unusable density, [`StageError::NotUploaded`] if `built` has a material that
    /// was not uploaded, [`StageError::Render`] if the renderer refuses a call. A failure to release the *old*
    /// textures comes back as `Render` after the switch has happened, so the new density is already in effect.
    pub fn resize_face(
        &mut self,
        renderer: &mut dyn Renderer,
        built: &BuiltCharacter,
        px_per_m: f32,
    ) -> Result<(), StageError> {
        check_density(px_per_m)?;
        if px_per_m.to_bits() == self.face_px_per_m.to_bits() {
            return Ok(());
        }
        let layers = create_face_layers(renderer, built, px_per_m)?;
        let plan = match self.repoint_plan(built, &layers) {
            Ok(plan) => plan,
            Err(error) => {
                discard_textures(renderer, layers.values().map(|layer| layer.texture));
                return Err(error);
            }
        };
        for (done, step) in plan.iter().enumerate() {
            if let Err(source) = renderer.update_material(step.id, &step.new) {
                for undone in &plan[..done] {
                    if let Err(error) = renderer.update_material(undone.id, &undone.old) {
                        tracing::warn!(%error, material = %undone.name, "restoring a material after a failed resize failed");
                    }
                }
                discard_textures(renderer, layers.values().map(|layer| layer.texture));
                return Err(StageError::render(format!("updating material '{}'", step.name), source));
            }
        }
        let old = std::mem::replace(&mut self.face_layers, layers);
        self.face_px_per_m = px_per_m;
        release_in_order(renderer, [], old.values().map(|layer| layer.texture), [])
            .map_err(|source| StageError::render("releasing the old face textures", source))
    }

    /// Every material that composites a face layer, before and after `layers` replaces the current ones. Computed
    /// before the renderer is touched, so a name that doesn't resolve fails with nothing changed.
    fn repoint_plan(
        &self,
        built: &BuiltCharacter,
        layers: &BTreeMap<FaceLayerId, FaceLayerTexture>,
    ) -> Result<Vec<Repoint>, StageError> {
        built
            .materials
            .iter()
            .filter(|(_, spec)| !spec.overlays.is_empty())
            .map(|(name, spec)| {
                let id = self
                    .materials
                    .get(name)
                    .copied()
                    .ok_or_else(|| StageError::NotUploaded { what: "material", name: name.to_string() })?;
                Ok(Repoint {
                    name: name.clone(),
                    id,
                    old: material_of(name, spec, &self.textures, &self.face_layers)?,
                    new: material_of(name, spec, &self.textures, layers)?,
                })
            })
            .collect()
    }
}

/// What [`StageCharacter::sync`] did on the renderer. The counts are the cost of a variant or palette change: everything
/// the new character shares with the old one is not in them.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Meshes created (new, or whose geometry changed).
    pub meshes_created: u32,
    /// Meshes the old character had that the new one has too, unchanged: nothing was sent for them.
    pub meshes_kept: u32,
    /// Old meshes released.
    pub meshes_released: u32,
    /// Materials created.
    pub materials_created: u32,
    /// Materials updated in place.
    pub materials_updated: u32,
    /// Procedural and face-layer textures created.
    pub textures_created: u32,
}

/// What a [`StageCharacter::sync`] has made on the renderer so far, to give back if a later step fails.
#[derive(Default)]
struct Created {
    textures: Vec<TextureId>,
    materials: Vec<MaterialId>,
    meshes: Vec<MeshId>,
}

impl Created {
    fn discard(self, renderer: &mut dyn Renderer) {
        if let Err(error) = release_in_order(renderer, self.materials, self.textures, self.meshes) {
            tracing::warn!(%error, "releasing what an abandoned sync created failed");
        }
    }
}

/// The resource tables of the character after a sync, built beside the live ones and swapped in on success.
struct Next {
    textures: BTreeMap<TextureName, TextureId>,
    layers: BTreeMap<FaceLayerId, FaceLayerTexture>,
    materials: BTreeMap<MaterialName, MaterialId>,
    meshes: Vec<UploadedMesh>,
    /// `meshes` by part and local index, built here so that [`StageCharacter::commit_sync`] only moves tables.
    mesh_index: BTreeMap<PartId, BTreeMap<u16, usize>>,
    /// Materials whose renderer copy must change, before and after.
    updates: Vec<Repoint>,
    /// Handles of the old character that nothing in the new one uses.
    stale_textures: Vec<TextureId>,
    stale_materials: Vec<MaterialId>,
    stale_meshes: Vec<MeshId>,
    report: SyncReport,
}

/// A sync whose resources exist on the renderer but are not yet the character's: see
/// [`StageCharacter::prepare_sync`].
pub(crate) struct PreparedSync {
    next: Next,
    created: Created,
}

impl PreparedSync {
    /// The face layers whose texture is not the one `current` holds: new, or recreated empty. The caller owes each a
    /// paint, whatever its face system believes it showed.
    pub(crate) fn fresh_face_layers<'a>(
        &'a self,
        current: &'a StageCharacter,
    ) -> impl Iterator<Item = FaceLayerId> + 'a {
        self.next
            .layers
            .iter()
            .filter(|(id, layer)| current.face_layers.get(*id).map(|c| c.texture) != Some(layer.texture))
            .map(|(id, _)| *id)
    }

    /// Gives back what the sync created and restores the materials it repointed: the character is as it was.
    pub(crate) fn abort(self, renderer: &mut dyn Renderer) {
        restore_materials(renderer, &self.next.updates);
        self.created.discard(renderer);
    }
}

impl MeshLookup for PreparedSync {
    fn mesh(&self, part: &PartId, local: u16) -> Option<&UploadedMesh> {
        self.next.meshes.iter().find(|m| m.part == *part && m.local == local)
    }
}

/// The old resources a committed sync left behind.
pub(crate) struct Retired {
    materials: Vec<MaterialId>,
    textures: Vec<TextureId>,
    meshes: Vec<MeshId>,
    /// What the sync did.
    pub(crate) report: SyncReport,
}

impl Retired {
    /// Releases them, in the renderer's safe order.
    ///
    /// # Errors
    /// [`StageError::Render`] with the first refusal. The character already uses the new resources.
    pub(crate) fn release(self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        release_in_order(renderer, self.materials, self.textures, self.meshes)
            .map_err(|source| StageError::render("releasing what the new character no longer uses", source))
    }
}

/// Finds an uploaded mesh by its part and local index: what the frame plans need, from a live character or one about
/// to become it.
pub(crate) trait MeshLookup {
    /// The `local`th mesh of `part`.
    fn mesh(&self, part: &PartId, local: u16) -> Option<&UploadedMesh>;
}

impl MeshLookup for StageCharacter {
    fn mesh(&self, part: &PartId, local: u16) -> Option<&UploadedMesh> {
        StageCharacter::mesh(self, part, local)
    }
}

/// Puts each material back to its `old` copy; a failure is logged, because there is no one left to tell.
fn restore_materials(renderer: &mut dyn Renderer, steps: &[Repoint]) {
    for undone in steps {
        if let Err(error) = renderer.update_material(undone.id, &undone.old) {
            tracing::warn!(%error, material = %undone.name, "restoring a material after an abandoned sync failed");
        }
    }
}

impl StageCharacter {
    /// Brings the renderer from `old` (the character this was uploaded from, or last synced to) to `new`, sending only
    /// what differs: a texture, material or mesh equal in both keeps its handle and costs no call. Changed materials are
    /// updated in place, so meshes keep pointing at them. The old resources nothing refers to any more are released
    /// last, in the renderer's safe order.
    ///
    /// A changed mesh, texture or face layer gets a fresh handle (the renderer can't change a static mesh or a texture's
    /// size in place), so a palette change that only recolours materials makes no `create_mesh` call, while one that
    /// changes a baked vertex colour recreates that mesh.
    ///
    /// All or nothing: if the renderer refuses a call, what was created is released and the materials are restored.
    ///
    /// This is [`prepare_sync`](Self::prepare_sync), [`commit_sync`](Self::commit_sync) and the release in one call; a
    /// caller that has more to do before it can commit (the stage's character install) uses the parts.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses a call, [`StageError::Material`] or
    /// [`StageError::UnknownMaterial`] for a `new` whose names don't resolve. A failure to release the *old* resources
    /// comes back as `Render` after the switch has happened.
    pub fn sync(
        &mut self,
        renderer: &mut dyn Renderer,
        old: &BuiltCharacter,
        new: &BuiltCharacter,
    ) -> Result<SyncReport, StageError> {
        let prepared = self.prepare_sync(renderer, old, new)?;
        let retired = self.commit_sync(prepared);
        let report = retired.report;
        retired.release(renderer).map(|()| report)
    }

    /// The part of [`sync`](Self::sync) that can fail: creates what `new` needs, repoints the changed materials, and
    /// returns the plan. `self` is untouched, so the caller can look up the new handles ([`PreparedSync::mesh`]), do its
    /// own fallible work, and then [`commit_sync`](Self::commit_sync) (which cannot fail) or
    /// [`PreparedSync::abort`].
    ///
    /// # Errors
    /// As [`sync`](Self::sync), except for releases. On an error nothing was changed on the renderer.
    pub(crate) fn prepare_sync(
        &self,
        renderer: &mut dyn Renderer,
        old: &BuiltCharacter,
        new: &BuiltCharacter,
    ) -> Result<PreparedSync, StageError> {
        let mut created = Created::default();
        let mut next = match self.plan_sync(renderer, old, new, &mut created) {
            Ok(next) => next,
            Err(error) => {
                created.discard(renderer);
                return Err(error);
            }
        };
        for (done, step) in next.updates.iter().enumerate() {
            if let Err(source) = renderer.update_material(step.id, &step.new) {
                restore_materials(renderer, &next.updates[..done]);
                created.discard(renderer);
                return Err(StageError::render(format!("updating material '{}'", step.name), source));
            }
        }
        next.report.materials_updated = u32::try_from(next.updates.len()).unwrap_or(u32::MAX);
        Ok(PreparedSync { next, created })
    }

    /// Makes the prepared resources the character's own. Infallible and allocation-free: only tables are swapped (the
    /// frame loop calls it inside `tick`, so everything that allocates happens in
    /// [`prepare_sync`](Self::prepare_sync)). The old resources nothing refers to any more come back in the [`Retired`]
    /// to be released.
    pub(crate) fn commit_sync(&mut self, prepared: PreparedSync) -> Retired {
        let next = prepared.next;
        self.textures = next.textures;
        self.face_layers = next.layers;
        self.materials = next.materials;
        self.mesh_index = next.mesh_index;
        self.meshes = next.meshes;
        Retired {
            materials: next.stale_materials,
            textures: next.stale_textures,
            meshes: next.stale_meshes,
            report: next.report,
        }
    }

    /// The creation half of [`sync`](Self::sync): makes every new resource and works out what to update and release.
    /// Touches nothing of `self`.
    fn plan_sync(
        &self,
        renderer: &mut dyn Renderer,
        old: &BuiltCharacter,
        new: &BuiltCharacter,
        created: &mut Created,
    ) -> Result<Next, StageError> {
        let mut report = SyncReport::default();
        let mut stale_textures = Vec::new();
        let textures = self.sync_textures(renderer, old, new, created, &mut report)?;
        stale_textures.extend(self.textures.iter().filter(|(n, id)| textures.get(*n) != Some(*id)).map(|(_, id)| *id));
        let layers = self.sync_layers(renderer, new, created, &mut report)?;
        stale_textures.extend(
            self.face_layers
                .iter()
                .filter(|(id, l)| layers.get(*id).map(|n| n.texture) != Some(l.texture))
                .map(|(_, l)| l.texture),
        );

        let mut materials = BTreeMap::new();
        let mut updates = Vec::new();
        for (name, spec) in &new.materials {
            let wanted = material_of(name, spec, &textures, &layers)?;
            let id = if let Some((id, was)) = self.materials.get(name).copied().zip(old.materials.get(name)) {
                let before = material_of(name, was, &self.textures, &self.face_layers)?;
                if before != wanted {
                    updates.push(Repoint { name: name.clone(), id, old: before, new: wanted });
                }
                id
            } else {
                let id = renderer
                    .create_material(&wanted)
                    .map_err(|source| StageError::render(format!("material '{name}'"), source))?;
                created.materials.push(id);
                report.materials_created += 1;
                id
            };
            materials.insert(name.clone(), id);
        }
        let stale_materials: Vec<_> =
            self.materials.iter().filter(|(name, _)| !materials.contains_key(*name)).map(|(_, id)| *id).collect();

        let meshes = self.sync_meshes(renderer, old, new, &materials, created, &mut report)?;
        let stale_meshes: Vec<_> =
            self.meshes.iter().map(|m| m.mesh).filter(|id| !meshes.iter().any(|m| m.mesh == *id)).collect();
        report.meshes_released = u32::try_from(stale_meshes.len()).unwrap_or(u32::MAX);
        let mut mesh_index: BTreeMap<PartId, BTreeMap<u16, usize>> = BTreeMap::new();
        for (at, mesh) in meshes.iter().enumerate() {
            mesh_index.entry(mesh.part.clone()).or_default().insert(mesh.local, at);
        }
        Ok(Next {
            textures,
            layers,
            materials,
            meshes,
            mesh_index,
            updates,
            stale_textures,
            stale_materials,
            stale_meshes,
            report,
        })
    }

    /// The procedural textures of `new`: an equal texture keeps its handle, any other is created.
    fn sync_textures(
        &self,
        renderer: &mut dyn Renderer,
        old: &BuiltCharacter,
        new: &BuiltCharacter,
        created: &mut Created,
        report: &mut SyncReport,
    ) -> Result<BTreeMap<TextureName, TextureId>, StageError> {
        let mut textures = BTreeMap::new();
        for (name, data) in &new.textures {
            let kept = old.textures.get(name).filter(|was| *was == data).and_then(|_| self.textures.get(name));
            let id = if let Some(id) = kept {
                *id
            } else {
                let id = renderer
                    .create_texture(&data.desc, Some(&data.rgba8))
                    .map_err(|source| StageError::render(format!("texture '{name}'"), source))?;
                created.textures.push(id);
                report.textures_created += 1;
                id
            };
            textures.insert(name.clone(), id);
        }
        Ok(textures)
    }

    /// The face-layer textures of `new` at the current density: a layer with an unchanged spec keeps its texture.
    fn sync_layers(
        &self,
        renderer: &mut dyn Renderer,
        new: &BuiltCharacter,
        created: &mut Created,
        report: &mut SyncReport,
    ) -> Result<BTreeMap<FaceLayerId, FaceLayerTexture>, StageError> {
        let mut layers = BTreeMap::new();
        for (spec, desc) in new.face_textures(self.face_px_per_m) {
            let kept = self.face_layers.get(&spec.id).filter(|layer| layer.spec == spec);
            let texture = if let Some(layer) = kept {
                layer.texture
            } else {
                let id = renderer
                    .create_texture(&desc, None)
                    .map_err(|source| StageError::render(format!("the {:?} face layer texture", spec.id), source))?;
                created.textures.push(id);
                report.textures_created += 1;
                id
            };
            layers.insert(spec.id, FaceLayerTexture { texture, spec });
        }
        Ok(layers)
    }

    /// The meshes of `new`, in build order: an equal mesh keeps its upload, any other is created.
    fn sync_meshes(
        &self,
        renderer: &mut dyn Renderer,
        old: &BuiltCharacter,
        new: &BuiltCharacter,
        materials: &BTreeMap<MaterialName, MaterialId>,
        created: &mut Created,
        report: &mut SyncReport,
    ) -> Result<Vec<UploadedMesh>, StageError> {
        let was: BTreeMap<(&PartId, u16), &BuiltMesh> = old.meshes.iter().map(|m| ((&m.part, m.local), m)).collect();
        let mut meshes = Vec::with_capacity(new.meshes.len());
        for built_mesh in &new.meshes {
            let (part, mesh) = (&built_mesh.part, &built_mesh.mesh);
            let material = *materials.get(&mesh.material).ok_or_else(|| StageError::UnknownMaterial {
                part: part.clone(),
                mesh: mesh.name.clone(),
                material: mesh.material.clone(),
            })?;
            let kept = was
                .get(&(part, built_mesh.local))
                .filter(|before| before.mesh == *mesh)
                .and_then(|_| self.mesh(part, built_mesh.local));
            if let Some(up) = kept {
                report.meshes_kept += 1;
                meshes.push(up.clone());
                continue;
            }
            // A changed mesh is recreated, which includes a hair mesh whose baked tint a palette change moved. That is
            // a V0 exception: #895 moves it to `update_vertices` so a palette change creates no mesh at all.
            let usage = if mesh.dynamic { MeshUsage::Dynamic } else { MeshUsage::Static };
            let id = renderer
                .create_mesh_with(&mesh.geometry, &MeshOptions { usage, morphs: None })
                .map_err(|source| StageError::render(format!("mesh '{}' of part '{part}'", mesh.name), source))?;
            created.meshes.push(id);
            report.meshes_created += 1;
            meshes.push(UploadedMesh {
                part: part.clone(),
                local: built_mesh.local,
                mesh: id,
                material,
                dynamic: mesh.dynamic,
            });
        }
        Ok(meshes)
    }
}
