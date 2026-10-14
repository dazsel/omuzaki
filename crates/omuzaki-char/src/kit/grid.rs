//! Point grids, their smooth normals and the mesh builder (port of `uniform-geo.js` lines 50 to 96 and 109 to 210).
//!
//! A [`Grid`] is `rows x cols` points; a surface is a grid of such points, joined into quads. The prototype builds every
//! body part from grids, so every surface is smooth, closed and skinned the same way. [`grid_normals`] gives smooth
//! per-vertex normals (with optional wrap-around and pole handling), [`grid_offset`] pushes a grid along them, and
//! [`MeshBuilder`] collects grids into one skinned mesh.

use std::collections::HashMap;

use omuzaki_char_api::{JointRef, MaterialName, PartMesh};
use omuzaki_core::glam::{DVec2, DVec3, Vec3};
use omuzaki_core::mesh::MeshData;

use super::KitError;
use super::curve::fraction;

/// A rectangular grid of points, `rows x cols`, stored row by row. Row `i` and column `j` are the surface's two
/// parameters. The shape is checked on construction (at least 2 x 2, all points finite), so the functions that take a
/// grid cannot fail on an empty or single-row one.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    rows: usize,
    cols: usize,
    points: Vec<DVec3>,
}

/// The number of points in a `rows x cols` grid, after checking the shape: at least 2 x 2, and a product that fits in
/// `usize`. Checked first, so a huge shape fails before any point is computed or allocated.
fn point_count(rows: usize, cols: usize) -> Result<usize, KitError> {
    if rows < 2 {
        return Err(KitError::TooFew { what: "rows of a grid", need: 2, got: rows });
    }
    if cols < 2 {
        return Err(KitError::TooFew { what: "columns of a grid", need: 2, got: cols });
    }
    rows.checked_mul(cols).ok_or(KitError::Overflow { what: "points of a grid (rows x cols)" })
}

impl Grid {
    /// A grid from row-major `points` (`rows * cols` of them).
    ///
    /// # Errors
    /// [`KitError`] for fewer than 2 rows or 2 columns, rows times columns that does not fit in `usize`, a length that is
    /// not `rows * cols`, or a NaN or infinite point.
    pub fn new(rows: usize, cols: usize, points: Vec<DVec3>) -> Result<Self, KitError> {
        let count = point_count(rows, cols)?;
        if points.len() != count {
            return Err(KitError::TooFew { what: "points of a grid (rows x cols)", need: count, got: points.len() });
        }
        if points.iter().any(|p| !p.is_finite()) {
            return Err(KitError::NonFinite { what: "a grid's points" });
        }
        Ok(Self { rows, cols, points })
    }

    /// A grid from rows of points.
    ///
    /// # Errors
    /// [`KitError::Ragged`] if the rows differ in length, otherwise as [`Grid::new`].
    pub fn from_rows(rows: &[Vec<DVec3>]) -> Result<Self, KitError> {
        let cols = rows.first().map_or(0, Vec::len);
        if let Some((row, r)) = rows.iter().enumerate().find(|(_, r)| r.len() != cols) {
            return Err(KitError::Ragged { row, expected: cols, got: r.len() });
        }
        Self::new(rows.len(), cols, rows.concat())
    }

    /// A grid whose point `(i, j)` is `f(i, j)`. `f` is not called if the size is wrong.
    ///
    /// # Errors
    /// As [`Grid::new`].
    pub fn from_fn(rows: usize, cols: usize, mut f: impl FnMut(usize, usize) -> DVec3) -> Result<Self, KitError> {
        // Checked before collecting: a huge shape must fail without calling `f` or building a huge vector.
        point_count(rows, cols)?;
        let points = (0..rows).flat_map(|i| (0..cols).map(move |j| (i, j))).map(|(i, j)| f(i, j)).collect();
        Self::new(rows, cols, points)
    }

    /// The number of rows (the `i` extent).
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The number of columns (the `j` extent).
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// The point at row `i`, column `j`.
    ///
    /// # Panics
    /// If `i >= rows()` or `j >= cols()`.
    pub fn at(&self, i: usize, j: usize) -> DVec3 {
        assert!(i < self.rows && j < self.cols, "grid index ({i}, {j}) outside {}x{}", self.rows, self.cols);
        self.points[i * self.cols + j]
    }

