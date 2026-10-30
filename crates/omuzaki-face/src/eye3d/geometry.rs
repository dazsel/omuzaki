//! The eye's geometry, in the eye's own frame (metres, the eyeball's centre at the origin, `+z` out of the face, `+x`
//! toward the inner corner, `+y` up):
//!
//! - **The eyeball**: a sphere whose front cap is the cornea, a smaller sphere bulging in front of the iris. The iris is
//!   not geometry: the `eye` shader finds it behind the cornea.
//! - **The lids and the skin round the eye**: rings laid out from the opening between the lids outward. Toward the eye
//!   each lid has a rounded margin of its own thickness that tucks in against the eyeball, and behind the margin a pocket
//!   (the conjunctiva) closes the gap round the ball's edge; outward the lids lie on the ball, the upper one folds at its
//!   crease, and both blend into the brow, the nose's side and the cheek.
//! - **The caruncle**: the pink mound in the inner corner.
//! - **The occlusion shell**: a film just over the eyeball inside the opening, dark at the lids' edges and clear a few
//!   millimetres in: the contact shadow where the lids meet the eye.
//! - **The lashes**: tapering ribbons rooted along each lid's front edge, curling away from the eye and gathering
//!   into clumps.

use std::f32::consts::PI;

use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::mesh::{ExtraAttribute, MeshData};

use super::design::{Aperture, Ball, EyeDesign, Lashes};
use super::noise::{Pcg, smoothstep};
use super::textures::SCLERA_REACH;

const MM: f32 = 0.001;
/// The limbus as a share of the iris's radius: the cornea is a little wider than the iris (the shader's `limbus`).
pub const LIMBUS: f32 = 1.04;
/// The eye's surface under the lids, for the lids to lie on: the eyeball's sphere widened toward the corners, where
/// the opening runs past the ball.
const PROXY_WIDEN: f32 = 1.12;
/// How far the lid margin reaches from the opening's edge to the lid's full thickness.
const MARGIN: f32 = 0.65 * MM;
/// Distances from the opening's edge, outward (negative: the pocket behind the margin), at which the rings sit.
const RINGS: [f32; 31] = [
    -3.2, -2.0, -1.2, -0.7, -0.35, -0.12, 0.0, 0.08, 0.18, 0.3, 0.45, 0.62, 0.8, 1.0, 1.3, 1.7, 2.2, 2.8, 3.6, 4.6,
    5.8, 7.0, 8.3, 9.8, 11.5, 13.5, 16.0, 19.0, 22.5, 26.0, 30.0,
];
/// Samples round the opening.
const AROUND: usize = 240;
const SKIN_INFO: &str = "skin-info";

/// Everything built from one design.
#[derive(Clone, Debug)]
pub struct EyeMeshes {
    /// The eyeball and cornea (the `eye` program).
    pub eyeball: MeshData,
    /// The lids, the skin round the eye and the caruncle (the `skin` program, `skin-info`).
    pub region: MeshData,
    /// The occlusion shell: black, its alpha in the vertex colours (blended, unlit).
    pub shell: MeshData,
    /// The lashes (the `skin` program with the lash parameters).
    pub lashes: MeshData,
}

/// Builds the eye's meshes. `view_from` is where the lashes are seen from, in the eye's frame: a ribbon turns to face
/// it. `gaze` is where the eyeball looks, in the same frame (`+z` straight ahead): the eyeball itself is built looking
/// down `+z` and turned by its instance's transform, and the occlusion shell follows the turned cornea.
pub fn meshes(design: &EyeDesign, view_from: Vec3, gaze: Vec3) -> EyeMeshes {
    let lids = Lids::new(design);
    EyeMeshes {
        eyeball: eyeball(&design.ball),
        region: lids.region(),
        shell: lids.shell(gaze.normalize_or(Vec3::Z)),
        lashes: lids.lashes(&design.lashes, design.seed, view_from),
    }
}

/// The cornea: its edge's radius and height, its sphere's radius and its centre's height.
struct Cornea {
    edge: f32,
    radius: f32,
    centre_z: f32,
}

