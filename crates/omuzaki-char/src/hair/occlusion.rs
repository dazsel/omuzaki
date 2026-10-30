//! Hair occlusion (#2526): how buried in the hair and head each hair vertex is, baked into its shade.
//!
//! Drawn hair reads as a volume because the light dies inside it: lock roots, the underside of the outer locks and the
//! layers beneath go darker than the surface. A clean toon ramp lights every lock alike, so the hair reads as a stack of
//! cut-outs. This bakes that darkening at build, as a game groom bakes its ambient occlusion:
//!
//! 1. **A density grid.** Every triangle of the hair meshes is splatted into a grid over the hair (cells of
//!    [`CELLS_PER_REACH`] to the reach), as area per volume: how much hair surface a ray meets per metre there. The
//!    head is marked solid in the same grid, from its own surface ([`HeadSurface`]).
//! 2. **Rays.** From each vertex, [`RAYS`] rays cover the half of space its normal faces (cosine-weighted within a cone, the same
//!    set turned to each normal), each marched out to `reach` through the grid. A ray's transmittance falls as
//!    `exp(-ABSORPTION * density * step)` and is gone where it enters the head.
//! 3. **The shade.** The vertex's occlusion is the rays' mean transmittance, `1` in the open; its `hair-info` shade is
//!    multiplied by `1 - strength * (1 - occlusion)`, never below [`FLOOR`].
//!
//! The rays start [`START`] cells out, past the vertex's own lock, so a lone lock in the open keeps its shade. The same
//! mesh, head and settings bake the same numbers on every build.

use omuzaki_char_api::{HAIR_INFO, HeadSurface};
use omuzaki_core::glam::{IVec3, Vec3};
use omuzaki_core::mesh::MeshData;

/// Rays cast from each vertex.
const RAYS: usize = 12;
/// Grid cells to the reach: the reach is marched in this many cells, two steps to a cell.
const CELLS_PER_REACH: f32 = 5.0;
/// The finest and coarsest cell, metres.
const CELL: (f32, f32) = (0.003, 0.012);
/// The most cells the grid may hold; a larger hair takes coarser cells.
const MAX_CELLS: usize = 1 << 21;
/// How strongly hair surface absorbs a ray: transmittance falls by `exp(-ABSORPTION * area / volume * distance)`. A
/// ray crossing a lock meets two of its faces; this makes that cost about half of the light, as a lock's strands, with
/// gaps between them, let through.
const ABSORPTION: f32 = 1.4;
/// How far from the normal the rays reach, as the sine of their widest angle: grazing rays would find the neighbouring
/// locks of the same outer layer and grey the outside of the hair, which drawn hair keeps bright.
const CONE: f32 = 0.75;
/// Where the rays start, in cells out from the vertex.
const START: f32 = 2.0;
/// The darkest a vertex's shade is taken to, against its own.
const FLOOR: f32 = 0.08;
/// How far past the head's largest radius (of the directions sampled) its solid cells are looked for.
const HEAD_MARGIN: f32 = 1.3;
/// How far inside the head's least radius (of the directions sampled) a cell is taken as solid without asking.
const HEAD_INNER: f32 = 0.85;

/// Darkens the `hair-info` shade of every vertex of `meshes` by how buried it is in the hair (all of `meshes`
/// together) and in `head`, at `strength` (`0..=1`) within `reach` metres. A mesh without `hair-info` (three
/// components) is left alone; nothing happens at strength 0 or without vertices.
pub fn bake(meshes: &mut [&mut MeshData], head: &dyn HeadSurface, strength: f32, reach: f32) {
    if strength <= 0.0 || reach <= 0.0 {
        return;
    }
    let views: Vec<&MeshData> = meshes.iter().map(|m| &**m).collect();
    let occlusion = measure(&views, head, reach);
    for (mesh, open) in meshes.iter_mut().zip(occlusion) {
        let Some(info) = mesh.extra.iter_mut().find(|a| a.name.as_str() == HAIR_INFO && a.components == 3) else {
            continue;
        };
        for (v, open) in open.into_iter().enumerate() {
            if let Some(shade) = info.data.get_mut(3 * v + 1) {
                // A shade of 0 means "unset" (drawn at 1), so the darkening starts from 1 there.
                let base = if *shade <= 0.0 { 1.0 } else { *shade };
                *shade = (base * (1.0 - strength.min(1.0) * (1.0 - open))).max(FLOOR * base);
            }
        }
    }
}

/// The occlusion of every vertex of `meshes` (one list per mesh), `1` in the open and toward `0` buried in the hair and
/// `head` within `reach` metres: what [`bake`] multiplies the shade by at strength 1.
#[must_use]
pub fn measure(meshes: &[&MeshData], head: &dyn HeadSurface, reach: f32) -> Vec<Vec<f32>> {
    let open = |m: &&MeshData| vec![1.0; m.positions.len()];
    let Some(grid) = Grid::of(meshes, head, reach) else { return meshes.iter().map(open).collect() };
    let rays = directions();
    meshes
        .iter()
        .map(|m| m.positions.iter().zip(&m.normals).map(|(p, n)| grid.occlusion(*p, *n, &rays, reach)).collect())
        .collect()
}

