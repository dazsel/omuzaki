//! [`ImportedAvatar`]: an imported model as a performer body of its own (design D1, D3, D4, D8; ADR 0033).
//!
//! It reuses the char-api types the stage already uploads ([`PartMesh`], [`MaterialSpec`], [`TextureData`], the
//! anim-api [`Skeleton`], [`SpringSet`] and [`ConstraintSet`]), so the stage learns one new source, not a new
//! vocabulary. What is new is the face (expressions, drivers, the gaze route), per-mesh inverse bind matrices and
//! morph targets, and what export retains.
//!
//! The avatar is rebuilt from the stored original and its [`AvatarDef`](crate::def::AvatarDef) on every load, so it
//! holds no per-frame state: the weights are the stage's and the scene's.

use std::collections::BTreeMap;

use omuzaki_anim_api::{ChannelMask, ConstraintSet, HumanoidBone, HumanoidPose, Region, Skeleton, SpringSet};
use omuzaki_char_api::{Anchor, AnchorId, JointRef, MaterialName, MaterialSpec, PartMesh, TextureData, TextureName};
use omuzaki_core::glam::Mat4;
use omuzaki_io_api::{ExpressionName, FirstPersonFlag, LookAtKind};
use omuzaki_render_api::MorphSet;
use omuzaki_store_api::ContentHash;

use crate::error::AvatarError;
use crate::expression::{AvatarExpression, ChannelDrivers, ExprIx};
use crate::gaze::{AvatarGaze, AvatarLookAt, LookDirection};
use crate::retained::Retained;

/// The most meshes, and the most expressions, an avatar may have: binds and drivers index them with a `u16`.
pub const MAX_INDEXED: usize = u16::MAX as usize + 1;

/// One mesh of an imported avatar: the mesh as the stage uploads it, the inverse bind matrices of its own skin, and
/// its morph targets.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AvatarMesh {
    /// Geometry, joint list, material, shadow flag and draw order.
    pub mesh: PartMesh,
    /// One inverse bind matrix per entry of `mesh.joint_bones`, as the file's skin gives them (ADR 0014 §2, per mesh
    /// because glTF skins may give one joint different matrices in different skins). Empty for a rigid mesh.
    pub inverse_bind: Vec<Mat4>,
    /// Its morph targets, sparse, in the file's target order; `None` if it has none.
    pub morphs: Option<MorphSet>,
}

impl AvatarMesh {
    /// A mesh with no inverse bind matrices and no morphs (a rigid mesh as is).
    pub fn new(mesh: PartMesh) -> Self {
        Self { mesh, inverse_bind: Vec::new(), morphs: None }
    }

    /// Sets the inverse bind matrices (builder style).
    #[must_use]
    pub fn with_inverse_bind(mut self, inverse_bind: Vec<Mat4>) -> Self {
        self.inverse_bind = inverse_bind;
        self
    }

    /// Sets the morph targets (builder style).
    #[must_use]
    pub fn with_morphs(mut self, morphs: MorphSet) -> Self {
        self.morphs = Some(morphs);
        self
    }

    /// How many morph targets it has.
    pub fn morph_count(&self) -> usize {
        self.morphs.as_ref().map_or(0, MorphSet::len)
    }
}

/// An imported avatar, ready to perform.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ImportedAvatar {
    /// The original file, in the store.
    pub source: ContentHash,
    /// The skeleton. Its rest is the VRM T-pose (D4); its bones keep the file's own axes.
    pub skeleton: Skeleton,
    /// The pose the meshes were authored in, read back through the humanoid rig (ADR 0014): identity for a file that
    /// already rests in T.
    pub bind: HumanoidPose,
    /// The meshes, in file order.
    pub meshes: Vec<AvatarMesh>,
    /// Materials by name, as the renderer draws them.
    pub materials: BTreeMap<MaterialName, MaterialSpec>,
    /// Decoded textures by name.
    pub textures: BTreeMap<TextureName, TextureData>,
    /// The expressions, in file order, names kept.
    pub expressions: Vec<AvatarExpression>,
    /// Which expression each channel drives.
    pub drivers: ChannelDrivers,
    /// How the `eyeLook*` channels reach the model.
    pub gaze: AvatarGaze,
    /// Spring chains and colliders (EX-11 shapes and limits).
    pub springs: SpringSet,
    /// Node constraints, in the VRM 1.0 order.
    pub constraints: ConstraintSet,
    /// The file's look-at, or `None` if it has none (the defaults of the route then apply, [`Self::look_at`]).
    pub look_at: Option<AvatarLookAt>,
    /// First-person flags by mesh, in mesh order; a mesh past the end of the list is `Auto`, the spec's default.
    /// Carried for export, not used at run time yet (D4).
    pub first_person: Vec<FirstPersonFlag>,
    /// Attachment points: one per humanoid bone plus named ones (head top, grips). Empty until the attachments work
    /// fills them (D1, condition 2).
    pub anchors: BTreeMap<AnchorId, Anchor>,
    /// What export needs and the runtime does not use (D8).
    pub retained: Retained,
}

