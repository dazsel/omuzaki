//! Putting an [`ImportedAvatar`] on a renderer: its textures, materials and meshes (with their morph targets), and the
//! materials an expression's colour or UV bind moves, kept ready to re-send.

use std::collections::BTreeMap;

use omuzaki_avatar::buffers::{BindDeltas, MaterialDelta};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_char_api::{MaterialError, MaterialName, MaterialSpec, TextureName};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec4;
use omuzaki_io_api::ColorBindKind;
use omuzaki_render_api::{
    Material, MaterialId, MeshId, MeshOptions, MeshUsage, OutlineStyle, Renderer, Shading, SurfaceFx, TextureId,
};

use crate::error::StageError;
use crate::upload::release::release_in_order;

/// One mesh of the avatar on the renderer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct UploadedAvatarMesh {
    /// The renderer's handle.
    pub mesh: MeshId,
    /// The handle of the material it uses.
    pub material: MaterialId,
    /// How many morph targets it has; 0 for none.
    pub morphs: u32,
}

/// A material an expression bind moves: its handle, the material as the file has it, and the copy that is re-sent with
/// the frame's deltas on top. The copy is made once, so a frame that re-sends it allocates nothing.
#[derive(Clone, Debug)]
struct BoundMaterial {
    id: MaterialId,
    base: Material,
    sent: Material,
}

/// An imported avatar's resources on a renderer.
///
/// Made with [`upload`](AvatarUpload::upload), given back with [`release`](AvatarUpload::release); dropping one without
/// releasing it panics in debug builds, as for a kit character's
/// [`StageCharacter`](crate::upload::StageCharacter).
#[derive(Debug)]
pub struct AvatarUpload {
    /// In the avatar's mesh order, so index `i` is `ImportedAvatar::meshes[i]`.
    meshes: Vec<UploadedAvatarMesh>,
    materials: BTreeMap<MaterialName, MaterialId>,
    textures: BTreeMap<TextureName, TextureId>,
    /// The materials a bind moves, in the order of the evaluator's [`BindDeltas`] (material name order).
    bound: Vec<BoundMaterial>,
    /// The surface fx the materials are drawn with, and the material-name substrings it spares.
    surface_fx: Option<(SurfaceFx, Vec<String>)>,
    released: bool,
}

impl AvatarUpload {
    /// Uploads `avatar`: textures, then the materials that read them, then the meshes with their morph targets.
    /// `bound` names the materials the expressions move (the evaluator's [`BindDeltas`]), which are kept ready to
    /// re-send.
    ///
    /// All or nothing: on an error what was created is released.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses a resource, [`StageError::Material`] for a material naming a
    /// texture the avatar lacks, [`StageError::Avatar`] for a mesh or bind naming a material it lacks.
    pub fn upload(
        renderer: &mut dyn Renderer,
        avatar: &ImportedAvatar,
        bound: &BindDeltas,
    ) -> Result<Self, StageError> {
        let mut upload = Self {
            meshes: Vec::with_capacity(avatar.meshes.len()),
            materials: BTreeMap::new(),
            textures: BTreeMap::new(),
            bound: Vec::with_capacity(bound.len()),
            surface_fx: None,
            released: false,
        };
        if let Err(error) = upload.fill(renderer, avatar, bound) {
            if let Err(cleanup) = upload.release(renderer) {
                tracing::warn!(%cleanup, "releasing a partly uploaded avatar failed too");
            }
            return Err(error);
        }
        Ok(upload)
    }

    fn fill(
        &mut self,
        renderer: &mut dyn Renderer,
        avatar: &ImportedAvatar,
        bound: &BindDeltas,
    ) -> Result<(), StageError> {
        for (name, data) in &avatar.textures {
            let id = renderer
                .create_texture(&data.desc, Some(&data.rgba8))
                .map_err(|source| StageError::render(format!("texture '{name}'"), source))?;
            self.textures.insert(name.clone(), id);
        }
        for (name, spec) in &avatar.materials {
            let material = self.material_of(name, spec, avatar)?;
            let id = renderer
                .create_material(&material)
                .map_err(|source| StageError::render(format!("material '{name}'"), source))?;
            self.materials.insert(name.clone(), id);
        }
        for (name, _) in bound.iter() {
            let (id, spec) = self.materials.get(name).copied().zip(avatar.materials.get(name)).ok_or_else(|| {
                StageError::Avatar { reason: format!("an expression binds material '{name}', which the avatar lacks") }
            })?;
            let base = self.material_of(name, spec, avatar)?;
            self.bound.push(BoundMaterial { id, sent: base.clone(), base });
        }
        for (index, avatar_mesh) in avatar.meshes.iter().enumerate() {
            let mesh = &avatar_mesh.mesh;
            let material = *self.materials.get(&mesh.material).ok_or_else(|| StageError::Avatar {
                reason: format!(
                    "mesh {index} ('{}') uses material '{}', which the avatar lacks",
                    mesh.name, mesh.material
                ),
            })?;
            let options = MeshOptions { usage: MeshUsage::Static, morphs: avatar_mesh.morphs.clone() };
            let id = renderer
                .create_mesh_with(&mesh.geometry, &options)
                .map_err(|source| StageError::render(format!("mesh {index} ('{}')", mesh.name), source))?;
            let morphs = u32::try_from(avatar_mesh.morph_count()).unwrap_or(u32::MAX);
            self.meshes.push(UploadedAvatarMesh { mesh: id, material, morphs });
        }
        Ok(())
    }