impl Cornea {
    fn new(ball: &Ball) -> Self {
        let edge = ball.iris_radius * LIMBUS;
        let edge_z = (ball.radius * ball.radius - edge * edge).max(0.0).sqrt();
        let apex = ball.radius * (1.0 + ball.cornea_rise);
        let sag = (apex - edge_z).max(1e-6);
        let radius = (edge * edge + sag * sag) / (2.0 * sag);
        Self { edge, radius, centre_z: apex - radius }
    }

    /// The outer surface's height at `(x, y)` with the eye looking along `gaze`: the higher of the ball and the
    /// cornea's sphere (which lies inside the ball everywhere but over the cornea).
    fn surface_z(&self, ball: &Ball, gaze: Vec3, x: f32, y: f32) -> f32 {
        let sphere = (ball.radius * ball.radius - x * x - y * y).max(0.0).sqrt();
        let c = gaze * self.centre_z;
        let rest = self.radius * self.radius - (x - c.x).powi(2) - (y - c.y).powi(2);
        if rest > 0.0 { sphere.max(c.z + rest.sqrt()) } else { sphere }
    }
}

/// The eyeball: rings from the cornea's apex back, the front cap on the cornea's sphere.
pub fn eyeball(ball: &Ball) -> MeshData {
    let cornea = Cornea::new(ball);
    let limbus_theta = (cornea.edge / ball.radius).clamp(-1.0, 1.0).asin();
    let alpha_edge = (cornea.edge / cornea.radius).clamp(-1.0, 1.0).asin();
    let segments = 160usize;
    // Polar angles of the rings: dense over the cornea and round the limbus.
    let mut thetas = Vec::new();
    for i in 1..=28 {
        #[allow(clippy::cast_precision_loss)]
        thetas.push(limbus_theta * (i as f32 / 28.0));
    }
    for i in 1..=60 {
        #[allow(clippy::cast_precision_loss)]
        let t = i as f32 / 60.0;
        thetas.push(limbus_theta + (PI - limbus_theta) * t * t);
    }
    let mut mesh = MeshData::default();
    let apex = Vec3::new(0.0, 0.0, cornea.centre_z + cornea.radius);
    mesh.positions.push(apex);
    mesh.normals.push(Vec3::Z);
    mesh.uvs.push(Vec2::new(0.75, 0.5));
    for &theta in &thetas {
        for j in 0..segments {
            #[allow(clippy::cast_precision_loss)]
            let phi = 2.0 * PI * j as f32 / segments as f32;
            let (s, c) = phi.sin_cos();
            let (p, n) = if theta <= limbus_theta + 1e-6 {
                let alpha = alpha_edge * theta / limbus_theta;
                let n = Vec3::new(alpha.sin() * c, alpha.sin() * s, alpha.cos());
                (Vec3::new(0.0, 0.0, cornea.centre_z) + n * cornea.radius, n)
            } else {
                let n = Vec3::new(theta.sin() * c, theta.sin() * s, theta.cos());
                (n * ball.radius, n)
            };
            mesh.positions.push(p);
            mesh.normals.push(n);
            let rho = (theta / SCLERA_REACH).min(1.0);
            mesh.uvs.push(Vec2::new(0.75 + 0.24 * rho * c, 0.5 - 0.48 * rho * s));
        }
    }
    let ring = |i: usize, j: usize| u32::try_from(1 + i * segments + j % segments).unwrap_or(0);
    for j in 0..segments {
        mesh.indices.extend_from_slice(&[0, ring(0, j), ring(0, j + 1)]);
    }
    for i in 0..thetas.len() - 1 {
        for j in 0..segments {
            let (a, b, c, d) = (ring(i, j), ring(i, j + 1), ring(i + 1, j + 1), ring(i + 1, j));
            mesh.indices.extend_from_slice(&[a, d, c, a, c, b]);
        }
    }
    mesh
}

/// A bump that is 0 at `u = 0` and `u = 1` and 1 at `u = peak`.
fn bump(u: f32, peak: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    let k = (0.5f32).ln() / peak.clamp(0.05, 0.95).ln();
    (PI * u.powf(k)).sin().max(0.0)
}

fn gauss(x: f32) -> f32 {
    (-x * x).exp()
}

