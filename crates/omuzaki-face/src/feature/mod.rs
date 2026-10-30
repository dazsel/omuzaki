//! The face's 3D features alone, before they join a head: each sculpted as a signed-distance field ([`sdf`]) and meshed
//! with surface nets ([`nets`]), so its undersides, holes and folds (nostrils, the ear's rim, the lips' parting) are
//! real geometry a camera can look into from any side.
//!
//! - [`nose::NoseDesign`]: the nose on a patch of face.
//! - [`lips::LipsDesign`]: the lips on the patch of face round the mouth.
//! - [`ear::EarDesign`]: the ear on a patch of the side of the head.
//! - [`skin_mesh`]: a field meshed for the renderer's `skin` program, with its colours and `skin-info` (wet, cavity,
//!   flush, curvature) taken from the field.
//! - [`closeup::Closeup`]: a feature built on a renderer, lit as the eye's close-up is, from the front, three-quarter,
//!   the side or below.

#![allow(clippy::many_single_char_names, reason = "geometry maths: x, y, z, p, d, k are the field's names")]

pub mod closeup;
pub mod ear;
pub mod lips;
pub mod nets;
pub mod nose;
pub mod sdf;

use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::mesh::{ExtraAttribute, MeshData};

use nets::Field;

/// The `skin` program's per-vertex attribute.
const SKIN_INFO: &str = "skin-info";

/// What a feature's mesh carries besides its shape, per point on the surface.
pub struct Paint<'a> {
    /// The skin's linear colour at a point.
    pub colour: &'a dyn Fn(Vec3) -> Vec3,
    /// How wet it is (0 dry, 1 a wet lip or margin).
    pub wet: &'a dyn Fn(Vec3) -> f32,
    /// Extra redness (the `skin` program's flush).
    pub flush: &'a dyn Fn(Vec3) -> f32,
}

/// The field meshed inside `min`–`max` at `cell` metres, with what the `skin` program reads: linear vertex colours,
/// planar UVs across the box, and `skin-info` with the cavity measured from the field (how much of the open space
/// above each point the surface itself fills) and the curvature from its Laplacian.
pub fn skin_mesh(field: &impl Field, min: Vec3, max: Vec3, cell: f32, paint: &Paint<'_>) -> MeshData {
    let surface = nets::mesh(field, min, max, cell);
    let size = (max - min).truncate().max(Vec2::splat(1e-6));
    let mut mesh = MeshData::default();
    let mut info = Vec::with_capacity(surface.positions.len() * 4);
    for (p, n) in surface.positions.iter().zip(&surface.normals) {
        let (p, n) = (*p, *n);
        mesh.positions.push(p);
        mesh.normals.push(n);
        mesh.uvs.push(Vec2::new((p.x - min.x) / size.x, (max.y - p.y) / size.y));
        let c = (paint.colour)(p);
        mesh.colours.push([c.x, c.y, c.z, 1.0]);
        info.extend_from_slice(&[(paint.wet)(p), cavity(field, p, n), (paint.flush)(p), curvature(field, p)]);
    }
    mesh.indices = surface.indices;
    if let Ok(name) = Slug::new(SKIN_INFO) {
        mesh.extra.push(ExtraAttribute { name, components: 4, data: info });
    }
    mesh
}

/// How open the space above `p` is (1 open, 0 deep in a fold or a hole): the field sampled out along the normal, where
/// a nearby surface makes it less than the distance travelled.
pub fn cavity(field: &impl Field, p: Vec3, n: Vec3) -> f32 {
    const STEPS: [f32; 5] = [0.0008, 0.0016, 0.0028, 0.0044, 0.0065];
    let mut occluded = 0.0;
    let mut weight = 0.0;
    let mut w = 1.0;
    for h in &STEPS {
        occluded += w * ((h - field(p + n * *h)) / h).clamp(0.0, 1.0);
        weight += w;
        w *= 0.75;
    }
    (1.0 - 1.6 * occluded / weight).clamp(0.0, 1.0)
}

/// The surface's curvature at `p` in 1/m (one over the radius of the sphere it bends like): half the field's
/// Laplacian.
pub fn curvature(field: &impl Field, p: Vec3) -> f32 {
    let h = 0.001;
    let centre = field(p);
    let sum: f32 =
        [Vec3::X, Vec3::Y, Vec3::Z].iter().map(|e| field(p + *e * h) + field(p - *e * h) - 2.0 * centre).sum();
    (0.5 * sum / (h * h)).abs()
}
