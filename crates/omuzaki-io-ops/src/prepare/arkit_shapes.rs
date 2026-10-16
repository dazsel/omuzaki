//! The geometry-based `ARKit` shape generator (design D9; the spec's `HANA_Tool` parity item, EX-12).
//!
//! [`arkit::synthesise`](super::arkit::synthesise) reuses what a model has. This makes the rest from the model's own
//! geometry, read through the expressions every VRM carries (a blink and an `aa`):
//! - **Halves of symmetric shapes.** A symmetric expression (a blink, a smile, `ee`, an angry brow) is split by the
//!   sagittal plane with a smooth falloff ([`FALLOFF`] of the moved region's half-width each side of the middle), so
//!   the left and right halves sum to the source exactly and each channel moves one side only.
//! - **Shapes from landmarks.** The vertices the blink moves are the upper lids, which locate the eyes; the vertices
//!   `aa` moves are the jaw and lower lip, whose upper edge is the mouth line. From those: `eyeWide*` lifts each lid a
//!   quarter of its closing travel, `eyeSquint*` lowers it a third, `cheekPuff` pushes the cheeks out along their normals,
//!   `mouthRoll*` and `mouthShrug*` roll the lips inward or raise them, and `eyeLook*` moves an iris, when the eyes have
//!   one of their own (the smallest separate part inside each eye that the blink does not move).
//!
//! Every amount is a fraction of the distance between the eyes ([`Landmarks::eye_spacing`]), so a shape fits any head
//! size; the fractions are starting values for art review, judged on contact sheets (`tools/arkit-shape-sheet.py`).
//! Each new shape is a morph target on the face mesh, named after its channel, and an expression of that name binding
//! it at weight 1 on every node that draws the mesh, so a generated channel exports as perfect sync like any other.
//! The outcome of each channel is [`Synthesis::Generated`], which the report lists as `arkit.generated`.
//!
//! Deterministic: meshes, primitives and vertices are visited in order, and no clock or randomness is read.

use std::collections::BTreeMap;

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{Expression, ExpressionName, IoScene, MorphBind, MorphTarget, NodeIx, PresetExpression};

use super::arkit::{ChannelOutcome, Synthesis};

/// Half the width of the blend between the two halves of a split, as a fraction of the moved region's half-width.
pub const FALLOFF: f32 = 0.15;

/// A vertex counts as moved when its delta is longer than this fraction of the eye spacing.
const MOVED: f32 = 1e-4;

/// How much of a lid's closing travel `eyeWide*` lifts it.
const WIDE: f32 = 0.25;

/// How much of a lid's closing travel `eyeSquint*` lowers it.
const SQUINT: f32 = 0.35;

/// How far the cheeks puff out, as a fraction of the eye spacing.
const PUFF: f32 = 0.08;

/// How much of a lip's height a roll pulls in toward the mouth line (the rest of the roll goes back, by [`ROLL_BACK`]).
const ROLL: f32 = 0.7;

/// How far a rolled lip goes back, as a fraction of the eye spacing.
const ROLL_BACK: f32 = 0.03;

/// How far a lip shrugs up, as a fraction of the eye spacing.
const SHRUG: f32 = 0.08;

/// How far an iris moves sideways, and up or down, as a fraction of its eye's width and height.
const LOOK: (f32, f32) = (0.2, 0.15);

/// The symmetric source each pair of channels splits, as `(left, right, preset, morph)`: the `VRoid` morph target of
/// that name on the face when the model has one (what synthesis binds at half weight for both sides), otherwise the
/// preset expression.
const SPLITS: [(&str, &str, PresetExpression, &str); 4] = [
    ("eyeBlinkLeft", "eyeBlinkRight", PresetExpression::Blink, "Fcl_EYE_Close"),
    ("mouthSmileLeft", "mouthSmileRight", PresetExpression::Happy, "Fcl_MTH_Joy"),
    ("mouthStretchLeft", "mouthStretchRight", PresetExpression::Ee, "Fcl_MTH_E"),
    ("browDownLeft", "browDownRight", PresetExpression::Angry, "Fcl_BRW_Angry"),
];

