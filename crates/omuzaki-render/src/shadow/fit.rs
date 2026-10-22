//! Fitting the shadow map's orthographic frustum to what it must hold, as plain maths (no GPU).
//!
//! The renderer fits the frustum itself (the contract has no shadow camera): it must hold every shadow caster and reach
//! as far as the surfaces that receive shadow, and it must not shimmer while things move. Three choices serve that:
//!
//! - the window is a **square around the casters' bounding sphere**, so turning or posing the character (which changes
//!   its box, not its sphere much) does not rescale the map;
//! - the sphere's radius is **rounded up to a geometric step** ([`STEP_PER_OCTAVE`] per doubling), so the scale, and with
//!   it the texel size, changes only when the casters have grown or shrunk by a fifth;
//! - the window's centre is **snapped to the texel grid** in light space, so a caster that drifts by less than a texel
//!   leaves the matrix, and every shadow edge, exactly where it was.
//!
//! The camera does not enter at all: the map depends on the light and the casters, so moving the camera cannot move a
//! shadow edge.
//!
//! **A light nearly parallel to the ground** (a low sun, a rim light) is the hard case. Its shadows reach far across the
//! ground, so the receivers sit far along the light, and the depth range would grow without limit. The fit caps it: the
//! far plane stops at [`RECEIVER_REACH_RADII`] window radii past the casters, and the basis stays well defined at grazing
//! angles because the up hint switches only near vertical. What lies beyond the cap is not drawn: the lookup in
//! `shader/toon.wgsl` treats a point past the far plane, or outside the window, as lit. So a long grazing shadow is
//! cut off at the cap, and the rest of the ground stays lit rather than shadowed at the wrong depth.

use omuzaki_core::glam::camera::rh::proj::directx;
use omuzaki_core::glam::{Mat4, Vec3};

/// The window's radius takes values `2^(k / STEP_PER_OCTAVE)` metres (about 19% apart).
const STEP_PER_OCTAVE: f32 = 4.0;
/// The smallest window radius, so a speck of a caster does not ask for a map of microscopic extent.
const MIN_RADIUS: f32 = 0.05;
/// Texels kept clear around the casters at the window's edge, for the filter's footprint.
const FILTER_MARGIN_TEXELS: f32 = 4.0;
/// How far past the casters the depth range may reach for receivers, in window radii. A shadow cast onto a surface
/// farther than this is not worth the depth precision it costs.
const RECEIVER_REACH_RADII: f32 = 8.0;

/// An axis-aligned box in world space. Empty until a point is added.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Aabb {
    pub(crate) min: Vec3,
    pub(crate) max: Vec3,
}

impl Aabb {
    /// The box that contains nothing: the identity of [`union`](Self::union).
    pub(crate) const EMPTY: Self = Self { min: Vec3::splat(f32::INFINITY), max: Vec3::splat(f32::NEG_INFINITY) };

    /// The box spanning `min` to `max`.
    #[cfg(test)]
    pub(crate) fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    /// Whether the box contains no point.
    pub(crate) fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    /// The box of `points`. Four boxes grow side by side and join at the end: one box is a chain of dependent min and max
    /// operations a vertex long, and a mesh's refit runs it over every vertex.
    pub(crate) fn of_points(points: &[Vec3]) -> Self {
        let mut lanes = [Self::EMPTY; 4];
        let (chunks, rest) = points.as_chunks::<4>();
        for chunk in chunks {
            for (lane, point) in lanes.iter_mut().zip(chunk) {
                lane.include(*point);
            }
        }
        for point in rest {
            lanes[0].include(*point);
        }
        let [mut all, b, c, d] = lanes;
        for other in [&b, &c, &d] {
            all.union(other);
        }
        all
    }

    /// Grows the box to hold `point`.
    pub(crate) fn include(&mut self, point: Vec3) {
        self.min = self.min.min(point);
        self.max = self.max.max(point);
    }

    /// Grows the box to hold `other`.
    pub(crate) fn union(&mut self, other: &Self) {
        if !other.is_empty() {
            self.min = self.min.min(other.min);
            self.max = self.max.max(other.max);
        }
    }

