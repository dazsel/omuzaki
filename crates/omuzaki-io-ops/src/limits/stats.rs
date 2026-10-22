//! The counts an avatar is measured by, and the registry of stats a limit table may cap.
//!
//! [`AvatarStats`] is the typed set of counts; [`stats_from_ioscene`] fills it from an export. Some counts are things
//! an export never writes (lights, particles, cloths, audio, trails, raycasts, physics bodies, constraint depth,
//! contacts), so they are 0 for a Omuzaki export: 0 is exact, not a guess. Texture memory is not a count: a table
//! turns the pixel counts into MiB with its own [`TextureEstimate`].
//!
//! Each stat a table can cap has one [`StatDef`] in [`STATS`], in the platform pages' order: its data-file key, unit,
//! the fix to suggest, and how to read it from the counts. A new stat is one field, one registry row and its counting
//! code, all here.

use std::collections::BTreeSet;
use std::io::Cursor;

use image::{ImageDecoder, ImageReader};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::{EncodedImage, IoScene, MeshData, Transform};
use thiserror::Error;

use super::table::{TextureEstimate, Unit};

/// The counts that decide an avatar's rank. Every count is for the whole avatar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AvatarStats {
    /// Triangles across all mesh primitives.
    pub triangles: u64,
    /// Vertices across all mesh primitives, counted once for each node that draws the mesh like the triangles.
    pub vertices: u64,
    /// Morph targets across all meshes, each mesh counted once (a mesh shared by two nodes has one set of targets).
    pub morph_targets: u64,
    /// The size of the avatar's meshes along each axis, in metres.
    pub bounds: [f64; 3],
    /// Texture pixels in images without an alpha channel.
    pub texture_px_opaque: u64,
    /// Texture pixels in images with an alpha channel.
    pub texture_px_alpha: u64,
    /// Images in the scene: the textures an upload carries.
    pub textures: u64,
    /// The longest side, in pixels, of any image.
    pub max_texture_side: u64,
    /// Mesh nodes, with or without a skin: `skinned_meshes` plus `basic_meshes`.
    pub meshes: u64,
    /// Attachment nodes: nodes that draw no mesh and whose name ends in `Attachment`, the name a Roblox attachment
    /// carries.
    pub attachments: u64,
    /// Mesh nodes with a skin (Unity: skinned mesh renderers).
    pub skinned_meshes: u64,
    /// Mesh nodes without a skin (Unity: mesh renderers).
    pub basic_meshes: u64,
    /// Material slots: one per mesh primitive on each mesh node.
    pub material_slots: u64,
    /// `PhysBones` components: one per spring chain.
    pub physbones_components: u64,
    /// `PhysBones` affected transforms: the joints of every spring chain.
    pub physbones_transforms: u64,
    /// `PhysBones` colliders.
    pub physbones_colliders: u64,
    /// `PhysBones` collision checks: each chain's joints times the colliders its groups hold.
    pub physbones_collision_checks: u64,
    /// Contacts (`VRChat` contact senders and receivers). An export does not carry them.
    pub contacts: u64,
    /// Node constraints.
    pub constraints: u64,
    /// The deepest constraint chain. An export does not carry the depth.
    pub constraint_depth: u64,
    /// Animators: one for an avatar with a humanoid rig.
    pub animators: u64,
    /// Bones: the distinct joints of all skins.
    pub bones: u64,
    /// Lights. An export does not carry them.
    pub lights: u64,
    /// Particle systems. An export does not carry them.
    pub particle_systems: u64,
    /// Active particles. An export does not carry them.
    pub total_particles: u64,
    /// Active mesh-particle polygons. An export does not carry them.
    pub mesh_particle_polys: u64,
    /// 1 if particle trails are enabled, else 0. An export does not carry them.
    pub particle_trails: u64,
    /// 1 if particle collision is enabled, else 0. An export does not carry them.
    pub particle_collisions: u64,
    /// Trail renderers. An export does not carry them.
    pub trail_renderers: u64,
    /// Line renderers. An export does not carry them.
    pub line_renderers: u64,
    /// Raycasts. An export does not carry them.
    pub raycasts: u64,
    /// Cloth components. An export does not carry them.
    pub cloths: u64,
    /// Cloth vertices. An export does not carry them.
    pub cloth_vertices: u64,
    /// Physics colliders. An export does not carry them.
    pub physics_colliders: u64,
    /// Physics rigidbodies. An export does not carry them.
    pub physics_rigidbodies: u64,
    /// Audio sources. An export does not carry them.
    pub audio_sources: u64,
}

