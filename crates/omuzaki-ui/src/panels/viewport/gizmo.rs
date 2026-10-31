//! Move, rotate and scale handles (#2590): drag the selected part's attachment in the viewport.
//!
//! The handles sit in the middle of the part as posed and are drawn along the world axes: three arrows to move, three
//! rings to turn, a square to scale uniformly. A drag is measured from where it started, never added up frame by frame, so the part
//! follows the pointer exactly and a drag back to the start gives back the start. What a drag gives is the
//! attachment's new offset ([`Drag::offset`]); the viewport writes it with `part.set_attach` inside one transaction,
//! so the whole drag is one undo step and the same op an agent calls. Ctrl snaps: 5 mm, 15 degrees, 0.05 of scale.

use egui::{Color32, Painter, Pos2, Stroke, Vec2, pos2};
use omuzaki_char::accessory::attach::{euler_xyz_deg, offset_rotation};
use omuzaki_char_api::{Attach, AttachOffset};
use omuzaki_core::glam::{DMat4, DVec3, Mat4, Vec3};

use super::camera::{FOV_DEG, OrbitCamera};

/// How long an arrow is on screen, in points.
const ARROW: f32 = 72.0;
/// How close the pointer must be to a handle to take it, in points.
const REACH: f32 = 7.0;
/// The scale square's half size, in points.
const SQUARE: f32 = 7.0;
/// Points on a ring.
const RING_STEPS: usize = 48;
/// Points of drag that double the scale.
const SCALE_PER_DOUBLING: f32 = 140.0;
/// What Ctrl snaps to.
const SNAP_METRES: f32 = 0.005;
const SNAP_DEGREES: f32 = 15.0;
const SNAP_SCALE: f64 = 0.05;

/// What the handles do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// Arrows that move along an axis.
    #[default]
    Move,
    /// Rings that turn about an axis.
    Rotate,
    /// A square that scales uniformly.
    Scale,
}

/// One handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// The arrow along world axis 0, 1 or 2 (x, y, z).
    Arrow(usize),
    /// The ring about world axis 0, 1 or 2.
    Ring(usize),
    /// The uniform scale square.
    Square,
}

/// The handles as they land in the view: the attach point and each axis's arrow end and ring, in points.
#[derive(Clone, Debug)]
pub struct Screen {
    /// The attach point, in the world.
    pub origin: Vec3,
    /// Where it lands.
    pub centre: Pos2,
    /// Each arrow's tip; `None` for an axis pointing at the camera, whose arrow would be a dot.
    pub tips: [Option<Pos2>; 3],
    /// Each ring's outline.
    pub rings: [Vec<Pos2>; 3],
    /// Metres the world moves per point of the view at the attach point's depth.
    pub metres_per_point: f32,
    /// The world length an arrow stands for.
    pub length: f32,
    /// Toward the camera from the attach point.
    pub toward_eye: Vec3,
}

impl Screen {
    /// The handles at `origin` (world) seen by `camera` in a view of `size` points; `None` when it is behind the camera.
    pub fn of(camera: &OrbitCamera, size: [f32; 2], origin: Vec3) -> Option<Self> {
        let centre = camera.project(origin, size)?;
        let depth = (origin - camera.eye()).length().max(1e-3);
        let metres_per_point = 2.0 * depth * (FOV_DEG.to_radians() * 0.5).tan() / size[1].max(1.0);
        let length = ARROW * metres_per_point;
        let axes = [Vec3::X, Vec3::Y, Vec3::Z];
        let tips = axes.map(|axis| {
            let tip = camera.project(origin + axis * length, size).map(|[x, y]| pos2(x, y))?;
            ((tip - pos2(centre[0], centre[1])).length() > REACH).then_some(tip)
        });
        let rings = axes.map(|axis| {
            let (u, v) = (axis.any_orthonormal_vector(), axis.cross(axis.any_orthonormal_vector()));
            (0..=RING_STEPS)
                .filter_map(|i| {
                    #[allow(clippy::cast_precision_loss, reason = "a few dozen steps")]
                    let a = i as f32 / RING_STEPS as f32 * std::f32::consts::TAU;
                    camera.project(origin + (u * a.cos() + v * a.sin()) * length, size).map(|[x, y]| pos2(x, y))
                })
                .collect()
        });
        Some(Self {
            origin,
            centre: pos2(centre[0], centre[1]),
            tips,
            rings,
            metres_per_point,
            length,
            toward_eye: (camera.eye() - origin).normalize_or_zero(),
        })
    }