/// The deltas of one mesh's primitives: positions and normals, one per vertex (normals empty when none move).
#[derive(Clone, Debug, Default, PartialEq)]
struct Deltas {
    positions: Vec<Vec<Vec3>>,
    normals: Vec<Vec<Vec3>>,
}

impl Deltas {
    /// No delta on any vertex of `mesh`.
    fn zero(scene: &IoScene, mesh: usize) -> Self {
        let counts: Vec<usize> = scene.meshes[mesh].primitives.iter().map(|p| p.mesh.positions.len()).collect();
        Self {
            positions: counts.iter().map(|&n| vec![Vec3::ZERO; n]).collect(),
            normals: counts.iter().map(|&n| vec![Vec3::ZERO; n]).collect(),
        }
    }

    /// Every delta times `factor(position)` of its vertex, `positions` giving each primitive's vertex positions.
    fn scaled(&self, positions: &[Vec<Vec3>], factor: &dyn Fn(Vec3) -> f32) -> Self {
        let scale = |deltas: &[Vec<Vec3>]| -> Vec<Vec<Vec3>> {
            deltas.iter().zip(positions).map(|(d, p)| d.iter().zip(p).map(|(d, &p)| *d * factor(p)).collect()).collect()
        };
        Self { positions: scale(&self.positions), normals: scale(&self.normals) }
    }

    /// True if no vertex moves.
    fn is_zero(&self) -> bool {
        self.positions.iter().flatten().chain(self.normals.iter().flatten()).all(|d| *d == Vec3::ZERO)
    }
}

/// What an expression does to each mesh it moves, by mesh index: its morph binds' deltas summed with their weights
/// (the binds of the first node that draws each mesh).
fn expression_deltas(scene: &IoScene, expression: &Expression) -> BTreeMap<usize, Deltas> {
    let mut out: BTreeMap<usize, Deltas> = BTreeMap::new();
    let mut first_node: BTreeMap<usize, NodeIx> = BTreeMap::new();
    for bind in &expression.morph_binds {
        let Some(mesh) = scene.nodes.get(bind.node.index()).and_then(|n| n.mesh).map(omuzaki_io_api::MeshIx::index)
        else {
            continue;
        };
        if *first_node.entry(mesh).or_insert(bind.node) != bind.node {
            continue;
        }
        let deltas = out.entry(mesh).or_insert_with(|| Deltas::zero(scene, mesh));
        for (p, primitive) in scene.meshes[mesh].primitives.iter().enumerate() {
            let Some(target) = primitive.morph_targets.get(bind.index as usize) else { continue };
            for (slot, d) in deltas.positions[p].iter_mut().zip(&target.positions) {
                *slot += *d * bind.weight;
            }
            for (slot, d) in deltas.normals[p].iter_mut().zip(&target.normals) {
                *slot += *d * bind.weight;
            }
        }
    }
    out
}

/// Each primitive's vertex positions of mesh `mesh`.
fn positions(scene: &IoScene, mesh: usize) -> Vec<Vec<Vec3>> {
    scene.meshes[mesh].primitives.iter().map(|p| p.mesh.positions.clone()).collect()
}

/// Where the face is, read from what its blink and `aa` move (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct Landmarks {
    /// The mesh the blink moves (the face).
    pub mesh: usize,
    /// The sagittal plane: `x` of the middle of the face.
    pub middle: f32,
    /// The centre of each eye's opening, left then right.
    pub eyes: [Vec3; 2],
    /// Each eye's width and how far its lid travels to close it, left then right.
    pub eye_size: [(f32, f32); 2],
    /// The distance between the eyes' centres: the unit every amount is measured in.
    pub eye_spacing: f32,
    /// The middle of the mouth line (the upper edge of what `aa` moves), or `None` when there is no `aa`.
    pub mouth: Option<Vec3>,
    /// Half the mouth's width.
    pub mouth_half_width: f32,
}