    /// All points, row by row.
    pub fn points(&self) -> &[DVec3] {
        &self.points
    }
}

/// How a vertex is skinned: which bones move it and by how much. At most four influences survive per vertex (the
/// largest), normalised to sum to 1; a vertex left with none falls back to the hips, as in the prototype.
#[derive(Clone)]
pub enum Skin<'a> {
    /// Entirely one bone.
    Bone(JointRef),
    /// The same blend for every vertex.
    Blend(Vec<(JointRef, f32)>),
    /// A blend per vertex, from the vertex position and its grid indices `(i, j)` (the prototype's `w(x, y, z, i, j)`).
    Per(&'a dyn Fn(DVec3, usize, usize) -> Vec<(JointRef, f32)>),
}

impl Default for Skin<'_> {
    /// Everything on the hips, the prototype's default when no weights are given.
    fn default() -> Self {
        Self::Bone(JointRef::HIPS)
    }
}

/// Options for one grid, for [`grid_normals`], [`grid_offset`] and [`MeshBuilder::add_grid`]. The first five only shape
/// the normals (and so the winding); the rest only matter to the builder.
///
/// Normals point along the grid's natural winding, `d(point)/di x d(point)/dj`. Which side is "outward" is decided by
/// [`inside`](Self::inside) when given, else by [`flip`](Self::flip); [`invert`](Self::invert) then reverses the result.
#[derive(Clone, Default)]
#[allow(clippy::struct_excessive_bools)] // the prototype's independent switches; a bitset would hide them from callers
pub struct GridOptions<'a> {
    /// The last row duplicates the first (a ring closed along `i`, such as a torus's major circle).
    pub wrap_i: bool,
    /// The last column duplicates the first (a ring closed along `j`, such as a lathe's turn).
    pub wrap_j: bool,
    /// Use the opposite of the natural winding. Ignored when `inside` is set.
    pub flip: bool,
    /// Reverse whatever orientation was chosen (also for pole normals).
    pub invert: bool,
    /// An interior reference point for vertex `(i, j)`: normals are oriented away from it, by majority of the surface.
    pub inside: Option<&'a dyn Fn(usize, usize) -> DVec3>,
    /// Texture coordinates of vertex `(i, j)`. Default: `u = j / (cols - 1)`, `v = i / (rows - 1)`.
    pub uv: Option<&'a dyn Fn(usize, usize) -> DVec2>,
    /// Linear RGBA vertex colour of vertex `(i, j)` at a position. Default: opaque white. Used only when the builder
    /// was made [`with_colours`](MeshBuilder::with_colours).
    pub colour: Option<&'a dyn Fn(usize, usize, DVec3) -> [f32; 4]>,
    /// How the vertices are skinned. Default: all on the hips.
    pub skin: Skin<'a>,
}

/// Smooth normals for a grid, one per point in the grid's order.
#[derive(Clone, Debug, PartialEq)]
pub struct GridNormals {
    /// Unit normals, row by row.
    pub normals: Vec<DVec3>,
    /// True if the surface is wound opposite to the grid's natural order (the builder flips its triangles to match).
    pub flipped: bool,
}

/// Index of the neighbours used for a central difference at `k` along an axis of `last + 1` points.
fn neighbours(k: usize, last: usize, wrap: bool) -> (usize, usize) {
    if wrap {
        // A wrapped axis repeats its first point at the end, so both ends use the points either side of the seam.
        if k == 0 || k == last { (last - 1, 1) } else { (k - 1, k + 1) }
    } else {
        (k.saturating_sub(1), (k + 1).min(last))
    }
}

/// `v` divided by its length, or by 1 if the length is zero or NaN (the prototype's `length || 1`).
fn unit_or_same(v: DVec3) -> DVec3 {
    let l = v.length();
    v / if l == 0.0 || l.is_nan() { 1.0 } else { l }
}

