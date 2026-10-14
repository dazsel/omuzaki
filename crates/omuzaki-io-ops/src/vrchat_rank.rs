//! `VRChat`'s avatar performance rank, worked out from an avatar's counts (VF-07).
//!
//! The limits are `VRChat`'s published Avatar Performance Ranking numbers. They live only in
//! `data/vrchat-performance.toml`, with the source URL and the date they were read, and this module reads them with
//! `include_str!`. [`rank`] compares each stat with its limits and takes the worst as the avatar's rank. A stat the page
//! lists as disabled on a platform is [`StatRank::rank`] `None` (Unsupported) and does not count toward the rank.
//!
//! [`stats_from_ioscene`] counts what an export carries. Some stats are things an export never writes (lights,
//! particles, cloths, audio, trails, raycasts, physics bodies, constraint depth, contacts), so they are 0 for a Omuzaki
//! export: 0 is exact, not a guess. Texture memory is an estimate, see [`texture_mib`].

use std::collections::BTreeSet;
use std::io::Cursor;

use image::{ImageDecoder, ImageReader};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::{EncodedImage, IoScene, MeshData, Transform};
use serde::Deserialize;
use thiserror::Error;

/// The published limits, read at build time. Parsed on each [`rank`] call, which is cheap for a file this size.
const LIMITS_TOML: &str = include_str!("../data/vrchat-performance.toml");

/// Bytes per pixel the texture estimate assumes, per platform (see [`texture_mib`]).
const PC_BYTES_OPAQUE: f64 = 0.5;
const PC_BYTES_ALPHA: f64 = 1.0;
/// ASTC 6x6 is 3.56 bits per pixel on Android.
const ASTC_6X6_BYTES: f64 = 3.56 / 8.0;
/// A full mip chain adds a third on top of the base level.
const MIP_FACTOR: f64 = 4.0 / 3.0;
const MIB: f64 = 1024.0 * 1024.0;

/// The platform whose limits apply.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Windows and other desktop `VRChat`.
    Pc,
    /// Android (the page's mobile table; iOS shares it).
    Android,
}

/// An avatar's rank. Ordered from best to worst, so the overall rank is the maximum of the stat ranks.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rank {
    /// At or under the Excellent limit.
    Excellent,
    /// Over Excellent, at or under Good.
    Good,
    /// Over Good, at or under Medium.
    Medium,
    /// Over Medium, at or under Poor.
    Poor,
    /// Over Poor. The page leaves this one unbounded.
    VeryPoor,
}

impl Rank {
    /// The page's name for the rank.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Excellent => "Excellent",
            Self::Good => "Good",
            Self::Medium => "Medium",
            Self::Poor => "Poor",
            Self::VeryPoor => "Very Poor",
        }
    }
}

/// The rank of one stat. `rank` is `None` when the platform does not support the stat.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StatRank {
    /// The stat's key in the data file.
    pub name: &'static str,
    /// The stat's rank, or `None` (Unsupported): the page says the component is disabled on this platform, so the
    /// platform strips it and it does not count toward the avatar's rank.
    pub rank: Option<Rank>,
}

/// An avatar's overall rank and the rank of every stat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ranking {
    /// The worst rank among the supported stats.
    pub rank: Rank,
    /// Every stat in the data file, in its order.
    pub stats: Vec<StatRank>,
}

/// The counts that decide an avatar's rank. Every count is for the whole avatar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AvatarStats {
    /// Triangles across all mesh primitives.
    pub triangles: u64,
    /// The size of the avatar's meshes along each axis, in metres.
    pub bounds: [f64; 3],
    /// Texture pixels in images without an alpha channel.
    pub texture_px_opaque: u64,
    /// Texture pixels in images with an alpha channel.
    pub texture_px_alpha: u64,
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