/// The landmarks of `scene`'s face, or `None` when it has no blink that moves two eyes.
pub fn landmarks(scene: &IoScene) -> Option<Landmarks> {
    let avatar = scene.avatar.as_ref()?;
    let blink = avatar.expressions.get(&ExpressionName::Preset(PresetExpression::Blink))?;
    let (mesh, lids) = expression_deltas(scene, blink).into_iter().max_by_key(|(_, d)| moved_count(d, 0.0))?;
    let points = positions(scene, mesh);
    let moved: Vec<(Vec3, Vec3)> = moved_vertices(&lids, &points, 0.0).collect();
    let (lo, hi) = moved.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), (p, _)| (lo.min(p.x), hi.max(p.x)));
    if moved.is_empty() || hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    let middle = f32::midpoint(lo, hi);
    let eye = |left: bool| {
        let side: Vec<&(Vec3, Vec3)> = moved.iter().filter(|(p, _)| (p.x > middle) == left).collect();
        let weight: f32 = side.iter().map(|(_, d)| d.length()).sum();
        let centre = side.iter().fold(Vec3::ZERO, |acc, (p, d)| acc + (*p + *d * 0.5) * d.length())
            / weight.max(f32::MIN_POSITIVE);
        let (lo, hi) =
            side.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), (p, _)| (lo.min(p.x), hi.max(p.x)));
        let travel = side.iter().map(|(_, d)| d.length()).fold(0.0, f32::max);
        (centre, (hi - lo, travel))
    };
    let ((left, left_size), (right, right_size)) = (eye(true), eye(false));
    let eye_spacing = left.distance(right);
    if eye_spacing.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    let (mouth, mouth_half_width) = avatar
        .expressions
        .get(&ExpressionName::Preset(PresetExpression::Aa))
        .and_then(|aa| expression_deltas(scene, aa).remove(&mesh))
        .and_then(|jaw| {
            let opened: Vec<Vec3> = moved_vertices(&jaw, &points, MOVED * eye_spacing).map(|(p, _)| p).collect();
            let top = opened.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
            // The mouth line is the upper edge of what opens; its width is that of the moved vertices near it.
            let band: Vec<&Vec3> = opened.iter().filter(|p| top - p.y < 0.1 * eye_spacing).collect();
            let half = band.iter().map(|p| (p.x - middle).abs()).fold(0.0, f32::max);
            let z = band.iter().map(|p| p.z).fold(f32::NEG_INFINITY, f32::max);
            (!band.is_empty()).then(|| (Some(Vec3::new(middle, top, z)), half))
        })
        .unwrap_or((None, 0.0));
    Some(Landmarks {
        mesh,
        middle,
        eyes: [left, right],
        eye_size: [left_size, right_size],
        eye_spacing,
        mouth,
        mouth_half_width,
    })
}

/// How many vertices `deltas` moves by more than `threshold`.
fn moved_count(deltas: &Deltas, threshold: f32) -> usize {
    deltas.positions.iter().flatten().filter(|d| d.length() > threshold).count()
}

/// Each moved vertex's position and delta.
fn moved_vertices<'a>(
    deltas: &'a Deltas,
    points: &'a [Vec<Vec3>],
    threshold: f32,
) -> impl Iterator<Item = (Vec3, Vec3)> + 'a {
    deltas
        .positions
        .iter()
        .zip(points)
        .flat_map(|(d, p)| d.iter().zip(p))
        .filter(move |(d, _)| d.length() > threshold)
        .map(|(d, p)| (*p, *d))
}