impl ImportedAvatar {
    /// An avatar of `source` on `skeleton`, with nothing else yet: no meshes, materials or expressions, the inert gaze
    /// route and rest as its bind. The importer fills the public fields.
    pub fn new(source: ContentHash, skeleton: Skeleton) -> Self {
        Self {
            source,
            skeleton,
            bind: HumanoidPose::default(),
            meshes: Vec::new(),
            materials: BTreeMap::new(),
            textures: BTreeMap::new(),
            expressions: Vec::new(),
            drivers: ChannelDrivers::new(),
            gaze: AvatarGaze::Inert,
            springs: SpringSet::default(),
            constraints: ConstraintSet::default(),
            look_at: None,
            first_person: Vec::new(),
            anchors: BTreeMap::new(),
            retained: Retained::default(),
        }
    }

    /// The attachment points.
    pub fn anchors(&self) -> &BTreeMap<AnchorId, Anchor> {
        &self.anchors
    }

    /// The expression called `name`, if the avatar has one.
    pub fn expression(&self, name: &ExpressionName) -> Option<ExprIx> {
        find_expression(&self.expressions, name)
    }

    /// What drives the avatar's face: its expressions, drivers, gaze route and look-at.
    pub fn face(&self) -> AvatarFace<'_> {
        AvatarFace { expressions: &self.expressions, drivers: &self.drivers, gaze: self.gaze, look_at: self.look_at() }
    }

    /// What export reads beyond the body: the original, the first-person flags, the retained source data and the
    /// file's own look-at.
    pub fn export(&self) -> AvatarExport<'_> {
        AvatarExport {
            source: &self.source,
            first_person: &self.first_person,
            retained: &self.retained,
            look_at: self.look_at.as_ref(),
        }
    }

    /// The look-at the gaze routes read: the file's, or the defaults of its route (bone type on the bone route,
    /// expression type otherwise) when the file has none.
    pub fn look_at(&self) -> AvatarLookAt {
        self.look_at.unwrap_or_else(|| {
            AvatarLookAt::default_for(match self.gaze {
                AvatarGaze::Bone => LookAtKind::Bone,
                _ => LookAtKind::Expression,
            })
        })
    }

    /// The channels that reach the model (D3, "What a missing channel does"), so liveliness measures and the report
    /// count only what can show:
    /// - every channel whose driver names an existing expression (gaze channels only on the native `ARKit` route);
    /// - the head channels when the skeleton has a head bone, and the body channels when it has hips and a spine;
    /// - the gaze channels the route can show, per direction: on the bone route, both eye bones exist and the
    ///   direction's range map has an output above 0; on the presets route, the direction's range map has an output
    ///   above 0 and its preset exists; nothing on the inert route.
    pub fn driven(&self) -> ChannelMask {
        let mut mask = ChannelMask::EMPTY;
        let native = self.gaze == AvatarGaze::NativeArkit;
        for (channel, expression) in self.drivers.iter() {
            if expression.index() < self.expressions.len() && (native || channel.region() != Region::Gaze) {
                mask.insert(channel);
            }
        }
        let has = |bone| self.skeleton.humanoid_bone(bone).is_some();
        if has(HumanoidBone::Head) {
            mask |= ChannelMask::HEAD;
        }
        if has(HumanoidBone::Hips) && has(HumanoidBone::Spine) {
            mask |= ChannelMask::BODY;
        }
        let look_at = self.look_at();
        match self.gaze {
            AvatarGaze::Bone if has(HumanoidBone::LeftEye) && has(HumanoidBone::RightEye) => {
                for (map, channels) in look_at.bone_maps() {
                    if map.output_scale > 0.0 {
                        mask |= ChannelMask::from_channels(&channels);
                    }
                }
            }
            AvatarGaze::Presets => {
                for direction in LookDirection::ALL {
                    let preset = ExpressionName::Preset(direction.preset());
                    if look_at.preset_map(direction).output_scale > 0.0 && self.expression(&preset).is_some() {
                        mask |= ChannelMask::from_channels(&direction.channels());
                    }
                }
            }
            AvatarGaze::Bone | AvatarGaze::NativeArkit | AvatarGaze::Inert => {}
        }
        mask
    }

    /// Checks the avatar's references, so the stage and the evaluator can index without bounds worries:
    /// - at most [`MAX_INDEXED`] meshes and expressions;
    /// - every mesh validates, follows bones the skeleton has, has as many inverse bind matrices as joints, has morph
    ///   targets that fit its vertices, and draws a material the avatar has;
    /// - every material passes the renderer's rules, draws only textures the avatar has and composites no face layers
    ///   (an imported avatar has no painted face), and every texture's pixels match its description;
    /// - the retained morph-target names, when kept, have one list per mesh, each empty (the file named none) or naming
    ///   every target;
    /// - every anchor follows a bone the skeleton has;
    /// - every morph bind names a mesh and a target that exist, and every colour and UV bind a material that exists;
    /// - every driver names an expression that exists, and gaze channels drive expressions only on the native `ARKit`
    ///   route;
    /// - the springs and constraints fit the skeleton.
    ///
    /// # Errors
    /// The first broken rule, as an [`AvatarError`].
    pub fn validate(&self) -> Result<(), AvatarError> {
        for (what, count) in [("meshes", self.meshes.len()), ("expressions", self.expressions.len())] {
            if count > MAX_INDEXED {
                return Err(AvatarError::too_many(what, count));
            }
        }
        for (index, mesh) in self.meshes.iter().enumerate() {
            self.validate_mesh(index, mesh)?;
        }
        self.validate_materials()?;
        self.validate_morph_names()?;
        for (id, anchor) in &self.anchors {
            self.resolve(&anchor.bone, || format!("anchor {id}"))?;
        }
        for (index, expression) in self.expressions.iter().enumerate() {
            self.validate_expression(index, expression)?;
        }
        for (channel, expression) in self.drivers.iter() {
            if expression.index() >= self.expressions.len() {
                return Err(AvatarError::driver_out_of_range(channel, expression, self.expressions.len()));
            }
            if channel.region() == Region::Gaze && self.gaze != AvatarGaze::NativeArkit {
                return Err(AvatarError::gaze_driver_off_route(channel));
            }
        }
        self.springs.validate(&self.skeleton).map_err(AvatarError::Springs)?;
        self.constraints.validate(&self.skeleton).map_err(AvatarError::Constraints)
    }

    fn validate_mesh(&self, index: usize, mesh: &AvatarMesh) -> Result<(), AvatarError> {
        mesh.mesh.validate().map_err(|why| AvatarError::mesh(index, why))?;
        for joint in &mesh.mesh.joint_bones {
            self.resolve(joint, || format!("mesh {index}"))?;
        }
        if mesh.inverse_bind.len() != mesh.mesh.joint_bones.len() {
            return Err(AvatarError::inverse_bind_length(index, mesh.inverse_bind.len(), mesh.mesh.joint_bones.len()));
        }
        // The renderer checks the targets against the geometry at upload; check the same rule here, so a valid
        // avatar uploads.
        if let Some(morphs) = &mesh.morphs {
            morphs.validate(mesh.mesh.geometry.positions.len()).map_err(|why| AvatarError::morphs(index, why))?;
        }
        self.material(&mesh.mesh.material, || format!("mesh {index}"))
    }

    /// The materials and textures, as upload will check them (`MaterialSpec::to_material`, `Material::validate`).
    fn validate_materials(&self) -> Result<(), AvatarError> {
        for (name, material) in &self.materials {
            material.validate().map_err(|why| AvatarError::material(name.clone(), why))?;
            if let Some(texture) = material.base_texture.as_ref().filter(|t| !self.textures.contains_key(*t)) {
                return Err(AvatarError::unknown_texture(name.clone(), texture.clone()));
            }
            if !material.overlays.is_empty() {
                return Err(AvatarError::face_overlay(name.clone()));
            }
        }
        for (name, texture) in &self.textures {
            texture.validate().map_err(|why| AvatarError::texture(name.clone(), why))?;
        }
        Ok(())
    }

    /// The retained morph-target names, when there are any: one list per mesh, one name per target.
    fn validate_morph_names(&self) -> Result<(), AvatarError> {
        let names = &self.retained.morph_names;
        if names.is_empty() {
            return Ok(());
        }
        // Every index either side has: a list for a mesh that doesn't exist is as wrong as a mesh without its list. An
        // empty list means the file named none of that mesh's targets (glTF names are optional per mesh), so it fits
        // any mesh; a list with names names every target.
        for index in 0..names.len().max(self.meshes.len()) {
            let kept = names.get(index).map(Vec::len);
            let targets = self.meshes.get(index).map(AvatarMesh::morph_count);
            let fits = match (kept, targets) {
                (Some(kept), Some(targets)) => kept == 0 || kept == targets,
                _ => false,
            };
            if !fits {
                return Err(AvatarError::morph_names(index, kept.unwrap_or(0), targets.unwrap_or(0)));
            }
        }
        Ok(())
    }

    fn validate_expression(&self, index: usize, expression: &AvatarExpression) -> Result<(), AvatarError> {
        for bind in &expression.morph {
            let target_exists =
                self.meshes.get(usize::from(bind.mesh)).is_some_and(|m| usize::from(bind.target) < m.morph_count());
            if !target_exists {
                return Err(AvatarError::morph_out_of_range(index, bind.mesh, bind.target));
            }
        }
        let materials = expression.colour.iter().map(|b| &b.material).chain(expression.uv.iter().map(|b| &b.material));
        for material in materials {
            self.material(material, || format!("expression {index} ({})", expression.name))?;
        }
        Ok(())
    }

    fn resolve(&self, joint: &JointRef, at: impl FnOnce() -> String) -> Result<(), AvatarError> {
        let found = match joint {
            JointRef::Humanoid(bone) => self.skeleton.humanoid_bone(*bone),
            JointRef::Named(name) => self.skeleton.find(name.as_str()),
        };
        found.map(|_| ()).ok_or_else(|| AvatarError::unresolved_bone(at(), joint.clone()))
    }

    fn material(&self, material: &MaterialName, at: impl FnOnce() -> String) -> Result<(), AvatarError> {
        if self.materials.contains_key(material) {
            Ok(())
        } else {
            Err(AvatarError::unknown_material(at(), material.clone()))
        }
    }

    /// GPU bytes of every texture: RGBA8, with a full mip chain when `desc.mips`. Reported, never acted on: a large
    /// texture is never downgraded (the lead's ruling on texture memory).
    pub fn texture_bytes(&self) -> u64 {
        self.textures.values().map(texture_bytes).sum()
    }
}

