//! The solids prop kinds are made of: boxes (with a bevel), frustums and cylinders, ellipsoids, tubes along a path and
//! flat panels, each placed by a transform and painted with a vertex colour, appended to one mesh per material.
//!
//! Every solid is closed, wound counter-clockwise seen from outside, with normals that agree with the winding (the
//! tests check both), so a kind is watertight by construction and outlines and shadows treat it as a solid.

use std::f32::consts::TAU;

use omuzaki_core::glam::{Affine3A, Mat3A, Quat, Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_render_api::{Colour, Material, Outline, Shading};

/// A mesh being built, with a linear vertex colour on every vertex.
#[derive(Clone, Debug, Default)]
pub struct Shape {
    mesh: MeshData,
}

/// Where a solid goes: its own frame's origin, turn and scale in the prop's frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct At(pub Affine3A);

impl At {
    /// At `position`, unturned.
    pub fn pos(position: Vec3) -> Self {
        Self(Affine3A::from_translation(position))
    }

    /// At `(x, y, z)`, unturned.
    pub fn xyz(x: f32, y: f32, z: f32) -> Self {
        Self::pos(Vec3::new(x, y, z))
    }

    /// The same place, turned by `rotation` about its own origin.
    #[must_use]
    pub fn turned(self, rotation: Quat) -> Self {
        Self(self.0 * Affine3A::from_quat(rotation))
    }

    /// The same place, turned `degrees` about its own +Y.
    #[must_use]
    pub fn yaw(self, degrees: f32) -> Self {
        self.turned(Quat::from_rotation_y(degrees.to_radians()))
    }

    /// The same place, turned `degrees` about its own +X.
    #[must_use]
    pub fn pitch(self, degrees: f32) -> Self {
        self.turned(Quat::from_rotation_x(degrees.to_radians()))
    }

    /// The same place, turned `degrees` about its own +Z.
    #[must_use]
    pub fn roll(self, degrees: f32) -> Self {
        self.turned(Quat::from_rotation_z(degrees.to_radians()))
    }
}

impl Shape {
    /// An empty shape.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing was added.
    pub fn is_empty(&self) -> bool {
        self.mesh.indices.is_empty()
    }

    /// The mesh built so far.
    pub fn into_mesh(self) -> MeshData {
        self.mesh
    }

    fn base(&self) -> u32 {
        u32::try_from(self.mesh.positions.len()).unwrap_or(u32::MAX)
    }

    /// Appends vertices given in a solid's own frame, placed by `at`, painted `colour`, and triangles indexing them
    /// from 0.
    fn append(&mut self, at: At, colour: Colour, positions: &[Vec3], normals: &[Vec3], uvs: &[Vec2], indices: &[u32]) {
        let base = self.base();
        let normal_matrix: Mat3A = at.0.matrix3.inverse().transpose();
        let linear = colour.to_linear();
        let paint = [linear.x, linear.y, linear.z, 1.0];
        for ((p, n), uv) in positions.iter().zip(normals).zip(uvs) {
            self.mesh.positions.push(at.0.transform_point3(*p));
            self.mesh.normals.push((normal_matrix * *n).normalize_or(Vec3::Y));
            self.mesh.uvs.push(*uv);
            self.mesh.colours.push(paint);
        }
        // A mirroring transform turns every triangle inside out; swap its winding back.
        let flip = at.0.matrix3.determinant() < 0.0;
        for tri in indices.as_chunks::<3>().0 {
            let [a, b, c] = *tri;
            if flip {
                self.mesh.indices.extend([base + a, base + c, base + b]);
            } else {
                self.mesh.indices.extend([base + a, base + b, base + c]);
            }
        }
    }

    /// A box of half-extents `half`, centred on `at`, hard-edged (24 vertices).
    pub fn cuboid(&mut self, at: At, half: Vec3, colour: Colour) -> &mut Self {
        let (positions, normals, uvs, indices) = cuboid(half);
        self.append(at, colour, &positions, &normals, &uvs, &indices);
        self
    }

    /// A box from `min` to `max` in the prop's frame (axis-aligned), the usual way to lay out furniture.
    pub fn block(&mut self, min: Vec3, max: Vec3, colour: Colour) -> &mut Self {
        self.cuboid(At::pos((min + max) * 0.5), (max - min).abs() * 0.5, colour)
    }

    /// A box with its edges rounded by `bevel` (one 45° chamfer band per edge and a corner facet), centred on `at`: a
    /// cushion, a mattress, a book's soft back.
    pub fn bevelled(&mut self, at: At, half: Vec3, bevel: f32, colour: Colour) -> &mut Self {
        let bevel = bevel.clamp(0.0, half.min_element() * 0.95);
        if bevel <= 1e-5 {
            return self.cuboid(at, half, colour);
        }
        let (positions, normals, uvs, indices) = bevelled(half, bevel);
        self.append(at, colour, &positions, &normals, &uvs, &indices);
        self
    }

    /// A frustum along `at`'s +Y from `y = 0` to `height`, radius `bottom` then `top`, `segments` around, with the caps
    /// asked for. Smooth sides; a radius of 0 makes a cone point.
    #[allow(clippy::too_many_arguments)] // a frustum is these numbers
    pub fn frustum(
        &mut self,
        at: At,
        bottom: f32,
        top: f32,
        height: f32,
        segments: u32,
        caps: (bool, bool),
        colour: Colour,
    ) -> &mut Self {
        let (positions, normals, uvs, indices) = frustum(bottom, top, height, segments.max(3), caps);
        self.append(at, colour, &positions, &normals, &uvs, &indices);
        self
    }

    /// A closed cylinder along `at`'s +Y from `y = 0` to `height`.
    pub fn cylinder(&mut self, at: At, radius: f32, height: f32, segments: u32, colour: Colour) -> &mut Self {
        self.frustum(at, radius, radius, height, segments, (true, true), colour)
    }

    /// An open tube along `at`'s +Y with walls `thickness` thick and a bottom: a cup, a pot, a lamp shade without the
    /// bottom (`floor = false`).
    #[allow(clippy::too_many_arguments)] // a vessel is these numbers
    pub fn vessel(
        &mut self,
        at: At,
        bottom: f32,
        top: f32,
        height: f32,
        thickness: f32,
        floor: bool,
        segments: u32,
        colour: Colour,
    ) -> &mut Self {
        let (positions, normals, uvs, indices) = vessel(bottom, top, height, thickness, floor, segments.max(3));
        self.append(at, colour, &positions, &normals, &uvs, &indices);
        self
    }

    /// An ellipsoid of radii `radii` centred on `at`, `rings` from pole to pole and `segments` around.
    pub fn ellipsoid(&mut self, at: At, radii: Vec3, rings: u32, segments: u32, colour: Colour) -> &mut Self {
        let (positions, normals, uvs, indices) = ellipsoid(radii, rings.max(2), segments.max(3));
        self.append(at, colour, &positions, &normals, &uvs, &indices);
        self
    }

    /// A closed tube of radius `radius` through `points` (at least two), `segments` around, capped at both ends: a
    /// handle, a lamp's arm, a plant's stem.
    pub fn tube(&mut self, points: &[Vec3], radius: f32, segments: u32, colour: Colour) -> &mut Self {
        if points.len() < 2 {
            return self;
        }
        let (positions, normals, uvs, indices) = tube(points, radius, segments.max(3));
        self.append(At(Affine3A::IDENTITY), colour, &positions, &normals, &uvs, &indices);
        self
    }

    /// A flat, one-sided sheet from `origin` along `u` and `v` (it faces `u × v`), cut into `cols` by `rows` cells, each
    /// vertex painted `paint(s, t)` at its place `(s, t)` in `0..=1` along `u` and `v`: a wall's plaster, a stain.
    pub fn sheet(&mut self, origin: Vec3, u: Vec3, v: Vec3, (cols, rows): (u32, u32), paint: impl Fn(f32, f32) -> Colour) -> &mut Self {
        let (cols, rows) = (cols.clamp(1, 512), rows.clamp(1, 512));
        let base = self.base();
        let normal = u.cross(v).normalize_or(Vec3::Z);
        for j in 0..=rows {
            for i in 0..=cols {
                #[allow(clippy::cast_precision_loss)] // at most 512 cells a side
                let (s, t) = (i as f32 / cols as f32, j as f32 / rows as f32);
                let linear = paint(s, t).to_linear();
                self.mesh.positions.push(origin + u * s + v * t);
                self.mesh.normals.push(normal);
                self.mesh.uvs.push(Vec2::new(s, 1.0 - t));
                self.mesh.colours.push([linear.x, linear.y, linear.z, 1.0]);
            }
        }
        let at = |i: u32, j: u32| base + j * (cols + 1) + i;
        for j in 0..rows {
            for i in 0..cols {
                self.mesh.indices.extend([at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j), at(i + 1, j + 1), at(i, j + 1)]);
            }
        }
        self
    }

    /// A solid turned on a lathe about `at`'s +Y: `profile` holds `(radius, y)` from the bottom up (at least two),
    /// `segments` around, capped flat where an end's radius is above 0: a baluster, a newel post, a candlestick.
    pub fn lathe(&mut self, at: At, profile: &[Vec2], segments: u32, colour: Colour) -> &mut Self {
        if profile.len() < 2 {
            return self;
        }
        let (positions, normals, uvs, indices) = lathe(profile, segments.max(3));
        self.append(at, colour, &positions, &normals, &uvs, &indices);
        self
    }

    /// A thin leaf: a lens-shaped blade of `length` and `width` along `at`'s +Y, bent forward by `curl` metres at its
    /// tip, with two sides so it reads from behind.
    pub fn leaf(&mut self, at: At, length: f32, width: f32, curl: f32, colour: Colour) -> &mut Self {
        let (positions, normals, uvs, indices) = leaf(length, width, curl);
        self.append(at, colour, &positions, &normals, &uvs, &indices);
        self
    }
}

