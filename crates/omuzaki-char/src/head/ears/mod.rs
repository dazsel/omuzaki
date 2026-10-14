//! Ears attached to the head surface.
//!
//! Every ear kind roots on the head *as built* ([`root`], on the surface `mesh::build_head` returns and the head part
//! publishes), not on the unreshaped cranium: the reshaped lower face pulls the side of the head up to about 1 cm
//! inside the cranium at ear height, where an ear rooted on the cranium would float.

pub mod animal;
pub mod elf;
pub mod human;

use omuzaki_char_api::{Ears, HeadSurface};
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::narrow;

/// Where the left ear's root sits, bind space: on `head`'s surface in the direction of [`Ears::position`] from the head
/// centre. The position is authored on the cranium (the prototype's); the root follows the head as built, so it never
/// floats off a reshaped face. The right ear mirrors it in x.
#[must_use]
pub fn root(ears: &Ears, head: &dyn HeadSurface) -> Vec3 {
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