/// Smooth per-vertex normals for `grid`, from central differences of the neighbouring rows and columns.
///
/// A collapsed ring (a pole, where a whole row is one point) has no defined normal; its vertices point away from the
/// neighbouring row, and an end ring gets one shared normal, the average of its copies. As in the prototype the average
/// includes the repeated seam column of a wrapped ring, so the shared normal leans toward the seam by an angle that
/// shrinks as segments are added. Orientation follows the [`GridOptions`] doc.
///
/// # Errors
/// [`KitError::TooFew`] if `wrap_i` is set with fewer than 3 rows or `wrap_j` with fewer than 3 columns (a wrapped
/// axis repeats its first point, so 2 would be a single point).
pub fn grid_normals(grid: &Grid, options: &GridOptions<'_>) -> Result<GridNormals, KitError> {
    let (rows, cols) = (grid.rows, grid.cols);
    if options.wrap_i && rows < 3 {
        return Err(KitError::TooFew { what: "rows of a grid wrapped in i", need: 3, got: rows });
    }
    if options.wrap_j && cols < 3 {
        return Err(KitError::TooFew { what: "columns of a grid wrapped in j", need: 3, got: cols });
    }
    let (last_i, last_j) = (rows - 1, cols - 1);
    // (unit normal, whether the vertex is on a collapsed ring)
    let mut raw: Vec<(DVec3, bool)> = Vec::with_capacity(rows * cols);
    let mut orient = 0.0;
    for i in 0..rows {
        for j in 0..cols {
            let (i0, i1) = neighbours(i, last_i, options.wrap_i);
            let (j0, j1) = neighbours(j, last_j, options.wrap_j);
            let di = grid.at(i1, j) - grid.at(i0, j);
            let dj = grid.at(i, j1) - grid.at(i, j0);
            let n = di.cross(dj);
            if n.length() < 1e-14 {
                let away = if i == 0 {
                    grid.at(0, j) - grid.at(1.min(last_i), j)
                } else if i == last_i {
                    grid.at(last_i, j) - grid.at(last_i.saturating_sub(1), j)
                } else {
                    grid.at(i, j) - grid.at(i0, j)
                };
                raw.push((unit_or_same(away), true));
            } else {
                raw.push((unit_or_same(n), false));
                if let Some(inside) = options.inside {
                    // Weighted by the (unnormalised) cross product, like the prototype: big faces decide.
                    orient += n.dot(grid.at(i, j) - inside(i, j));
                }
            }
        }
    }
    let mut flipped = options.flip;
    if options.inside.is_some() {
        flipped = orient < 0.0;
    }
    if options.invert {
        flipped = !flipped;
    }
    let surface_sign = if flipped { -1.0 } else { 1.0 };
    let pole_sign = if options.invert { -1.0 } else { 1.0 };
    let mut normals: Vec<DVec3> =
        raw.iter().map(|&(n, pole)| n * if pole { pole_sign } else { surface_sign }).collect();
    // A collapsed end ring is one point: all its copies share one averaged normal.
    for i in [0, last_i] {
        if !raw[i * cols].1 {
            continue;
        }
        let ring = i * cols..(i + 1) * cols;
        let shared = unit_or_same(normals[ring.clone()].iter().sum());
        normals[ring].fill(shared);
    }
    Ok(GridNormals { normals, flipped })
}

/// `grid` with every point moved by `distance` along its (outward) normal.
///
/// # Errors
/// As [`grid_normals`]; also [`KitError::NonFinite`] if `distance` is NaN or infinite.
pub fn grid_offset(grid: &Grid, distance: f64, options: &GridOptions<'_>) -> Result<Grid, KitError> {
    let GridNormals { normals, .. } = grid_normals(grid, options)?;
    let points = grid.points.iter().zip(&normals).map(|(p, n)| *p + *n * distance).collect();
    Grid::new(grid.rows, grid.cols, points)
}

/// The joint on the opposite side of the body. A humanoid bone takes its VRM counterpart (`leftHand` and `rightHand`);
/// a named bone swaps its `l` and `r` segments (`ear_l_1` and `ear_r_1`); anything else is its own mirror.
pub fn mirror_joint(joint: &JointRef) -> JointRef {
    joint.mirrored()
}

/// One vertex's skin: up to four joint indices (into the builder's joint list) and their weights.
type Influences = ([u16; 4], [f32; 4]);