type Solid = (Vec<Vec3>, Vec<Vec3>, Vec<Vec2>, Vec<u32>);

fn quad_into(solid: &mut Solid, corners: [Vec3; 4], outward: Vec3) {
    let wound = (corners[1] - corners[0]).cross(corners[2] - corners[0]).dot(outward) > 0.0;
    let corners = if wound { corners } else { [corners[0], corners[3], corners[2], corners[1]] };
    let normal = (corners[1] - corners[0]).cross(corners[2] - corners[0]).normalize_or(outward);
    let base = u32::try_from(solid.0.len()).unwrap_or(0);
    for (i, c) in corners.iter().enumerate() {
        solid.0.push(*c);
        solid.1.push(normal);
        solid.2.push(Vec2::new(if i == 1 || i == 2 { 1.0 } else { 0.0 }, if i >= 2 { 0.0 } else { 1.0 }));
    }
    solid.3.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn axis(k: usize, d: f32) -> Vec3 {
    let mut v = Vec3::ZERO;
    v[k] = d;
    v
}

fn cuboid(half: Vec3) -> Solid {
    let mut solid: Solid = Default::default();
    for k in 0..3 {
        let (a, b) = ((k + 1) % 3, (k + 2) % 3);
        for d in [1.0_f32, -1.0] {
            let corner = |sa: f32, sb: f32| (axis(k, d) + axis(a, sa) + axis(b, sb)) * half;
            let rect = [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)];
            quad_into(&mut solid, rect, axis(k, d));
        }
    }
    solid
}

