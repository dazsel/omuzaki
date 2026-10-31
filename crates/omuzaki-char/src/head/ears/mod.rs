//! Ears attached to the head surface.
//!
//! Every ear kind roots on the head *as built* ([`root`], on the surface `mesh::build_head` returns and the head part
//! publishes), not on the unreshaped cranium: the reshaped lower face pulls the side of the head up to about 1 cm
//! inside the cranium at ear height, where an ear rooted on the cranium would float.
//!
//! **Human and elf** ([`human`], [`elf`]) are one shell, the port of the prototype's `buildEar` (`head.js` 142 to 175):
//! a 40 x 8 grid on a C-shaped outline with a rolled rim and a concha, placed by [`Frame`] at the root. The elf draws
//! the same shell out into a pointed tip. [`build`] makes the mesh of either, [`anchor`] the attachment point hair and
//! hats read, and [`cover`] the disc of head the ear hides, which `head` publishes as [`fact::EarCoverFact`].
//!
//! **Animal ears** ([`animal`]) stand on top of the head instead: a cupped shell with an inner panel, four species, the
//! rabbit's on a spring chain. They publish no cover and no `ear_l`/`ear_r` anchor, but `ear_top_l`/`ear_top_r` at their
//! tips, for hats and accessories.
//!
//! **Orientation.** The prototype hard-codes the left ear's shell normal `(0.74, 0.05, 0.66)` and height direction
//! `(0, 1, 0.12)`. The contract's defaults, [`Ears::default`]'s `flare_deg` 42 and `tilt_deg` 7, *are* those two vectors
//! (`atan2(0.66, 0.74)` is 41.7 degrees and `atan(0.12)` is 6.8): a flare of 42 degrees means the prototype's normal,
//! and any other flare or tilt turns it by the difference. That keeps the default ear within the prototype's tolerance
//! rather than 0.3 degrees off it, which is 0.2 mm at the rim.

pub mod animal;
pub mod elf;
pub mod fact;
pub mod human;
pub mod sculpted;

use omuzaki_char_api::{Anchor, EarKind, Ears, HeadSurface, JointRef};
use omuzaki_core::glam::{DVec3, Mat3, Quat, Vec3};
use omuzaki_core::mesh::{MeshData, MeshError};
use omuzaki_face_api::narrow;

use super::mesh::vertex_normals;

/// The prototype's left-ear shell normal (`head.js` 168), before normalising.
const PROTOTYPE_NORMAL: DVec3 = DVec3::new(0.74, 0.05, 0.66);
/// The prototype's ear height direction (`head.js` 169), before normalising.
const PROTOTYPE_UP: DVec3 = DVec3::new(0.0, 1.0, 0.12);
/// The `flare_deg` that stands for [`PROTOTYPE_NORMAL`] ([`Ears::default`]).
const DEFAULT_FLARE_DEG: f64 = 42.0;
/// The `tilt_deg` that stands for [`PROTOTYPE_UP`] ([`Ears::default`]).
const DEFAULT_TILT_DEG: f64 = 7.0;

/// Columns of the ear grid: steps once round the outline (the grid has one more column, repeating the first).
const SEGMENTS: u32 = 40;
/// Rows of the ear grid: steps from the concha's centre to the rim.
const RINGS: u32 = 8;

/// Which side of the head an ear is on. The character's left is +X. Ordered left first, so a map keyed by side lists the
/// left ear before the right.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    /// The character's left, +X.
    Left,
    /// The character's right, -X.
    Right,
}

impl Side {
    /// Both sides, left first.
    pub const BOTH: [Self; 2] = [Self::Left, Self::Right];

    /// +1 for the left, -1 for the right: the sign of the ear's x.
    #[must_use]
    pub const fn sign(self) -> f64 {
        match self {
            Self::Left => 1.0,
            Self::Right => -1.0,
        }
    }
}

/// An ear's orthonormal basis in head-local axes: where its width, height and shell normal point.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Frame {
    /// Across the ear's width.
    pub across: DVec3,
    /// Along the ear's height, lobe to top.
    pub up: DVec3,
    /// Out of the shell's face (towards the front and the side, so the concha reads from the front).
    pub normal: DVec3,
}

