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
    /// - every index the retained scene graph holds (children, roots, meshes, skins, joints, materials, bones,
    ///   instance nodes, first-person annotations, morph-bind nodes, spring colliders, groups, joints and centres)
    ///   points into its list, every retained skin has one inverse bind matrix per joint, or none, and each retained
    ///   spring group or chain names a runtime one that exists, and no other record names it;
    /// - the retained source meshes, when kept, list every node that draws them once, one runtime mesh per primitive
    ///   at each, every runtime mesh exactly once overall, and their target names fit;
    /// - every anchor follows a bone the skeleton has;
    /// - every morph bind names a mesh and a target that exist, and, when it names a source node and the source meshes
    ///   are kept, a mesh of that node's instance; every colour and UV bind names a material that exists;
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
        self.validate_retained_graph()?;
        self.validate_retained_meshes()?;
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

    /// The retained source meshes, when there are any:
    /// - each instance lists one runtime mesh per primitive, at a node that draws that source mesh, and every node
    ///   that draws a source mesh is one of its instances exactly once;
    /// - every runtime mesh comes from exactly one primitive of one instance;
    /// - a source mesh's target names, when it has any, count the targets of each of its runtime meshes that has
    ///   targets (glTF requires every primitive of a mesh to have the same number).
    ///
    /// Runs after [`validate_retained_graph`](Self::validate_retained_graph), so every node index is in range.
    fn validate_retained_meshes(&self) -> Result<(), AvatarError> {
        let r = &self.retained;
        if r.meshes.is_empty() {
            return Ok(());
        }
        let mut groups = vec![0usize; self.meshes.len()];
        for (source, retained) in r.meshes.iter().enumerate() {
            let names = retained.morph_target_names.len();
            let mut fits_a_primitive = names == 0 || retained.instances.is_empty();
            for instance in &retained.instances {
                let draws_it =
                    r.nodes.get(as_index(instance.node)).is_some_and(|n| n.mesh.map(as_index) == Some(source));
                if !draws_it {
                    return Err(AvatarError::instance_node(source, instance.node));
                }
                if instance.meshes.len() != retained.primitives.len() {
                    let (meshes, primitives) = (instance.meshes.len(), retained.primitives.len());
                    return Err(AvatarError::instance_primitives(source, instance.node, meshes, primitives));
                }
                for &mesh in &instance.meshes {
                    let at = usize::from(mesh);
                    let (Some(mesh), Some(count)) = (self.meshes.get(at), groups.get_mut(at)) else {
                        return Err(AvatarError::unknown_primitive(source, at));
                    };
                    *count += 1;
                    match mesh.morph_count() {
                        0 => {}
                        targets if names == 0 || targets == names => fits_a_primitive = true,
                        targets => return Err(AvatarError::morph_names(source, names, targets)),
                    }
                }
            }
            if !fits_a_primitive {
                // Names for a mesh none of whose primitives has a target.
                return Err(AvatarError::morph_names(source, names, 0));
            }
        }
        for (node, drawn) in r.nodes.iter().enumerate().filter_map(|(n, node)| Some((n, as_index(node.mesh?)))) {
            let instances =
                r.meshes.get(drawn).map_or(0, |m| m.instances.iter().filter(|i| as_index(i.node) == node).count());
            if instances != 1 {
                return Err(AvatarError::node_instances(node, drawn, instances));
            }
        }
        match groups.iter().position(|g| *g != 1) {
            Some(mesh) => Err(AvatarError::mesh_grouping(mesh, groups[mesh])),
            None => Ok(()),
        }
    }

    /// The retained scene graph: every index it holds points into the list it names, so export can rebuild the file's
    /// nodes, meshes, skins and springs without a bounds check.
    fn validate_retained_graph(&self) -> Result<(), AvatarError> {
        let r = &self.retained;
        let nodes = r.nodes.len();
        for (n, node) in r.nodes.iter().enumerate() {
            for &child in &node.children {
                check_index(&|| format!("node {n}'s child"), child, nodes)?;
            }
            if let Some(mesh) = node.mesh {
                check_index(&|| format!("node {n}'s mesh"), mesh, r.meshes.len())?;
            }
            if let Some(skin) = node.skin {
                check_index(&|| format!("node {n}'s skin"), skin, r.skins.len())?;
            }
            if let Some(bone) = node.bone {
                check_index(&|| format!("node {n}'s bone"), u32::from(bone.0), self.skeleton.len())?;
            }
        }
        for &root in &r.roots {
            check_index(&|| "a root".to_owned(), root, nodes)?;
        }
        for (s, skin) in r.skins.iter().enumerate() {
            for &joint in skin.joints.iter().chain(&skin.skeleton) {
                check_index(&|| format!("skin {s}'s joint"), joint, nodes)?;
            }
            if !skin.inverse_bind.is_empty() && skin.inverse_bind.len() != skin.joints.len() {
                return Err(AvatarError::skin_inverse_bind(s, skin.inverse_bind.len(), skin.joints.len()));
            }
        }
        for (m, mesh) in r.meshes.iter().enumerate() {
            for material in mesh.primitives.iter().filter_map(|p| p.material) {
                check_index(&|| format!("mesh {m}'s material"), material, r.materials.len())?;
            }
            for instance in &mesh.instances {
                check_index(&|| format!("mesh {m}'s instance node"), instance.node, nodes)?;
            }
        }
        for annotation in r.first_person.iter().flatten() {
            check_index(&|| "a first-person annotation's node".to_owned(), annotation.node, nodes)?;
        }
        for (e, expression) in self.expressions.iter().enumerate() {
            for node in expression.morph.iter().filter_map(|b| b.node) {
                check_index(&|| format!("expression {e}'s morph bind node"), node, nodes)?;
            }
        }
        self.validate_retained_springs()
    }

    /// The retained spring records: every collider, joint and centre names a node, every group names colliders and
    /// every chain names groups that exist, and each runtime chain or group named is in the spring set and named by one
    /// record only.
    fn validate_retained_springs(&self) -> Result<(), AvatarError> {
        let (r, springs) = (&self.retained, &self.retained.springs);
        let nodes = r.nodes.len();
        for (c, collider) in springs.colliders.iter().enumerate() {
            check_index(&|| format!("spring collider {c}'s node"), collider.node, nodes)?;
        }
        for (g, group) in springs.groups.iter().enumerate() {
            for &collider in &group.colliders {
                check_index(&|| format!("collider group {g}'s collider"), collider, springs.colliders.len())?;
            }
        }
        for (c, chain) in springs.chains.iter().enumerate() {
            for joint in &chain.joints {
                check_index(&|| format!("spring chain {c}'s joint"), joint.node, nodes)?;
            }
            for &group in &chain.groups {
                check_index(&|| format!("spring chain {c}'s collider group"), group, springs.groups.len())?;
            }
            if let Some(center) = chain.center {
                check_index(&|| format!("spring chain {c}'s center"), center, nodes)?;
            }
        }
        let (chains, groups) = (&self.springs.chains, &self.springs.groups);
        let claimed = springs.chains.iter().filter_map(|c| c.runtime.as_deref());
        claim_each_once("chain", claimed, |name| chains.iter().any(|c| c.name == name))?;
        let claimed = springs.groups.iter().filter_map(|g| g.runtime.as_deref());
        claim_each_once("collider group", claimed, |name| groups.iter().any(|g| g.name == name))
    }

    fn validate_expression(&self, at: usize, expression: &AvatarExpression) -> Result<(), AvatarError> {
        for bind in &expression.morph {
            let target_exists =
                self.meshes.get(usize::from(bind.mesh)).is_some_and(|m| usize::from(bind.target) < m.morph_count());
            if !target_exists {
                return Err(AvatarError::morph_out_of_range(at, bind.mesh, bind.target));
            }
            if let Some(node) = bind.node.filter(|_| !self.retained.meshes.is_empty()) {
                // The node's instance must hold the mesh the bind moves, so export writes the bind on that node.
                let instance = self
                    .retained
                    .nodes
                    .get(as_index(node))
                    .and_then(|n| n.mesh)
                    .and_then(|m| self.retained.meshes.get(as_index(m)).and_then(|m| m.instance(node)));
                if !instance.is_some_and(|i| i.meshes.contains(&bind.mesh)) {
                    return Err(AvatarError::morph_bind_instance(at, bind.mesh, node));
                }
            }
        }
        let materials = expression.colour.iter().map(|b| &b.material).chain(expression.uv.iter().map(|b| &b.material));
        for material in materials {
            self.material(material, || format!("expression {at} ({})", expression.name))?;
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

/// A retained `u32` index as a `usize` (saturating on a 16-bit target, where it is out of range anyway).
fn as_index(i: u32) -> usize {
    usize::try_from(i).unwrap_or(usize::MAX)
}

/// Checks that `index`, held by what `at` names, points into a list of `len`.
fn check_index(at: &dyn Fn() -> String, i: u32, len: usize) -> Result<(), AvatarError> {
    if as_index(i) < len { Ok(()) } else { Err(AvatarError::retained_index(at(), as_index(i), len)) }
}

/// Checks that every runtime `kind` name in `claims` exists and appears in `claims` once.
fn claim_each_once<'a>(
    kind: &'static str,
    claims: impl Iterator<Item = &'a str>,
    exists: impl Fn(&str) -> bool,
) -> Result<(), AvatarError> {
    let mut seen = std::collections::BTreeSet::new();
    for claim in claims {
        if !exists(claim) {
            return Err(AvatarError::unknown_runtime_spring(kind, claim));
        }
        if !seen.insert(claim) {
            return Err(AvatarError::shared_runtime_spring(kind, claim));
        }
    }
    Ok(())
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
