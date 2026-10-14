//! Measuring a built head's front silhouette: the numbers a face shape is designed to and checked against.
//!
//! A preset is designed from a written description ("a soft round U with a short lower face"), so the description is
//! turned into numbers first (width to height, chin angle, where the eye line sits) and the preset is fitted until the
//! *built head* measures them, not just until the outline parameters look right: the plausible space
//! ([`super::super::outline::plausible`]) and the chin stretch both move the mesh away from the raw superellipse.
//!
//! Everything is in head-local metres (the mesh's own height minus the head centre's) and reads the front view: the
//! half-width at a height is the largest `|x|` of the surface there.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "row counts are a few hundred; heights are metres"
)]

use omuzaki_core::mesh::MeshData;

/// The sampling step down the silhouette, metres. Half a millimetre is far below a face shape's differences (a
/// preset's silhouette ratios differ by percent, i.e. millimetres to centimetres) and well above the mesh's faceting.
const STEP: f64 = 0.0005;
/// The chin-angle circle's radius as a fraction of the face's half-width, as the head-quality tests and
/// `outline::plausible` measure it, so the three agree.
const TIP_SCALE: f64 = 0.25;

/// A head's front silhouette: the half-width at every [`STEP`] from the lowest point up.
#[derive(Clone, Debug, PartialEq)]
pub struct Silhouette {
    /// Head-local height of the lowest point.
    bottom: f64,
    /// Half-widths from `bottom` up, one per step; `NaN`-free, zero where the surface does not reach.
    half_widths: Vec<f64>,
}

/// The measured numbers of one silhouette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SilhouetteMetrics {
    /// The widest the head is, end to end, metres.
    pub head_width: f64,
    /// Crown to chin, metres.
    pub head_height: f64,
    /// Head-local height of the widest point.
    pub widest_y: f64,
    /// Head-local height of the chin (the lowest point).
    pub chin_y: f64,
    /// The face's width at the eye line (the outline's top), metres.
    pub face_width: f64,
    /// Eye line to chin, metres.
    pub face_length: f64,
    /// The jaw's width, a quarter of the face's length above the chin, metres: how narrow the chin end is.
    pub jaw_width: f64,
    /// The face's width half way down the lower face over its width at the eye line: 1 is a box, lower tapers.
    pub mid_face_fullness: f64,
    /// The chin's front angle in degrees: 180 is flat, smaller is pointier.
    pub chin_angle_deg: f64,
}

impl SilhouetteMetrics {
    /// Head width over head height.
    pub fn width_to_height(&self) -> f64 {
        self.head_width / self.head_height
    }

    /// The lower face's length over its width at the eye line: bigger is a longer face.
    pub fn face_length_to_width(&self) -> f64 {
        self.face_length / self.face_width
    }

    /// Where the eye line sits on the head, as a fraction of the head's height measured up from the chin.
    pub fn eye_line_fraction(&self) -> f64 {
        self.face_length / self.head_height
    }
}