/// A bevelled box as the hull of three inset boxes: each face is the inner rectangle of its own box, each edge a
/// chamfer between two faces, each corner a triangle. Flat-shaded.
fn bevelled(half: Vec3, bevel: f32) -> Solid {
    let mut solid: Solid = Default::default();
    let inner = half - Vec3::splat(bevel);
    // The corner of face `k` toward the box corner of signs `s`: inset on the other two axes, out on `k`.
    let point = |s: Vec3, k: usize| {
        let mut p = inner * s;
        p[k] = half[k] * s[k];
        p
    };
    for k in 0..3 {
        let (a, b) = ((k + 1) % 3, (k + 2) % 3);
        for d in [1.0_f32, -1.0] {
            let corner = |sa: f32, sb: f32| point(axis(k, d) + axis(a, sa) + axis(b, sb), k);
            let rect = [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)];
            quad_into(&mut solid, rect, axis(k, d));
        }
    }
    for k in 0..3 {
        let (j, m) = ((k + 1) % 3, (k + 2) % 3);
        for dk in [1.0_f32, -1.0] {
            for dj in [1.0_f32, -1.0] {
                let at = |dm: f32, on: usize| point(axis(k, dk) + axis(j, dj) + axis(m, dm), on);
                let rect = [at(-1.0, k), at(1.0, k), at(1.0, j), at(-1.0, j)];
                quad_into(&mut solid, rect, axis(k, dk) + axis(j, dj));
            }
        }
    }
    for sx in [1.0_f32, -1.0] {
        for sy in [1.0_f32, -1.0] {
            for sz in [1.0_f32, -1.0] {
                let s = Vec3::new(sx, sy, sz);
                let mut tri = [point(s, 0), point(s, 1), point(s, 2)];
                if (tri[1] - tri[0]).cross(tri[2] - tri[0]).dot(s) < 0.0 {
                    tri.swap(1, 2);
                }
                let normal = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalize_or(s.normalize());
                let base = u32::try_from(solid.0.len()).unwrap_or(0);
                for p in tri {
                    solid.0.push(p);
                    solid.1.push(normal);
                    solid.2.push(Vec2::ZERO);
                }
                solid.3.extend([base, base + 1, base + 2]);
            }
        }
    }
    solid
}

