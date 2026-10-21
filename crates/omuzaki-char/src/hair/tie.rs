//! Hair ties: the short band that hides where a tail's locks start (a torus round the gather).
//!
//! A [`Tie`] is data a generator hands to the hair part ([`GroupGeometry::Tied`](super::compile::GroupGeometry::Tied));
//! the part draws every tie of a style into one mesh with its own material (the `tie` colour of the shading), so a
//! recolour of the hair never touches the band and the band can be recoloured alone.

use std::f32::consts::TAU;

use omuzaki_core::glam::{Quat, Vec2, Vec3};
use omuzaki_core::mesh::MeshData;

/// Segments round the band.
pub const AROUND: usize = 20;
/// Segments round the band's tube.
pub const ACROSS: usize = 8;

/// A torus: the band round a gather.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tie {
    /// Centre of the ring, bind space.
    pub centre: Vec3,
    /// Unit direction the gathered hair runs along (the ring's axis).
    pub axis: Vec3,
    /// Radius of the ring's centre line, metres.
    pub radius: f32,
    /// Radius of the band's own tube, metres.
    pub tube: f32,
}

impl Tie {
    /// The tie reflected in the plane `x = mid_x`.
    #[must_use]
    pub fn mirrored(self, mid_x: f32) -> Self {
        Self {
            centre: Vec3::new(2.0 * mid_x - self.centre.x, self.centre.y, self.centre.z),
            axis: Vec3::new(-self.axis.x, self.axis.y, self.axis.z),
            ..self
        }
    }
}

/// The mesh of `ties`: [`AROUND`] x [`ACROSS`] quads each, smooth normals, wound outward, `uvs` as the torus's own
/// two angles in `0..1`. `None` for no ties.
pub fn mesh(ties: &[Tie]) -> Option<MeshData> {
    if ties.is_empty() {
        return None;
    }
    let mut out = MeshData::default();
    for tie in ties {
        push(&mut out, tie);
    }
    Some(out)
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // grid indices are tiny
fn push(out: &mut MeshData, tie: &Tie) {
    let turn = Quat::from_rotation_arc(Vec3::Y, tie.axis.try_normalize().unwrap_or(Vec3::Y));
    let base = out.positions.len() as u32;
    for i in 0..=AROUND {
        let a = i as f32 / AROUND as f32 * TAU;
        let (sa, ca) = a.sin_cos();
        // The ring lies in the local XZ plane; `ring` points from the axis to the ring's centre line.
        let ring = Vec3::new(ca, 0.0, sa);
        for j in 0..=ACROSS {
            let b = j as f32 / ACROSS as f32 * TAU;
            let out_dir = ring * b.cos() + Vec3::Y * b.sin();
            out.positions.push(tie.centre + turn * (ring * tie.radius + out_dir * tie.tube));
            out.normals.push(turn * out_dir);
            out.uvs.push(Vec2::new(i as f32 / AROUND as f32, j as f32 / ACROSS as f32));
        }
    }
    let row = ACROSS as u32 + 1;
    for i in 0..AROUND as u32 {
        for j in 0..ACROSS as u32 {
            let (a, b) = (base + i * row + j, base + (i + 1) * row + j);
            out.indices.extend([a, a + 1, b, b, a + 1, b + 1]);
        }
    }
}