    /// The renderer material for `spec` with the textures uploaded so far, drawing the `MToon` outline the importer
    /// kept for `name` (`avatar.outlines`, #647) in place of the kit's line and lit by the `MToon` it kept
    /// (`avatar.mtoon`, #592). An imported avatar has no painted face, so a face layer never resolves.
    fn material_of(
        &self,
        name: &MaterialName,
        spec: &MaterialSpec,
        avatar: &ImportedAvatar,
    ) -> Result<Material, StageError> {
        let mut material = spec
            .to_material(&|texture| self.textures.get(texture).copied(), &|_| None)
            .map_err(|source| StageError::Material { material: name.clone(), source })?;
        if let (Some(line), Some(outline)) = (material.outline.as_mut(), avatar.outlines.get(name)) {
            line.width = outline.width;
            material.outline_style = OutlineStyle {
                width_mode: outline.width_mode,
                width_texture: outline.width_texture.as_ref().and_then(|t| self.textures.get(t).copied()),
                lighting_mix: outline.lighting_mix,
            };
        }
        if let Some(mtoon) = avatar.mtoon.get(name) {
            let lit = mtoon.try_map(|texture| {
                self.textures.get(texture).copied().ok_or_else(|| StageError::Material {
                    material: name.clone(),
                    source: MaterialError::MissingTexture(texture.clone()),
                })
            })?;
            material.mtoon = Some(Box::new(lit));
        }
        Ok(material)
    }

    /// Draws every material of `avatar` whose name contains none of `spare` with the surface fx `fx` (`None`: the file's
    /// own), updated in place, the materials expressions move included. All or nothing: every material is converted
    /// before the renderer is called, and if it refuses one the earlier ones are put back.
    ///
    /// # Errors
    /// [`StageError::Material`] for a material that doesn't convert, [`StageError::Render`] if the renderer refuses one.
    pub(crate) fn set_surface_fx(
        &mut self,
        renderer: &mut dyn Renderer,
        avatar: &ImportedAvatar,
        fx: Option<SurfaceFx>,
        spare: &[String],
    ) -> Result<(), StageError> {
        let spared = |name: &MaterialName| spare.iter().any(|part| name.as_str().contains(part.as_str()));
        let mut updates = Vec::with_capacity(self.materials.len());
        for (name, id) in &self.materials {
            let spec = avatar.materials.get(name).ok_or_else(|| StageError::Avatar {
                reason: format!("the upload has material '{name}', which the avatar lacks"),
            })?;
            let mut material = self.material_of(name, spec, avatar)?;
            let previous = material.clone();
            material.fx = fx.filter(|_| !spared(name));
            updates.push((*id, name.clone(), previous, material));
        }
        for (i, (id, name, _, material)) in updates.iter().enumerate() {
            if let Err(source) = renderer.update_material(*id, material) {
                for (id, _, previous, _) in &updates[..i] {
                    if let Err(error) = renderer.update_material(*id, previous) {
                        tracing::warn!(%error, "putting a material back after a refused surface fx failed too");
                    }
                }
                return Err(StageError::render(format!("material '{name}' with a surface fx"), source));
            }
        }
        self.surface_fx = fx.map(|fx| (fx, spare.to_vec()));
        for bound in &mut self.bound {
            if let Some((.., material)) = updates.iter().find(|(id, ..)| *id == bound.id) {
                bound.base.fx = material.fx;
                bound.sent.fx = material.fx;
            }
        }
        Ok(())
    }

    /// The surface fx the materials are drawn with, and the material-name substrings it spares.
    pub fn surface_fx(&self) -> Option<(&SurfaceFx, &[String])> {
        self.surface_fx.as_ref().map(|(fx, spared)| (fx, spared.as_slice()))
    }

    /// Every mesh, in the avatar's mesh order.
    pub fn meshes(&self) -> &[UploadedAvatarMesh] {
        &self.meshes
    }