/// Errors from [`stats_from_ioscene`].
#[derive(Debug, Error)]
pub enum StatsError {
    /// An image's header could not be read, so its pixel count and alpha channel are unknown.
    #[error("image '{name}' could not be read for its size: {reason}")]
    Image {
        /// The image's name in the scene.
        name: String,
        /// What the decoder said.
        reason: String,
    },
}

/// What fixes a stat that is over its limit, shown as the hint of a finding.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Fix {
    /// A fit stage (`fit.decimate`, ...) reduces it; the hint is the stage id so the planner and the report agree.
    Stage(&'static str),
    /// No fit stage changes it; the hint says what the person changes by hand.
    Hand(&'static str),
}

/// One stat a limit table may cap.
#[derive(Copy, Clone)]
pub struct StatDef {
    /// The key in a table's `[stats]`.
    pub key: &'static str,
    /// The unit its limits and values are in.
    pub unit: Unit,
    /// What fixes it when it is over.
    pub fix: Fix,
    /// Reads the value from the counts. Texture memory needs the table's estimate; no other stat does.
    read: fn(&AvatarStats, &TextureEstimate) -> f64,
}

impl std::fmt::Debug for StatDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatDef")
            .field("key", &self.key)
            .field("unit", &self.unit)
            .field("fix", &self.fix)
            .finish_non_exhaustive()
    }
}

/// A count stat: `key` is the `AvatarStats` field of the same name.
macro_rules! count {
    ($key:ident, $fix:expr) => {
        StatDef { key: stringify!($key), unit: Unit::Count, fix: $fix, read: |s, _| to_f64(s.$key) }
    };
}

/// A bounds axis, in metres.
macro_rules! axis {
    ($key:literal, $axis:literal) => {
        StatDef {
            key: $key,
            unit: Unit::Metres,
            fix: Fix::Hand("scale the avatar down in the source model"),
            read: |s, _| s.bounds[$axis],
        }
    };
}

const BY_HAND_SHAPES: Fix = Fix::Hand("remove unused shape keys in the source model");
const BY_HAND_SPRINGS: Fix = Fix::Hand("remove or merge spring chains and colliders in the source model");
const BY_HAND_PARTICLES: Fix = Fix::Hand("remove the particle systems from the source model");

/// Every stat a table may cap, in the platform pages' order. A row in a table that is not here is refused.
pub const STATS: &[StatDef] = &[
    count!(triangles, Fix::Stage("fit.decimate")),
    count!(vertices, Fix::Stage("fit.decimate")),
    count!(morph_targets, BY_HAND_SHAPES),
    axis!("bounds_x", 0),
    axis!("bounds_y", 1),
    axis!("bounds_z", 2),
    StatDef { key: "texture_mib", unit: Unit::Mib, fix: Fix::Stage("fit.textures"), read: |s, t| s.texture_mib(t) },
    count!(textures, Fix::Stage("fit.atlas")),
    count!(max_texture_side, Fix::Stage("fit.textures")),
    count!(meshes, Fix::Stage("fit.meshes")),
    count!(attachments, Fix::Hand("remove the extra attachment nodes in the source model")),
    count!(skinned_meshes, Fix::Stage("fit.meshes")),
    count!(basic_meshes, Fix::Stage("fit.meshes")),
    count!(material_slots, Fix::Stage("fit.atlas")),
    count!(physbones_components, BY_HAND_SPRINGS),
    count!(physbones_transforms, BY_HAND_SPRINGS),
    count!(physbones_colliders, BY_HAND_SPRINGS),
    count!(physbones_collision_checks, BY_HAND_SPRINGS),
    count!(contacts, Fix::Hand("remove contacts from the source model")),
    count!(constraints, Fix::Hand("remove constraints from the source model")),
    count!(constraint_depth, Fix::Hand("flatten the constraint chains in the source model")),
    count!(animators, Fix::Hand("keep one animator on the avatar root")),
    count!(bones, Fix::Stage("fit.bones")),
    count!(lights, Fix::Hand("remove the lights from the source model")),
    count!(particle_systems, BY_HAND_PARTICLES),
    count!(total_particles, BY_HAND_PARTICLES),
    count!(mesh_particle_polys, BY_HAND_PARTICLES),
    count!(particle_trails, BY_HAND_PARTICLES),
    count!(particle_collisions, BY_HAND_PARTICLES),
    count!(trail_renderers, Fix::Hand("remove the trail renderers from the source model")),
    count!(line_renderers, Fix::Hand("remove the line renderers from the source model")),
    count!(raycasts, Fix::Hand("remove the raycasts from the source model")),
    count!(cloths, Fix::Hand("remove the cloth components from the source model")),
    count!(cloth_vertices, Fix::Hand("remove the cloth components from the source model")),
    count!(physics_colliders, Fix::Hand("remove the physics colliders from the source model")),
    count!(physics_rigidbodies, Fix::Hand("remove the rigidbodies from the source model")),
    count!(audio_sources, Fix::Hand("remove the audio sources from the source model")),
];