/// Errors from [`rank`].
#[derive(Debug, Error)]
pub enum LimitsError {
    /// The bundled limits file does not parse. A build-time bug, reported rather than panicked.
    #[error("the VRChat limits data does not parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// A stat's value and its limits have different shapes. Only a bug in the stat table can cause this.
    #[error("stat '{0}' has limits of the wrong shape")]
    Shape(&'static str),
}

/// The limits for one platform, as the data file lays them out. `None` is Unsupported on that platform.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    triangles: Option<[f64; 4]>,
    bounds: Option<[[f64; 3]; 4]>,
    texture_mib: Option<[f64; 4]>,
    skinned_meshes: Option<[f64; 4]>,
    basic_meshes: Option<[f64; 4]>,
    material_slots: Option<[f64; 4]>,
    physbones_components: Option<[f64; 4]>,
    physbones_transforms: Option<[f64; 4]>,
    physbones_colliders: Option<[f64; 4]>,
    physbones_collision_checks: Option<[f64; 4]>,
    contacts: Option<[f64; 4]>,
    constraints: Option<[f64; 4]>,
    constraint_depth: Option<[f64; 4]>,
    animators: Option<[f64; 4]>,
    bones: Option<[f64; 4]>,
    lights: Option<[f64; 4]>,
    particle_systems: Option<[f64; 4]>,
    total_particles: Option<[f64; 4]>,
    mesh_particle_polys: Option<[f64; 4]>,
    particle_trails: Option<[f64; 4]>,
    particle_collisions: Option<[f64; 4]>,
    trail_renderers: Option<[f64; 4]>,
    line_renderers: Option<[f64; 4]>,
    raycasts: Option<[f64; 4]>,
    cloths: Option<[f64; 4]>,
    cloth_vertices: Option<[f64; 4]>,
    physics_colliders: Option<[f64; 4]>,
    physics_rigidbodies: Option<[f64; 4]>,
    audio_sources: Option<[f64; 4]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitsFile {
    pc: Table,
    android: Table,
}

/// A stat's value as the rank sees it.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Measure {
    Scalar(f64),
    Bounds([f64; 3]),
}

/// A stat's limits as the data file gives them: [Excellent, Good, Medium, Poor], or one such row per axis for bounds.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Limits {
    Scalar([f64; 4]),
    Bounds([[f64; 3]; 4]),
}

/// One row of the stat table: how to read the stat from the counts and from the limits file.
struct Stat {
    name: &'static str,
    value: fn(&AvatarStats, Platform) -> Measure,
    limits: fn(&Table) -> Option<Limits>,
}

