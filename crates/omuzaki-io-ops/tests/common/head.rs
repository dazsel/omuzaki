//! An original anime-style head for the `ARKit` face-shape tests: a skinned "Face" mesh on the fixture humanoid's `head`
//! bone, built procedurally, with the six `VRoid` morph targets the generator's tests start from. Nothing third-party
//! or private. The eyes, brows, mouth and lips are thin layers lifted off the skull by a few millimetres, so they never
//! z-fight and a shape that moves one of them stays on the skin.
use std::f64::consts::PI;
use std::fmt::Write as _;
use std::path::Path;

use omuzaki_core::glam::{DVec3, Vec2, Vec4};
use omuzaki_io_api::{Expression, ExpressionName, IoScene, Mesh, MeshData, MorphTarget, Node, Primitive, Skin};

use super::{Fixture, humanoid, toon_material, world};

/// The morph targets of the face mesh, in order: `VRoid`'s names.
pub const TARGET_NAMES: [&str; 6] =
    ["Fcl_EYE_Close", "Fcl_MTH_A", "Fcl_MTH_U", "Fcl_MTH_Joy", "Fcl_MTH_E", "Fcl_BRW_Angry"];

/// The VRM expression preset that drives each target in [`TARGET_NAMES`], same order.
pub const PRESETS: [&str; 6] = ["blink", "aa", "ou", "happy", "ee", "angry"];

/// Materials, one primitive each, in primitive order, with their linear base colours.
pub const MATERIALS: [(&str, [f32; 3]); 7] = [
    ("Skin", [0.93, 0.78, 0.70]),
    ("EyeWhite", [1.0, 1.0, 1.0]),
    ("Iris", [0.12, 0.07, 0.05]),
    ("Brow", [0.15, 0.10, 0.08]),
    ("MouthInside", [0.35, 0.08, 0.08]),
    ("Lips", [0.80, 0.42, 0.42]),
    // The upper-lid line. Without it a closed eye is skin on skin and reads as nothing in a flat-shaded render.
    ("Lash", [0.10, 0.06, 0.05]),
];

const TARGETS: usize = TARGET_NAMES.len();
const BLINK: usize = 0;
const MOUTH_A: usize = 1;
const MOUTH_U: usize = 2;
const MOUTH_JOY: usize = 3;
const MOUTH_E: usize = 4;
const BROW_ANGRY: usize = 5;

/// Skull radii (half width, half height, half depth) in metres.
const RX: f64 = 0.075;
const RY: f64 = 0.095;
const RZ: f64 = 0.085;
/// Skull centre above the head bone.
const CENTRE_ABOVE_BONE: f64 = 0.08;
/// Eye centre: sideways from the middle and above the skull centre. The eye is 3 cm wide, 2.5 cm tall.
const EYE_X: f64 = 0.032;
const EYE_ABOVE_CENTRE: f64 = 0.015;
const EYE_HALF_WIDTH: f64 = 0.015;
const EYE_TOP: f64 = 0.0145;
const EYE_BOTTOM: f64 = 0.0105;
/// Mouth centre below the skull centre and its half width.
const MOUTH_BELOW_CENTRE: f64 = 0.04;
pub const MOUTH_HALF_WIDTH: f64 = 0.0125;
/// How far the jaw drops at `Fcl_MTH_A`.
const JAW_DROP: f64 = 0.012;

/// Heights above the skull surface of each layer: eye white 1 mm, iris 0.5 mm in front of it.
const OFF_WHITE: f64 = 0.0010;
const OFF_IRIS: f64 = 0.0015;
const OFF_BROW: f64 = 0.0015;
const OFF_INSIDE: f64 = 0.0020;
const OFF_LID: f64 = 0.0025;
const OFF_LASH: f64 = 0.0032;
const OFF_LIPS: f64 = 0.0030;

/// The skull surface and the places on it where the face is drawn.
#[derive(Clone, Copy)]
struct Skull {
    centre: DVec3,
    eye_y: f64,
    mouth_y: f64,
}

