//! An orbit camera, parametrised so it can reproduce the JS prototype's views exactly.

use glam::camera::rh::{proj::directx, view};
use glam::{Mat4, Vec3};

/// Orbits a target point at a distance, looking at it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitCamera {
    /// The point looked at, in metres.
    pub target: Vec3,
    /// Angle around the vertical axis, radians; 0 looks at the character's front (camera on +Z).
    pub yaw: f32,
    /// Angle above the horizon, radians.
    pub pitch: f32,
    /// Distance from the target, metres.
    pub distance: f32,
    /// Vertical field of view, radians.
    pub fov_y: f32,
    /// Near plane distance, metres.
    pub near: f32,
    /// Far plane distance, metres.
    pub far: f32,
}

impl OrbitCamera {
    /// The prototype's camera: look at `(0, ty, 0)` from `horizontal` metres away, `height` metres above the target.
    /// (the prototype: `position = (sin a * dist, ty + cy, cos a * dist)`, fov 30, near 0.02, far 50.)
    #[must_use]
    pub fn js_view(yaw_degrees: f32, horizontal: f32, height: f32, target_y: f32) -> Self {
        Self {
            target: Vec3::new(0.0, target_y, 0.0),
            yaw: yaw_degrees.to_radians(),
            pitch: height.atan2(horizontal),
            distance: horizontal.hypot(height),
            fov_y: 30f32.to_radians(),
            near: 0.02,
            far: 50.0,
        }
    }

    /// The prototype's default front view.
    #[must_use]
    pub fn front() -> Self {
        Self::js_view(0.0, 2.4, 0.12, 0.5)
    }

    /// World-space eye position.
    #[must_use]
    pub fn position(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        self.target + self.distance * Vec3::new(sy * cp, sp, cy * cp)
    }

    /// World to view matrix.
    #[must_use]
    pub fn view(&self) -> Mat4 {
        view::look_at_mat4(self.position(), self.target, Vec3::Y)
    }

    /// View to clip matrix for the given aspect ratio (right-handed, depth 0..1 as wgpu expects).
    #[must_use]
    pub fn projection(&self, aspect: f32) -> Mat4 {
        directx::perspective(self.fov_y, aspect, self.near, self.far)
    }

    /// Orbits by a drag in pixels (a full viewport height is half a turn).
    pub fn orbit(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let k = std::f32::consts::PI / viewport_height.max(1.0);
        self.yaw -= dx * k;
        self.pitch = (self.pitch + dy * k).clamp(-1.5, 1.5);
    }

    /// Dollies by scroll steps; each step is a tenth of the distance.
    pub fn zoom(&mut self, scroll: f32) {
        self.distance = (self.distance * (1.0 - 0.1 * scroll.signum() * scroll.abs().min(3.0))).clamp(0.3, 20.0);
    }

    /// Pans the target in the view plane by a drag in pixels.
    pub fn pan(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let per_pixel = 2.0 * self.distance * (self.fov_y * 0.5).tan() / viewport_height.max(1.0);
        let view = self.view().inverse();
        let right = view.x_axis.truncate();
        let up = view.y_axis.truncate();
        self.target += (-right * dx + up * dy) * per_pixel;
    }
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self::front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_view_matches_the_prototype_camera_position() {
        let p = OrbitCamera::front().position();
        assert!((p - Vec3::new(0.0, 0.62, 2.4)).length() < 1e-5, "{p}");
    }

    #[test]
    fn side_view_is_a_quarter_turn_around() {
        let p = OrbitCamera::js_view(90.0, 2.4, 0.12, 0.5).position();
        assert!((p - Vec3::new(2.4, 0.62, 0.0)).length() < 1e-5, "{p}");
    }

    #[test]
    fn zoom_stays_inside_its_limits() {
        let mut c = OrbitCamera::front();
        for _ in 0..200 {
            c.zoom(1.0);
        }
        assert!((c.distance - 0.3).abs() < 1e-6);
        for _ in 0..200 {
            c.zoom(-1.0);
        }
        assert!((c.distance - 20.0).abs() < 1e-6);
    }

    #[test]
    fn pan_moves_the_target_sideways_for_a_front_view() {
        let mut c = OrbitCamera::front();
        c.pan(100.0, 0.0, 720.0);
        assert!(c.target.x < 0.0 && c.target.z.abs() < 1e-5, "{}", c.target);
    }
}