#[allow(clippy::cast_precision_loss)] // segment counts are small
fn ring(segments: u32) -> impl Iterator<Item = (f32, Vec2)> {
    (0..=segments).map(move |i| {
        let t = i as f32 / segments as f32;
        let a = t * TAU;
        (t, Vec2::new(a.sin(), a.cos()))
    })
}

fn frustum(bottom: f32, top: f32, height: f32, segments: u32, caps: (bool, bool)) -> Solid {
    let mut solid: Solid = Default::default();
    // The side's normal leans by the slope (bottom - top) / height.
    let slope = (bottom - top) / height.max(1e-6);
    for (t, dir) in ring(segments) {
        let n = Vec3::new(dir.x, slope, dir.y).normalize();
        solid.0.push(Vec3::new(dir.x * bottom, 0.0, dir.y * bottom));
        solid.1.push(n);
        solid.2.push(Vec2::new(t, 1.0));
        solid.0.push(Vec3::new(dir.x * top, height, dir.y * top));
        solid.1.push(n);
        solid.2.push(Vec2::new(t, 0.0));
    }
    for i in 0..segments {
        let (b0, t0, b1, t1) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
        solid.3.extend([b0, b1, t1, b0, t1, t0]);
    }
    for (on, y, radius, normal) in [(caps.0, 0.0, bottom, Vec3::NEG_Y), (caps.1, height, top, Vec3::Y)] {
        if !on || radius <= 0.0 {
            continue;
        }
        let centre = u32::try_from(solid.0.len()).unwrap_or(0);
        solid.0.push(Vec3::new(0.0, y, 0.0));
        solid.1.push(normal);
        solid.2.push(Vec2::splat(0.5));
        for (_, dir) in ring(segments) {
            solid.0.push(Vec3::new(dir.x * radius, y, dir.y * radius));
            solid.1.push(normal);
            solid.2.push(Vec2::new(0.5 + dir.x * 0.5, 0.5 - dir.y * 0.5));
        }
        for i in 0..segments {
            solid.3.extend([centre, centre + 1 + i, centre + 2 + i]);
        }
    }
    fix_winding(&mut solid);
    solid
}