impl Skull {
    /// Width factor of the cross-section at height `yn` (-1 chin to 1 crown): a gently pointed chin.
    fn width(yn: f64) -> f64 {
        if yn < 0.0 { 1.0 - 0.25 * yn * yn } else { 1.0 }
    }

    /// Zero on the surface: the ellipsoid, narrowed towards the chin.
    fn implicit(&self, p: DVec3) -> f64 {
        let q = p - self.centre;
        let yn = q.y / RY;
        let w = Self::width(yn);
        (q.x / (RX * w)).powi(2) + (q.z / (RZ * w)).powi(2) + yn * yn
    }

    /// The outward normal at (or near) `p`.
    fn normal(&self, p: DVec3) -> DVec3 {
        const H: f64 = 1e-5;
        let d = |axis: DVec3| self.implicit(p + axis * H) - self.implicit(p - axis * H);
        DVec3::new(d(DVec3::X), d(DVec3::Y), d(DVec3::Z)).try_normalize().unwrap_or(DVec3::Z)
    }

    /// The front surface point above the world position `(x, y)`.
    fn surface(&self, x: f64, y: f64) -> DVec3 {
        let yn = (y - self.centre.y) / RY;
        let w = Self::width(yn);
        let rest = 1.0 - yn * yn - ((x - self.centre.x) / (RX * w)).powi(2);
        DVec3::new(x, y, self.centre.z + RZ * w * rest.max(0.0).sqrt())
    }

    /// The point `off` metres above the front surface at `(x, y)`.
    fn at(&self, x: f64, y: f64, off: f64) -> DVec3 {
        let p = self.surface(x, y);
        p + self.normal(p) * off
    }
}

/// `smoothstep` from `e0` (0) to `e1` (1); `e0 > e1` is allowed.
fn smooth(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How far the jaw has dropped, 0..=1, for a mouth part at rest height `y`: the upper lip stays, the lower one goes.
fn jaw_part(skull: &Skull, y: f64) -> f64 {
    smooth(skull.mouth_y + 0.001, skull.mouth_y - 0.001, y)
}

/// The same for the skin: a wider ramp and a fade to the sides, so the cheeks and the back of the head stay put.
fn jaw_skin(skull: &Skull, p: DVec3) -> f64 {
    smooth(skull.mouth_y + 0.001, skull.mouth_y - 0.025, p.y)
        * (1.0 - smooth(0.016, 0.060, p.x.abs()))
        * smooth(-0.02, 0.04, p.z - skull.centre.z)
}

/// How a decal vertex at `(x, y)` moves for one target: a shift of its place on the skin, plus a free offset.
#[derive(Clone, Copy, Default)]
struct Move {
    dx: f64,
    dy: f64,
    extra: DVec3,
}

type Moves = [Move; TARGETS];

/// One primitive's vertices under construction.
#[derive(Default)]
struct Part {
    pos: Vec<DVec3>,
    normal: Vec<DVec3>,
    deltas: Vec<[DVec3; TARGETS]>,
    idx: Vec<u32>,
}

impl Part {
    fn push(&mut self, pos: DVec3, normal: DVec3, deltas: [DVec3; TARGETS]) -> u32 {
        self.pos.push(pos);
        self.normal.push(normal);
        self.deltas.push(deltas);
        u32::try_from(self.pos.len() - 1).unwrap_or(u32::MAX)
    }

    /// A vertex `off` above the skin at `(x, y)`, moved by `moves` with the place on the skin re-projected, so a
    /// shifted decal stays on the surface.
    fn decal(&mut self, skull: &Skull, x: f64, y: f64, off: f64, moves: &Moves) -> u32 {
        let p = skull.at(x, y, off);
        let deltas = moves.map(|m| skull.at(x + m.dx, y + m.dy, off) - p + m.extra);
        let n = skull.normal(skull.surface(x, y));
        self.push(p, n, deltas)
    }

    /// A triangle, wound counter-clockwise seen from outside (the vertex normals say where outside is).
    fn tri(&mut self, a: u32, b: u32, c: u32) {
        let (pa, pb, pc) = (self.pos[a as usize], self.pos[b as usize], self.pos[c as usize]);
        let n = (pb - pa).cross(pc - pa);
        if n.length_squared() < 1e-18 {
            return;
        }
        let outside = self.normal[a as usize] + self.normal[b as usize] + self.normal[c as usize];
        let (b, c) = if n.dot(outside) < 0.0 { (c, b) } else { (b, c) };
        self.idx.extend([a, b, c]);
    }

    /// Quads between consecutive rows of vertex indices of equal length.
    fn strip(&mut self, rows: &[Vec<u32>]) {
        for pair in rows.windows(2) {
            for j in 0..pair[0].len() - 1 {
                let (a, b, c, d) = (pair[0][j], pair[0][j + 1], pair[1][j + 1], pair[1][j]);
                self.tri(a, b, c);
                self.tri(a, c, d);
            }
        }
    }

    fn primitive(&self, material: u32) -> Primitive {
        let f = |v: DVec3| v.as_vec3();
        let n = self.pos.len();
        let data = MeshData {
            positions: self.pos.iter().map(|&p| f(p)).collect(),
            normals: self.normal.iter().map(|&v| f(v)).collect(),
            uvs: vec![Vec2::ZERO; n],
            joints: vec![[0; 4]; n],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; n],
            indices: self.idx.clone(),
            ..MeshData::default()
        };
        let mut p = Primitive::new(data);
        p.material = Some(omuzaki_io_api::MaterialIx(material));
        p.morph_targets =
            (0..TARGETS).map(|t| MorphTarget::new(self.deltas.iter().map(|d| f(d[t])).collect())).collect();
        p
    }
}