/// Every stat the page lists, in the page's order. The names are the keys in the data file.
const STATS: &[Stat] = &[
    Stat { name: "triangles", value: |s, _| scalar(s.triangles), limits: |t| t.triangles.map(Limits::Scalar) },
    Stat { name: "bounds", value: |s, _| Measure::Bounds(s.bounds), limits: |t| t.bounds.map(Limits::Bounds) },
    Stat {
        name: "texture_mib",
        value: |s, p| Measure::Scalar(texture_mib(s, p)),
        limits: |t| t.texture_mib.map(Limits::Scalar),
    },
    Stat {
        name: "skinned_meshes",
        value: |s, _| scalar(s.skinned_meshes),
        limits: |t| t.skinned_meshes.map(Limits::Scalar),
    },
    Stat { name: "basic_meshes", value: |s, _| scalar(s.basic_meshes), limits: |t| t.basic_meshes.map(Limits::Scalar) },
    Stat {
        name: "material_slots",
        value: |s, _| scalar(s.material_slots),
        limits: |t| t.material_slots.map(Limits::Scalar),
    },
    Stat {
        name: "physbones_components",
        value: |s, _| scalar(s.physbones_components),
        limits: |t| t.physbones_components.map(Limits::Scalar),
    },
    Stat {
        name: "physbones_transforms",
        value: |s, _| scalar(s.physbones_transforms),
        limits: |t| t.physbones_transforms.map(Limits::Scalar),
    },
    Stat {
        name: "physbones_colliders",
        value: |s, _| scalar(s.physbones_colliders),
        limits: |t| t.physbones_colliders.map(Limits::Scalar),
    },
    Stat {
        name: "physbones_collision_checks",
        value: |s, _| scalar(s.physbones_collision_checks),
        limits: |t| t.physbones_collision_checks.map(Limits::Scalar),
    },
    Stat { name: "contacts", value: |s, _| scalar(s.contacts), limits: |t| t.contacts.map(Limits::Scalar) },
    Stat { name: "constraints", value: |s, _| scalar(s.constraints), limits: |t| t.constraints.map(Limits::Scalar) },
    Stat {
        name: "constraint_depth",
        value: |s, _| scalar(s.constraint_depth),
        limits: |t| t.constraint_depth.map(Limits::Scalar),
    },
    Stat { name: "animators", value: |s, _| scalar(s.animators), limits: |t| t.animators.map(Limits::Scalar) },
    Stat { name: "bones", value: |s, _| scalar(s.bones), limits: |t| t.bones.map(Limits::Scalar) },
    Stat { name: "lights", value: |s, _| scalar(s.lights), limits: |t| t.lights.map(Limits::Scalar) },
    Stat {
        name: "particle_systems",
        value: |s, _| scalar(s.particle_systems),
        limits: |t| t.particle_systems.map(Limits::Scalar),
    },
    Stat {
        name: "total_particles",
        value: |s, _| scalar(s.total_particles),
        limits: |t| t.total_particles.map(Limits::Scalar),
    },
    Stat {
        name: "mesh_particle_polys",
        value: |s, _| scalar(s.mesh_particle_polys),
        limits: |t| t.mesh_particle_polys.map(Limits::Scalar),
    },
    Stat {
        name: "particle_trails",
        value: |s, _| scalar(s.particle_trails),
        limits: |t| t.particle_trails.map(Limits::Scalar),
    },
    Stat {
        name: "particle_collisions",
        value: |s, _| scalar(s.particle_collisions),
        limits: |t| t.particle_collisions.map(Limits::Scalar),
    },
    Stat {
        name: "trail_renderers",
        value: |s, _| scalar(s.trail_renderers),
        limits: |t| t.trail_renderers.map(Limits::Scalar),
    },
    Stat {
        name: "line_renderers",
        value: |s, _| scalar(s.line_renderers),
        limits: |t| t.line_renderers.map(Limits::Scalar),
    },
    Stat { name: "raycasts", value: |s, _| scalar(s.raycasts), limits: |t| t.raycasts.map(Limits::Scalar) },
    Stat { name: "cloths", value: |s, _| scalar(s.cloths), limits: |t| t.cloths.map(Limits::Scalar) },
    Stat {
        name: "cloth_vertices",
        value: |s, _| scalar(s.cloth_vertices),
        limits: |t| t.cloth_vertices.map(Limits::Scalar),
    },
    Stat {
        name: "physics_colliders",
        value: |s, _| scalar(s.physics_colliders),
        limits: |t| t.physics_colliders.map(Limits::Scalar),
    },
    Stat {
        name: "physics_rigidbodies",
        value: |s, _| scalar(s.physics_rigidbodies),
        limits: |t| t.physics_rigidbodies.map(Limits::Scalar),
    },
    Stat {
        name: "audio_sources",
        value: |s, _| scalar(s.audio_sources),
        limits: |t| t.audio_sources.map(Limits::Scalar),
    },
];