/// A turned solid: rings at each `(radius, y)` of `profile`, smooth along it, with flat caps on the ends.
fn lathe(profile: &[Vec2], segments: u32) -> Solid {
    let mut solid: Solid = Default::default();
    let n = profile.len();
    let rows = u32::try_from(n).unwrap_or(u32::MAX);
    // A row's normal is the profile's tangent turned outward: (dy, -dr) for a tangent (dr, dy).
    let bend: Vec<Vec2> = (0..n)
        .map(|j| {
            let t = profile[(j + 1).min(n - 1)] - profile[j.saturating_sub(1)];
            Vec2::new(t.y, -t.x).normalize_or(Vec2::X)
        })
        .collect();
    for (u, dir) in ring(segments) {
        for (j, (p, b)) in profile.iter().zip(&bend).enumerate() {
            solid.0.push(Vec3::new(dir.x * p.x, p.y, dir.y * p.x));
            solid.1.push(Vec3::new(dir.x * b.x, b.y, dir.y * b.x).normalize_or(Vec3::Y));
            #[allow(clippy::cast_precision_loss)] // a profile has a few dozen points
            solid.2.push(Vec2::new(u, 1.0 - j as f32 / n_minus_one(n)));
        }
    }
    for i in 0..segments {
        for j in 0..rows - 1 {
            let (a, b) = (i * rows + j, (i + 1) * rows + j);
            solid.3.extend([a, b, b + 1, a, b + 1, a + 1]);
        }
    }
    for (p, normal) in [(profile[0], Vec3::NEG_Y), (profile[n - 1], Vec3::Y)] {
        if p.x <= 0.0 {
            continue;
        }
        let centre = u32::try_from(solid.0.len()).unwrap_or(0);
        solid.0.push(Vec3::new(0.0, p.y, 0.0));
        solid.1.push(normal);
        solid.2.push(Vec2::splat(0.5));
        for (_, dir) in ring(segments) {
            solid.0.push(Vec3::new(dir.x * p.x, p.y, dir.y * p.x));
            solid.1.push(normal);
            solid.2.push(Vec2::new(0.5 + dir.x * 0.5, 0.5 - dir.y * 0.5));
        }
        for i in 0..segments {
            solid.3.extend([centre, centre + 1 + i, centre + 2 + i]);
        }
    }
    fix_winding(&mut solid);
    solid
}

/// Turns every triangle whose winding disagrees with its vertices' normals, so each solid is written in whichever
/// order is shortest and corrected here.
fn fix_winding(solid: &mut Solid) {
    let (positions, normals, indices) = (&solid.0, &solid.1, &mut solid.3);
    for tri in indices.as_chunks_mut::<3>().0 {
        let [a, b, c] = tri.map(|i| i as usize);
        let face = (positions[b] - positions[a]).cross(positions[c] - positions[a]);
        let normal = normals[a] + normals[b] + normals[c];
        if face.dot(normal) < 0.0 {
            tri.swap(1, 2);
        }
    }
}

fn vessel(bottom: f32, top: f32, height: f32, thickness: f32, floor: bool, segments: u32) -> Solid {
    let thickness = thickness.clamp(1e-4, bottom.min(top) * 0.9);
    // The outside wall, with the base when there is a floor.
    let mut solid = frustum(bottom, top, height, segments, (floor, false));
    // The inside wall, facing in, standing on the floor's thickness; its own cap is the floor's top.
    let floor_y = if floor { thickness.min(height * 0.5) } else { 0.0 };
    let mut inside = frustum(bottom - thickness, top - thickness, height - floor_y, segments, (floor, false));
    for p in &mut inside.0 {
        p.y += floor_y;
    }
    for n in &mut inside.1 {
        *n = -*n;
    }
    fix_winding(&mut inside);
    merge(&mut solid, inside);
    // Flat rings joining the two walls: the rim at the top and, without a floor, the bottom edge.
    let mut bands = vec![(height, Vec3::Y, top)];
    if !floor {
        bands.push((0.0, Vec3::NEG_Y, bottom));
    }
    for (y, normal, outer) in bands {
        let mut band: Solid = Default::default();
        for (t, dir) in ring(segments) {
            for r in [outer, outer - thickness] {
                band.0.push(Vec3::new(dir.x * r, y, dir.y * r));
                band.1.push(normal);
                band.2.push(Vec2::new(t, 0.0));
            }
        }
        for i in 0..segments {
            let (o0, i0, o1, i1) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
            band.3.extend([o0, o1, i1, o0, i1, i0]);
        }
        fix_winding(&mut band);
        merge(&mut solid, band);
    }
    solid
}