/// Accumulates grids and polygons into one skinned mesh: positions, normals, UVs, optional vertex colours, triangles
/// and, per vertex, four normalised skin weights keyed by [`JointRef`]. This is the prototype's `MB`.
///
/// Skin weights refer to bones by role or name, not by a skeleton's index: [`finish`](Self::finish) produces a
/// [`PartMesh`] whose `joint_bones` list is exactly the joints used, in first-use order.
#[derive(Clone, Debug, Default)]
pub struct MeshBuilder {
    mesh: MeshData,
    bones: Vec<JointRef>,
    /// `bones` by joint, so a vertex's influences are found without scanning the list.
    bone_index: HashMap<JointRef, u16>,
    colours: bool,
}

impl MeshBuilder {
    /// An empty builder without vertex colours.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty builder that records a vertex colour (white unless given) for every vertex.
    pub fn with_colours() -> Self {
        Self { colours: true, ..Self::default() }
    }

    /// The vertices so far.
    pub fn vertex_count(&self) -> usize {
        self.mesh.positions.len()
    }

    /// The triangles so far.
    pub fn triangle_count(&self) -> usize {
        self.mesh.triangle_count()
    }

    /// The geometry so far, unvalidated (skin joint indices refer to [`bones`](Self::bones)).
    pub fn mesh(&self) -> &MeshData {
        &self.mesh
    }

    /// The joints the skin indices refer to, in first-use order.
    pub fn bones(&self) -> &[JointRef] {
        &self.bones
    }

    /// The index of `joint` in the joint list, adding it on first use.
    fn joint_index(&mut self, joint: &JointRef) -> Result<u16, KitError> {
        if let Some(&index) = self.bone_index.get(joint) {
            return Ok(index);
        }
        let index = u16::try_from(self.bones.len())
            .map_err(|_| KitError::TooMany { what: "skin joints", max: usize::from(u16::MAX) + 1 })?;
        self.bones.push(joint.clone());
        self.bone_index.insert(joint.clone(), index);
        Ok(index)
    }

    /// Keeps the four largest positive weights (ties keep their given order), merges repeated joints, normalises them to
    /// sum to 1 and indexes them. No usable weight means all on the hips.
    #[allow(clippy::cast_possible_truncation)] // weights are in 0..=1; f32 is the mesh's precision
    fn influences(&mut self, blend: &[(JointRef, f32)]) -> Result<Influences, KitError> {
        let mut acc: Vec<(&JointRef, f64)> = Vec::with_capacity(blend.len());
        for (joint, w) in blend.iter().filter(|(_, w)| w.is_finite() && *w > 0.0) {
            match acc.iter_mut().find(|(k, _)| *k == joint) {
                Some(entry) => entry.1 += f64::from(*w),
                None => acc.push((joint, f64::from(*w))),
            }
        }
        let hips = JointRef::HIPS;
        if acc.is_empty() {
            acc.push((&hips, 1.0));
        }
        acc.sort_by(|a, b| b.1.total_cmp(&a.1));
        acc.truncate(4);
        let total: f64 = acc.iter().map(|(_, w)| w).sum();
        let (mut joints, mut weights) = ([0_u16; 4], [0.0_f32; 4]);
        for (slot, (joint, w)) in acc.into_iter().enumerate() {
            joints[slot] = self.joint_index(joint)?;
            weights[slot] = (w / total) as f32;
        }
        Ok((joints, weights))
    }

    /// Runs `build`; if it fails, forgets the joints it registered, so a failed add leaves the builder as it was.
    /// (`build` must push vertices and triangles only after its last fallible step.)
    fn rolled_back<T>(&mut self, build: impl FnOnce(&mut Self) -> Result<T, KitError>) -> Result<T, KitError> {
        let bones_before = self.bones.len();
        let result = build(self);
        if result.is_err() {
            for joint in self.bones.drain(bones_before..) {
                self.bone_index.remove(&joint);
            }
        }
        result
    }

    /// Checks that `extra` more vertices still fit the `u32` index range, and returns the index the next one gets.
    fn next_vertex(&self, extra: usize) -> Result<u32, KitError> {
        let end = self.vertex_count().checked_add(extra).ok_or_else(too_many_vertices)?;
        u32::try_from(end).map_err(|_| too_many_vertices())?;
        index_u32(self.vertex_count())
    }