/// The seven parts, in [`MATERIALS`] order.
#[derive(Default)]
struct Parts {
    skin: Part,
    white: Part,
    iris: Part,
    brow: Part,
    inside: Part,
    lips: Part,
    lash: Part,
}

/// The almond eye's lid curve, 1 at the middle to 0 at the corners; `u` is -1..=1 across the eye.
fn almond(u: f64) -> f64 {
    (1.0 - u.clamp(-1.0, 1.0).powi(2)).powf(0.7)
}

/// The skull: a latitude-longitude ellipsoid with `LON` columns, every other column the mirror of one on the other
/// side, and the jaw field of `Fcl_MTH_A` as its only morph.
fn build_skull(skull: &Skull, part: &mut Part) {
    const LAT: usize = 48;
    const LON: usize = 64;
    let vertex = |part: &mut Part, theta: f64, phi: f64| {
        let w = Skull::width(theta.cos());
        let (s, c) = (theta.sin(), theta.cos());
        let p = DVec3::new(
            skull.centre.x + RX * w * s * phi.sin(),
            skull.centre.y + RY * c,
            skull.centre.z + RZ * w * s * phi.cos(),
        );
        let mut deltas = [DVec3::ZERO; TARGETS];
        deltas[MOUTH_A] = DVec3::new(0.0, -JAW_DROP * jaw_skin(skull, p), 0.0);
        part.push(p, skull.normal(p), deltas)
    };
    let top = vertex(part, 0.0, 0.0);
    let bottom = vertex(part, PI, 0.0);
    let rings: Vec<Vec<u32>> = (1..LAT)
        .map(|i| {
            let theta = PI * i as f64 / LAT as f64;
            let mut ring: Vec<u32> = (0..LON).map(|j| vertex(part, theta, 2.0 * PI * j as f64 / LON as f64)).collect();
            ring.push(ring[0]);
            ring
        })
        .collect();
    for j in 0..LON {
        part.tri(top, rings[0][j + 1], rings[0][j]);
        part.tri(bottom, rings[LAT - 2][j], rings[LAT - 2][j + 1]);
    }
    part.strip(&rings);
}