/// The registry row for a table key, or `None` if no stat has it.
pub fn stat_def(key: &str) -> Option<&'static StatDef> {
    STATS.iter().find(|def| def.key == key)
}

// Counts stay far below 2^53, where f64 stops representing every integer exactly.
#[allow(clippy::cast_precision_loss)]
fn to_f64(n: u64) -> f64 {
    n as f64
}

impl AvatarStats {
    /// The texture memory the avatar's images would upload, in MiB, under one table's estimate.
    ///
    /// An estimate of what the platform's import uploads, so treat the rank of a texture-heavy avatar as approximate.
    pub fn texture_mib(&self, estimate: &TextureEstimate) -> f64 {
        estimate.mib(to_f64(self.texture_px_opaque), to_f64(self.texture_px_alpha))
    }

    /// The value of the stat a table calls `key`, or `None` if no stat has that key.
    pub fn get(&self, key: &str, estimate: &TextureEstimate) -> Option<f64> {
        stat_def(key).map(|def| (def.read)(self, estimate))
    }
}

/// Counts an export's avatar from its scene: what the scene carries, with 0 for what an export never writes.
///
/// Triangles and bounds count every mesh node, so two nodes that share one mesh count twice, and each node's vertices
/// are taken to the world space of its parent chain. Texture pixels come from each image's header, not its pixels.
///
/// # Errors
///
/// Fails if an image's header cannot be read.
pub fn stats_from_ioscene(scene: &IoScene) -> Result<AvatarStats, StatsError> {
    let mut stats = AvatarStats::default();
    let world = world_matrices(scene);
    stats.bounds = bounds_of(scene, &world);
    count_mesh_nodes(scene, &mut stats);
    count_textures(scene, &mut stats)?;
    count_avatar(scene, &mut stats);
    Ok(stats)
}

fn count_mesh_nodes(scene: &IoScene, stats: &mut AvatarStats) {
    for node in &scene.nodes {
        let Some(mesh) = node.mesh.and_then(|m| scene.meshes.get(m.index())) else {
            continue;
        };
        // A node is one instance of its mesh, so a mesh shared by two nodes is drawn, and ranked, twice.
        let triangles: usize = mesh.primitives.iter().map(|p| p.mesh.triangle_count()).sum();
        stats.triangles += len(triangles);
        stats.vertices += len(mesh.primitives.iter().map(|p| p.mesh.positions.len()).sum::<usize>());
        stats.meshes += 1;
        stats.material_slots += len(mesh.primitives.len());
        if node.skin.is_some() {
            stats.skinned_meshes += 1;
        } else {
            stats.basic_meshes += 1;
        }
    }
    stats.morph_targets = len(scene.meshes.iter().map(omuzaki_io_api::Mesh::morph_target_count).sum());
    stats.attachments = len(scene.nodes.iter().filter(|n| n.mesh.is_none() && n.name.ends_with("Attachment")).count());
    let joints: BTreeSet<_> = scene.skins.iter().flat_map(|skin| skin.joints.iter().copied()).collect();
    stats.bones = len(joints.len());
}