    /// Appends one vertex with its attributes.
    fn push_vertex(&mut self, position: DVec3, normal: DVec3, uv: DVec2, colour: [f32; 4], skin: Influences) {
        self.mesh.positions.push(position.as_vec3());
        self.mesh.normals.push(normal.as_vec3());
        self.mesh.uvs.push(uv.as_vec2());
        if self.colours {
            self.mesh.colours.push(colour);
        }
        self.mesh.joints.push(skin.0);
        self.mesh.weights.push(skin.1);
    }

    /// Appends `grid` as a quad surface: its vertices with smooth normals, UVs, colours and skin, and two triangles per
    /// cell, wound to match the orientation [`grid_normals`] chose. On error the builder is left unchanged.
    ///
    /// # Errors
    /// As [`grid_normals`]; [`KitError::TooMany`] if the mesh would exceed the index range.
    pub fn add_grid(&mut self, grid: &Grid, options: &GridOptions<'_>) -> Result<(), KitError> {
        self.rolled_back(|b| b.add_grid_unchecked(grid, options))
    }

    fn add_grid_unchecked(&mut self, grid: &Grid, options: &GridOptions<'_>) -> Result<(), KitError> {
        let GridNormals { normals, flipped } = grid_normals(grid, options)?;
        let (rows, cols) = (grid.rows, grid.cols);
        let base = self.next_vertex(rows * cols)?;
        let skins: Vec<Influences> = match &options.skin {
            Skin::Bone(joint) => vec![self.influences(&[(joint.clone(), 1.0)])?; rows * cols],
            Skin::Blend(blend) => vec![self.influences(blend)?; rows * cols],
            Skin::Per(per_vertex) => (0..rows * cols)
                .map(|k| self.influences(&per_vertex(grid.points[k], k / cols, k % cols)))
                .collect::<Result<_, _>>()?,
        };
        for i in 0..rows {
            for j in 0..cols {
                let k = i * cols + j;
                let p = grid.points[k];
                let uv =
                    options.uv.map_or_else(|| DVec2::new(fraction(j, cols - 1), fraction(i, rows - 1)), |f| f(i, j));
                let colour = options.colour.map_or([1.0; 4], |f| f(i, j, p));
                self.push_vertex(p, normals[k], uv, colour, skins[k]);
            }
        }
        // `next_vertex` checked that every index below fits a u32, so these conversions and sums cannot overflow.
        let (rows, cols) = (index_u32(rows)?, index_u32(cols)?);
        for i in 0..rows - 1 {
            for j in 0..cols - 1 {
                let first = base + i * cols + j;
                let below = first + cols;
                let (right, diagonal) = (first + 1, below + 1);
                if flipped {
                    self.mesh.indices.extend([first, right, below, below, right, diagonal]);
                } else {
                    self.mesh.indices.extend([first, below, right, below, diagonal, right]);
                }
            }
        }
        Ok(())
    }

    /// Appends a flat-shaded convex polygon (a fan from its first point) with one `normal`, one `skin` and no UVs. For
    /// decals and dashes. On error the builder is left unchanged.
    ///
    /// # Errors
    /// [`KitError::TooFew`] for fewer than 3 points, [`KitError::NonFinite`] for a NaN or infinite point or normal,
    /// [`KitError::TooMany`] if the mesh would exceed the index range.
    pub fn add_polygon(
        &mut self,
        points: &[DVec3],
        normal: DVec3,
        colour: [f32; 4],
        skin: &[(JointRef, f32)],
    ) -> Result<(), KitError> {
        if points.len() < 3 {
            return Err(KitError::TooFew { what: "points of a polygon", need: 3, got: points.len() });
        }
        if !normal.is_finite() || points.iter().any(|p| !p.is_finite()) {
            return Err(KitError::NonFinite { what: "a polygon's points and normal" });
        }
        self.rolled_back(|b| {
            let base = b.next_vertex(points.len())?;
            let influences = b.influences(skin)?;
            for &p in points {
                b.push_vertex(p, normal, DVec2::ZERO, colour, influences);
            }
            for k in 1..index_u32(points.len())? - 1 {
                b.mesh.indices.extend([base, base + k, base + k + 1]);
            }
            Ok(())
        })
    }