/// Everything of one eye: white, iris, lash line, brow and the upper lid (a skin patch that slides down to close).
/// `side` is +1 for the character's left eye (+X) and -1 for the right.
fn build_eye(skull: &Skull, side: f64, parts: &mut Parts) {
    let cy = skull.eye_y;
    let x_of = |u: f64| side * (EYE_X + u * EYE_HALF_WIDTH);
    let none = Moves::default();

    // The eye white, an almond: rows from the lower to the upper lid.
    let rows: Vec<Vec<u32>> = (0..=8)
        .map(|k| {
            let t = -1.0 + 0.25 * f64::from(k);
            (0..=20)
                .map(|j| {
                    let u = -1.0 + 0.1 * f64::from(j);
                    let reach = if t >= 0.0 { EYE_TOP } else { EYE_BOTTOM };
                    parts.white.decal(skull, x_of(u), cy + reach * almond(u) * t, OFF_WHITE, &none)
                })
                .collect()
        })
        .collect();
    parts.white.strip(&rows);

    // The iris, an oval disk centred in the eye.
    let centre = parts.iris.decal(skull, x_of(0.0), cy, OFF_IRIS, &none);
    let mut previous: Vec<u32> = vec![centre; 33];
    for ring in 1..=6 {
        let r = f64::from(ring) / 6.0;
        let mut row: Vec<u32> = (0..32)
            .map(|k| {
                let a = 2.0 * PI * f64::from(k) / 32.0;
                parts.iris.decal(
                    skull,
                    side * (EYE_X + 0.008 * r * a.cos()),
                    cy + 0.0095 * r * a.sin(),
                    OFF_IRIS,
                    &none,
                )
            })
            .collect();
        row.push(row[0]);
        parts.iris.strip(&[previous, row.clone()]);
        previous = row;
    }

    // The brow, a tapering arched strip, outer end (t = 0) low; the angry shape lowers it and tilts the inner end down.
    let brow_y = cy + 0.030;
    let rows: Vec<Vec<u32>> = [-1.0, 0.0, 1.0]
        .iter()
        .map(|&v| {
            (0..=12)
                .map(|k| {
                    let t = f64::from(k) / 12.0;
                    let x = side * (EYE_X + 0.017 - 0.034 * t);
                    let thick = 0.0052 * (PI * (0.08 + 0.84 * t)).sin();
                    let y = brow_y + 0.003 * (PI * t).sin() - 0.004 * (1.0 - t) + v * thick / 2.0;
                    let mut m = Moves::default();
                    m[BROW_ANGRY] = Move { dx: -side * 0.0015 * t, dy: -0.003 - 0.010 * t, extra: DVec3::ZERO };
                    parts.brow.decal(skull, x, y, OFF_BROW, &m)
                })
                .collect()
        })
        .collect();
    parts.brow.strip(&rows);

    // The upper lid and the lash line on its lower edge. Open, the lid is a thin skin-coloured strip above the eye;
    // closed, it has slid down to cover the whole eye, its edge sagging into a smile-shaped line.
    let open_edge = |u: f64| cy + EYE_TOP * almond(u) + 0.0006;
    let closed_edge = |u: f64| cy - 0.0015 - EYE_BOTTOM * (1.0 - u.clamp(-1.0, 1.0).powi(2)).sqrt();
    let closed_top = |u: f64| cy + EYE_TOP * almond(u) + 0.0020;
    let columns: Vec<f64> = (0..=24).map(|j| -1.12 + 2.24 * f64::from(j) / 24.0).collect();
    let rows: Vec<Vec<u32>> = (0..=3)
        .map(|r| {
            let v = f64::from(r) / 3.0;
            columns
                .iter()
                .map(|&u| {
                    let height = 0.0070 * (0.3 + 0.7 * almond(u));
                    let y_open = open_edge(u) + height * v;
                    let y_closed = closed_edge(u) + (closed_top(u) - closed_edge(u)) * v;
                    let mut m = Moves::default();
                    m[BLINK].dy = y_closed - y_open;
                    parts.skin.decal(skull, x_of(u), y_open, OFF_LID, &m)
                })
                .collect()
        })
        .collect();
    parts.skin.strip(&rows);
    let rows: Vec<Vec<u32>> = [-1.0, 1.0]
        .iter()
        .map(|&side_of_edge| {
            columns
                .iter()
                .map(|&u| {
                    let thick = 0.0024 * (0.4 + 0.6 * almond(u)) + 0.0014 * u.clamp(0.0, 1.0).powi(2);
                    let shift = side_of_edge * thick / 2.0;
                    let mut m = Moves::default();
                    m[BLINK].dy = closed_edge(u) - open_edge(u);
                    parts.lash.decal(skull, x_of(u), open_edge(u) + shift, OFF_LASH, &m)
                })
                .collect()
        })
        .collect();
    parts.lash.strip(&rows);
}