/// The lids: the opening's outline and the surface laid out from it.
struct Lids {
    ball: Ball,
    aperture: Aperture,
    thickness: f32,
    crease: f32,
    crease_depth: f32,
    /// The opening's outline, `AROUND` points: the upper lid from the outer corner to the inner one, then the lower
    /// lid back.
    edge: Vec<Vec2>,
    centre: Vec2,
}

impl Lids {
    fn new(design: &EyeDesign) -> Self {
        let a = design.aperture.clone();
        let half = AROUND / 2;
        let mut edge = Vec::with_capacity(AROUND);
        for k in 0..AROUND {
            let upper = k < half;
            #[allow(clippy::cast_precision_loss)]
            let u = if upper { k as f32 / half as f32 } else { 1.0 - (k - half) as f32 / half as f32 };
            edge.push(Self::outline(&a, u, upper));
        }
        let centre = Vec2::new(f32::midpoint(a.inner_x, a.outer_x), 0.25 * a.upper - 0.25 * a.lower);
        Self {
            ball: design.ball.clone(),
            aperture: a,
            thickness: design.skin.lid_thickness,
            crease: design.skin.crease,
            crease_depth: design.skin.crease_depth,
            edge,
            centre,
        }
    }

    /// A point of the opening's outline at `u` (0 the outer corner, 1 the inner) on the upper or lower lid.
    fn outline(a: &Aperture, u: f32, upper: bool) -> Vec2 {
        let x = a.outer_x + (a.inner_x - a.outer_x) * u;
        let base = a.outer_y + (a.inner_y - a.outer_y) * u;
        // The inner corner is rounder than the outer one.
        let round = 1.0 - 0.35 * smoothstep(0.7, 1.0, u);
        let y = if upper {
            base + a.upper * bump(u, a.upper_peak).powf(0.85 * round)
        } else {
            base - a.lower * bump(u, a.lower_peak).powf(1.1 * round)
        };
        Vec2::new(x, y)
    }

    /// Whether outline sample `k` is on the upper lid.
    fn is_upper(k: usize) -> bool {
        k < AROUND / 2
    }

    /// Where along its lid sample `k` sits: 0 the outer corner, 1 the inner.
    fn along(k: usize) -> f32 {
        let half = AROUND / 2;
        #[allow(clippy::cast_precision_loss)]
        if k < half { k as f32 / half as f32 } else { 1.0 - (k - half) as f32 / half as f32 }
    }

    /// The outer boundary's point matching outline sample `k`. Its angle round the centre follows the sample's place
    /// along its lid (the outer corner at the far left, the inner at the right, each lid sweeping its half), so the
    /// lines out from the outline fan evenly instead of bunching where the outline turns at the corners.
    fn boundary(&self, k: usize) -> Vec2 {
        let u = Self::along(k);
        let angle = if Self::is_upper(k) { PI * (1.0 - u) } else { PI * (1.0 + u) };
        // Wider toward the temple, so a view from the nose's side sees skin past the far corner.
        let rx = if angle.cos() < 0.0 { 56.0 * MM } else { 42.0 * MM };
        let ry = 34.0 * MM;
        self.centre + Vec2::new(rx * angle.cos(), ry * angle.sin() + 2.0 * MM)
    }

    /// The point `d` metres out from outline sample `k` (negative: into the pocket), in the frontal plane.
    fn plane_point(&self, k: usize, d: f32) -> Vec2 {
        let e = self.edge[k];
        if d < 0.0 {
            return e + (self.centre - e).normalize_or_zero() * (-d);
        }
        let b = self.boundary(k);
        let span = (b - e).length().max(1e-6);
        e + (b - e) * (d / span).min(1.0)
    }

    /// The eye's surface under the lids at `(x, y)`, pushed out by `out` metres.
    fn proxy(&self, x: f32, y: f32, out: f32) -> f32 {
        let r = self.ball.radius + out;
        // Past the ball's edge the surface eases onto a floor instead of breaking off it: a smooth maximum.
        let q = r * r - (x / PROXY_WIDEN).powi(2) - y * y;
        let floor = (0.45 * r).powi(2);
        let soft = 0.3 * r * r;
        (0.5 * (q + floor + ((q - floor).powi(2) + soft * soft).sqrt())).sqrt()
    }