    /// The handle of `tool` under `pointer`, if any: the nearest within reach.
    pub fn handle_at(&self, tool: Tool, pointer: Pos2) -> Option<Handle> {
        let mut best: Option<(f32, Handle)> = None;
        let mut offer = |distance: f32, handle| {
            if distance <= REACH && best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, handle));
            }
        };
        match tool {
            Tool::Move => {
                for (axis, tip) in self.tips.iter().enumerate() {
                    if let Some(tip) = tip {
                        offer(to_segment(pointer, self.centre, *tip), Handle::Arrow(axis));
                    }
                }
            }
            Tool::Rotate => {
                for (axis, ring) in self.rings.iter().enumerate() {
                    let near = ring.windows(2).map(|w| to_segment(pointer, w[0], w[1])).fold(f32::MAX, f32::min);
                    offer(near, Handle::Ring(axis));
                }
            }
            Tool::Scale => {
                let d = pointer - self.centre;
                offer((d.x.abs().max(d.y.abs()) - SQUARE).max(0.0), Handle::Square);
            }
        }
        best.map(|(_, handle)| handle)
    }

    /// Draws the handles of `tool`, `hot` brighter, with the view's top left at `corner` of the painter.
    pub fn draw(&self, painter: &Painter, tool: Tool, hot: Option<Handle>, corner: Vec2) {
        let colour = |axis: usize, lit: bool| {
            let base =
                [Color32::from_rgb(232, 80, 80), Color32::from_rgb(110, 200, 90), Color32::from_rgb(80, 140, 240)]
                    [axis.min(2)];
            if lit { base.gamma_multiply(1.4) } else { base }
        };
        let width = |lit: bool| if lit { 3.5 } else { 2.0 };
        match tool {
            Tool::Move => {
                for (axis, tip) in self.tips.iter().enumerate() {
                    let Some(tip) = tip else { continue };
                    let lit = hot == Some(Handle::Arrow(axis));
                    let stroke = Stroke::new(width(lit), colour(axis, lit));
                    painter.line_segment([self.centre + corner, *tip + corner], stroke);
                    painter.circle_filled(*tip + corner, if lit { 5.5 } else { 4.5 }, colour(axis, lit));
                }
            }
            Tool::Rotate => {
                for (axis, ring) in self.rings.iter().enumerate() {
                    let lit = hot == Some(Handle::Ring(axis));
                    let ring = ring.iter().map(|p| *p + corner).collect();
                    painter.line(ring, Stroke::new(width(lit), colour(axis, lit)));
                }
            }
            Tool::Scale => {
                let lit = hot == Some(Handle::Square);
                let fill = if lit { Color32::from_gray(250) } else { Color32::from_gray(220) };
                let square = egui::Rect::from_center_size(self.centre + corner, Vec2::splat(SQUARE * 2.0));
                painter.rect_filled(square, 2.0, fill);
                painter.rect_stroke(square, 2.0, Stroke::new(1.0, Color32::from_gray(40)), egui::StrokeKind::Outside);
            }
        }
        painter.circle_filled(self.centre + corner, 2.5, Color32::from_gray(240));
    }
}

/// A drag of one handle, from where it started.
#[derive(Clone, Debug)]
pub struct Drag {
    /// The handle taken.
    pub handle: Handle,
    /// Where the pointer was when it was taken.
    pub start: Pos2,
    /// The handles as they were then.
    pub screen: Screen,
    /// The offset as it was then.
    pub from: AttachOffset,
    /// The frame the offset is in, as posed (the bone's or the anchor's).
    pub target: Mat4,
}

