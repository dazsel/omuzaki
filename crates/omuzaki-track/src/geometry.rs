//! Head pose from landmarks: a port of MediaPipe's face-geometry pipeline (`geometry_pipeline.cc`,
//! `procrustes_solver.cc`) for the face-landmark input source.
//!
//! The landmarks are screen coordinates with a relative depth. The pipeline unprojects them through a virtual
//! pinhole camera (63 degrees vertical field of view, as MediaPipe's default environment), recovering the metric
//! scale with two Procrustes fits against the canonical face, then fits the final rigid pose. The result maps the
//! canonical face (centimetres, x right, y up, z toward the viewer) into camera space, with a uniform scale in the
//! rotation block.

use nalgebra::{Matrix3, Matrix4, Vector3};

use crate::landmarks::Landmark;
use crate::metadata::{GeometryMetadata, MESH_LANDMARKS};

/// Vertical field of view of the virtual camera, degrees.
const VERTICAL_FOV_DEGREES: f64 = 63.0;
/// Near plane distance in centimetres; also the depth the projected landmarks are measured against.
const NEAR: f64 = 1.0;
/// Fits below these are treated as degenerate, as in MediaPipe.
const EPS: f64 = 1e-9;
/// Landmark sets spanning less than this (normalised units) are too small to fit.
const MIN_EXTENT: f64 = 1e-3;

type Points = Vec<Vector3<f64>>;

/// Weighted similarity fit of `target ~ s * R * source + t` (Akca, "Generalized Procrustes analysis", section 2.4).
/// Returns the 4x4 matrix `[sR | t]`, or `None` when the fit is degenerate (no weight, a collapsed point set, a
/// failed decomposition or a non-positive scale). `centered` is scratch space, grown on the first call only.
fn procrustes(
    source: &[Vector3<f64>],
    target: &[Vector3<f64>],
    weights: &[f64],
    centered: &mut Points,
) -> Option<Matrix4<f64>> {
    let total: f64 = weights.iter().sum();
    if total <= EPS {
        return None;
    }
    let centroid = source.iter().zip(weights).map(|(p, w)| p * *w).sum::<Vector3<f64>>() / total;
    centered.clear();
    centered.extend(source.iter().map(|p| p - centroid));
    // sqrt-weighted point sets, as in the paper with Q = diag(sqrt(w)).
    let design: Matrix3<f64> =
        target.iter().zip(centered.iter()).zip(weights).map(|((t, c), w)| (t * *w) * c.transpose()).sum();
    if design.norm() <= EPS {
        return None;
    }
    let svd = design.svd(true, true);
    let (Some(mut u), Some(v_t)) = (svd.u, svd.v_t) else {
        return None;
    };
    if u.determinant() * v_t.determinant() < 0.0 {
        u.set_column(2, &(-u.column(2)));
    }
    let rotation = u * v_t;
    let numerator: f64 = centered.iter().zip(target).zip(weights).map(|((c, t), w)| (rotation * c).dot(t) * w).sum();
    let denominator: f64 = centered.iter().zip(source).zip(weights).map(|((c, s), w)| c.dot(s) * w).sum();
    if denominator <= EPS || numerator / denominator <= EPS {
        return None;
    }
    let rotation_scale = rotation * (numerator / denominator);
    let translation =
        target.iter().zip(source).zip(weights).map(|((t, s), w)| (t - rotation_scale * s) * *w).sum::<Vector3<f64>>()
            / total;
    let mut m = Matrix4::identity();
    m.fixed_view_mut::<3, 3>(0, 0).copy_from(&rotation_scale);
    m.fixed_view_mut::<3, 1>(0, 3).copy_from(&translation);
    Some(m)
}

/// Face pose estimator: the canonical face plus the scratch buffers a fit needs, so a frame allocates nothing.
#[derive(Debug, Clone)]
pub struct PoseEstimator {
    canonical: Points,
    weights: Vec<f64>,
    /// The landmarks on the virtual camera's near plane.
    screen: Points,
    /// The point set handed to a fit.
    shape: Points,
    /// Procrustes scratch.
    centered: Points,
}