    /// Appends all of `other` (the prototype's `MB.add`). Vertex colours follow this builder's mode: a coloured builder
    /// takes white for vertices from a colourless one, a colourless builder drops `other`'s colours. On error the
    /// builder is left unchanged.
    ///
    /// # Errors
    /// [`KitError::TooMany`] if the combined mesh would exceed the index range.
    pub fn merge(&mut self, other: &Self) -> Result<(), KitError> {
        self.rolled_back(|b| {
            let base = b.next_vertex(other.vertex_count())?;
            let remap = other.bones.iter().map(|joint| b.joint_index(joint)).collect::<Result<Vec<u16>, _>>()?;
            let (mine, theirs) = (&mut b.mesh, &other.mesh);
            mine.positions.extend(&theirs.positions);
            mine.normals.extend(&theirs.normals);
            mine.uvs.extend(&theirs.uvs);
            if b.colours {
                if other.colours {
                    mine.colours.extend(&theirs.colours);
                } else {
                    mine.colours.resize(mine.positions.len(), [1.0; 4]);
                }
            }
            // Every joint index of `other` is below its joint list's length, and a slot a vertex doesn't use (weight 0)
            // holds index 0, which any non-empty list has.
            mine.joints.extend(theirs.joints.iter().map(|slots| slots.map(|j| remap[usize::from(j)])));
            mine.weights.extend(&theirs.weights);
            mine.indices.extend(theirs.indices.iter().map(|i| base + i));
            Ok(())
        })
    }

    /// The left/right mirror image: x negated, triangle winding reversed (so a closed shell keeps a positive volume and
    /// its normals stay outward) and every skin joint replaced by its opposite-side joint ([`mirror_joint`]).
    #[must_use]
    pub fn mirrored(&self) -> Self {
        let flip_x = |v: &Vec3| Vec3::new(-v.x, v.y, v.z);
        let mesh = MeshData {
            positions: self.mesh.positions.iter().map(flip_x).collect(),
            normals: self.mesh.normals.iter().map(flip_x).collect(),
            uvs: self.mesh.uvs.clone(),
            colours: self.mesh.colours.clone(),
            joints: self.mesh.joints.clone(),
            weights: self.mesh.weights.clone(),
            extra: Vec::new(),
            indices: self
                .mesh
                .indices
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|&[first, second, third]| [first, third, second])
                .collect(),
        };
        let bones: Vec<JointRef> = self.bones.iter().map(mirror_joint).collect();
        // `mirror_joint` is its own inverse, so the mirrored joints are as distinct as the originals.
        let bone_index = bones.iter().cloned().zip(0..=u16::MAX).collect();
        Self { mesh, bones, bone_index, colours: self.colours }
    }

    /// The finished part mesh called `name` using `material`: casts shadows, draws at order 0, not dynamic (change the
    /// fields for anything else).
    ///
    /// # Errors
    /// [`KitError::NonFinite`] for a NaN or infinite UV or colour (`validate` checks positions and normals only, and
    /// `MeshData` lives outside this crate); [`KitError::Mesh`] if the geometry breaks a `MeshData` or `PartMesh` rule,
    /// which includes an empty builder.
    pub fn finish(self, name: impl Into<String>, material: MaterialName) -> Result<PartMesh, KitError> {
        if self.mesh.uvs.iter().any(|uv| !uv.is_finite()) {
            return Err(KitError::NonFinite { what: "a mesh's UVs" });
        }
        if self.mesh.colours.iter().flatten().any(|c| !c.is_finite()) {
            return Err(KitError::NonFinite { what: "a mesh's colours" });
        }
        let part = PartMesh {
            name: name.into(),
            geometry: self.mesh,
            joint_bones: self.bones,
            material,
            casts_shadow: true,
            render_order: 0,
            dynamic: false,
        };
        part.validate()?;
        Ok(part)
    }
}

/// A vertex count or index as a `u32`.
fn index_u32(n: usize) -> Result<u32, KitError> {
    u32::try_from(n).map_err(|_| too_many_vertices())
}

fn too_many_vertices() -> KitError {
    KitError::TooMany { what: "vertices", max: u32::MAX as usize }
}
