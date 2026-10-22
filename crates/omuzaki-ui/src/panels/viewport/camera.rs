//! The viewport's camera: an orbit around a target point (`shell.md` section 5), turned into the render camera.
//!
//! Orbit is middle drag or Alt+left drag, pan is Shift+middle drag, zoom is the wheel or a pinch. The presets Front,
//! ¾, Side and Back (keys 1 to 4) keep the target and the distance and set the angles; framing sets the target and the
//! distance so a box fills the view.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{Camera, Projection};

/// The vertical field of view, degrees: the render camera's default, which the stills use too.
pub const FOV_DEG: f32 = 30.0;
/// Degrees of orbit per point dragged.
const ORBIT_PER_POINT: f32 = 0.4;
/// Pitch stops short of straight up and down, where the up vector would flip.
const MAX_PITCH: f32 = 89.0;
/// The closest and farthest the camera gets, metres.
const DISTANCE: (f32, f32) = (0.05, 50.0);
/// How much of the view a framed box fills (the rest is margin).
const FILL: f32 = 0.85;

/// A box in character space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    /// The smallest corner.
    pub min: Vec3,
    /// The largest corner.
    pub max: Vec3,
}

impl Bounds {
    /// The box around `points`, or `None` when there are none (or none finite).
    pub fn of(points: impl IntoIterator<Item = Vec3>) -> Option<Self> {
        points.into_iter().filter(|p| p.is_finite()).fold(None, |b: Option<Self>, p| {
            Some(b.map_or(Self { min: p, max: p }, |b| Self { min: b.min.min(p), max: b.max.max(p) }))
        })
    }

    /// The box around both.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self { min: self.min.min(other.min), max: self.max.max(other.max) }
    }

    /// Its centre.
    pub fn centre(self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Half its diagonal: the radius of a sphere around it.
    pub fn radius(self) -> f32 {
        (self.max - self.min).length() * 0.5
    }
}

/// A view the presets name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum View {
    /// From the front (+z).
    Front,
    /// Three quarters, from the character's left front.
    ThreeQuarter,
    /// From the character's left side.
    Side,
    /// From behind.
    Back,
}

impl View {
    /// All four, in key order (1 to 4).
    pub const ALL: [Self; 4] = [Self::Front, Self::ThreeQuarter, Self::Side, Self::Back];

    /// The camera's yaw for the view, degrees around +y from the front.
    pub const fn yaw(self) -> f32 {
        match self {
            Self::Front => 0.0,
            Self::ThreeQuarter => 45.0,
            Self::Side => 90.0,
            Self::Back => 180.0,
        }
    }
}

/// An orbit camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitCamera {
    /// What it looks at and orbits around.
    pub target: Vec3,
    /// Degrees around +y; 0 looks at the character's front from +z.
    pub yaw: f32,
    /// Degrees above the target's horizon.
    pub pitch: f32,
    /// Metres from the target.
    pub distance: f32,
}

impl Default for OrbitCamera {
    /// The front view of a character of about 1.6 m, as the stills frame one before its size is known.
    fn default() -> Self {
        Self { target: Vec3::new(0.0, 0.8, 0.0), yaw: 0.0, pitch: 0.0, distance: 3.2 }
    }
}

impl OrbitCamera {
    /// Where the camera is.
    pub fn eye(&self) -> Vec3 {
        let (yaw, pitch) = (self.yaw.to_radians(), self.pitch.to_radians());
        self.target + self.distance * Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos())
    }

    /// The render camera: perspective, its clip planes set around the distance so near detail is not cut.
    pub fn camera(&self) -> Camera {
        let near = (self.distance * 0.01).max(0.01);
        let far = (self.distance * 20.0).max(near * 2.0);
        Camera::look_at(self.eye(), self.target, Vec3::Y, Projection::Perspective { fov_y_deg: FOV_DEG, near, far })
    }

    /// Turns by `dx`, `dy` points of drag.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw = (self.yaw - dx * ORBIT_PER_POINT).rem_euclid(360.0);
        self.pitch = (self.pitch + dy * ORBIT_PER_POINT).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Slides the target by `dx`, `dy` points of drag in a view `height` points tall, so the point under the pointer
    /// stays under it.
    pub fn pan(&mut self, dx: f32, dy: f32, height: f32) {
        let per_point = 2.0 * self.distance * (FOV_DEG.to_radians() * 0.5).tan() / height.max(1.0);
        let forward = (self.target - self.eye()).normalize_or_zero();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward);
        self.target += (-dx * right + dy * up) * per_point;
    }

    /// Moves closer (`factor` below 1) or farther.
    pub fn zoom(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.distance = (self.distance * factor).clamp(DISTANCE.0, DISTANCE.1);
        }
    }

    /// Looks from `view`, keeping the target and the distance.
    pub fn look_from(&mut self, view: View) {
        self.yaw = view.yaw();
        self.pitch = 0.0;
    }

    /// Centres `bounds` and moves back until it fills the view, keeping the angles.
    pub fn frame(&mut self, bounds: Bounds) {
        self.target = bounds.centre();
        let half = (FOV_DEG.to_radians() * 0.5).sin();
        self.distance = (bounds.radius() / (half * FILL)).clamp(DISTANCE.0, DISTANCE.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_front_view_looks_down_minus_z_from_plus_z() {
        let camera = OrbitCamera::default();
        let eye = camera.eye();
        assert!((eye - Vec3::new(0.0, 0.8, 3.2)).length() < 1e-5, "{eye}");
        let mut side = camera;
        side.look_from(View::Side);
        assert!(side.eye().x > 3.0, "the side view is from +x: {}", side.eye());
        let mut back = camera;
        back.look_from(View::Back);
        assert!(back.eye().z < -3.0);
    }

    #[test]
    fn orbit_clamps_pitch_and_zoom_clamps_distance() {
        let mut camera = OrbitCamera::default();
        camera.orbit(0.0, 10_000.0);
        assert!((camera.pitch - MAX_PITCH).abs() < f32::EPSILON);
        camera.zoom(1.0e-9);
        assert!((camera.distance - DISTANCE.0).abs() < f32::EPSILON);
        camera.zoom(f32::NAN);
        assert!((camera.distance - DISTANCE.0).abs() < f32::EPSILON, "a bad factor changes nothing");
    }

    #[test]
    fn panning_moves_the_target_across_the_view() {
        let mut camera = OrbitCamera::default();
        camera.pan(100.0, 0.0, 500.0);
        assert!(camera.target.x < 0.0, "dragging right moves the scene right, so the target goes left");
        assert!((camera.target.y - 0.8).abs() < 1e-5);
    }

    #[test]
    fn framing_fits_the_box() {
        let mut camera = OrbitCamera::default();
        let head = Bounds { min: Vec3::new(-0.1, 1.4, -0.1), max: Vec3::new(0.1, 1.65, 0.1) };
        camera.frame(head);
        assert!((camera.target - head.centre()).length() < 1e-6);
        let small = camera.distance;
        camera.frame(head.union(Bounds { min: Vec3::ZERO, max: Vec3::new(0.1, 0.1, 0.1) }));
        assert!(camera.distance > small, "a bigger box needs more distance");
        assert_eq!(Bounds::of([]), None);
    }
}