    /// The eight corners.
    pub(crate) fn corners(&self) -> [Vec3; 8] {
        let (a, b) = (self.min, self.max);
        [
            Vec3::new(a.x, a.y, a.z),
            Vec3::new(b.x, a.y, a.z),
            Vec3::new(a.x, b.y, a.z),
            Vec3::new(b.x, b.y, a.z),
            Vec3::new(a.x, a.y, b.z),
            Vec3::new(b.x, a.y, b.z),
            Vec3::new(a.x, b.y, b.z),
            Vec3::new(b.x, b.y, b.z),
        ]
    }

    /// The box that holds this one after `transform` (an empty box stays empty).
    pub(crate) fn transformed(&self, transform: &Mat4) -> Self {
        let mut out = Self::EMPTY;
        if !self.is_empty() {
            for corner in self.corners() {
                out.include(transform.transform_point3(corner));
            }
        }
        out
    }

    fn centre(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Half the diagonal: the radius of the sphere around the box.
    fn radius(&self) -> f32 {
        (self.max - self.min).length() * 0.5
    }
}

/// A fitted shadow frustum.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Fit {
    /// World to shadow clip space: x and y in `-1..=1` across the window, depth in `0..=1` (wgpu).
    pub(crate) view_proj: Mat4,
    /// World metres covered by one texel of the map.
    pub(crate) texel: f32,
    /// World metres between the near and far planes: what depth bias in metres divides by.
    pub(crate) depth_range: f32,
}

/// The window radius for casters of bounding-sphere `radius` in a map of `map_size` texels: the radius plus the room
/// for the filter and the normal offset, rounded up to a step of the geometric ladder.
fn window_radius(radius: f32, map_size: u32, normal_bias: f32) -> f32 {
    #[allow(clippy::cast_precision_loss)] // map sizes are far below 2^24
    let margin = 1.0 + 2.0 * FILTER_MARGIN_TEXELS / map_size.max(1) as f32;
    let wanted = ((radius.max(MIN_RADIUS) + normal_bias) * margin).max(MIN_RADIUS);
    ((wanted.log2() * STEP_PER_OCTAVE).ceil() / STEP_PER_OCTAVE).exp2()
}

/// Fits the frustum for a directional light shining from direction `to_light` onto `casters`, reaching `receivers` (the
/// surfaces that take shadow; may be empty).
///
/// `normal_bias` is the offset along the surface normal the lookup applies, in metres: the window keeps that much extra
/// room at its edge.
///
/// Returns `None` when there is nothing to fit (no caster, or a light direction that is zero or not finite).
pub(crate) fn fit(to_light: Vec3, casters: &Aabb, receivers: &Aabb, map_size: u32, normal_bias: f32) -> Option<Fit> {
    let forward = (-to_light).try_normalize()?;
    if casters.is_empty() || !casters.min.is_finite() || !casters.max.is_finite() {
        return None;
    }
    // A basis that depends on the light alone, so the texel grid is fixed in the world.
    let up_hint = if forward.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let right = forward.cross(up_hint).normalize();
    let up = right.cross(forward);

    let centre = casters.centre();
    let radius = casters.radius();
    let half = window_radius(radius, map_size, normal_bias);
    #[allow(clippy::cast_precision_loss)]
    let texel = 2.0 * half / map_size.max(1) as f32;
    let snap = |v: f32| (v / texel).round() * texel;
    let eye = right * snap(centre.dot(right)) + up * snap(centre.dot(up)) + forward * centre.dot(forward);

    // Depth along the light, measured from the eye: the casters fill -radius..radius, and the far plane reaches the
    // farthest receiver (within RECEIVER_REACH_RADII), so a shadow lands on the ground beyond the casters.
    let near = -radius - normal_bias;
    let mut far = radius + normal_bias;
    if !receivers.is_empty() {
        let farthest = receivers.corners().iter().map(|c| (*c - eye).dot(forward)).fold(f32::NEG_INFINITY, f32::max);
        far = far.max(farthest.min(radius + RECEIVER_REACH_RADII * half));
    }
    // The light's camera: placed at `eye`, looking along `forward`, so its +Z axis points back at the light.
    let view = Mat4::from_cols(right.extend(0.0), up.extend(0.0), (-forward).extend(0.0), eye.extend(1.0)).inverse();
    let proj = directx::orthographic(-half, half, -half, half, near, far);
    Some(Fit { view_proj: proj * view, texel, depth_range: far - near })
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss)] // map sizes are far below 2^24
mod tests {
    use super::*;