fn merge(into: &mut Solid, from: Solid) {
    let base = u32::try_from(into.0.len()).unwrap_or(0);
    into.0.extend(from.0);
    into.1.extend(from.1);
    into.2.extend(from.2);
    into.3.extend(from.3.into_iter().map(|i| i + base));
}

#[allow(clippy::cast_precision_loss)] // ring and segment counts are small
fn ellipsoid(radii: Vec3, rings: u32, segments: u32) -> Solid {
    let mut solid: Solid = Default::default();
    for r in 0..=rings {
        let v = r as f32 / rings as f32;
        let polar = v * std::f32::consts::PI;
        let (s, c) = polar.sin_cos();
        for (t, dir) in ring(segments) {
            let unit = Vec3::new(dir.x * s, c, dir.y * s);
            solid.0.push(unit * radii);
            solid.1.push((unit / radii.max(Vec3::splat(1e-6))).normalize_or(unit));
            solid.2.push(Vec2::new(t, v));
        }
    }
    let row = segments + 1;
    for r in 0..rings {
        for i in 0..segments {
            let (a, b, c, d) = (r * row + i, r * row + i + 1, (r + 1) * row + i, (r + 1) * row + i + 1);
            if r != 0 {
                solid.3.extend([a, b, c]);
            }
            if r != rings - 1 {
                solid.3.extend([b, d, c]);
            }
        }
    }
    fix_winding(&mut solid);
    solid
}

#[allow(clippy::cast_precision_loss)] // point and segment counts are small
fn tube(points: &[Vec3], radius: f32, segments: u32) -> Solid {
    let mut solid: Solid = Default::default();
    let n = points.len();
    let tangent = |i: usize| {
        let prev = points[i.saturating_sub(1)];
        let next = points[(i + 1).min(n - 1)];
        (next - prev).normalize_or(Vec3::Y)
    };
    // A rotation-minimising frame carried along the path, so the tube doesn't twist.
    let first = tangent(0);
    let mut side = first.any_orthonormal_vector();
    let mut frames = Vec::with_capacity(n);
    for i in 0..n {
        let t = tangent(i);
        side = (side - t * side.dot(t)).normalize_or(t.any_orthonormal_vector());
        frames.push((t, side, t.cross(side)));
    }
    for (i, (p, (_, s, u))) in points.iter().zip(&frames).enumerate() {
        for (k, dir) in ring(segments) {
            let n = *s * dir.x + *u * dir.y;
            solid.0.push(*p + n * radius);
            solid.1.push(n);
            solid.2.push(Vec2::new(k, i as f32 / (n_minus_one(points.len()))));
        }
    }
    let row = segments + 1;
    for i in 0..u32::try_from(n - 1).unwrap_or(0) {
        for k in 0..segments {
            let (a, b, c, d) = (i * row + k, i * row + k + 1, (i + 1) * row + k, (i + 1) * row + k + 1);
            solid.3.extend([a, c, d, a, d, b]);
        }
    }
    fix_winding(&mut solid);
    // Caps: a fan at each end, facing out along the path.
    for (end, (p, (t, s, u))) in [(0, (points[0], frames[0])), (1, (points[n - 1], frames[n - 1]))] {
        let normal = if end == 0 { -t } else { t };
        let mut cap: Solid = Default::default();
        cap.0.push(p);
        cap.1.push(normal);
        cap.2.push(Vec2::splat(0.5));
        for (_, dir) in ring(segments) {
            cap.0.push(p + (s * dir.x + u * dir.y) * radius);
            cap.1.push(normal);
            cap.2.push(Vec2::splat(0.5) + dir * 0.5);
        }
        for k in 0..segments {
            cap.3.extend([0, 1 + k, 2 + k]);
        }
        fix_winding(&mut cap);
        merge(&mut solid, cap);
    }
    solid
}

#[allow(clippy::cast_precision_loss)]
fn n_minus_one(n: usize) -> f32 {
    (n.max(2) - 1) as f32
}