/// The index of the expression called `name` in `expressions`.
fn find_expression(expressions: &[AvatarExpression], name: &ExpressionName) -> Option<ExprIx> {
    let index = expressions.iter().position(|e| &e.name == name)?;
    u16::try_from(index).ok().map(ExprIx)
}

/// What drives an imported avatar's face, borrowed: what the expression evaluator and the look-at stage read
/// ([`ImportedAvatar::face`]).
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct AvatarFace<'a> {
    /// The expressions, in file order.
    pub expressions: &'a [AvatarExpression],
    /// Which expression each channel drives.
    pub drivers: &'a ChannelDrivers,
    /// How the `eyeLook*` channels reach the model.
    pub gaze: AvatarGaze,
    /// The look-at the gaze routes read: the file's, or the route's defaults ([`ImportedAvatar::look_at`]).
    pub look_at: AvatarLookAt,
}

impl AvatarFace<'_> {
    /// The expression called `name`, if there is one.
    pub fn expression(&self, name: &ExpressionName) -> Option<ExprIx> {
        find_expression(self.expressions, name)
    }
}

/// What export reads from an imported avatar beyond its body, borrowed ([`ImportedAvatar::export`]).
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct AvatarExport<'a> {
    /// The original file, in the store.
    pub source: &'a ContentHash,
    /// First-person flags by mesh.
    pub first_person: &'a [FirstPersonFlag],
    /// The source data export writes back exactly.
    pub retained: &'a Retained,
    /// The file's own look-at, `None` when the file has none. The face carries the resolved one
    /// ([`AvatarFace::look_at`]); export writes this, so a file without `lookAt` re-exports without one (D8).
    pub look_at: Option<&'a AvatarLookAt>,
}

/// GPU bytes of one texture: four bytes a texel, over the full mip chain down to 1×1 when it has mips.
pub fn texture_bytes(texture: &TextureData) -> u64 {
    let desc = &texture.desc;
    let (mut width, mut height) = (u64::from(desc.size.x), u64::from(desc.size.y));
    let mut bytes = width * height * 4;
    while desc.mips && (width > 1 || height > 1) {
        width = (width / 2).max(1);
        height = (height / 2).max(1);
        bytes += width * height * 4;
    }
    bytes
}