    /// The face round the eye, far from the lids: the brow ridge above, the nose's side toward the inner corner, the
    /// temple falling away past the outer one and the cheek below.
    fn face(&self, x: f32, y: f32) -> f32 {
        let base = self.ball.radius + self.thickness - 0.8 * MM;
        let curve = -(x * x) / (2.0 * 85.0 * MM) - (y * y) / (2.0 * 90.0 * MM);
        let brow = 3.6 * MM * smoothstep(9.0 * MM, 20.0 * MM, y);
        let nose = 0.55 * (x - 14.0 * MM).max(0.0) * smoothstep(-22.0 * MM, -4.0 * MM, y);
        let temple = -0.22 * (-x - 14.0 * MM).max(0.0);
        let cheek = 1.8 * MM * smoothstep(-9.0 * MM, -22.0 * MM, y);
        base + curve + brow + nose + temple + cheek
    }

    /// The distance from `p` to the opening's outline, and whether the nearest point is on the upper lid and where
    /// along its lid it lies (0 the outer corner, 1 the inner).
    fn nearest(&self, p: Vec2) -> (f32, bool, f32) {
        let mut best = (f32::INFINITY, true, 0.0);
        for j in 0..AROUND {
            let next = (j + 1) % AROUND;
            let (a, b) = (self.edge[j], self.edge[next]);
            let ab = b - a;
            let t = ((p - a).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
            let dist = (a + ab * t - p).length();
            if dist < best.0 {
                let end = if t < 0.5 { j } else { next };
                best = (dist, Self::is_upper(end), Self::along(j) + (Self::along(next) - Self::along(j)) * t);
            }
        }
        best
    }

    /// The surface point `d` metres from outline sample `k`, and its skin info (wet, cavity, flush).
    fn surface(&self, k: usize, d: f32) -> (Vec3, [f32; 4]) {
        let p = self.plane_point(k, d);
        // Outside the opening, the lids' shapes follow the true distance to its outline rather than the distance along
        // this sample's line out: past a sharp corner the lines out run nearly parallel, and measuring along them would
        // fold each shape into a chevron with a ridge running out of the corner.
        let (d, upper, u) = if d > 0.0 { self.nearest(p) } else { (d, Self::is_upper(k), Self::along(k)) };
        // The margin: tucked under the ball's surface in the pocket, rounding up to the lid's thickness.
        let offset = if d <= 0.0 {
            0.15 * MM + 1.05 * d
        } else {
            0.15 * MM + (self.thickness - 0.15 * MM) * (0.5 * PI * (d / MARGIN).min(1.0)).sin()
        };
        let mut z = self.proxy(p.x, p.y, offset);
        // The upper lid's crease and the skin above it folding forward; the lower lid's gentle rise to the cheek. Toward
        // the corners each lid's shape eases to the two lids' mean, so the lids meet there without a seam.
        let fold = gauss((d - self.crease) / (1.4 * MM));
        // The skin above the crease hangs forward over it, so the fold reads as a shelf with a shadow under it.
        let hood = 2.2 * MM * smoothstep(self.crease - 0.5 * MM, self.crease + 3.5 * MM, d);
        let upper_z = -self.crease_depth * fold + hood;
        let lower_z = 0.5 * MM * smoothstep(2.0 * MM, 7.0 * MM, d);
        let own = smoothstep(0.0, 0.3, u.min(1.0 - u));
        let side = |upper_v: f32, lower_v: f32| {
            let mean = f32::midpoint(upper_v, lower_v);
            mean + ((if upper { upper_v } else { lower_v }) - mean) * own
        };
        z += side(upper_z, lower_z);
        // The lash line: the roots' dense shadow along each lid's front edge, darker on the upper lid.
        let lash_line = gauss((d - 0.55 * MM) / (0.3 * MM));
        let mut cavity = 1.0 - side(0.5 * fold + 0.45 * lash_line, 0.25 * lash_line);
        // The lids blend into the face round them, sooner at the corners.
        let corner = (2.0 * (u - 0.5)).abs();
        let reach = side(14.0, 9.0) * MM * (1.0 - 0.45 * corner);
        let blend = smoothstep(0.35 * reach, reach, d);
        z = z + (self.face(p.x, p.y) - z) * blend;
        let wet = 1.0 - smoothstep(0.2 * MM, 0.45 * MM, d);
        if d < 0.0 {
            cavity = 0.55;
        }
        // The inner corner and the lid margins are flushed.
        let inner = smoothstep(0.75, 1.0, u) * (1.0 - smoothstep(0.0, 6.0 * MM, d));
        let flush = 0.35 * (1.0 - smoothstep(0.0, 5.0 * MM, d)) + 0.5 * inner;
        (Vec3::new(p.x, p.y, z), [wet, cavity, flush, 0.0])
    }

    /// The lids and the skin round the eye, and the caruncle.
    fn region(&self) -> MeshData {
        let mut mesh = MeshData::default();
        let mut info = Vec::new();
        let rings = RINGS.len();
        for &d in &RINGS {
            for k in 0..AROUND {
                let (p, i) = self.surface(k, d * MM);
                mesh.positions.push(p);
                mesh.uvs.push(skin_uv(p));
                info.extend_from_slice(&i);
            }
        }
        let at = |r: usize, k: usize| u32::try_from(r * AROUND + k % AROUND).unwrap_or(0);
        for r in 0..rings - 1 {
            for k in 0..AROUND {
                // Ring `r + 1` is farther out; the outline runs clockwise seen from the front (upper lid outer to inner
                // with +x inward... see `Lids::new`), so the winding is chosen to face +z.
                let (a, b, c, d) = (at(r, k), at(r, k + 1), at(r + 1, k + 1), at(r + 1, k));
                mesh.indices.extend_from_slice(&[a, c, d, a, b, c]);
            }
        }
        self.caruncle(&mut mesh, &mut info);
        mesh.normals = smooth_normals(&mesh.positions, &mesh.indices);
        orient_toward(&mut mesh, Vec3::Z);
        // The skin program scatters light round the surface's curvature.
        for (i, c) in curvature(&mesh.positions, &mesh.normals, &mesh.indices).into_iter().enumerate() {
            info[i * 4 + 3] = c;
        }
        self.meniscus(&mut mesh, &mut info);
        mesh.extra.push(ExtraAttribute { name: slug(SKIN_INFO), components: 4, data: info });
        mesh
    }

    /// The tear meniscus: a thin wet bead where each lid's margin meets the eye, fuller along the lower lid and
    /// thinning out at the corners. It catches the light as a bright line.
    fn meniscus(&self, mesh: &mut MeshData, info: &mut Vec<f32>) {
        const SIDES: usize = 8;
        let first = mesh.positions.len();
        for k in 0..AROUND {
            let u = Self::along(k);
            let full = if Self::is_upper(k) { 0.06 } else { 0.14 } * MM;
            let radius = 0.02 * MM + full * bump(u, 0.5).powf(0.5);
            let p = self.plane_point(k, -0.05 * MM);
            let centre = Vec3::new(p.x, p.y, self.proxy(p.x, p.y, 0.04 * MM));
            let next = self.edge[(k + 1) % AROUND];
            let prev = self.edge[(k + AROUND - 1) % AROUND];
            let tangent = Vec3::new(next.x - prev.x, next.y - prev.y, 0.0).normalize_or(Vec3::X);
            let out = centre.normalize_or(Vec3::Z);
            let side = tangent.cross(out).normalize_or(Vec3::Y);
            for j in 0..SIDES {
                #[allow(clippy::cast_precision_loss)]
                let phi = 2.0 * PI * j as f32 / SIDES as f32;
                let n = out * phi.cos() + side * phi.sin();
                let q = centre + n * radius;
                mesh.positions.push(q);
                mesh.normals.push(n);
                mesh.uvs.push(skin_uv(q));
                info.extend_from_slice(&[1.0, 1.0, 0.5, 1.0 / radius]);
            }
        }
        let at = |k: usize, j: usize| u32::try_from(first + (k % AROUND) * SIDES + j % SIDES).unwrap_or(0);
        for k in 0..AROUND {
            for j in 0..SIDES {
                let (a, b, c, d) = (at(k, j), at(k + 1, j), at(k + 1, j + 1), at(k, j + 1));
                // Wind each quad to face along its normal.
                let pa = mesh.positions[a as usize];
                let facing = (mesh.positions[b as usize] - pa).cross(mesh.positions[c as usize] - pa);
                if facing.dot(mesh.normals[a as usize]) >= 0.0 {
                    mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
                } else {
                    mesh.indices.extend_from_slice(&[a, c, b, a, d, c]);
                }
            }
        }
    }

    /// The caruncle: a small wet mound in the inner corner.
    fn caruncle(&self, mesh: &mut MeshData, info: &mut Vec<f32>) {
        let a = &self.aperture;
        let at = Vec2::new(a.inner_x - 1.3 * MM, a.inner_y + 0.3 * MM);
        let centre = Vec3::new(at.x, at.y, self.proxy(at.x, at.y, -1.5 * MM));
        // Low and flattened against the corner's floor: a wet fold, not a bead.
        let radii = Vec3::new(1.5 * MM, 1.0 * MM, 0.45 * MM);
        let first = u32::try_from(mesh.positions.len()).unwrap_or(0);
        let (rings, segments) = (12u32, 20u32);
        for i in 0..=rings {
            #[allow(clippy::cast_precision_loss)]
            let theta = PI * i as f32 / rings as f32;
            for j in 0..segments {
                #[allow(clippy::cast_precision_loss)]
                let phi = 2.0 * PI * j as f32 / segments as f32;
                let n = Vec3::new(theta.sin() * phi.cos(), theta.sin() * phi.sin(), theta.cos());
                // Lumpy: a few bumps over its surface.
                let lump = 1.0 + 0.08 * (phi * 3.0).sin() * (theta * 4.0).sin();
                let p = centre + n * radii * lump;
                mesh.positions.push(p);
                mesh.uvs.push(skin_uv(p));
                info.extend_from_slice(&[0.85, 0.8, 1.0, 0.0]);
            }
        }
        let idx = |i: u32, j: u32| first + i * segments + j % segments;
        for i in 0..rings {
            for j in 0..segments {
                let (a, b, c, d) = (idx(i, j), idx(i, j + 1), idx(i + 1, j + 1), idx(i + 1, j));
                mesh.indices.extend_from_slice(&[a, d, c, a, c, b]);
            }
        }
    }

    /// The occlusion shell: a film over the eyeball inside the opening, its alpha in the vertex colours.
    fn shell(&self, gaze: Vec3) -> MeshData {
        let cornea = Cornea::new(&self.ball);
        let mut mesh = MeshData::default();
        let depths = [0.0, 0.15, 0.35, 0.6, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.0, 6.0, 7.0];
        for &d in &depths {
            for k in 0..AROUND {
                let p = self.plane_point(k, -d * MM);
                let z = cornea.surface_z(&self.ball, gaze, p.x, p.y).max(self.proxy(p.x, p.y, -0.6 * MM)) + 0.06 * MM;
                mesh.positions.push(Vec3::new(p.x, p.y, z));
                // The upper lid shades more and farther than the lower, and the corners, deep in the socket, most.
                let (strength, reach) = if Self::is_upper(k) { (0.85, 3.0) } else { (0.65, 1.6) };
                let socket = 1.0 - smoothstep(0.0, 0.25, Self::along(k).min(1.0 - Self::along(k)));
                let alpha =
                    (strength * (-d / (reach * (1.0 + socket))).exp() * (1.0 - smoothstep(5.0, 7.0, d))).min(0.92);
                mesh.colours.push([0.0, 0.0, 0.0, alpha]);
            }
        }
        let at = |r: usize, k: usize| u32::try_from(r * AROUND + k % AROUND).unwrap_or(0);
        for r in 0..depths.len() - 1 {
            for k in 0..AROUND {
                let (a, b, c, d) = (at(r, k), at(r, k + 1), at(r + 1, k + 1), at(r + 1, k));
                mesh.indices.extend_from_slice(&[a, d, c, a, c, b]);
            }
        }
        mesh.normals = smooth_normals(&mesh.positions, &mesh.indices);
        orient_toward(&mut mesh, Vec3::Z);
        mesh
    }

    /// The lashes along both lids.
    fn lashes(&self, lashes: &Lashes, seed: u32, view_from: Vec3) -> MeshData {
        let mut mesh = MeshData::default();
        let mut info = Vec::new();
        let mut rng = Pcg::new(seed, 0x1a5e);
        for upper in [true, false] {
            let count = if upper { lashes.upper } else { lashes.lower };
            // Roots at uneven spacing: lashes grow in loose runs with gaps, never as a comb.
            let gaps: Vec<f32> = (0..count).map(|_| rng.range(0.25, 1.0).powi(2) + 0.08).collect();
            let total: f32 = gaps.iter().sum::<f32>().max(1e-6);
            let mut strands: Vec<Vec<Vec3>> = Vec::new();
            let mut at = 0.0;
            for gap in &gaps {
                at += gap / total;
                let u = 0.03 + 0.92 * at;
                // The lower lid's lashes thin out toward the inner corner.
                if !upper && rng.unit() < 0.85 * smoothstep(0.62, 0.95, u) {
                    continue;
                }
                strands.push(self.strand(lashes, upper, u, &mut rng));
            }
            clump(&mut strands, lashes.clumping, &mut rng);
            let width = lashes.thickness * if upper { 1.0 } else { 0.75 };
            for strand in &strands {
                ribbon(&mut mesh, &mut info, strand, width, view_from);
            }
        }
        mesh.extra.push(ExtraAttribute { name: slug(SKIN_INFO), components: 4, data: info });
        mesh
    }

    /// One lash's centre line, rooted at `u` along a lid (0 outer, 1 inner).
    fn strand(&self, lashes: &Lashes, upper: bool, u: f32, rng: &mut Pcg) -> Vec<Vec3> {
        // The outline sample at `u` on this lid, and its neighbour toward the outer corner.
        let half = AROUND / 2;
        // In range: u in 0..1.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let k = if upper {
            ((u * half as f32) as usize).min(half - 1)
        } else {
            (half + ((1.0 - u) * half as f32) as usize).min(AROUND - 1)
        };
        // Rooted along the margin's front edge, in two or three loose rows.
        let row = rng.range(0.38, 0.62) * MM;
        let (root, _) = self.surface(k, row);
        let (ahead, _) = self.surface(k, row + 0.4 * MM);
        let out = (ahead - root).normalize_or_zero();
        let normal = root.normalize_or_zero();
        let up = if upper { Vec3::Y } else { -Vec3::Y };
        // Outer lashes sweep toward the outer corner.
        let sweep = Vec3::new(-1.0, 0.0, 0.0) * (0.42 * (1.0 - u)).powi(2)
            + Vec3::new(0.25, 0.0, 0.0) * smoothstep(0.8, 1.0, u);
        let length = if upper { lashes.upper_length } else { lashes.lower_length }
            * (0.3 + 0.7 * bump(u, if upper { 0.38 } else { 0.4 }).powf(0.6))
            * if rng.unit() < 0.15 { rng.range(0.35, 0.7) } else { rng.range(0.75, 1.08) };
        let start = (normal * 0.9 + out * 0.12 - up * 0.2 + sweep * 0.7).normalize_or_zero();
        let curl = (up * 1.2 - normal * 0.35).normalize_or_zero()
            * lashes.curl
            * if upper { 1.0 } else { 0.5 }
            * rng.range(0.7, 1.2);
        // A few strays point their own way.
        let wander = if rng.unit() < 0.05 { 0.35 } else { 0.1 };
        let jitter = Vec3::new(rng.normal(), rng.normal(), rng.normal()) * wander;
        (0..=10)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32 / 10.0;
                root + length * ((start + jitter) * t + curl * t * t)
            })
            .collect()
    }
}