/// The share of the left half at `x` across the sagittal plane at `middle`: 1 well on the character's left (+X), 0
/// well on the right, a smoothstep across `±width`.
fn left_share(x: f32, middle: f32, width: f32) -> f32 {
    if width <= 0.0 {
        return if x > middle {
            1.0
        } else if x < middle {
            0.0
        } else {
            0.5
        };
    }
    let t = ((x - middle + width) / (2.0 * width)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The left and right halves of `deltas` across the plane at `middle`, blended over [`FALLOFF`] of the moved region's
/// half-width; they sum to `deltas`.
fn split(deltas: &Deltas, points: &[Vec<Vec3>], middle: f32) -> (Deltas, Deltas) {
    let half = moved_vertices(deltas, points, 0.0).map(|(p, _)| (p.x - middle).abs()).fold(0.0, f32::max);
    let width = FALLOFF * half;
    let left = deltas.scaled(points, &|p| left_share(p.x, middle, width));
    let right = Deltas {
        positions: subtract(&deltas.positions, &left.positions),
        normals: subtract(&deltas.normals, &left.normals),
    };
    (left, right)
}

/// `a − b`, vertex by vertex.
fn subtract(a: &[Vec<Vec3>], b: &[Vec<Vec3>]) -> Vec<Vec<Vec3>> {
    a.iter().zip(b).map(|(a, b)| a.iter().zip(b).map(|(a, b)| *a - *b).collect()).collect()
}

/// A bump: 1 at the centre of the ellipse with radii `radii` around `centre` (in x and y), falling smoothly to 0 at its
/// edge.
fn bump(p: Vec3, centre: Vec3, radii: (f32, f32)) -> f32 {
    let (dx, dy) = ((p.x - centre.x) / radii.0, (p.y - centre.y) / radii.1);
    let r2 = dx * dx + dy * dy;
    if r2 >= 1.0 { 0.0 } else { (1.0 - r2) * (1.0 - r2) }
}

/// One generated shape: the deltas it moves the face mesh by, and where it came from (for the report).
struct Shape {
    deltas: Deltas,
    source: String,
}

/// Whether generation should try `outcome`'s channel: one synthesis could not build, or built from a symmetric source
/// at half weight on each side (a blink, or `Fcl_MTH_Joy`, for one eye or one corner), which a split of the source does
/// better.
pub fn worth_generating(outcome: &ChannelOutcome) -> bool {
    let split = SPLITS.iter().find(|(l, r, ..)| *l == outcome.channel || *r == outcome.channel);
    match (&outcome.how, split) {
        (Synthesis::Missing, _) => true,
        (Synthesis::FromPreset { preset, .. }, Some((_, _, p, _))) => preset == p,
        (Synthesis::FromMorph { target }, Some((_, _, _, morph))) => target.eq_ignore_ascii_case(morph),
        _ => false,
    }
}

/// Makes the named `ARKit` channels from geometry (see the module docs) and returns one outcome per channel, in the
/// order given: [`Synthesis::Generated`] for each one made, [`Synthesis::Missing`] for one the model has nothing to make
/// it from. An expression of a generated channel's name is replaced: callers pass the channels synthesis built from a
/// symmetric preset or could not build, never one the file has. A scene without a blink that moves two eyes is left
/// alone.
pub fn generate(scene: &mut IoScene, channels: &[&str]) -> Vec<ChannelOutcome> {
    let marks = landmarks(scene);
    let mut outcomes = Vec::with_capacity(channels.len());
    for &channel in channels {
        let shape = marks.as_ref().and_then(|m| make(scene, m, channel));
        let how = match (shape, &marks) {
            (Some(shape), Some(m)) if !shape.deltas.is_zero() => {
                add_shape(scene, m.mesh, channel, &shape.deltas);
                Synthesis::Generated { source: shape.source }
            }
            _ => Synthesis::Missing,
        };
        outcomes.push(ChannelOutcome { channel: arkit_name(channel), how });
    }
    outcomes
}

/// The `ARKit` table's own `&'static str` for `channel` (the outcome type holds one).
fn arkit_name(channel: &str) -> &'static str {
    omuzaki_io_api::ARKIT_52.iter().copied().find(|c| *c == channel).unwrap_or("")
}

/// The shape of one channel, or `None` when the model has nothing to make it from.
fn make(scene: &IoScene, m: &Landmarks, channel: &str) -> Option<Shape> {
    let points = positions(scene, m.mesh);
    let side = |name: &str| {
        if name.ends_with("Left") {
            Some(true)
        } else if name.ends_with("Right") {
            Some(false)
        } else {
            None
        }
    };
    if let Some(&(left, _, preset, morph)) = SPLITS.iter().find(|(l, r, ..)| *l == channel || *r == channel) {
        let (source, named) = match target_deltas(scene, m.mesh, morph) {
            Some(deltas) => (deltas, format!("the {morph} shape")),
            None => (preset_deltas(scene, preset, m.mesh)?, format!("the {} expression", preset.as_str())),
        };
        let (l, r) = split(&source, &points, m.middle);
        let (half, which) = if channel == left { (l, "left") } else { (r, "right") };
        return Some(Shape { deltas: half, source: format!("the {which} half of {named}") });
    }
    let lid = |left: bool| -> Option<Deltas> {
        let blink = preset_deltas(scene, PresetExpression::Blink, m.mesh)?;
        let (l, r) = split(&blink, &points, m.middle);
        Some(if left { l } else { r })
    };
    match channel {
        "eyeWideLeft" | "eyeWideRight" => {
            let left = side(channel)?;
            let deltas = lid(left)?.scaled(&points, &|_| -WIDE);
            Some(Shape { deltas, source: "the lid of the blink, lifted".into() })
        }
        "eyeSquintLeft" | "eyeSquintRight" => {
            let left = side(channel)?;
            let deltas = lid(left)?.scaled(&points, &|_| SQUINT);
            Some(Shape { deltas, source: "the lid of the blink, partly lowered".into() })
        }
        "cheekPuff" => cheek_puff(scene, m, &points),
        "mouthRollUpper" | "mouthRollLower" | "mouthShrugUpper" | "mouthShrugLower" => lips(scene, m, &points, channel),
        _ if channel.starts_with("eyeLook") => look(scene, m, &points, channel),
        _ => None,
    }
}

/// The deltas of the morph target of mesh `mesh` called `name` (ignoring case), if it has one.
fn target_deltas(scene: &IoScene, mesh: usize, name: &str) -> Option<Deltas> {
    let source = scene.meshes.get(mesh)?;
    let index = source.morph_target_names.iter().position(|n| n.eq_ignore_ascii_case(name))?;
    let mut deltas = Deltas::zero(scene, mesh);
    for (p, primitive) in source.primitives.iter().enumerate() {
        let Some(target) = primitive.morph_targets.get(index) else { continue };
        for (slot, d) in deltas.positions[p].iter_mut().zip(&target.positions) {
            *slot = *d;
        }
        for (slot, d) in deltas.normals[p].iter_mut().zip(&target.normals) {
            *slot = *d;
        }
    }
    Some(deltas)
}

/// The deltas the `preset` expression moves mesh `mesh` by, if the model has the preset and it moves that mesh.
fn preset_deltas(scene: &IoScene, preset: PresetExpression, mesh: usize) -> Option<Deltas> {
    let expression = scene.avatar.as_ref()?.expressions.get(&ExpressionName::Preset(preset))?;
    expression_deltas(scene, expression).remove(&mesh)
}

/// The vertices of the face mesh some preset expression moves: the eyes, mouth and brows, which a cheek or lip shape
/// leaves to their own channels.
fn expressive(scene: &IoScene, m: &Landmarks, points: &[Vec<Vec3>]) -> Vec<Vec<bool>> {
    let mut moved: Vec<Vec<bool>> = points.iter().map(|p| vec![false; p.len()]).collect();
    let presets =
        scene.avatar.iter().flat_map(|a| &a.expressions).filter(|(n, _)| matches!(n, ExpressionName::Preset(_)));
    for (_, expression) in presets {
        if let Some(d) = expression_deltas(scene, expression).remove(&m.mesh) {
            for (flags, deltas) in moved.iter_mut().zip(&d.positions) {
                for (flag, delta) in flags.iter_mut().zip(deltas) {
                    *flag |= delta.length() > MOVED * m.eye_spacing;
                }
            }
        }
    }
    moved
}

/// `cheekPuff`: each cheek pushed out along its normals, in an ellipse beside the mouth corner and below the eye, on
/// the part of the face that looks forward (so the head's outline does not bulge).
fn cheek_puff(scene: &IoScene, m: &Landmarks, points: &[Vec<Vec3>]) -> Option<Shape> {
    let mouth = m.mouth?;
    let s = m.eye_spacing;
    let fixed = expressive(scene, m, points);
    let normals: Vec<&Vec<Vec3>> = scene.meshes[m.mesh].primitives.iter().map(|p| &p.mesh.normals).collect();
    let mut deltas = Deltas::zero(scene, m.mesh);
    for (left, eye) in [(true, m.eyes[0]), (false, m.eyes[1])] {
        let sign = if left { 1.0 } else { -1.0 };
        let x = m.middle + sign * (m.mouth_half_width + 0.45 * s);
        let centre = Vec3::new(x, mouth.y + 0.2 * (eye.y - mouth.y), mouth.z);
        let radii = (0.5 * s, 0.45 * (eye.y - mouth.y).abs().max(0.2 * s));
        for (p, (prim, deltas)) in deltas.positions.iter_mut().enumerate().map(|(p, d)| (p, (&points[p], d))) {
            for (v, (point, delta)) in prim.iter().zip(deltas.iter_mut()).enumerate() {
                let normal = normals[p].get(v).copied().unwrap_or(Vec3::Z);
                // The face that looks forward or sideways, fading out toward the back of the head (a hard cut would
                // leave a crease), and nothing an expression already moves.
                if fixed[p][v] || (point.x - m.middle) * sign < 0.0 {
                    continue;
                }
                let facing = smoothstep(-0.2, 0.4, normal.z);
                *delta += normal * (PUFF * s * facing * bump(*point, centre, radii));
            }
        }
    }
    Some(Shape { deltas, source: "the cheeks beside the mouth".into() })
}

/// The lip shapes, on the band just above or below the mouth line: a roll pulls the lip in toward the line (so less of
/// it shows) and back; a shrug raises it.
fn lips(scene: &IoScene, m: &Landmarks, points: &[Vec<Vec3>], channel: &str) -> Option<Shape> {
    let mouth = m.mouth?;
    let s = m.eye_spacing;
    let upper = channel.ends_with("Upper");
    let roll = channel.starts_with("mouthRoll");
    let band = if roll { 0.1 * s } else { 0.2 * s };
    let half_width = (m.mouth_half_width * 1.15).max(0.1 * s);
    let mut deltas = Deltas::zero(scene, m.mesh);
    for (prim, slots) in points.iter().zip(deltas.positions.iter_mut()) {
        for (point, slot) in prim.iter().zip(slots.iter_mut()) {
            let depth = if upper { point.y - mouth.y } else { mouth.y - point.y };
            if !(0.0..=band).contains(&depth) || point.z < mouth.z - 0.3 * s {
                continue;
            }
            // Full strength along the middle of the mouth, easing out toward the corners.
            let across = ((point.x - m.middle) / half_width).abs();
            let w = if across >= 1.0 { 0.0 } else { (1.0 - across * across).powi(2) };
            // Strongest at the mouth line, easing to nothing at the band's far edge, so the skin beyond never creases.
            let ease = (1.0 - (depth / band).powi(2)).powi(2);
            *slot = if roll {
                let toward_line = if upper { -1.0 } else { 1.0 };
                Vec3::new(0.0, toward_line * ROLL * depth, -ROLL_BACK * s) * (w * ease)
            } else {
                Vec3::Y * (SHRUG * s * w * ease)
            };
        }
    }
    let which = if upper { "upper" } else { "lower" };
    Some(Shape { deltas, source: format!("the {which} lip, above the mouth line `aa` opens") })
}

/// `eyeLook*`: one eye's iris moved, when the eye has one of its own: the primitive of the face mesh with the smallest
/// extent whose vertices all lie inside that eye's opening and that the blink does not move.
fn look(scene: &IoScene, m: &Landmarks, points: &[Vec<Vec3>], channel: &str) -> Option<Shape> {
    let left = channel.ends_with("Left");
    let eye = usize::from(!left);
    let (centre, (width, height)) = (m.eyes[eye], m.eye_size[eye]);
    let fixed = expressive(scene, m, points);
    let inside = |p: &Vec3| {
        ((p.x - centre.x) / (0.75 * width)).powi(2) + ((p.y - centre.y) / (0.9 * height.max(1e-6))).powi(2) <= 1.0
    };
    // Per primitive, the vertices in this eye, when every one of them is still and the primitive is not the face.
    let candidates: Vec<(usize, f32)> = points
        .iter()
        .enumerate()
        .filter_map(|(p, prim)| {
            let in_eye: Vec<&Vec3> =
                prim.iter().zip(&fixed[p]).filter(|(v, still)| !**still && inside(v)).map(|(v, _)| v).collect();
            let side_only = prim.iter().filter(|v| (v.x - m.middle) * if left { 1.0 } else { -1.0 } > 0.0).count();
            let (lo, hi) =
                in_eye.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(v.x), hi.max(v.x)));
            (in_eye.len() >= 3 && in_eye.len() * 2 >= side_only).then_some((p, hi - lo))
        })
        .collect();
    let (iris, _) = candidates.into_iter().min_by(|a, b| a.1.total_cmp(&b.1))?;
    let toward_nose = if left { -1.0 } else { 1.0 };
    let shift = match channel.trim_start_matches("eyeLook").trim_end_matches("Left").trim_end_matches("Right") {
        "Up" => Vec3::Y * (LOOK.1 * height),
        "Down" => Vec3::NEG_Y * (LOOK.1 * height),
        "In" => Vec3::X * (toward_nose * LOOK.0 * width),
        "Out" => Vec3::X * (-toward_nose * LOOK.0 * width),
        _ => return None,
    };
    // The iris slides over the surface it sits on (the eye white), keeping its height above it, so it neither sinks
    // into a curved eye nor floats off it.
    let surface: Vec<Vec3> = points
        .iter()
        .enumerate()
        .filter(|(p, _)| *p != iris)
        .flat_map(|(p, prim)| prim.iter().zip(&fixed[p]).filter(|(v, still)| !**still && inside(v)).map(|(v, _)| *v))
        .collect();
    let mut deltas = Deltas::zero(scene, m.mesh);
    for (point, slot) in points[iris].iter().zip(deltas.positions[iris].iter_mut()) {
        if !inside(point) {
            continue;
        }
        let (x, y) = (point.x + shift.x, point.y + shift.y);
        let rise = surface_height(&surface, x, y)
            .zip(surface_height(&surface, point.x, point.y))
            .map_or(0.0, |(to, from)| to - from);
        // A little clearance, half the iris's own height above the surface, so it never sinks into a coarse eye white.
        let clearance = surface_height(&surface, point.x, point.y).map_or(0.0, |h| 0.5 * (point.z - h).max(0.0));
        *slot = Vec3::new(shift.x, shift.y, rise + clearance);
    }
    Some(Shape { deltas, source: format!("the {} iris", if left { "left" } else { "right" }) })
}