impl Frame {
    /// The frame of the `side` ear for `ears`' flare and tilt, built the way `buildEar` does: the width is
    /// `up x normal`, then the height is made square to both. See the module docs for how the angles map onto the
    /// prototype's vectors. The right ear mirrors the normal's x, not the basis, so it is the prototype's right ear.
    #[must_use]
    pub fn of(ears: &Ears, side: Side) -> Self {
        let flare = (ears.flare_deg - DEFAULT_FLARE_DEG).to_radians();
        let tilt = (ears.tilt_deg - DEFAULT_TILT_DEG).to_radians();
        let n = PROTOTYPE_NORMAL.normalize();
        // The flare turns the normal about the vertical, from outward (+x) towards the front (+z).
        let (sf, cf) = flare.sin_cos();
        let normal = DVec3::new(n.x * cf - n.z * sf, n.y, n.x * sf + n.z * cf);
        let normal = DVec3::new(side.sign() * normal.x, normal.y, normal.z);
        // The tilt leans the height direction about the x axis, in the prototype's sense (towards +z).
        let u = PROTOTYPE_UP.normalize();
        let (st, ct) = tilt.sin_cos();
        let up = DVec3::new(u.x, u.y * ct - u.z * st, u.y * st + u.z * ct);
        let across = up.cross(normal).normalize();
        Self { across, up: normal.cross(across).normalize(), normal }
    }

    /// The frame as the rotation of an anchor: x across, y up, z the normal.
    #[must_use]
    pub fn rotation(&self) -> Quat {
        Quat::from_mat3(&Mat3::from_cols(self.across.as_vec3(), self.up.as_vec3(), self.normal.as_vec3()))
    }
}

/// The disc of head an ear hides, for hair that notches around the ear or tucks locks behind it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EarCover {
    /// The ear's root, bind space.
    pub centre: Vec3,
    /// The shell's normal: the disc lies in the plane square to it.
    pub normal: Vec3,
    /// The disc's radius, metres: the ear's half height, which reaches past its half width.
    pub radius: f32,
    /// The highest point of the ear, bind space: the top of the disc, or the end of a pointed ear's tip.
    pub tip: Vec3,
    /// Whether the ear is pointed (elf), which hair makes way for ([`omuzaki_char_api::PointedEars`]).
    pub pointed: bool,
}

impl EarCover {
    /// The disc at `s` times its size, for a head built at the template's size and scaled to the character's stature
    /// (the anchors are scaled the same way). The normal is a direction and stays as it is.
    #[must_use]
    pub fn scaled(self, s: f32) -> Self {
        Self { centre: self.centre * s, normal: self.normal, radius: self.radius * s, tip: self.tip * s, ..self }
    }
}

/// The tallest half of the outline, metres at size 1 (`oy` in the prototype): the cover's radius.
const HALF_HEIGHT: f64 = 0.043;

/// The cover disc of the `side` ear rooted at `root`, or `None` for kinds with no ear on the side of the head
/// (none, and animal ears, which stand on top).
#[must_use]
pub fn cover(ears: &Ears, side: Side, root: Vec3) -> Option<EarCover> {
    on_the_side(ears.kind).then(|| {
        let frame = Frame::of(ears, side);
        let top = root.as_dvec3() + frame.up * HALF_HEIGHT * ears.size;
        let pointed = ears.kind == EarKind::Elf;
        let tip = if pointed { top + elf::tip_direction(ears, side) * elf::tip_length_of(ears) } else { top };
        EarCover {
            centre: root,
            normal: frame.normal.as_vec3(),
            radius: narrow(HALF_HEIGHT * ears.size),
            tip: tip.as_vec3(),
            pointed,
        }
    })
}

/// The anchor of the `side` ear (`ear_l`, `ear_r`): at its root on the head bone, turned by [`Frame::rotation`]. `None`
/// for kinds with no ear on the side of the head.
#[must_use]
pub fn anchor(ears: &Ears, side: Side, root: Vec3) -> Option<Anchor> {
    on_the_side(ears.kind).then(|| Anchor {
        bone: JointRef::HEAD,
        position: root,
        rotation: Frame::of(ears, side).rotation(),
    })
}

/// The name an ear's anchor is published under.
#[must_use]
pub const fn anchor_name(side: Side) -> &'static str {
    match side {
        Side::Left => "ear_l",
        Side::Right => "ear_r",
    }
}

/// Whether `kind` puts a shell on the side of the head.
const fn on_the_side(kind: EarKind) -> bool {
    matches!(kind, EarKind::Human | EarKind::Elf)
}