/// The hair's surface density and the head's solid cells over the hair's bounds.
struct Grid {
    origin: Vec3,
    cell: f32,
    size: IVec3,
    /// Hair area per volume, per cell.
    density: Vec<f32>,
    /// Whether the cell's centre lies inside the head.
    solid: Vec<bool>,
}

/// The head's radius over a grid of latitude and longitude, read bilinearly: a cell needs its radius to a fraction of
/// itself, and the table costs a few thousand radii where the cells would cost tens of thousands.
struct Radii {
    radii: Vec<f32>,
}

/// The table's columns (longitude) and rows (latitude, pole to pole inclusive).
const RADII: (usize, usize) = (72, 37);

impl Radii {
    fn of(head: &dyn HeadSurface) -> Self {
        let (columns, rows) = RADII;
        let mut radii = Vec::with_capacity(columns * rows);
        for row in 0..rows {
            #[allow(clippy::cast_precision_loss)] // small table
            let polar = std::f32::consts::PI * row as f32 / (rows - 1) as f32;
            for column in 0..columns {
                #[allow(clippy::cast_precision_loss)] // small table
                let azimuth = std::f32::consts::TAU * column as f32 / columns as f32;
                let dir = Vec3::new(polar.sin() * azimuth.cos(), polar.cos(), polar.sin() * azimuth.sin());
                radii.push(head.radius(dir));
            }
        }
        Self { radii }
    }

    /// The least and largest radius in the table.
    fn bounds(&self) -> (f32, f32) {
        self.radii.iter().fold((f32::INFINITY, 0.0_f32), |(lo, hi), r| (lo.min(*r), hi.max(*r)))
    }

    /// The radius toward the unit direction `dir`.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // small table
    fn at(&self, dir: Vec3) -> f32 {
        let (columns, rows) = RADII;
        let polar = dir.y.clamp(-1.0, 1.0).acos() / std::f32::consts::PI * (rows - 1) as f32;
        let azimuth = dir.z.atan2(dir.x).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * columns as f32;
        let (row, column) = (polar.floor(), azimuth.floor());
        let (fr, fc) = (polar - row, azimuth - column);
        let (r0, c0) = ((row as usize).min(rows - 1), (column as usize) % columns);
        let (r1, c1) = ((r0 + 1).min(rows - 1), (c0 + 1) % columns);
        let get = |r: usize, c: usize| self.radii.get(r * columns + c).copied().unwrap_or(0.0);
        let top = get(r0, c0) * (1.0 - fc) + get(r0, c1) * fc;
        let bottom = get(r1, c0) * (1.0 - fc) + get(r1, c1) * fc;
        top * (1.0 - fr) + bottom * fr
    }
}