impl Drag {
    /// The offset the drag gives with the pointer at `pointer`; `snap` (Ctrl) snaps the move, the turn or the scale.
    ///
    /// The drag is a move of the world about the handles' point (the part's middle): a shift along an axis, a turn
    /// about an axis through it, or a scale about it. It is carried into the target's frame and put before the offset
    /// the drag started from, so the part turns and scales about its own middle, not about the point it is fixed to.
    pub fn offset(&self, pointer: Pos2, snap: bool) -> AttachOffset {
        let pivot = self.screen.origin.as_dvec3();
        let about = |m: DMat4| DMat4::from_translation(pivot) * m * DMat4::from_translation(-pivot);
        let world = match self.handle {
            Handle::Arrow(axis) => {
                let Some(tip) = self.screen.tips.get(axis).copied().flatten() else { return self.from };
                let along = tip - self.screen.centre;
                let points = (pointer - self.start).dot(along.normalized());
                let mut metres = points * self.screen.length / along.length().max(1e-3);
                if snap {
                    metres = (metres / SNAP_METRES).round() * SNAP_METRES;
                }
                DMat4::from_translation((unit(axis) * metres).as_dvec3())
            }
            Handle::Ring(axis) => {
                let (a, b) = (self.start - self.screen.centre, pointer - self.screen.centre);
                // Screen y grows down: flip it so a turn the person sees as anticlockwise is positive.
                let mut degrees = ((-b.y).atan2(b.x) - (-a.y).atan2(a.x)).to_degrees();
                degrees = (degrees + 540.0).rem_euclid(360.0) - 180.0;
                if unit(axis).dot(self.screen.toward_eye) < 0.0 {
                    degrees = -degrees;
                }
                if snap {
                    degrees = (degrees / SNAP_DEGREES).round() * SNAP_DEGREES;
                }
                about(DMat4::from_axis_angle(unit(axis).as_dvec3(), f64::from(degrees.to_radians())))
            }
            Handle::Square => {
                let d = pointer - self.start;
                let factor = f64::from(((d.x - d.y) / SCALE_PER_DOUBLING).exp2());
                let mut scale = (self.from.scale * factor).max(1e-3);
                if snap {
                    scale = ((scale / SNAP_SCALE).round() * SNAP_SCALE).max(SNAP_SCALE);
                }
                about(DMat4::from_scale(DVec3::splat(scale / self.from.scale.max(1e-9))))
            }
        };
        let target = self.target.as_dmat4();
        let from = DMat4::from_scale_rotation_translation(
            DVec3::splat(self.from.scale),
            offset_rotation(self.from.rotation_deg),
            DVec3::from_array(self.from.translation),
        );
        let (scale, rotation, translation) = (target.inverse() * world * target * from).to_scale_rotation_translation();
        AttachOffset {
            translation: translation.to_array(),
            rotation_deg: euler_xyz_deg(rotation.normalize()),
            scale: (scale.x + scale.y + scale.z) / 3.0,
        }
    }
}

/// `attach` with its offset replaced by `offset`.
pub fn with_offset(attach: &Attach, offset: AttachOffset) -> Attach {
    let mut out = attach.clone();
    match &mut out {
        Attach::Bone { offset: o, .. } | Attach::Anchor { offset: o, .. } => *o = offset,
    }
    out
}

/// World axis `axis` (0, 1, 2).
fn unit(axis: usize) -> Vec3 {
    [Vec3::X, Vec3::Y, Vec3::Z].get(axis).copied().unwrap_or(Vec3::X)
}