    /// The handle of the material called `name`.
    pub fn material(&self, name: &MaterialName) -> Option<MaterialId> {
        self.materials.get(name).copied()
    }

    /// The handle of the texture called `name`.
    pub fn texture(&self, name: &TextureName) -> Option<TextureId> {
        self.textures.get(name).copied()
    }

    /// Re-sends every bound material whose deltas moved this frame, as the file's material plus the deltas. Returns
    /// how many were sent. Hot path: the copies are made at upload, so nothing allocates.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses an update; the materials before it were sent.
    pub(crate) fn send_binds(&mut self, renderer: &mut dyn Renderer, binds: &BindDeltas) -> Result<u32, StageError> {
        let mut sent = 0;
        for (bound, (name, delta)) in self.bound.iter_mut().zip(binds.iter()) {
            if !delta.moved() {
                continue;
            }
            apply_delta(&bound.base, delta, &mut bound.sent);
            renderer
                .update_material(bound.id, &bound.sent)
                .map_err(|source| StageError::render(format!("material '{name}' moved by an expression"), source))?;
            sent += 1;
        }
        Ok(sent)
    }

    /// Releases everything: materials, then the textures they read, then meshes. Consuming `self` makes a second
    /// release impossible.
    ///
    /// # Errors
    /// [`StageError::Render`] with the first release the renderer refused; everything else was still released.
    pub fn release(mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        self.released = true;
        release_in_order(
            renderer,
            self.materials.values().copied(),
            self.textures.values().copied(),
            self.meshes.iter().map(|m| m.mesh),
        )
        .map_err(|source| StageError::render("releasing the avatar", source))
    }
}

impl Drop for AvatarUpload {
    fn drop(&mut self) {
        let leaked = !self.released && !std::thread::panicking();
        if leaked {
            tracing::error!("an AvatarUpload was dropped without release(): its renderer resources leak");
        }
        debug_assert!(!leaked, "an AvatarUpload was dropped without release(): its renderer resources leak");
    }
}

/// `colour` plus a linear-light delta (the alpha of `delta` added to its alpha), clamped to what a colour holds.
fn shifted(colour: Colour, delta: Vec4) -> Colour {
    Colour::from_linear(colour.to_linear() + delta.truncate(), colour.alpha() + delta.w)
}

/// Writes `base` with `delta` on top into `out`, field by field, so `out` keeps its allocations (VRM 1.0
/// `expressions.md`, material colour and texture transform binds). A material lit by `MToon` takes the shade, rim and
/// matcap binds on its `MToon` colours; without `MToon` the matcap is not drawn (`look.mtoon_not_drawn` reports it at
/// import).
fn apply_delta(base: &Material, delta: &MaterialDelta, out: &mut Material) {
    out.base = shifted(base.base, delta.colour(ColorBindKind::Color));
    out.uv_transform.scale = base.uv_transform.scale + delta.uv_scale();
    out.uv_transform.offset = base.uv_transform.offset + delta.uv_offset();
    if let (Shading::Toon(from), Shading::Toon(to)) = (&base.shading, &mut out.shading) {
        to.emissive = shifted(from.emissive, delta.colour(ColorBindKind::EmissionColor));
        to.shade = shifted(from.shade, delta.colour(ColorBindKind::ShadeColor));
        to.rim.colour = shifted(from.rim.colour, delta.colour(ColorBindKind::RimColor));
    }
    if let (Some(from), Some(to)) = (&base.mtoon, &mut out.mtoon) {
        to.shade = shifted(from.shade, delta.colour(ColorBindKind::ShadeColor));
        to.rim = shifted(from.rim, delta.colour(ColorBindKind::RimColor));
        to.matcap = shifted(from.matcap, delta.colour(ColorBindKind::MatcapColor));
    }
    if let (Some(from), Some(to)) = (&base.outline, &mut out.outline) {
        to.colour = shifted(from.colour, delta.colour(ColorBindKind::OutlineColor));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_delta_leaves_a_colour_as_it_was() {
        let c = Colour::rgba(200, 120, 40, 255);
        assert_eq!(shifted(c, Vec4::ZERO), c);
    }

    #[test]
    fn a_delta_moves_the_colour_in_linear_light_and_clamps() {
        let black = Colour::rgba(0, 0, 0, 255);
        assert_eq!(shifted(black, Vec4::new(1.0, 0.0, 0.0, 0.0)), Colour::rgba(255, 0, 0, 255));
        assert_eq!(shifted(black, Vec4::new(-1.0, 0.0, 0.0, -2.0)), Colour::rgba(0, 0, 0, 0));
    }
}