/// The height (`z`) of `surface` at `(x, y)`: the inverse-distance blend of its nearest points, so it is smooth however
/// coarse the surface is; `None` for no surface.
fn surface_height(surface: &[Vec3], x: f32, y: f32) -> Option<f32> {
    const NEAREST: usize = 6;
    let mut nearest: Vec<(f32, f32)> = surface.iter().map(|v| ((v.x - x).powi(2) + (v.y - y).powi(2), v.z)).collect();
    nearest.sort_by(|a, b| a.0.total_cmp(&b.0));
    nearest.truncate(NEAREST);
    let (sum, weight) = nearest.iter().fold((0.0, 0.0), |(sum, weight), &(d2, z)| {
        let w = 1.0 / (d2 + 1e-12);
        (sum + z * w, weight + w)
    });
    (weight > 0.0).then(|| sum / weight)
}

/// 0 below `edge0`, 1 above `edge1`, a smoothstep between.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Adds `deltas` to mesh `mesh` as a new morph target named `channel` (on every primitive that has targets or that
/// it moves, padding the others' lists so all have the same count), and the expression `channel` binding it at weight
/// 1 on every node that draws the mesh.
fn add_shape(scene: &mut IoScene, mesh: usize, channel: &str, deltas: &Deltas) {
    let index = scene.meshes[mesh].morph_target_count();
    let named = !scene.meshes[mesh].morph_target_names.is_empty() || index == 0;
    let source = &mut scene.meshes[mesh];
    for (p, primitive) in source.primitives.iter_mut().enumerate() {
        let moves = deltas.positions[p].iter().chain(&deltas.normals[p]).any(|d| *d != Vec3::ZERO);
        if primitive.morph_targets.is_empty() && !moves {
            continue;
        }
        let count = primitive.mesh.positions.len();
        while primitive.morph_targets.len() < index {
            primitive.morph_targets.push(MorphTarget::new(vec![Vec3::ZERO; count]));
        }
        let mut target = MorphTarget::new(deltas.positions[p].clone());
        if deltas.normals[p].iter().any(|d| *d != Vec3::ZERO) {
            target.normals.clone_from(&deltas.normals[p]);
        }
        primitive.morph_targets.push(target);
    }
    if named {
        source.morph_target_names.push(channel.to_owned());
    }
    let Ok(index) = u32::try_from(index) else { return };
    let nodes: Vec<NodeIx> = scene
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.mesh.map(omuzaki_io_api::MeshIx::index) == Some(mesh))
        .filter_map(|(i, _)| u32::try_from(i).ok().map(NodeIx))
        .collect();
    let mut expression = Expression::new();
    expression.morph_binds = nodes.into_iter().map(|node| MorphBind { node, index, weight: 1.0 }).collect();
    if let (Some(avatar), Ok(name)) = (scene.avatar.as_mut(), ExpressionName::parse(channel)) {
        avatar.expressions.insert(name, expression);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_left_share_is_a_smoothstep_across_the_plane() {
        assert_eq!(left_share(1.0, 0.0, 0.1), 1.0);
        assert_eq!(left_share(-1.0, 0.0, 0.1), 0.0);
        assert_eq!(left_share(0.0, 0.0, 0.1), 0.5);
        assert!((left_share(0.05, 0.0, 0.1) - 0.84375).abs() < 1e-6);
        assert_eq!((left_share(0.2, 0.0, 0.0), left_share(-0.2, 0.0, 0.0), left_share(0.0, 0.0, 0.0)), (1.0, 0.0, 0.5));
    }

    #[test]
    fn a_bump_is_one_at_its_centre_and_zero_at_its_edge() {
        let c = Vec3::ZERO;
        assert_eq!(bump(c, c, (1.0, 2.0)), 1.0);
        assert_eq!(bump(Vec3::X, c, (1.0, 2.0)), 0.0);
        assert_eq!(bump(Vec3::new(0.0, 1.0, 0.0), c, (1.0, 2.0)), 0.5625);
    }
}