/// How far `p` is from the segment `a`–`b`.
fn to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (OrbitCamera, Screen) {
        let camera = OrbitCamera { target: Vec3::new(0.0, 1.5, 0.0), yaw: 30.0, pitch: 10.0, distance: 2.0 };
        let screen = Screen::of(&camera, [800.0, 600.0], Vec3::new(0.05, 1.55, 0.02)).unwrap_or_else(|| panic!("seen"));
        (camera, screen)
    }

    fn drag(screen: &Screen, handle: Handle, target: Mat4) -> Drag {
        let start = match handle {
            Handle::Arrow(axis) => screen.tips[axis].unwrap_or_else(|| panic!("an arrow")),
            Handle::Ring(axis) => screen.rings[axis][5],
            Handle::Square => screen.centre,
        };
        Drag { handle, start, screen: screen.clone(), from: AttachOffset::default(), target }
    }

    #[test]
    fn each_handle_is_found_under_the_pointer_and_nothing_away_from_them() {
        let (_, screen) = setup();
        for axis in 0..3 {
            let tip = screen.tips[axis].unwrap_or_else(|| panic!("axis {axis}"));
            let middle = screen.centre + (tip - screen.centre) * 0.6;
            assert_eq!(screen.handle_at(Tool::Move, middle), Some(Handle::Arrow(axis)));
        }
        assert_eq!(screen.handle_at(Tool::Rotate, screen.rings[1][7]), Some(Handle::Ring(1)));
        assert_eq!(screen.handle_at(Tool::Scale, screen.centre + Vec2::splat(3.0)), Some(Handle::Square));
        assert_eq!(screen.handle_at(Tool::Move, screen.centre + Vec2::new(200.0, 200.0)), None);
    }

    #[test]
    fn dragging_an_arrow_by_its_own_length_moves_the_arrows_length_in_the_targets_frame() {
        let (_, screen) = setup();
        // The target turned a quarter about y: world +x is the frame's +z.
        let target = Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let d = drag(&screen, Handle::Arrow(0), target);
        let tip = screen.tips[0].unwrap_or_else(|| panic!("x"));
        let moved = d.offset(d.start + (tip - screen.centre), false);
        let t = DVec3::from_array(moved.translation);
        assert!((t - DVec3::new(0.0, 0.0, f64::from(screen.length))).length() < 1e-4, "{t}");
        // Back to the start gives the start back.
        assert_eq!(d.offset(d.start, false).translation, [0.0; 3]);
        // Snapped, it lands on whole 5 mm steps.
        let snapped = DVec3::from_array(d.offset(d.start + (tip - screen.centre) * 0.37, true).translation);
        let steps = snapped.length() / f64::from(SNAP_METRES);
        assert!((steps - steps.round()).abs() < 1e-3, "{snapped}");
    }

    #[test]
    fn a_ring_turns_about_its_axis_and_snaps_to_fifteen_degrees() {
        let (_, screen) = setup();
        let d = drag(&screen, Handle::Ring(1), Mat4::IDENTITY);
        let later = screen.rings[1][9];
        let turned = d.offset(later, true);
        let [x, y, z] = turned.rotation_deg;
        assert!(x.abs() < 1e-6 && z.abs() < 1e-6, "a turn about y only: {:?}", turned.rotation_deg);
        assert!(y.abs() > 1.0 && ((y / 15.0) - (y / 15.0).round()).abs() < 1e-6, "{y}");
    }

    #[test]
    fn a_turn_and_a_scale_keep_the_parts_middle_where_it_is() {
        let (_, screen) = setup();
        let target =
            Mat4::from_rotation_translation(omuzaki_core::glam::Quat::from_rotation_x(0.3), Vec3::new(0.0, 1.45, 0.01));
        let from = AttachOffset { translation: [0.02, 0.08, -0.01], rotation_deg: [10.0, -20.0, 5.0], scale: 1.2 };
        let placed = |offset: AttachOffset| {
            target.as_dmat4()
                * DMat4::from_scale_rotation_translation(
                    DVec3::splat(offset.scale),
                    offset_rotation(offset.rotation_deg),
                    DVec3::from_array(offset.translation),
                )
        };
        // The point of the part that sits at the handles.
        let local = placed(from).inverse().transform_point3(screen.origin.as_dvec3());
        for (handle, to) in
            [(Handle::Ring(0), screen.rings[0][11]), (Handle::Square, screen.centre + Vec2::new(60.0, 0.0))]
        {
            let mut d = drag(&screen, handle, target);
            d.from = from;
            let moved = d.offset(to, false);
            let middle = placed(moved).transform_point3(local);
            assert!(middle.distance(screen.origin.as_dvec3()) < 1e-5, "{handle:?}: the middle moved to {middle}");
            assert_ne!(moved, from, "{handle:?} changed the offset");
        }
    }

    #[test]
    fn the_square_doubles_the_scale_per_its_drag_and_never_reaches_zero() {
        let (_, screen) = setup();
        let d = drag(&screen, Handle::Square, Mat4::IDENTITY);
        let bigger = d.offset(d.start + Vec2::new(SCALE_PER_DOUBLING, 0.0), false);
        assert!((bigger.scale - 2.0).abs() < 1e-6, "{}", bigger.scale);
        let tiny = d.offset(d.start + Vec2::new(-5000.0, 5000.0), false);
        assert!(tiny.scale > 0.0);
    }
}
