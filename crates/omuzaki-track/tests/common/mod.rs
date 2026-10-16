//! Shared fixtures: a synthetic canonical face, hand-encoded geometry metadata, faces projected at known poses, and
//! scripted sessions that replay the models' raw outputs. No model file, image or private material is involved.
//!
//! Used by `tests/pipeline_fake.rs`, `tests/alloc.rs` and `benches/pipeline.rs`, each of which uses its own part.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use nalgebra::{Rotation3, Vector3};
use omuzaki_ai_api::{DType, RunError, Session, SessionInfo, Signature, TensorBuf, TensorRef};
use omuzaki_ai_mock::FakeSession;
use omuzaki_track::{
    FaceGraphs, GeometryMetadata, NormRect, RgbFrame, blendshapes_signature, detector_signature, landmarks_signature,
};

/// Frames are square so the detector's letterbox and the landmark crop add no padding, which keeps the scripted
/// outputs easy to invert.
pub const SIDE: u32 = 256;
/// Landmarks the model predicts.
pub const LANDMARKS: usize = 478;
/// The mesh part of them, which the geometry fit reads.
pub const MESH: usize = 468;
/// The ROI the scripted detector's face turns into: centred, 0.4 of the frame, grown by the detector's 1.5.
pub const FACE_ROI: NormRect = NormRect { x_center: 0.5, y_center: 0.5, width: 0.6, height: 0.6, rotation: 0.0 };

/// A synthetic canonical face in centimetres (x right, y up, z toward the viewer): an ellipsoid grid (golden-angle
/// spiral over its front and sides) with a nose bump, so it has no rotational symmetry the pose fit could mix up.
pub fn canonical_face() -> Vec<[f32; 3]> {
    let golden = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
    let mut points: Vec<[f32; 3]> = (0..MESH)
        .map(|i| {
            let t = (i as f32 + 0.5) / MESH as f32;
            let z = 1.0 - 1.6 * t;
            let ring = (1.0 - z * z).sqrt();
            let angle = i as f32 * golden;
            let (x, y) = (7.0 * ring * angle.cos(), 9.0 * ring * angle.sin());
            let nose = if z > 0.0 { 2.5 * (-(x * x + (y + 1.0) * (y + 1.0)) / 6.0).exp() } else { 0.0 };
            [x, y, 6.0 * z + nose]
        })
        .collect();
    // The ten iris points sit on the eyes.
    let eye = points[33];
    let other_eye = points[263];
    points.extend((0..LANDMARKS - MESH).map(|i| if i < 5 { eye } else { other_eye }));
    points
}

fn varint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn len_field(field: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = varint(field << 3 | 2);
    out.extend(varint(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

/// A serialized `GeometryPipelineMetadata` for `canonical`: its first 468 points as the mesh (five floats a
/// vertex) and weight 1 on each of them.
pub fn metadata_bytes(canonical: &[[f32; 3]]) -> Vec<u8> {
    let mut mesh = varint(1 << 3); // vertex_type = VERTEX_PT
    mesh.push(0);
    let floats: Vec<u8> =
        canonical[..MESH].iter().flat_map(|p| [p[0], p[1], p[2], 0.0, 0.0]).flat_map(f32::to_le_bytes).collect();
    mesh.extend(len_field(3, &floats));
    let mut message = len_field(1, &mesh);
    for id in 0..MESH as u64 {
        let mut reference = varint(1 << 3);
        reference.extend(varint(id));
        reference.extend(varint(2 << 3 | 5));
        reference.extend(1.0f32.to_le_bytes());
        message.extend(len_field(2, &reference));
    }
    message
}

/// The synthetic face's metadata, through the public parser.
pub fn geometry() -> GeometryMetadata {
    GeometryMetadata::parse(&metadata_bytes(&canonical_face())).unwrap()
}

/// A head pose in the camera's frame (x right, y up, z toward the viewer, so a face in front of the lens has z < 0).
#[derive(Copy, Clone, Debug)]
pub struct Pose {
    /// Yaw, pitch and roll in radians, about the camera's y, x and z axes (applied roll first, then pitch, then yaw).
    pub angles: [f64; 3],
    /// Where the face centre is, in centimetres.
    pub translation: [f64; 3],
}

impl Pose {
    /// A face 50 cm in front of the lens with the given angles.
    pub fn facing(yaw: f64, pitch: f64, roll: f64) -> Self {
        Self { angles: [yaw, pitch, roll], translation: [0.0, 0.0, -50.0] }
    }

    fn rotation(&self) -> Rotation3<f64> {
        let [yaw, pitch, roll] = self.angles;
        Rotation3::from_axis_angle(&Vector3::y_axis(), yaw)
            * Rotation3::from_axis_angle(&Vector3::x_axis(), pitch)
            * Rotation3::from_axis_angle(&Vector3::z_axis(), roll)
    }
}

/// `canonical` placed at `pose` and seen through the 63-degree virtual camera the geometry fit assumes, as the
/// normalised landmarks the landmark model would report. Depth is encoded so the fit's own unprojection inverts it
/// exactly: x and y by perspective division, z as the distance from the mean depth in near-plane units.
pub fn project(canonical: &[[f32; 3]], pose: &Pose, frame_w: u32, frame_h: u32) -> Vec<[f32; 3]> {
    let rotation = pose.rotation();
    let translation = Vector3::from(pose.translation);
    let placed: Vec<Vector3<f64>> = canonical
        .iter()
        .map(|p| rotation * Vector3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2])) + translation)
        .collect();
    let mean_depth = placed[..MESH].iter().map(|m| -m.z).sum::<f64>() / MESH as f64;
    let height_at_near = 2.0 * (0.5 * 63.0_f64.to_radians()).tan();
    let width_at_near = f64::from(frame_w) / f64::from(frame_h) * height_at_near;
    placed
        .iter()
        .map(|m| {
            let depth = -m.z;
            let (x, y) = (m.x / depth, m.y / depth);
            let z = (depth - mean_depth) / mean_depth / width_at_near;
            [
                ((x + 0.5 * width_at_near) / width_at_near) as f32,
                (1.0 - (y + 0.5 * height_at_near) / height_at_near) as f32,
                z as f32,
            ]
        })
        .collect()
}