#[allow(clippy::cast_precision_loss)] // a leaf has a handful of rows
fn leaf(length: f32, width: f32, curl: f32) -> Solid {
    const ROWS: u32 = 6;
    let mut front: Solid = Default::default();
    for r in 0..=ROWS {
        let v = r as f32 / ROWS as f32;
        let half = width * 0.5 * (v * std::f32::consts::PI).sin().max(0.0);
        let y = v * length;
        let z = curl * v * v;
        // The blade bends toward +Z; its normal leans back by the bend's slope.
        let slope = 2.0 * curl * v / length.max(1e-6);
        let normal = Vec3::new(0.0, -slope, 1.0).normalize();
        for x in [-half, half] {
            front.0.push(Vec3::new(x, y, z));
            front.1.push(normal);
            front.2.push(Vec2::new(if x < 0.0 { 0.0 } else { 1.0 }, 1.0 - v));
        }
    }
    for r in 0..ROWS {
        let (a, b, c, d) = (2 * r, 2 * r + 1, 2 * r + 2, 2 * r + 3);
        front.3.extend([a, b, d, a, d, c]);
    }
    fix_winding(&mut front);
    let mut back = front.clone();
    for n in &mut back.1 {
        *n = -*n;
    }
    fix_winding(&mut back);
    merge(&mut front, back);
    front
}

/// A toon material that takes its colour from the vertices, with a thin outline in `line`: what furniture is drawn
/// with, so it sits beside a character in the same hand.
pub fn painted(line: Colour) -> Material {
    let mut material = Material::toon(Colour::WHITE);
    material.vertex_colours = true;
    material.outline = Some(Outline { colour: line, width: 0.0035 });
    if let Shading::Toon(toon) = &mut material.shading {
        toon.softness = 0.18;
        toon.rim.strength = 0.0;
    }
    material
}

/// [`painted`] without a line: walls and floors, all edge at a grazing angle, and anything whose silhouette would
/// crowd the frame.
pub fn painted_plain() -> Material {
    let mut material = painted(Colour::BLACK);
    material.outline = None;
    material
}

/// A surface that glows `colour` and takes no light: a bulb, a window's daylit pane.
pub fn glowing(colour: Colour) -> Material {
    let mut material = Material::unlit(colour);
    material.receives_shadows = false;
    material
}

/// `colour` scaled toward black by `amount` (0 is unchanged, 1 is black), in linear light: a line or a shade.
pub fn darker(colour: Colour, amount: f32) -> Colour {
    Colour::from_linear(colour.to_linear() * (1.0 - amount.clamp(0.0, 1.0)), colour.alpha())
}