impl PoseEstimator {
    /// Builds an estimator from the bundle's canonical mesh.
    pub fn new(metadata: &GeometryMetadata) -> Self {
        Self {
            canonical: metadata
                .canonical
                .iter()
                .map(|p| Vector3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2])))
                .collect(),
            weights: metadata.weights.iter().map(|&w| f64::from(w)).collect(),
            screen: Vec::with_capacity(MESH_LANDMARKS),
            shape: Vec::with_capacity(MESH_LANDMARKS),
            centered: Vec::with_capacity(MESH_LANDMARKS),
        }
    }

    /// The canonical-to-camera transform for the first 468 `landmarks` of a `frame_w` x `frame_h` frame,
    /// column-major, translation in centimetres (elements 12, 13, 14).
    ///
    /// Returns `None` when there are too few landmarks, they span almost nothing, or a fit degenerates: no usable
    /// face in them.
    #[allow(clippy::cast_precision_loss)] // frame sizes are far below f64's 53 bits
    pub fn estimate(&mut self, landmarks: &[Landmark], frame_w: usize, frame_h: usize) -> Option<[f32; 16]> {
        if landmarks.len() < MESH_LANDMARKS || extent(&landmarks[..MESH_LANDMARKS]) <= MIN_EXTENT {
            return None;
        }
        // Frustum of the virtual camera at the near plane.
        let height_at_near = 2.0 * NEAR * (0.5 * VERTICAL_FOV_DEGREES.to_radians()).tan();
        let width_at_near = frame_w as f64 * height_at_near / frame_h as f64;
        let (left, bottom) = (-0.5 * width_at_near, -0.5 * height_at_near);
        // Project x, y onto the near plane (image y points down, the camera's up), and scale z like x.
        self.screen.clear();
        self.screen.extend(landmarks[..MESH_LANDMARKS].iter().map(|p| {
            let (x, y, z) = (f64::from(p[0]), f64::from(p[1]), f64::from(p[2]));
            Vector3::new(x * width_at_near + left, (1.0 - y) * height_at_near + bottom, z * width_at_near)
        }));
        let depth_offset = self.screen.iter().map(|p| p.z).sum::<f64>() / self.screen.len() as f64;

        // First pass on the projected landmarks (z flipped to right-handed), whose xy cannot yet be unprojected.
        self.shape.clear();
        self.shape.extend(self.screen.iter().map(|p| Vector3::new(p.x, p.y, -p.z)));
        let first = self.fit_scale()?;
        self.unproject(depth_offset, first);
        let second = self.fit_scale()?;
        self.unproject(depth_offset, first * second);
        procrustes(&self.canonical, &self.shape, &self.weights, &mut self.centered).map(|m| column_major(&m))
    }

    /// The scale of the similarity that maps the canonical face onto `self.shape`.
    fn fit_scale(&mut self) -> Option<f64> {
        Some(procrustes(&self.canonical, &self.shape, &self.weights, &mut self.centered)?.column(0).norm())
    }

    /// Fills `self.shape` with `self.screen` moved out to metric depth for `scale`.
    fn unproject(&mut self, depth_offset: f64, scale: f64) {
        self.shape.clear();
        self.shape.extend(self.screen.iter().map(|p| {
            let z = (p.z - depth_offset + NEAR) / scale;
            Vector3::new(p.x * z / NEAR, p.y * z / NEAR, -z) // -z: screen space is left-handed, metric space right-handed
        }));
    }
}

/// Larger side of the landmarks' xy bounding box, i.e. how big the face is in normalised units.
fn extent(points: &[Landmark]) -> f64 {
    let (mut min, mut max) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in points {
        for a in 0..2 {
            min[a] = min[a].min(f64::from(p[a]));
            max[a] = max[a].max(f64::from(p[a]));
        }
    }
    (max[0] - min[0]).max(max[1] - min[1])
}