/// How the mouth targets move a mouth vertex at `(x, y)`.
fn mouth_moves(skull: &Skull, x: f64, y: f64) -> Moves {
    let u = x / MOUTH_HALF_WIDTH;
    let mut m = Moves::default();
    m[MOUTH_A].extra = DVec3::new(0.0, -JAW_DROP * jaw_part(skull, y), 0.0);
    m[MOUTH_U] = Move { dx: -0.45 * x, dy: 0.0, extra: DVec3::new(0.0, 0.0, 0.003) };
    m[MOUTH_JOY] = Move { dx: 0.0015 * u, dy: 0.006 * u * u, extra: DVec3::ZERO };
    m[MOUTH_E] = Move { dx: 0.005 * u, dy: 0.0, extra: DVec3::ZERO };
    m
}

/// The mouth: a dark oval and an upper and a lower lip strip around it. The lips overlap the oval's rim so a closed
/// mouth shows only a thin dark line.
fn build_mouth(skull: &Skull, parts: &mut Parts) {
    const COLUMNS: u32 = 24;
    let my = skull.mouth_y;
    let column = |j: u32| -1.0 + 2.0 * f64::from(j) / f64::from(COLUMNS);
    let round = |u: f64| (1.0 - u * u).max(0.0).sqrt();
    let rows: Vec<Vec<u32>> = (0..=6)
        .map(|k| {
            let t = -1.0 + f64::from(k) / 3.0;
            (0..=COLUMNS)
                .map(|j| {
                    let x = column(j) * MOUTH_HALF_WIDTH;
                    let y = my + 0.0045 * t * round(column(j));
                    parts.inside.decal(skull, x, y, OFF_INSIDE, &mouth_moves(skull, x, y))
                })
                .collect()
        })
        .collect();
    parts.inside.strip(&rows);
    for (sign, depth) in [(1.0, 0.0045), (-1.0, 0.0055)] {
        let rows: Vec<Vec<u32>> = (0..=2)
            .map(|r| {
                let v = f64::from(r) / 2.0;
                (0..=COLUMNS)
                    .map(|j| {
                        let s = round(column(j));
                        let x = column(j) * MOUTH_HALF_WIDTH;
                        let y = my + sign * (0.001 * s + v * depth * s.powf(0.7));
                        parts.lips.decal(skull, x, y, OFF_LIPS, &mouth_moves(skull, x, y))
                    })
                    .collect()
            })
            .collect();
        parts.lips.strip(&rows);
    }
}

