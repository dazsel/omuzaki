//! The sculpted ear (`face/ear3d`): the face's signed-distance ear (helix, antihelix, concha, tragus, lobe) grown out of
//! this head's own side, through the feature mesher the binary links ([`FeatureMesherRegistration`]).
//!
//! The mesher works in the ear's frame in real metres (x toward the back of the head, y up, z out of its side, the
//! canal's mouth near the origin), and the head is built at the template's size, so the head's surface is handed to
//! it in that frame and scale, and the mesh comes back the same way. The ear's root blends into a copy of the head's
//! side sunk just under the head's mesh; the faces of that copy are dropped, so what is left is the auricle and the
//! fillet where it meets the head, with its edge hidden inside the head.

use omuzaki_char_api::{Ears, FeatureMesherRegistration, HeadSurface};
use omuzaki_core::glam::{DMat3, DVec3, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::sculpt::EarDesign;

use super::Side;

/// How far under the head's mesh the ear's copy of the head lies, real metres: deeper than the mesh's faceting.
const SINK_M: f32 = 0.0008;
/// A triangle whose corners are all this far inside the head (real metres) is the hidden copy, and is dropped.
const HIDDEN_M: f32 = 0.0004;

/// The ear's frame at `root` on the `side` of the head centred at `centre`, head-local axes: x toward the back, y up,
/// z out of the side, leaned back by `ears.tilt_deg` about z. For the right ear this is the left's mirror image, so
/// its triangles are turned over ([`build`]).
fn frame(ears: &Ears, root: Vec3, centre: Vec3, side: Side) -> DMat3 {
    let flat = (root - centre).as_dvec3() * DVec3::new(1.0, 0.0, 1.0);
    let out = if flat.length() > 1e-9 { flat.normalize() } else { DVec3::X * side.sign() };
    let back = DVec3::NEG_Z + out * out.z;
    let back = if back.length() > 1e-9 { back.normalize() } else { DVec3::NEG_Z };
    let up = out.cross(back) * side.sign();
    let (s, c) = ears.tilt_deg.to_radians().sin_cos();
    DMat3::from_cols(back * c - up * s, up * c + back * s, out)
}

/// Where a sculpted ear goes: its root on the head and the head's surface, the stature's scale and the mesher's cell.
pub struct OnHead<'a> {
    /// The root, bind space, on the head: where the canal's mouth is.
    pub root: Vec3,
    /// The head's surface, which the ear grows out of.
    pub surface: &'a dyn HeadSurface,
    /// How the character's stature scales the template's metres to real ones.
    pub scale: f32,
    /// The cell the ear is meshed at, real metres.
    pub cell: f32,
}

/// The sculpted `side` ear of `design` placed `on` the head, meshed by `mesher`: bind space, the template's metres,
/// counter-clockwise from outside, `None` if nothing of the ear stands out of the head.
pub fn build(
    mesher: &FeatureMesherRegistration,
    design: &EarDesign,
    ears: &Ears,
    side: Side,
    on: &OnHead<'_>,
) -> Option<MeshData> {
    let (root, surface, cell) = (on.root, on.surface, on.cell);
    let scale = on.scale.max(1e-3);
    let centre = surface.centre();
    let axes = frame(ears, root, centre, side);
    let to_head = |v: Vec3| root + (axes * (v / scale).as_dvec3()).as_vec3();
    // The head's signed distance in the ear's frame, real metres: along the ray from the centre, which is the distance
    // near the surface, where the ear's root blends into it.
    let distance = |v: Vec3| {
        let from = to_head(v) - centre;
        let length = from.length();
        if length <= 1e-9 {
            return -1.0;
        }
        (length - surface.radius(from / length)) * scale
    };
    let sunk = |v: Vec3| distance(v) + SINK_M;
    let ear = (mesher.ear)(design, &sunk, cell);

    let inside: Vec<bool> = ear.positions.iter().map(|v| distance(*v) < -HIDDEN_M).collect();
    let mut indices = Vec::with_capacity(ear.indices.len());
    for tri in ear.indices.as_chunks::<3>().0 {
        if tri.iter().all(|&i| inside.get(i as usize).copied().unwrap_or(true)) {
            continue;
        }
        indices.extend(match side {
            Side::Left => *tri,
            Side::Right => [tri[0], tri[2], tri[1]],
        });
    }
    if indices.is_empty() {
        return None;
    }
    let mut mesh = ear;
    mesh.positions = mesh.positions.iter().map(|v| to_head(*v)).collect();
    mesh.normals = mesh.normals.iter().map(|n| (axes * n.as_dvec3()).normalize_or_zero().as_vec3()).collect();
    mesh.indices = indices;
    Some(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frames_point_back_up_and_out_and_mirror() {
        let ears = Ears { tilt_deg: 0.0, ..Ears::default() };
        let centre = Vec3::new(0.0, 1.0, 0.0);
        let left = frame(&ears, centre + Vec3::new(0.18, 0.0, -0.01), centre, Side::Left);
        let right = frame(&ears, centre + Vec3::new(-0.18, 0.0, -0.01), centre, Side::Right);
        for (m, out) in [(left, 1.0), (right, -1.0)] {
            assert!(m.x_axis.z < -0.99, "x toward the back: {:?}", m.x_axis);
            assert!(m.y_axis.y > 0.99, "y up: {:?}", m.y_axis);
            assert!(m.z_axis.x * out > 0.99, "z out of the side: {:?}", m.z_axis);
        }
        assert!(left.determinant() > 0.99 && right.determinant() < -0.99, "the right ear is the left's mirror");
    }
}