/// An `f32` tensor buffer holding `data` shaped `shape`.
pub fn tensor(data: &[f32], shape: &[usize]) -> TensorBuf {
    let mut buf = TensorBuf::new(DType::F32);
    buf.set(&TensorRef::f32(data, shape)).unwrap();
    buf
}

/// The detector's anchor the scripted face comes from: the first of the six at cell (3, 3) of the 8x8 grid.
const FACE_ANCHOR: usize = 512 + (3 * 8 + 3) * 6;
const ANCHORS: usize = 896;
const COORDS: usize = 16;

/// One scripted detector answer: a face (or none) whose box is `FACE_ROI` before the detector's growth, with level
/// eyes. Replayed on every run.
pub fn detector_script(face: bool) -> Vec<Vec<TensorBuf>> {
    let mut regressors = vec![0.0f32; ANCHORS * COORDS];
    let mut scores = vec![-10.0f32; ANCHORS];
    if face {
        let anchor = 3.5 / 8.0;
        let to_raw = |v: f32| (v - anchor) * 128.0;
        let row = &mut regressors[FACE_ANCHOR * COORDS..(FACE_ANCHOR + 1) * COORDS];
        row[0] = to_raw(0.5);
        row[1] = to_raw(0.5);
        row[2] = 0.4 * 128.0;
        row[3] = 0.4 * 128.0;
        for (k, (x, y)) in [(0.42, 0.45), (0.58, 0.45)].into_iter().enumerate() {
            row[4 + 2 * k] = to_raw(x);
            row[5 + 2 * k] = to_raw(y);
        }
        scores[FACE_ANCHOR] = 10.0;
    }
    vec![vec![tensor(&regressors, &[1, ANCHORS, COORDS]), tensor(&scores, &[1, ANCHORS, 1])]]
}

/// One scripted landmark answer for the face `points` (normalised frame coordinates) cropped through the upright
/// square-frame `roi`: the raw output in input pixels that the tracker projects back to `points`.
pub fn landmarks_script(points: &[[f32; 3]], roi: NormRect, presence_logit: f32) -> Vec<Vec<TensorBuf>> {
    let side = 256.0;
    let raw: Vec<f32> = points
        .iter()
        .flat_map(|p| {
            let u = (p[0] - roi.x_center) / roi.width + 0.5;
            let v = (p[1] - roi.y_center) / roi.height + 0.5;
            [u * side, v * side, p[2] / roi.width * side]
        })
        .collect();
    vec![vec![tensor(&raw, &[1, 1, 1, LANDMARKS * 3]), tensor(&[presence_logit], &[1, 1, 1, 1])]]
}

/// The coefficient the scripted blendshape graph reports for model output `i` (`_neutral` is 0).
pub fn coefficient(i: usize) -> f32 {
    if i == 0 { 0.99 } else { i as f32 / 100.0 }
}

/// One scripted blendshape answer: `coefficient(i)` for each of the 52 outputs.
pub fn blendshapes_script() -> Vec<Vec<TensorBuf>> {
    let scores: Vec<f32> = (0..52).map(coefficient).collect();
    vec![vec![tensor(&scores, &[52])]]
}

/// A session that counts its successful runs where the test can read them after the session has been boxed.
pub struct Counted {
    inner: FakeSession,
    runs: Arc<AtomicU64>,
    fail: Arc<AtomicBool>,
}

impl Counted {
    /// Wraps `inner`; the second value reads the count and the third makes every run fail while it is set.
    pub fn new(inner: FakeSession) -> (Self, Arc<AtomicU64>, Arc<AtomicBool>) {
        let runs = Arc::new(AtomicU64::new(0));
        let fail = Arc::new(AtomicBool::new(false));
        (Self { inner, runs: Arc::clone(&runs), fail: Arc::clone(&fail) }, runs, fail)
    }
}