fn len(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Each node's matrix from its own transform composed down the parent chain, from the roots. A node no root reaches
/// keeps the identity, which is what a node with no parent in the scene should get.
fn world_matrices(scene: &IoScene) -> Vec<Mat4> {
    let mut world = vec![Mat4::IDENTITY; scene.nodes.len()];
    let mut visited = vec![false; scene.nodes.len()];
    let mut stack: Vec<(usize, Mat4)> = scene.roots.iter().map(|r| (r.index(), Mat4::IDENTITY)).collect();
    while let Some((ix, parent)) = stack.pop() {
        let (Some(node), Some(seen), Some(slot)) = (scene.nodes.get(ix), visited.get_mut(ix), world.get_mut(ix)) else {
            continue;
        };
        if std::mem::replace(seen, true) {
            continue;
        }
        let matrix = parent * trs(&node.transform);
        *slot = matrix;
        stack.extend(node.children.iter().map(|c| (c.index(), matrix)));
    }
    world
}

fn trs(transform: &Transform) -> Mat4 {
    Mat4::from_scale_rotation_translation(transform.scale, transform.rotation, transform.translation)
}

/// The size of the meshes in world space, the extent of every vertex once it is placed.
///
/// A rigid primitive sits at its node's world transform. A skinned primitive ignores its node's transform and is placed
/// by its weighted joints (glTF 2.0, skins), so its vertices are placed by [`skinned_point`].
fn bounds_of(scene: &IoScene, world: &[Mat4]) -> [f64; 3] {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for (ix, node) in scene.nodes.iter().enumerate() {
        let Some(mesh) = node.mesh.and_then(|m| scene.meshes.get(m.index())) else {
            continue;
        };
        let node_matrix = world.get(ix).copied().unwrap_or(Mat4::IDENTITY);
        let joints = node.skin.and_then(|s| scene.skins.get(s.index())).map(|skin| joint_matrices(skin, world));
        for primitive in &mesh.primitives {
            let data = &primitive.mesh;
            for (vertex, position) in data.positions.iter().enumerate() {
                let placed = match &joints {
                    Some(joints) if !data.joints.is_empty() => skinned_point(joints, data, vertex, *position),
                    _ => node_matrix.transform_point3(*position),
                };
                let p = placed.to_array().map(f64::from);
                for axis in 0..3 {
                    min[axis] = min[axis].min(p[axis]);
                    max[axis] = max[axis].max(p[axis]);
                }
            }
        }
    }
    // No vertex was seen, so the bounds were never lowered from their infinite start.
    if min[0] > max[0] {
        return [0.0; 3];
    }
    std::array::from_fn(|axis| max[axis] - min[axis])
}

/// Each joint's matrix for skinning: its world matrix times its inverse bind matrix. A missing inverse bind matrix,
/// or a joint past the end of the list, is the identity, as the glTF spec has it.
fn joint_matrices(skin: &omuzaki_io_api::Skin, world: &[Mat4]) -> Vec<Mat4> {
    skin.joints
        .iter()
        .enumerate()
        .map(|(j, joint)| {
            let joint_world = world.get(joint.index()).copied().unwrap_or(Mat4::IDENTITY);
            let inverse_bind = skin.inverse_bind.get(j).copied().unwrap_or(Mat4::IDENTITY);
            joint_world * inverse_bind
        })
        .collect()
}

/// Where a skinned vertex is placed: the joint matrices of its weights, each applied to the vertex and summed. The
/// weights are normalised (they sum to 1), so this is the spec's `sum(w_i * M_i) * v`, by linearity. A joint index past
/// the end of `joints` is the identity.
fn skinned_point(joints: &[Mat4], data: &MeshData, vertex: usize, position: Vec3) -> Vec3 {
    let (Some(indices), Some(weights)) = (data.joints.get(vertex), data.weights.get(vertex)) else {
        return position;
    };
    indices
        .iter()
        .zip(weights)
        .map(|(&j, &w)| {
            let matrix = joints.get(usize::from(j)).copied().unwrap_or(Mat4::IDENTITY);
            matrix.transform_point3(position) * w
        })
        .sum()
}

fn count_textures(scene: &IoScene, stats: &mut AvatarStats) -> Result<(), StatsError> {
    stats.textures = len(scene.images.len());
    for image in &scene.images {
        let (pixels, alpha, side) = image_pixels(image)?;
        stats.max_texture_side = stats.max_texture_side.max(side);
        if alpha {
            stats.texture_px_alpha += pixels;
        } else {
            stats.texture_px_opaque += pixels;
        }
    }
    Ok(())
}

/// The pixel count, whether the image has an alpha channel, and its longest side, read from its header only.
fn image_pixels(image: &EncodedImage) -> Result<(u64, bool, u64), StatsError> {
    let fail = |reason: String| StatsError::Image { name: image.name.clone(), reason };
    let reader =
        ImageReader::new(Cursor::new(image.bytes.as_slice())).with_guessed_format().map_err(|e| fail(e.to_string()))?;
    let decoder = reader.into_decoder().map_err(|e| fail(e.to_string()))?;
    let (width, height) = decoder.dimensions();
    Ok((u64::from(width) * u64::from(height), decoder.color_type().has_alpha(), u64::from(width.max(height))))
}

fn count_avatar(scene: &IoScene, stats: &mut AvatarStats) {
    let Some(avatar) = &scene.avatar else {
        return;
    };
    stats.animators = u64::from(!avatar.humanoid.bones.is_empty());
    stats.constraints = len(avatar.constraints.len());
    let springs = &avatar.spring_bones;
    stats.physbones_colliders = len(springs.colliders.len());
    stats.physbones_components = len(springs.chains.len());
    for chain in &springs.chains {
        let joints = len(chain.joints.len());
        let colliders: u64 = chain
            .collider_groups
            .iter()
            .filter_map(|&g| springs.collider_groups.get(usize::try_from(g).unwrap_or(usize::MAX)))
            .map(|group| len(group.colliders.len()))
            .sum();
        stats.physbones_transforms += joints;
        stats.physbones_collision_checks += joints * colliders;
    }
}