/// Pulls each run of neighbouring strands toward their shared tip, more toward the tip.
fn clump(strands: &mut [Vec<Vec3>], clumping: f32, rng: &mut Pcg) {
    let mut i = 0;
    while i < strands.len() {
        // In range: a small count.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let size = (rng.range(2.0, 7.0) as usize).min(strands.len() - i);
        let group = &mut strands[i..i + size];
        #[allow(clippy::cast_precision_loss)]
        let tip = group.iter().filter_map(|s| s.last().copied()).sum::<Vec3>() / size as f32;
        for strand in group.iter_mut() {
            let own = strand.last().copied().unwrap_or(tip);
            let n = strand.len().max(2) - 1;
            for (j, p) in strand.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let t = j as f32 / n as f32;
                *p += (tip - own) * clumping.clamp(0.0, 1.0) * t.powf(1.6);
            }
        }
        i += size;
    }
}

/// A ribbon along `line`, tapering from `width` to a point, turned to face `view_from`.
fn ribbon(mesh: &mut MeshData, info: &mut Vec<f32>, line: &[Vec3], width: f32, view_from: Vec3) {
    let first = u32::try_from(mesh.positions.len()).unwrap_or(0);
    let n = line.len();
    for (i, &p) in line.iter().enumerate() {
        let tangent = (line[(i + 1).min(n - 1)] - line[i.saturating_sub(1)]).normalize_or_zero();
        let view = (view_from - p).normalize_or_zero();
        let side = tangent.cross(view).normalize_or_zero();
        let normal = side.cross(tangent).normalize_or_zero();
        #[allow(clippy::cast_precision_loss)]
        let t = i as f32 / (n - 1) as f32;
        let w = width * (1.0 - 0.92 * t.powf(1.3));
        for s in [-0.5, 0.5] {
            mesh.positions.push(p + side * w * s);
            mesh.normals.push(normal);
            mesh.uvs.push(Vec2::new(s + 0.5, t));
            // Darker toward the root, where the lash is thickest and its neighbours shade it.
            info.extend_from_slice(&[0.0, 0.45 + 0.55 * t.sqrt(), 0.0, 0.0]);
        }
    }
    for i in 0..u32::try_from(n - 1).unwrap_or(0) {
        let (a, b, c, d) = (first + 2 * i, first + 2 * i + 1, first + 2 * i + 3, first + 2 * i + 2);
        mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
    }
}