    #[test]
    fn the_box_of_points_holds_every_point_whatever_the_count() {
        let point = |i: u32| Vec3::new(i as f32, -(i as f32) * 2.0, (i % 5) as f32);
        for count in [0u32, 1, 3, 4, 5, 8, 9, 101] {
            let points: Vec<Vec3> = (0..count).map(point).collect();
            let mut want = Aabb::EMPTY;
            for p in &points {
                want.include(*p);
            }
            assert_eq!(Aabb::of_points(&points), want, "{count} points");
        }
    }

    const SIZE: u32 = 1024;
    const BIAS: f32 = 0.01;
    const TO_LIGHT: Vec3 = Vec3::new(-0.8, 1.6, 2.2);

    fn character() -> Aabb {
        Aabb::new(Vec3::new(-0.3, 0.0, -0.15), Vec3::new(0.3, 1.7, 0.15))
    }

    fn ground() -> Aabb {
        Aabb::new(Vec3::new(-0.8, 0.0, -0.8), Vec3::new(0.8, 0.0, 0.8))
    }

    fn fitted(casters: &Aabb) -> Fit {
        fit(TO_LIGHT, casters, &ground(), SIZE, BIAS).expect("a fit")
    }

    fn clip(fit: &Fit, p: Vec3) -> Vec3 {
        fit.view_proj.project_point3(p)
    }

    #[test]
    fn the_window_holds_every_caster_corner_and_the_ground_is_inside_the_depth_range() {
        let f = fitted(&character());
        for c in character().corners() {
            let p = clip(&f, c);
            assert!(p.x.abs() <= 1.0 && p.y.abs() <= 1.0, "corner {c} at {p}");
            assert!((0.0..=1.0).contains(&p.z), "corner {c} depth {}", p.z);
        }
        for c in ground().corners() {
            let z = clip(&f, c).z;
            assert!((0.0..=1.0).contains(&z), "ground corner {c} depth {z}");
        }
    }

    #[test]
    fn a_character_fills_most_of_a_standing_window_at_about_two_millimetres_a_texel() {
        // The prototype's rig: a 2 m window over a 1024 map, 1.95 mm a texel.
        let f = fitted(&character());
        assert!((0.001..0.003).contains(&f.texel), "texel {} m", f.texel);
    }

    #[test]
    fn a_caster_drifting_less_than_a_texel_leaves_the_matrix_alone() {
        let base = fitted(&character());
        let right = (-TO_LIGHT).normalize().cross(Vec3::Y).normalize();
        // The centre of the box sits at some place in its texel; a shift smaller than the room left to the rounding
        // thresholds keeps it in the same texel. Find that room, then shift by half of it.
        let centre = character().centre();
        let cell = centre.dot(right) / base.texel;
        let room = 0.5 - (cell - cell.round()).abs();
        assert!(room > 0.05, "test setup: the centre sits on a rounding threshold");
        let mut moved = character();
        let shift = right * base.texel * room * 0.5;
        moved.min += shift;
        moved.max += shift;
        assert_eq!(fitted(&moved).view_proj, base.view_proj, "sub-texel shift moved the shadow map");
    }