impl Grid {
    fn of(meshes: &[&MeshData], head: &dyn HeadSurface, reach: f32) -> Option<Self> {
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for p in meshes.iter().flat_map(|m| &m.positions) {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        if !lo.is_finite() || !hi.is_finite() {
            return None;
        }
        lo -= Vec3::splat(reach);
        hi += Vec3::splat(reach);
        let extent = hi - lo;
        let mut cell = (reach / CELLS_PER_REACH).clamp(CELL.0, CELL.1);
        // A grid too large for the budget takes coarser cells.
        #[allow(clippy::cast_precision_loss)] // a cell budget of 2^21
        let budget = MAX_CELLS as f32;
        let volume = extent.x * extent.y * extent.z;
        if volume / cell.powi(3) > budget {
            cell = (volume / budget).cbrt();
        }
        #[allow(clippy::cast_possible_truncation)] // bounded by the budget above
        let size = (extent / cell).ceil().as_ivec3().max(IVec3::ONE);
        let cells = usize::try_from(size.x * size.y * size.z).ok()?;
        let mut grid = Self { origin: lo, cell, size, density: vec![0.0; cells], solid: vec![false; cells] };
        let per_volume = 1.0 / cell.powi(3);
        for mesh in meshes {
            for &[i0, i1, i2] in mesh.indices.as_chunks::<3>().0 {
                let corner = |i: u32| mesh.positions.get(i as usize).copied();
                let (Some(v0), Some(v1), Some(v2)) = (corner(i0), corner(i1), corner(i2)) else { continue };
                let area = 0.5 * (v1 - v0).cross(v2 - v0).length();
                if area.is_nan() || area <= 0.0 {
                    continue;
                }
                // Enough samples that each covers about a cell's face.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 1..=64
                let samples = ((area / (cell * cell)).ceil() as usize).clamp(1, SAMPLES.len());
                #[allow(clippy::cast_precision_loss)] // at most 64
                let share = area * per_volume / samples as f32;
                for (s, t) in SAMPLES.iter().take(samples) {
                    let at = v0 + (v1 - v0) * *s + (v2 - v0) * *t;
                    if let Some(i) = grid.index(grid.cell_of(at)) {
                        grid.density[i] += share;
                    }
                }
            }
        }
        // The head is solid. A radius costs microseconds and the box around the head holds tens of thousands of cells,
        // so the radii come from a table over latitude and longitude, and only the cells in the shell between the
        // head's least and largest radius ask it: those nearer the centre are inside, those further out are not.
        let centre = head.centre();
        let radii = Radii::of(head);
        let (least, largest) = radii.bounds();
        let (inner, outer) = (least * HEAD_INNER, largest * HEAD_MARGIN);
        let from = grid.cell_of(centre - Vec3::splat(outer)).max(IVec3::ZERO);
        let to = grid.cell_of(centre + Vec3::splat(outer)).min(size - IVec3::ONE);
        for z in from.z..=to.z {
            for y in from.y..=to.y {
                for x in from.x..=to.x {
                    let at = IVec3::new(x, y, z);
                    let d = grid.origin + (at.as_vec3() + 0.5) * cell - centre;
                    let distance = d.length();
                    let inside = distance < inner
                        || (distance < outer && d.try_normalize().is_none_or(|dir| distance < radii.at(dir)));
                    if let (true, Some(i)) = (inside, grid.index(at)) {
                        grid.solid[i] = true;
                    }
                }
            }
        }
        Some(grid)
    }

    fn cell_of(&self, p: Vec3) -> IVec3 {
        ((p - self.origin) / self.cell).floor().as_ivec3()
    }

    fn index(&self, at: IVec3) -> Option<usize> {
        if at.cmplt(IVec3::ZERO).any() || at.cmpge(self.size).any() {
            return None;
        }
        usize::try_from(at.x + self.size.x * (at.y + self.size.y * at.z)).ok()
    }

    /// The density at `p`, trilinear between cell centres; 0 outside the grid.
    fn density(&self, at: Vec3) -> f32 {
        let scaled = (at - self.origin) / self.cell - 0.5;
        let base = scaled.floor();
        let frac = scaled - base;
        let base = base.as_ivec3();
        let mut sum = 0.0;
        for corner in 0..8 {
            let step = IVec3::new(corner & 1, (corner >> 1) & 1, (corner >> 2) & 1);
            let weight = Vec3::select(step.cmpeq(IVec3::ONE), frac, Vec3::ONE - frac);
            if let Some(i) = self.index(base + step) {
                sum += self.density[i] * weight.x * weight.y * weight.z;
            }
        }
        sum
    }

    fn solid(&self, p: Vec3) -> bool {
        self.index(self.cell_of(p)).is_some_and(|i| self.solid[i])
    }

    /// The mean transmittance of the rays from `p` over the half space `n` faces, within `reach`.
    fn occlusion(&self, from: Vec3, normal: Vec3, rays: &[Vec3; RAYS], reach: f32) -> f32 {
        let Some(normal) = normal.try_normalize() else { return 1.0 };
        let (tangent, bitangent) = normal.any_orthonormal_pair();
        let step = self.cell * 0.5;
        let start = self.cell * START;
        let mut open = 0.0;
        for local in rays {
            let dir = tangent * local.x + bitangent * local.y + normal * local.z;
            let mut depth = 0.0;
            let mut at = start;
            let mut blocked = false;
            while at < reach {
                let point = from + dir * at;
                if self.solid(point) {
                    blocked = true;
                    break;
                }
                depth += self.density(point) * step;
                at += step;
            }
            if !blocked {
                open += (-ABSORPTION * depth).exp();
            }
        }
        #[allow(clippy::cast_precision_loss)] // RAYS is 12
        let rays = RAYS as f32;
        open / rays
    }
}

/// [`RAYS`] cosine-weighted directions within [`CONE`] of `+z`: a Fibonacci spiral on a disc lifted to the hemisphere, so every vertex casts the same, evenly spread set turned to its normal.
fn directions() -> [Vec3; RAYS] {
    let golden = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
    std::array::from_fn(|i| {
        #[allow(clippy::cast_precision_loss)] // i < 12
        let (i, n) = (i as f32, RAYS as f32);
        let r = CONE * ((i + 0.5) / n).sqrt();
        let phi = golden * i;
        Vec3::new(r * phi.cos(), r * phi.sin(), (1.0 - r * r).max(0.0).sqrt())
    })
}

/// Barycentric sample points `(s, t)` inside a triangle (`s + t <= 1`), spread evenly: a triangle takes the first `k`.
const SAMPLES: [(f32, f32); 64] = samples();

const fn samples() -> [(f32, f32); 64] {
    let mut out = [(0.0, 0.0); 64];
    // An 8 x 8 grid on the unit square, each point folded into the triangle; the order interleaves rows and columns
    // so any prefix spreads over the triangle.
    let mut i = 0;
    while i < 64 {
        let (row, col) = (i % 8, (i / 8 + (i % 8) * 3) % 8);
        #[allow(clippy::cast_precision_loss)] // < 8
        let (mut s, mut t) = ((row as f32 + 0.5) / 8.0, (col as f32 + 0.5) / 8.0);
        if s + t > 1.0 {
            s = 1.0 - s;
            t = 1.0 - t;
        }
        out[i] = (s, t);
        i += 1;
    }
    out
}