#[allow(clippy::cast_possible_truncation)] // the output format is f32
fn column_major(m: &Matrix4<f64>) -> [f32; 16] {
    let mut out = [0.0; 16];
    for (o, v) in out.iter_mut().zip(m.iter()) {
        *o = *v as f32;
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::many_single_char_names)] // test fixtures
    use super::*;
    use nalgebra::{Rotation3, Vector3};

    /// A fake canonical face: a deterministic cloud with depth, weights on every point.
    fn canonical() -> GeometryMetadata {
        let canonical: Vec<[f32; 3]> = (0..MESH_LANDMARKS)
            .map(|i| {
                let t = i as f32;
                [8.0 * (t * 0.37).sin(), 10.0 * (t * 0.11).cos(), 4.0 * (t * 0.23).sin() + 3.0 * (t * 0.05).cos()]
            })
            .collect();
        GeometryMetadata { canonical, weights: vec![1.0; MESH_LANDMARKS] }
    }

    #[test]
    fn procrustes_recovers_a_known_similarity() {
        let pts: Points = (0..40)
            .map(f64::from)
            .map(|i| Vector3::new((i * 0.7).sin(), (i * 0.3).cos() * 2.0, (i * 0.17).sin()))
            .collect();
        let rot = Rotation3::from_euler_angles(0.3, -0.5, 0.2);
        let (s, t) = (2.5, Vector3::new(1.0, -2.0, 3.0));
        let target: Points = pts.iter().map(|p| rot * p * s + t).collect();
        let mut scratch = Vec::new();
        let m = procrustes(&pts, &target, &vec![1.0; pts.len()], &mut scratch).unwrap();
        let expected = rot.matrix() * s;
        assert!((m.fixed_view::<3, 3>(0, 0) - expected).norm() < 1e-9);
        assert!((m.fixed_view::<3, 1>(0, 3) - t).norm() < 1e-9);
        assert!(procrustes(&pts, &target, &vec![0.0; pts.len()], &mut scratch).is_none());
    }

    /// Projects the canonical face through the same virtual camera the estimator assumes, at a known pose, and
    /// checks the pose comes back.
    #[test]
    fn recovers_a_pose_projected_through_the_virtual_camera() {
        let meta = canonical();
        let mut est = PoseEstimator::new(&meta);
        let (fw, fh) = (640usize, 480usize);
        let rot = Rotation3::from_euler_angles(0.1, 0.35, -0.2);
        let t = Vector3::new(1.5, -1.0, -45.0); // 45 cm in front of the camera (-z is forward in metric space)
        let height_at_near = 2.0 * NEAR * (0.5 * VERTICAL_FOV_DEGREES.to_radians()).tan();
        let width_at_near = fw as f64 / fh as f64 * height_at_near;
        let landmarks: Vec<Landmark> = meta
            .canonical
            .iter()
            .map(|p| {
                let m = rot * Vector3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2])) + t;
                let depth = -m.z; // distance in front of the camera
                // Perspective divide onto the near plane, then to normalised image coordinates (y down).
                let (nx, ny) = (m.x * NEAR / depth, m.y * NEAR / depth);
                let x = (nx + 0.5 * width_at_near) / width_at_near;
                let y = 1.0 - (ny + 0.5 * height_at_near) / height_at_near;
                // Relative depth in near-plane units (weak perspective: the same 1 / mean-depth factor as x and y).
                let z = (depth - 45.0) / 45.0 / width_at_near;
                [x as f32, y as f32, z as f32]
            })
            .collect();
        let pose = est.estimate(&landmarks, fw, fh).unwrap();
        // The rotation block should match `rot` up to the metric scale (canonical units are already cm: scale ~ 1).
        let scale = Vector3::new(pose[0], pose[1], pose[2]).norm();
        assert!((scale - 1.0).abs() < 0.05, "scale {scale}");
        for row in 0..3 {
            for col in 0..3 {
                assert!(
                    (f64::from(pose[col * 4 + row]) / f64::from(scale) - rot[(row, col)]).abs() < 0.02,
                    "r{row}{col}"
                );
            }
        }
        assert!((f64::from(pose[14]) - t.z).abs() < 3.0, "depth {}", pose[14]);
        assert_eq!((pose[3], pose[15]), (0.0, 1.0), "the bottom row of an affine matrix");
    }

    #[test]
    fn rejects_too_few_or_collapsed_landmarks() {
        let mut est = PoseEstimator::new(&canonical());
        assert!(est.estimate(&[[0.5, 0.5, 0.0]; 10], 640, 480).is_none());
        assert!(est.estimate(&[[0.5, 0.5, 0.0]; MESH_LANDMARKS], 640, 480).is_none());
    }
}