/// The skin texture's coordinates for a point: a planar map of the patch round the eye.
pub fn skin_uv(p: Vec3) -> Vec2 {
    Vec2::new((p.x + SKIN_HALF.x) / (2.0 * SKIN_HALF.x), (SKIN_TOP - p.y) / (2.0 * SKIN_HALF.y))
}

/// The skin texture covers `x` in `-SKIN_HALF.x..SKIN_HALF.x` and `y` from `SKIN_TOP` down `2 * SKIN_HALF.y`.
pub const SKIN_HALF: Vec2 = Vec2::new(0.036, 0.032);
/// The top edge of the skin texture, metres.
pub const SKIN_TOP: f32 = 0.034;

fn slug(name: &str) -> Slug {
    // The names here are constants that are valid slugs.
    Slug::new(name).unwrap_or_else(|_| Slug::new("skin-info").unwrap_or_else(|_| unreachable!()))
}

/// Each vertex's curvature (1/m): how fast the normal turns along the edges round it, averaged.
fn curvature(positions: &[Vec3], normals: &[Vec3], indices: &[u32]) -> Vec<f32> {
    let mut sum = vec![0.0f32; positions.len()];
    let mut count = vec![0.0f32; positions.len()];
    for t in indices.as_chunks::<3>().0 {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let (a, b) = (a as usize, b as usize);
            let length = (positions[a] - positions[b]).length();
            if length > 1e-7 {
                let k = (normals[a] - normals[b]).length() / length;
                for v in [a, b] {
                    sum[v] += k;
                    count[v] += 1.0;
                }
            }
        }
    }
    sum.iter().zip(&count).map(|(s, c)| if *c > 0.0 { s / c } else { 0.0 }).collect()
}

/// Area-weighted vertex normals.
fn smooth_normals(positions: &[Vec3], indices: &[u32]) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; positions.len()];
    for t in indices.as_chunks::<3>().0 {
        let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
        let n = (positions[b] - positions[a]).cross(positions[c] - positions[a]);
        normals[a] += n;
        normals[b] += n;
        normals[c] += n;
    }
    normals.iter().map(|n| n.normalize_or(Vec3::Z)).collect()
}

/// Flips the winding (and the normals) if most of the surface faces away from `toward`.
fn orient_toward(mesh: &mut MeshData, toward: Vec3) {
    let facing: f32 = mesh.normals.iter().map(|n| n.dot(toward)).sum();
    if facing < 0.0 {
        for t in mesh.indices.as_chunks_mut::<3>().0 {
            t.swap(1, 2);
        }
        for n in &mut mesh.normals {
            *n = -*n;
        }
    }
}