impl Session for Counted {
    fn signature(&self) -> &Signature {
        self.inner.signature()
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(RunError::Failed("scripted failure".into()));
        }
        self.inner.run(inputs, outputs)?;
        self.runs.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn reset(&mut self) {
        self.inner.reset();
    }

    fn info(&self) -> SessionInfo {
        self.inner.info()
    }
}

/// How many times each graph ran.
pub struct Runs {
    /// The detector.
    pub detector: Arc<AtomicU64>,
    /// The landmark model.
    pub landmarks: Arc<AtomicU64>,
    /// The blendshape model.
    pub blendshapes: Arc<AtomicU64>,
    /// While set, every blendshape run fails.
    pub fail_blendshapes: Arc<AtomicBool>,
}

impl Runs {
    /// `(detector, landmarks, blendshapes)` run counts.
    pub fn get(&self) -> (u64, u64, u64) {
        (
            self.detector.load(Ordering::Relaxed),
            self.landmarks.load(Ordering::Relaxed),
            self.blendshapes.load(Ordering::Relaxed),
        )
    }
}

/// Scripted sessions for the three graphs with run counters.
pub fn counted_graphs(
    detector: Vec<Vec<TensorBuf>>,
    landmarks: Vec<Vec<TensorBuf>>,
    blendshapes: Vec<Vec<TensorBuf>>,
) -> (FaceGraphs, Runs) {
    let (d, detector_runs, _) = Counted::new(FakeSession::scripted(detector_signature(), detector));
    let (l, landmark_runs, _) = Counted::new(FakeSession::scripted(landmarks_signature(), landmarks));
    let (b, blendshape_runs, fail_blendshapes) =
        Counted::new(FakeSession::scripted(blendshapes_signature(), blendshapes));
    (
        FaceGraphs { detector: Box::new(d), landmarks: Box::new(l), blendshapes: Box::new(b) },
        Runs { detector: detector_runs, landmarks: landmark_runs, blendshapes: blendshape_runs, fail_blendshapes },
    )
}

/// The same scripted sessions without counters.
pub fn graphs(
    detector: Vec<Vec<TensorBuf>>,
    landmarks: Vec<Vec<TensorBuf>>,
    blendshapes: Vec<Vec<TensorBuf>>,
) -> FaceGraphs {
    FaceGraphs {
        detector: Box::new(FakeSession::scripted(detector_signature(), detector)),
        landmarks: Box::new(FakeSession::scripted(landmarks_signature(), landmarks)),
        blendshapes: Box::new(FakeSession::scripted(blendshapes_signature(), blendshapes)),
    }
}

/// A flat grey `SIDE` x `SIDE` image: the scripted sessions ignore pixels, but the crop warp still reads them.
pub fn grey_pixels() -> Vec<u8> {
    vec![128; (SIDE * SIDE * 3) as usize]
}

/// A tightly packed frame over `pixels`.
pub fn frame(pixels: &[u8]) -> RgbFrame<'_> {
    RgbFrame { data: pixels, width: SIDE, height: SIDE, stride: SIDE as usize * 3 }
}

/// A `SIDE` x `SIDE` frame of soft triangle-wave stripes shifted `shift` pixels to the right, with `noise` levels of
/// deterministic per-pixel sensor noise (a hash of the pixel and `seed`). One pixel of shift changes a stripe by about
/// three levels, so slow drift stays under the scheme's skip threshold for a frame or two and a fast turn does not.
pub fn textured_pixels(shift: u32, noise: u32, seed: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((SIDE * SIDE * 3) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let phase = (x + shift) % 64;
            let stripe = 40 + 5 * if phase < 32 { phase } else { 64 - phase };
            let hash = (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663) ^ seed.wrapping_mul(83_492_791)) >> 7;
            let jitter = if noise == 0 { 0 } else { (hash % (2 * noise + 1)) as i32 - noise as i32 };
            let level = (stripe as i32 + jitter).clamp(0, 255) as u8;
            pixels.extend([level; 3]);
        }
    }
    pixels
}

/// The scripted face moved and scaled so that the ROI tracking derives from its landmarks is `FACE_ROI` again: level
/// eyes, a bounding box centred in the frame with a long side of `FACE_ROI.width / 1.5`. With it, a landmark script made
/// for `FACE_ROI` stays right on every tracked frame, as a face that does not move would.
pub fn steady_face() -> Vec<[f32; 3]> {
    let mut points = project(&canonical_face(), &Pose::facing(0.2, -0.1, 0.05), SIDE, SIDE);
    points[263][1] = points[33][1];
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in &points {
        for axis in 0..2 {
            lo[axis] = lo[axis].min(p[axis]);
            hi[axis] = hi[axis].max(p[axis]);
        }
    }
    let long = (hi[0] - lo[0]).max(hi[1] - lo[1]);
    let scale = FACE_ROI.width / 1.5 / long;
    for p in &mut points {
        p[0] = 0.5 + (p[0] - f32::midpoint(lo[0], hi[0])) * scale;
        p[1] = 0.5 + (p[1] - f32::midpoint(lo[1], hi[1])) * scale;
        p[2] *= scale;
    }
    points
}