/// The mesh of the `side` ear rooted at `root` (bind space), or `None` when `ears.kind` builds nothing on the side of
/// the head. Unskinned: the head part skins it rigidly to the head. Every vertex stands at least [`human::CLEARANCE`]
/// outside `head`'s surface: where the shell would sink into the head it is bent to ride the surface instead.
///
/// # Errors
/// [`MeshError`] if the size makes a vertex that is not finite.
pub fn build(ears: &Ears, side: Side, root: Vec3, head: &dyn HeadSurface) -> Result<Option<MeshData>, MeshError> {
    match ears.kind {
        EarKind::Human => human::build(ears, side, root, head).map(Some),
        EarKind::Elf => elf::build(ears, side, root, head).map(Some),
        EarKind::None | EarKind::Animal => Ok(None),
    }
}

/// The grid's triangle list, the prototype's: each cell `(a, a+1, b)` then `(b, a+1, b+1)`, `b` one column on.
fn grid_indices() -> Vec<u32> {
    let row = RINGS + 1;
    let mut indices = Vec::with_capacity((SEGMENTS * RINGS * 6) as usize);
    for i in 0..SEGMENTS {
        for k in 0..RINGS {
            let a = i * row + k;
            let b = a + row;
            indices.extend([a, a + 1, b, b, a + 1, b + 1]);
        }
    }
    indices
}

/// A shell mesh from `positions` (the grid, column by column), with the smooth normals of its triangles.
fn shell_mesh(positions: Vec<Vec3>) -> Result<MeshData, MeshError> {
    let indices = grid_indices();
    let mut normals = vertex_normals(&positions, &indices);
    // The concha's centre is one point repeated down a whole column's worth of grid cells, so its first vertex belongs
    // only to zero-area triangles and has no normal of its own (three.js leaves float noise there). It takes the
    // centre's other normals, which are all the shell's direction.
    let centre: Vec3 = normals.iter().step_by((RINGS + 1) as usize).sum::<Vec3>().normalize_or_zero();
    for n in normals.iter_mut().filter(|n| **n == Vec3::ZERO) {
        *n = centre;
    }
    let mesh = MeshData { positions, normals, indices, ..MeshData::default() };
    mesh.validate()?;
    Ok(mesh)
}

/// Where the left ear's root sits, bind space: on `head`'s surface in the direction of [`Ears::position`] from the head
/// centre (an animal ear's is on top of the head, [`animal::root`]). The position is authored on the cranium (the prototype's); the root follows the head as built, so it never
/// floats off a reshaped face. The right ear mirrors it in x.
#[must_use]
pub fn root(ears: &Ears, head: &dyn HeadSurface) -> Vec3 {
    if ears.kind == EarKind::Animal {
        return animal::root(ears, head);
    }
    let towards = Vec3::from_array(ears.position.map(narrow)).normalize_or_zero();
    // A position at the very centre has no direction; the left side is where a left ear belongs.
    head.point(if towards == Vec3::ZERO { Vec3::X } else { towards }, 0.0)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use omuzaki_char_api::HeadParams;

    use super::*;
    use crate::head::cranium;
    use crate::head::mesh::{Resolution, build_head};

    #[test]
    fn the_root_sits_on_the_built_head_in_the_direction_of_the_position() {
        let params = HeadParams::default();
        let built = build_head(&params, Resolution::LIVE, None).unwrap();
        let head: Arc<dyn HeadSurface> = built.surface;
        let at = root(&params.ears, head.as_ref());
        let towards = Vec3::from_array(params.ears.position.map(narrow)).normalize();
        assert!((at - head.centre()).normalize().dot(towards) > 1.0 - 1e-6, "along the position's direction");
        assert!(((at - head.centre()).length() - head.radius(towards)).abs() < 1e-6, "on the surface");
        // The cranium would have put it outside the reshaped side of the face.
        let on_cranium = cranium::surface(&params, None).point(towards, 0.0);
        assert!((on_cranium - head.centre()).length() > (at - head.centre()).length() + 0.004);
    }

    #[test]
    fn a_position_at_the_centre_roots_on_the_left_side() {
        let params = HeadParams::default();
        let head = cranium::surface(&params, None);
        let ears = Ears { position: [0.0; 3], ..Ears::default() };
        assert!(root(&ears, head.as_ref()).x > head.centre().x);
    }
}