    #[test]
    fn a_shift_of_whole_texels_moves_the_window_by_exactly_those_texels() {
        let base = fitted(&character());
        let right = (-TO_LIGHT).normalize().cross(Vec3::Y).normalize();
        let mut moved = character();
        moved.min += right * base.texel * 7.0;
        moved.max += right * base.texel * 7.0;
        let f = fitted(&moved);
        assert_eq!(f.texel, base.texel);
        // The world origin lands 7 texels further to the left in the shifted window: 7 texels is 14/1024 of the width.
        let (a, b) = (clip(&base, Vec3::ZERO), clip(&f, Vec3::ZERO));
        assert!((a.x - b.x - 14.0 / SIZE as f32).abs() < 1e-4, "{} vs {}", a.x, b.x);
        assert!((a.y - b.y).abs() < 1e-4);
    }

    #[test]
    fn small_growth_keeps_the_scale_and_big_growth_steps_it() {
        let base = fitted(&character());
        let mut slightly_taller = character();
        slightly_taller.max.y += 0.02;
        assert_eq!(fitted(&slightly_taller).texel, base.texel, "1% more height must not rescale the map");
        let mut much_taller = character();
        much_taller.max.y += 2.0;
        assert!(fitted(&much_taller).texel > base.texel * 1.19, "twice the size must step the scale up");
    }

    #[test]
    fn the_scale_is_a_rung_of_the_ladder() {
        let f = fitted(&character());
        let half = f.texel * SIZE as f32 / 2.0;
        let rung = half.log2() * STEP_PER_OCTAVE;
        assert!((rung - rung.round()).abs() < 1e-3, "window radius {half} is between rungs");
    }

    #[test]
    fn a_huge_ground_does_not_stretch_the_depth_range_without_limit() {
        let floor = Aabb::new(Vec3::new(-500.0, 0.0, -500.0), Vec3::new(500.0, 0.0, 500.0));
        let f = fit(TO_LIGHT, &character(), &floor, SIZE, BIAS).expect("a fit");
        let half = f.texel * SIZE as f32 / 2.0;
        assert!(f.depth_range < 2.0 * 1.0 + RECEIVER_REACH_RADII * half + 0.1, "range {}", f.depth_range);
    }

    #[test]
    fn a_light_straight_down_or_up_has_a_usable_basis() {
        for to_light in [Vec3::Y, Vec3::NEG_Y] {
            let f = fit(to_light, &character(), &ground(), SIZE, BIAS).expect("a fit");
            for c in character().corners() {
                let p = clip(&f, c);
                assert!(p.x.abs() <= 1.0 && p.y.abs() <= 1.0 && (0.0..=1.0).contains(&p.z), "{to_light}: {p}");
            }
        }
    }

    #[test]
    fn nothing_to_fit_is_none() {
        assert!(fit(TO_LIGHT, &Aabb::EMPTY, &ground(), SIZE, BIAS).is_none());
        assert!(fit(Vec3::ZERO, &character(), &ground(), SIZE, BIAS).is_none());
        assert!(fit(Vec3::NAN, &character(), &ground(), SIZE, BIAS).is_none());
        let broken = Aabb::new(Vec3::splat(f32::NAN), Vec3::ONE);
        assert!(fit(TO_LIGHT, &broken, &ground(), SIZE, BIAS).is_none());
    }

    #[test]
    fn with_no_receivers_the_depth_range_is_the_casters_alone() {
        let f = fit(TO_LIGHT, &character(), &Aabb::EMPTY, SIZE, BIAS).expect("a fit");
        let radius = character().radius();
        assert!((f.depth_range - 2.0 * (radius + BIAS)).abs() < 1e-4);
    }

    #[test]
    fn a_transformed_box_holds_the_transformed_corners() {
        let m = Mat4::from_rotation_y(0.7) * Mat4::from_translation(Vec3::X);
        let turned = character().transformed(&m);
        for c in character().corners() {
            let p = m.transform_point3(c);
            assert!(p.cmpge(turned.min - 1e-5).all() && p.cmple(turned.max + 1e-5).all());
        }
        assert!(Aabb::EMPTY.transformed(&m).is_empty());
    }

    #[test]
    fn unions_ignore_empty_boxes() {
        let mut b = Aabb::EMPTY;
        b.union(&Aabb::EMPTY);
        assert!(b.is_empty());
        b.union(&character());
        assert_eq!(b, character());
    }
}