/// sRGB byte of a linear channel.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255 first
fn srgb(linear: f32) -> u8 {
    let c = if linear <= 0.003_130_8 { 12.92 * linear } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 };
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The test head: the fixture humanoid plus a skinned `Face` node (mesh `Face`, skinned to `head`) with one primitive
/// per material in [`MATERIALS`] order, the six morph targets of [`TARGET_NAMES`] on every primitive, and the
/// expressions [`PRESETS`], each driving its target. Every target is mirror-symmetric.
pub fn head() -> Fixture {
    let mut f = humanoid(0.0);
    let world = world(&f.scene);
    let bone = f.node("head");
    let bone_at = world[bone.index()].w_axis.truncate().as_dvec3();
    let centre = bone_at + DVec3::Y * CENTRE_ABOVE_BONE;
    let skull = Skull { centre, eye_y: centre.y + EYE_ABOVE_CENTRE, mouth_y: centre.y - MOUTH_BELOW_CENTRE };

    let mut parts = Parts::default();
    build_skull(&skull, &mut parts.skin);
    for side in [1.0, -1.0] {
        build_eye(&skull, side, &mut parts);
    }
    build_mouth(&skull, &mut parts);

    for (name, rgb) in MATERIALS {
        f.scene.add_material(toon_material(name, Vec4::new(rgb[0], rgb[1], rgb[2], 1.0)));
    }
    let order = [&parts.skin, &parts.white, &parts.iris, &parts.brow, &parts.inside, &parts.lips, &parts.lash];
    let mut mesh = Mesh::new("Face");
    mesh.primitives = order.iter().zip(0u32..).map(|(p, m)| p.primitive(m)).collect();
    mesh.morph_target_names = TARGET_NAMES.iter().map(|n| (*n).to_owned()).collect();
    let mesh = f.scene.add_mesh(mesh);
    let skin = f.scene.add_skin(Skin::new("Face_skin", vec![bone], vec![world[bone.index()].inverse()]));
    let mut node = Node::new("Face");
    node.mesh = Some(mesh);
    node.skin = Some(skin);
    let face = f.scene.add_node(node);
    f.scene.attach(f.node("Root"), face);
    f.nodes.insert("Face".to_owned(), face);

    if let Some(avatar) = f.scene.avatar.as_mut() {
        for (t, preset) in (0u32..).zip(PRESETS) {
            if let Ok(name) = ExpressionName::parse(preset) {
                avatar.expressions.insert(name, Expression::morph(face, t));
            }
        }
    }
    f
}

/// Writes the face mesh of `scene` posed with expression `name` at `weight` (the expression's morph binds, each
/// delta times `weight` times its bind weight, summed) as an OBJ: `v` lines, a `g <material>` block of `f` lines per
/// primitive, and `# colour <material> r g b` lines (sRGB bytes) for `tools/arkit-shape-sheet.py`. Weight 0 writes the
/// neutral face. Panics with a message if the scene has no `Face` node or no such expression: this is test tooling.
#[allow(clippy::disallowed_methods)] // test tooling: the dump is a plain file the sheet script reads
pub fn dump_expression(scene: &IoScene, name: &str, weight: f32, path: &Path) {
    let face = scene.nodes.iter().position(|n| n.name == "Face").expect("a Face node");
    let mesh = &scene.meshes[scene.nodes[face].mesh.expect("a mesh on Face").index()];
    let expression = scene
        .avatar
        .as_ref()
        .and_then(|a| a.expressions.get(&ExpressionName::parse(name).expect("an expression name")))
        .expect("the expression");
    let mut out = String::new();
    for p in &mesh.primitives {
        let m = &scene.materials[p.material.expect("a material").index()];
        let [r, g, b] = [m.base_color.x, m.base_color.y, m.base_color.z].map(srgb);
        writeln!(out, "# colour {} {r} {g} {b}", m.name).expect("write to a string");
    }
    let mut first = 1usize;
    let mut blocks = String::new();
    for p in &mesh.primitives {
        for (v, &base) in p.mesh.positions.iter().enumerate() {
            let at = expression
                .morph_binds
                .iter()
                .filter(|b| b.node.index() == face)
                .fold(base, |acc, b| acc + p.morph_targets[b.index as usize].positions[v] * (weight * b.weight));
            writeln!(out, "v {:.6} {:.6} {:.6}", at.x, at.y, at.z).expect("write to a string");
        }
        let m = &scene.materials[p.material.expect("a material").index()];
        writeln!(blocks, "g {}", m.name).expect("write to a string");
        for t in p.mesh.indices.as_chunks::<3>().0 {
            writeln!(blocks, "f {} {} {}", first + t[0] as usize, first + t[1] as usize, first + t[2] as usize)
                .expect("write to a string");
        }
        first += p.mesh.positions.len();
    }
    out.push_str(&blocks);
    std::fs::write(path, out).expect("write the OBJ");
}
