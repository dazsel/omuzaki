//! Where the eyes' highlights slide as the camera moves round the face (`FaceView::highlight_shift`), from the camera
//! and the head: the prototype's reflection offset (`face.js` 440-441).

use omuzaki_core::glam::{Vec2, Vec3};

/// How far the highlights slide per radian the camera is off the face's axis.
pub const HIGHLIGHT_PER_RADIAN: f32 = 0.35;
/// The most the highlights slide sideways.
pub const HIGHLIGHT_MAX_X: f32 = 0.25;
/// The most they slide up or down.
pub const HIGHLIGHT_MAX_Y: f32 = 0.2;

/// The highlight shift for a camera at `camera` in head space (`+z` forward out of the face, `+x` toward the
/// character's left, `+y` up; only its direction counts): opposite the direction the camera sees the face from,
/// `x = clamp(-atan2(x, z) * 0.35, ±0.25)` and `y = clamp(-asin(y / |v|) * 0.35, ±0.2)`. Zero for a camera at the
/// head's origin or one that isn't a finite point.
pub fn highlight_shift(camera: Vec3) -> Vec2 {
    let length = camera.length();
    if !(length > 0.0 && length.is_finite()) {
        return Vec2::ZERO;
    }
    let x = -camera.x.atan2(camera.z) * HIGHLIGHT_PER_RADIAN;
    let y = -(camera.y / length).clamp(-1.0, 1.0).asin() * HIGHLIGHT_PER_RADIAN;
    Vec2::new(x.clamp(-HIGHLIGHT_MAX_X, HIGHLIGHT_MAX_X), y.clamp(-HIGHLIGHT_MAX_Y, HIGHLIGHT_MAX_Y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_camera_straight_ahead_leaves_the_highlights_centred() {
        assert_eq!(highlight_shift(Vec3::new(0.0, 0.0, 2.0)), Vec2::ZERO);
        assert_eq!(highlight_shift(Vec3::ZERO), Vec2::ZERO);
        assert_eq!(highlight_shift(Vec3::new(f32::NAN, 0.0, 1.0)), Vec2::ZERO);
    }

    #[test]
    fn the_shift_is_the_prototype_formula_and_its_clamps() {
        for camera in [
            Vec3::new(0.3, 0.1, 2.0),
            Vec3::new(-0.8, -0.4, 1.2),
            Vec3::new(2.0, 1.5, 0.5),
            Vec3::new(-0.1, 3.0, -1.0),
            Vec3::new(0.05, -0.02, 0.4),
        ] {
            let v = camera;
            let x = (-(v.x.atan2(v.z)) * 0.35).clamp(-0.25, 0.25);
            let y = (-(v.y / v.length()).asin() * 0.35).clamp(-0.2, 0.2);
            let shift = highlight_shift(camera);
            assert!((shift.x - x).abs() <= 1e-6 && (shift.y - y).abs() <= 1e-6, "{camera}: {shift} against ({x}, {y})");
        }
        // From the character's left and above, the highlights slide right and down.
        let shift = highlight_shift(Vec3::new(1.0, 1.0, 1.0));
        assert!(shift.x < 0.0 && shift.y < 0.0);
        assert_eq!(highlight_shift(Vec3::new(5.0, 0.0, 0.1)).x, -HIGHLIGHT_MAX_X);
        assert_eq!(highlight_shift(Vec3::new(0.0, -5.0, 0.1)).y, HIGHLIGHT_MAX_Y);
    }
}