/// Ranks an avatar on `platform`: each supported stat against its limits, the overall rank the worst of them.
///
/// # Errors
///
/// Fails only if the bundled limits file is broken, which the tests catch.
pub fn rank(stats: &AvatarStats, platform: Platform) -> Result<Ranking, LimitsError> {
    let file: LimitsFile = toml::from_str(LIMITS_TOML)?;
    let table = match platform {
        Platform::Pc => &file.pc,
        Platform::Android => &file.android,
    };
    let mut per_stat = Vec::with_capacity(STATS.len());
    for stat in STATS {
        let rank = match ((stat.limits)(table), (stat.value)(stats, platform)) {
            (None, _) => None,
            (Some(Limits::Scalar(limits)), Measure::Scalar(value)) => Some(band(value, limits)),
            (Some(Limits::Bounds(limits)), Measure::Bounds(value)) => Some(bounds_band(value, limits)),
            _ => return Err(LimitsError::Shape(stat.name)),
        };
        per_stat.push(StatRank { name: stat.name, rank });
    }
    let overall = per_stat.iter().filter_map(|s| s.rank).max().unwrap_or(Rank::Excellent);
    Ok(Ranking { rank: overall, stats: per_stat })
}

/// The rank a scalar value earns against `[Excellent, Good, Medium, Poor]`: the first limit it fits under.
fn band(value: f64, [excellent, good, medium, poor]: [f64; 4]) -> Rank {
    if value <= excellent {
        Rank::Excellent
    } else if value <= good {
        Rank::Good
    } else if value <= medium {
        Rank::Medium
    } else if value <= poor {
        Rank::Poor
    } else {
        Rank::VeryPoor
    }
}

/// Bounds are compared per axis: the rank is the worst of the three axes.
fn bounds_band(value: [f64; 3], limits: [[f64; 3]; 4]) -> Rank {
    (0..3)
        .map(|axis| {
            let per_rank: [f64; 4] = std::array::from_fn(|i| limits[i][axis]);
            band(value[axis], per_rank)
        })
        .max()
        .unwrap_or(Rank::Excellent)
}

// Counts stay far below 2^53, where f64 stops representing every integer exactly.
#[allow(clippy::cast_precision_loss)]
fn scalar(n: u64) -> Measure {
    Measure::Scalar(n as f64)
}

/// The texture memory the platform would upload for the avatar's images, in MiB.
///
/// An estimate of what Unity's default import uploads: PC uses 0.5 bytes per pixel for opaque images and 1 with alpha
/// (DXT1 and DXT5 or BC7), Android uses ASTC 6x6 at 3.56 bits per pixel, and a full mip chain adds a third. A real
/// import can differ, so treat the rank of a texture-heavy avatar as approximate.
pub fn texture_mib(stats: &AvatarStats, platform: Platform) -> f64 {
    let (opaque, alpha) = match platform {
        Platform::Pc => (PC_BYTES_OPAQUE, PC_BYTES_ALPHA),
        Platform::Android => (ASTC_6X6_BYTES, ASTC_6X6_BYTES),
    };
    // Pixel counts stay far below 2^53, where f64 stops representing every integer exactly.
    #[allow(clippy::cast_precision_loss)]
    let (px_opaque, px_alpha) = (stats.texture_px_opaque as f64, stats.texture_px_alpha as f64);
    (px_opaque * opaque + px_alpha * alpha) * MIP_FACTOR / MIB
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
        stats.material_slots += len(mesh.primitives.len());
        if node.skin.is_some() {
            stats.skinned_meshes += 1;
        } else {
            stats.basic_meshes += 1;
        }
    }
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
    for image in &scene.images {
        let (pixels, alpha) = image_pixels(image)?;
        if alpha {
            stats.texture_px_alpha += pixels;
        } else {
            stats.texture_px_opaque += pixels;
        }
    }
    Ok(())
}

/// The pixel count and whether the image has an alpha channel, read from its header only.
fn image_pixels(image: &EncodedImage) -> Result<(u64, bool), StatsError> {
    let fail = |reason: String| StatsError::Image { name: image.name.clone(), reason };
    let reader =
        ImageReader::new(Cursor::new(image.bytes.as_slice())).with_guessed_format().map_err(|e| fail(e.to_string()))?;
    let decoder = reader.into_decoder().map_err(|e| fail(e.to_string()))?;
    let (width, height) = decoder.dimensions();
    Ok((u64::from(width) * u64::from(height), decoder.color_type().has_alpha()))
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