/// `colour` moved toward `other` by `t`, in linear light.
pub fn mix(colour: Colour, other: Colour, t: f32) -> Colour {
    Colour::from_linear(colour.to_linear().lerp(other.to_linear(), t.clamp(0.0, 1.0)), 1.0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Every triangle faces the way its normals do, and every edge is shared by exactly two triangles (closed).
    pub(crate) fn assert_solid(mesh: &MeshData, closed: bool) {
        mesh.validate().unwrap();
        assert_eq!(mesh.colours.len(), mesh.positions.len(), "every vertex is painted");
        for tri in mesh.indices.as_chunks::<3>().0 {
            let [a, b, c] = tri.map(|i| i as usize);
            let (pa, pb, pc) = (mesh.positions[a], mesh.positions[b], mesh.positions[c]);
            let face = (pb - pa).cross(pc - pa);
            if face.length() < 1e-12 {
                continue;
            }
            let normal = mesh.normals[a] + mesh.normals[b] + mesh.normals[c];
            assert!(face.dot(normal) > 0.0, "triangle {tri:?} winds against its normals");
        }
        if closed {
            // Weld by position (the solids split vertices at hard edges and seams) and count each edge's uses.
            let key = |p: Vec3| (p * 1e5).round().as_ivec3().to_array();
            let mut edges = std::collections::HashMap::new();
            for tri in mesh.indices.as_chunks::<3>().0 {
                let k = tri.map(|i| key(mesh.positions[i as usize]));
                for (u, v) in [(k[0], k[1]), (k[1], k[2]), (k[2], k[0])] {
                    if u == v {
                        continue;
                    }
                    let (lo, hi) = if u < v { (u, v) } else { (v, u) };
                    *edges.entry((lo, hi)).or_insert(0) += 1;
                }
            }
            let open: Vec<_> = edges.iter().filter(|(_, n)| **n % 2 != 0).collect();
            assert!(open.is_empty(), "{} edges are not shared by two triangles: {:?}", open.len(), &open[..open.len().min(4)]);
        }
    }

    #[test]
    fn every_solid_is_closed_and_wound_outward() {
        let c = Colour::WHITE;
        let mut s = Shape::new();
        s.cuboid(At::xyz(0.0, 0.5, 0.0).yaw(30.0), Vec3::new(0.2, 0.5, 0.1), c);
        assert_solid(&s.clone().into_mesh(), true);
        let mut s = Shape::new();
        s.bevelled(At::xyz(0.0, 0.5, 0.0).pitch(10.0), Vec3::new(0.3, 0.1, 0.2), 0.03, c);
        assert_solid(&s.clone().into_mesh(), true);
        let mut s = Shape::new();
        s.frustum(At::xyz(0.0, 0.0, 0.0), 0.2, 0.1, 0.3, 12, (true, true), c);
        assert_solid(&s.clone().into_mesh(), true);
        let mut s = Shape::new();
        s.frustum(At::xyz(0.0, 0.0, 0.0), 0.2, 0.0, 0.3, 12, (true, false), c);
        assert_solid(&s.clone().into_mesh(), true);
        let mut s = Shape::new();
        s.vessel(At::xyz(0.0, 0.0, 0.0), 0.04, 0.05, 0.1, 0.004, true, 16, c);
        assert_solid(&s.clone().into_mesh(), true);
        let mut s = Shape::new();
        s.vessel(At::xyz(0.0, 0.0, 0.0), 0.2, 0.12, 0.2, 0.004, false, 16, c);
        assert_solid(&s.clone().into_mesh(), true);
        let mut s = Shape::new();
        s.ellipsoid(At::xyz(0.0, 1.0, 0.0), Vec3::new(0.3, 0.2, 0.25), 8, 12, c);
        assert_solid(&s.clone().into_mesh(), true);
        let mut s = Shape::new();
        s.tube(&[Vec3::ZERO, Vec3::new(0.0, 0.2, 0.0), Vec3::new(0.1, 0.3, 0.0), Vec3::new(0.2, 0.3, 0.05)], 0.01, 8, c);
        assert_solid(&s.clone().into_mesh(), true);
        let mut s = Shape::new();
        let baluster = [Vec2::new(0.03, 0.0), Vec2::new(0.03, 0.05), Vec2::new(0.015, 0.1), Vec2::new(0.035, 0.3), Vec2::new(0.012, 0.6), Vec2::new(0.03, 0.65), Vec2::new(0.03, 0.7)];
        s.lathe(At::xyz(0.0, 0.0, 0.0).roll(10.0), &baluster, 12, c);
        assert_solid(&s.clone().into_mesh(), true);
        let mut s = Shape::new();
        s.lathe(At::xyz(0.0, 0.0, 0.0), &[Vec2::new(0.1, 0.0), Vec2::new(0.06, 0.2), Vec2::new(0.0, 0.3)], 12, c);
        assert_solid(&s.clone().into_mesh(), true);
        let mut s = Shape::new();
        s.leaf(At::xyz(0.0, 0.0, 0.0).roll(20.0), 0.2, 0.05, 0.04, c);
        assert_solid(&s.into_mesh(), false);
    }

    #[test]
    fn a_mirrored_placement_keeps_the_winding() {
        let mut s = Shape::new();
        let mirror = At(Affine3A::from_scale(Vec3::new(-1.0, 1.0, 1.0)));
        s.cuboid(mirror, Vec3::splat(0.1), Colour::WHITE);
        assert_solid(&s.into_mesh(), true);
    }
}
