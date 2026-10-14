//! Putting a [`BuiltCharacter`] on a renderer: its meshes, procedural textures, face-layer textures and materials, and
//! the bookkeeping that lets the frame loop find them again.
//!
//! Everything is generic over `dyn Renderer`: the stage never learns which backend it drives (spec v3 §39 rule 1).

use std::collections::BTreeMap;

use omuzaki_char_api::{BuiltCharacter, MaterialName, MaterialSpec, PartId, TextureName};
use omuzaki_face_api::{DEFAULT_PX_PER_M, FaceLayerId, FaceLayerSpec};
use omuzaki_render_api::{Material, MaterialId, MeshId, MeshOptions, MeshUsage, Renderer, TextureId};

use crate::error::StageError;
use crate::release::{discard_textures, release_in_order};

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
