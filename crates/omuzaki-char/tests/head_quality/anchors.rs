//! What sits on the head lands on the head as built: ear roots ([`ears::root`]) and hair roots
//! ([`GuidePoint::resolve`]) placed on the surface the head build returns and the head part publishes lie on the mesh.
//! The reshaped lower face pulls the side of the head up to about 1 cm inside the cranium at ear height, where anchors
//! placed on the cranium floated.
//!
//! The distance is measured along each anchor's own ray from the head centre, against the mesh's triangles.

use omuzaki_char::head::ears;
use omuzaki_char_api::hair::GuidePoint;
use omuzaki_char_api::{Ears, HeadParams, HeadSurface};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::mesh::MeshData;

/// How far an anchor may sit from the mesh, either way, m.
pub const ON_SURFACE: f64 = 0.001;
/// The hair roots tried: every 5 degrees from the crown down to 125 (past the ears; the prototype's lowest hairline is
/// at 118), every 15 degrees round.
const ROOT_THETA_STEP: u32 = 5;
const ROOT_THETA_MAX: u32 = 125;
const ROOT_PHI_STEP: u32 = 15;
/// Polar rows of the ray caster, degrees.
const ROW_DEG: f64 = 1.0;

/// Both ear roots and every hair root tried, placed on `head`, bind space.
pub fn anchors(ears: &Ears, head: &dyn HeadSurface) -> Vec<Vec3> {
    let [x, y, z] = ears.position;
    let right = Ears { position: [-x, y, z], ..ears.clone() };
    let mut out = vec![ears::root(ears, head), ears::root(&right, head)];
    for theta in (0..=ROOT_THETA_MAX).step_by(ROOT_THETA_STEP as usize) {
        for phi in (0..360).step_by(ROOT_PHI_STEP as usize) {
            out.push(GuidePoint::Surface([f64::from(theta), f64::from(phi), 0.0]).resolve(head));
        }
    }
    out
}

/// The largest distance between an anchor and the mesh, measured along the anchor's ray from `centre`, m (infinite
/// where a ray misses the mesh).
pub fn worst_gap(mesh: &MeshData, centre: Vec3, anchors: &[Vec3]) -> f64 {
    let rays = Rays::new(mesh, centre);
    anchors
        .iter()
        .map(|a| {
            let offset = (*a - centre).as_dvec3();
            rays.hit(offset.normalize()).map_or(f64::INFINITY, |t| (offset.length() - t).abs())
        })
        .fold(0.0, f64::max)
}

/// A ray caster from the head centre: the mesh's triangles, head-local, filed by the polar angles their corners span.
struct Rays {
    triangles: Vec<[DVec3; 3]>,
    rows: Vec<Vec<usize>>,
}

impl Rays {
    fn new(mesh: &MeshData, centre: Vec3) -> Self {
        let local: Vec<DVec3> = mesh.positions.iter().map(|p| (*p - centre).as_dvec3()).collect();
        let triangles: Vec<[DVec3; 3]> =
            mesh.indices.as_chunks::<3>().0.iter().map(|t| t.map(|i| local[i as usize])).collect();
        let mut rows = vec![Vec::new(); row_of(180.0) + 1];
        for (k, corners) in triangles.iter().enumerate() {
            let polar = corners.map(polar_deg);
            let (low, high) = (polar.iter().copied().fold(180.0, f64::min), polar.iter().copied().fold(0.0, f64::max));
            for row in rows.iter_mut().take(row_of(high) + 1).skip(row_of(low)) {
                row.push(k);
            }
        }
        Self { triangles, rows }
    }

    /// The farthest distance from the centre at which the ray along the unit `dir` meets the mesh.
    fn hit(&self, dir: DVec3) -> Option<f64> {
        let row = row_of(polar_deg(dir));
        let near = row.saturating_sub(1)..=(row + 1).min(self.rows.len() - 1);
        self.rows[near]
            .iter()
            .flatten()
            .filter_map(|k| crossing(dir, &self.triangles[*k]))
            .fold(None, |best, t| Some(best.map_or(t, |b: f64| b.max(t))))
    }
}

/// Degrees from the top of the head.
fn polar_deg(p: DVec3) -> f64 {
    (p.y / p.length()).clamp(-1.0, 1.0).acos().to_degrees()
}

fn row_of(polar_deg: f64) -> usize {
    (polar_deg / ROW_DEG).floor() as usize
}

/// Where the ray from the origin along `dir` crosses the triangle, as a distance (Moller-Trumbore), counting its edges.
fn crossing(dir: DVec3, [corner, next, last]: &[DVec3; 3]) -> Option<f64> {
    const EDGE: f64 = 1e-9;
    let (first_edge, second_edge) = (*next - *corner, *last - *corner);
    let across = dir.cross(second_edge);
    let det = first_edge.dot(across);
    if det.abs() < 1e-18 {
        return None;
    }
    let from_corner = -*corner;
    let along_first = from_corner.dot(across) / det;
    let turned = from_corner.cross(first_edge);
    let along_second = dir.dot(turned) / det;
    let distance = second_edge.dot(turned) / det;
    let inside = along_first >= -EDGE && along_second >= -EDGE && along_first + along_second <= 1.0 + EDGE;
    (inside && distance > 0.0).then_some(distance)
}

/// Every anchor of `params` on `head`, against the mesh: the worst distance.
pub fn worst_on(params: &HeadParams, mesh: &MeshData, head: &dyn HeadSurface) -> f64 {
    worst_gap(mesh, head.centre(), &anchors(&params.ears, head))
}