impl Silhouette {
    /// The silhouette of `mesh`, whose head centre is at height `origin_y`. `None` for a mesh with no height.
    pub fn of(mesh: &MeshData, origin_y: f64) -> Option<Self> {
        let heights = mesh.positions.iter().map(|p| f64::from(p.y) - origin_y);
        let bottom = heights.clone().fold(f64::INFINITY, f64::min);
        let top = heights.fold(f64::NEG_INFINITY, f64::max);
        if !top.is_finite() || !bottom.is_finite() || top <= bottom {
            return None;
        }
        let rows = ((top - bottom) / STEP).floor() as usize + 1;
        let mut half_widths = vec![0.0_f64; rows];
        let point = |i: u32| {
            let p = mesh.positions[i as usize];
            (f64::from(p.y) - origin_y, f64::from(p.x).abs())
        };
        for tri in mesh.indices.as_chunks::<3>().0 {
            for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                let ((ya, xa), (yb, xb)) = (point(a), point(b));
                let (low, high) = if ya <= yb { (ya, yb) } else { (yb, ya) };
                let first = (((low - bottom) / STEP).ceil() as usize).min(rows - 1);
                let last = (((high - bottom) / STEP).floor() as usize).min(rows - 1);
                let span = yb - ya;
                for (row, width) in half_widths.iter_mut().enumerate().take(last + 1).skip(first) {
                    let y = bottom + row as f64 * STEP;
                    let t = if span.abs() < f64::EPSILON { 0.0 } else { ((y - ya) / span).clamp(0.0, 1.0) };
                    *width = width.max(xa + (xb - xa) * t);
                }
            }
        }
        Some(Self { bottom, half_widths })
    }

    /// Half the width at head-local height `y` (linear between steps; 0 outside the head).
    pub fn half_width_at(&self, y: f64) -> f64 {
        let at = (y - self.bottom) / STEP;
        if at < 0.0 || at > (self.half_widths.len() - 1) as f64 {
            return 0.0;
        }
        let low = at.floor() as usize;
        let high = (low + 1).min(self.half_widths.len() - 1);
        let t = at - low as f64;
        self.half_widths[low] * (1.0 - t) + self.half_widths[high] * t
    }

    /// The numbers of this silhouette, with the eye line at head-local height `eye_line`.
    pub fn metrics(&self, eye_line: f64) -> SilhouetteMetrics {
        let top = self.bottom + (self.half_widths.len() - 1) as f64 * STEP;
        let (widest_row, widest) = self
            .half_widths
            .iter()
            .enumerate()
            .fold((0, 0.0_f64), |best, (row, &w)| if w > best.1 { (row, w) } else { best });
        let face_half = self.half_width_at(eye_line);
        SilhouetteMetrics {
            head_width: 2.0 * widest,
            head_height: top - self.bottom,
            widest_y: self.bottom + widest_row as f64 * STEP,
            chin_y: self.bottom,
            face_width: 2.0 * face_half,
            face_length: eye_line - self.bottom,
            jaw_width: 2.0 * self.half_width_at(0.75 * self.bottom + 0.25 * eye_line),
            mid_face_fullness: self.half_width_at(f64::midpoint(eye_line, self.bottom)) / face_half,
            chin_angle_deg: self.chin_angle(TIP_SCALE * face_half),
        }
    }

    /// The angle at the chin between the two points where the silhouette crosses the circle of `radius` round the
    /// chin, degrees.
    fn chin_angle(&self, radius: f64) -> f64 {
        let crossing = self.half_widths.iter().enumerate().find_map(|(row, &w)| {
            let rise = row as f64 * STEP;
            (w.hypot(rise) >= radius).then_some((w, rise))
        });
        crossing.map_or(180.0, |(w, rise)| 2.0 * w.atan2(rise).to_degrees())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_core::glam::Vec3;

    /// A regular octahedron of radius 1 (head-local origin at its centre): the half-width at height `y` is `1 - |y|`.
    fn octahedron() -> MeshData {
        let positions = vec![
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
        ];
        let indices = vec![4, 0, 2, 4, 2, 1, 4, 1, 3, 4, 3, 0, 5, 2, 0, 5, 1, 2, 5, 3, 1, 5, 0, 3];
        MeshData { normals: vec![Vec3::Y; 6], positions, indices, ..MeshData::default() }
    }

    #[test]
    fn a_silhouette_reads_the_widest_x_at_each_height() {
        let s = Silhouette::of(&octahedron(), 0.0).unwrap();
        for y in [-0.9, -0.5, 0.0, 0.3, 0.8] {
            assert!((s.half_width_at(y) - (1.0 - f64::abs(y))).abs() < 1e-9, "{y}");
        }
        assert_eq!(s.half_width_at(-1.5), 0.0);
        assert_eq!(s.half_width_at(1.5), 0.0);
    }

    #[test]
    fn the_origin_shifts_every_height() {
        let s = Silhouette::of(&octahedron(), 0.25).unwrap();
        assert!((s.half_width_at(-0.25) - 1.0).abs() < 1e-9, "the widest point is 0.25 below the new origin");
    }

    #[test]
    fn metrics_report_the_ratios_of_the_shape() {
        let m = Silhouette::of(&octahedron(), 0.0).unwrap().metrics(0.5);
        assert!((m.head_width - 2.0).abs() < 1e-9 && (m.head_height - 2.0).abs() < 1e-9);
        assert!(m.widest_y.abs() < 1e-3 && (m.chin_y + 1.0).abs() < 1e-9);
        assert!((m.face_width - 1.0).abs() < 1e-9 && (m.face_length - 1.5).abs() < 1e-9);
        assert!((m.width_to_height() - 1.0).abs() < 1e-9 && (m.face_length_to_width() - 1.5).abs() < 1e-9);
        assert!((m.eye_line_fraction() - 0.75).abs() < 1e-9);
        // A straight 45 degree cone below the widest point: a right-angled tip.
        assert!((m.chin_angle_deg - 90.0).abs() < 0.2, "{}", m.chin_angle_deg);
    }

    #[test]
    fn a_mesh_with_no_height_has_no_silhouette() {
        assert!(Silhouette::of(&MeshData::default(), 0.0).is_none());
    }
}
