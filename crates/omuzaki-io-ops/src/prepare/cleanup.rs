//! Mesh cleanup: merge primitives by material, weld duplicate vertices and drop unused data, as one pure function over
//! an [`IoScene`] (spec v3 §27, EX-12).
//!
//! The steps run in a fixed order, so the whole function is deterministic and idempotent: merge first (so a weld can
//! see the vertices of every primitive that ended up together), then weld, then drop. Nodes are never removed, because
//! humanoid, skin, spring, constraint, look-at and expression data all refer to nodes by index.
//!
//! Scenes are expected to pass [`IoScene::check`]. A primitive whose attribute arrays disagree in length is left
//! untouched rather than guessed at, and raw JSON in a [`Passthrough`](omuzaki_io_api::Passthrough) is never
//! interpreted, so indices inside it are not remapped (the scene's own passthrough finding says so).

use std::collections::HashMap;

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    Finding, ImageIx, IoScene, MaterialIx, Mesh, MorphTarget, Primitive, Report, Shading, TexRef, TextureIx,
};

/// What [`cleanup`] should do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CleanupOptions {
    /// Weld vertices equal within this distance in metres (and equal in every other attribute); `None` skips welding.
    /// A negative or non-finite value welds only exactly equal positions.
    pub weld: Option<f32>,
    /// Within one mesh, merge primitives that share a material and the same attribute set. Primitives without a
    /// material merge with each other only.
    pub merge_by_material: bool,
    /// Remove materials, textures and images nothing refers to.
    pub drop_unused: bool,
}

/// What [`cleanup`] changed. Each pair is `(before, after)`, summed over every mesh of the scene.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// Vertex count.
    pub vertices: (usize, usize),
    /// Primitive count.
    pub primitives: (usize, usize),
    /// Materials removed.
    pub dropped_materials: usize,
    /// Textures removed.
    pub dropped_textures: usize,
    /// Images removed.
    pub dropped_images: usize,
}

impl CleanupReport {
    /// The changes as info findings, one per kind of change that happened (`cleanup.welded`, `cleanup.merged`,
    /// `cleanup.dropped_unused`). Nothing changed, nothing reported.
    pub fn report(&self) -> Report {
        let mut report = Report::new();
        if self.vertices.1 < self.vertices.0 {
            let msg = format!("welded duplicate vertices: {} before, {} after", self.vertices.0, self.vertices.1);
            report.push(Finding::info("cleanup.welded", msg).at("meshes"));
        }
        if self.primitives.1 < self.primitives.0 {
            let msg =
                format!("merged primitives by material: {} before, {} after", self.primitives.0, self.primitives.1);
            report.push(Finding::info("cleanup.merged", msg).at("meshes"));
        }
        if self.dropped_materials + self.dropped_textures + self.dropped_images > 0 {
            let msg = format!(
                "dropped unused data: {} materials, {} textures, {} images",
                self.dropped_materials, self.dropped_textures, self.dropped_images
            );
            report.push(Finding::info("cleanup.dropped_unused", msg).hint("nothing referred to them"));
        }
        report
    }
}

/// Cleans the scene in place and says what changed. Running it twice with the same options changes nothing the second
/// time.
pub fn cleanup(scene: &mut IoScene, options: &CleanupOptions) -> CleanupReport {
    let count = |scene: &IoScene| {
        let prims = || scene.meshes.iter().flat_map(|m| m.primitives.iter());
        (prims().map(|p| p.mesh.positions.len()).sum::<usize>(), prims().count())
    };
    let (vertices_before, primitives_before) = count(scene);
    if options.merge_by_material {
        scene.meshes.iter_mut().for_each(merge_by_material);
    }
    if let Some(tolerance) = options.weld {
        let tolerance = if tolerance.is_finite() && tolerance > 0.0 { tolerance } else { 0.0 };
        for prim in scene.meshes.iter_mut().flat_map(|m| m.primitives.iter_mut()) {
            weld(prim, tolerance);
        }
    }
    let (vertices_after, primitives_after) = count(scene);
    let mut report = CleanupReport {
        vertices: (vertices_before, vertices_after),
        primitives: (primitives_before, primitives_after),
        ..CleanupReport::default()
    };
    if options.drop_unused {
        drop_unused(scene, &mut report);
    }
    report
}

// ---- Welding ----------------------------------------------------------------------------------------------------

/// Whether every per-vertex array of the primitive is empty or as long as the position array, so that vertices can be
/// compared and moved as a unit.
fn is_consistent(p: &Primitive) -> bool {
    let m = &p.mesh;
    let n = m.positions.len();
    let ok = |len: usize| len == 0 || len == n;
    ok(m.normals.len())
        && ok(m.uvs.len())
        && ok(m.colours.len())
        && ok(m.joints.len())
        && ok(m.weights.len())
        && m.extra.iter().all(|a| a.data.len() == n * usize::from(a.components))
        && p.morph_targets.iter().all(|t| ok(t.positions.len()) && ok(t.normals.len()))
}

/// Whether vertices `first` and `second` of a consistent primitive are the same vertex: positions and morph deltas
/// within `tolerance`, everything else exactly equal.
#[allow(
    clippy::float_cmp,
    reason = "welding merges only vertices that are bit-for-bit equal in every non-position attribute"
)]
fn same_vertex(prim: &Primitive, first: usize, second: usize, tolerance: f32) -> bool {
    let near = |x: Vec3, y: Vec3| (x - y).length() <= tolerance;
    let mesh = &prim.mesh;
    near(mesh.positions[first], mesh.positions[second])
        && (mesh.normals.is_empty() || mesh.normals[first] == mesh.normals[second])
        && (mesh.uvs.is_empty() || mesh.uvs[first] == mesh.uvs[second])
        && (mesh.colours.is_empty() || mesh.colours[first] == mesh.colours[second])
        && (mesh.joints.is_empty() || mesh.joints[first] == mesh.joints[second])
        && (mesh.weights.is_empty() || mesh.weights[first] == mesh.weights[second])
        && mesh.extra.iter().all(|attr| {
            let width = usize::from(attr.components);
            attr.data[first * width..(first + 1) * width] == attr.data[second * width..(second + 1) * width]
        })
        && prim.morph_targets.iter().all(|t| {
            (t.positions.is_empty() || near(t.positions[first], t.positions[second]))
                && (t.normals.is_empty() || near(t.normals[first], t.normals[second]))
        })
}

/// The grid cell a position falls in; cells are `cell` wide, so equal-within-`cell` positions sit in neighbouring cells.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the cast saturates, which only crowds absurd coordinates into one cell"
)]
fn cell_of(position: Vec3, cell: f32) -> [i64; 3] {
    // Float-to-int casts saturate, so even absurd coordinates give a valid (if crowded) cell.
    [position.x, position.y, position.z].map(|c| (c / cell).floor() as i64)
}

/// Welds the primitive's duplicate vertices. A vertex merges into the first kept vertex it matches, so the result keeps
/// first-occurrence order and two kept vertices never match each other (which makes a second pass a no-op).
fn weld(p: &mut Primitive, tolerance: f32) {
    if !is_consistent(p) {
        return;
    }
    let n = p.mesh.positions.len();
    let cell = if tolerance > 0.0 { tolerance } else { 1.0 };
    let mut grid: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
    let mut kept: Vec<usize> = Vec::new();
    let mut new_index: Vec<u32> = Vec::with_capacity(n);
    for v in 0..n {
        let c = cell_of(p.mesh.positions[v], cell);
        let mut found: Option<usize> = None;
        for dx in -1..=1i64 {
            for dy in -1..=1i64 {
                for dz in -1..=1i64 {
                    let key = [c[0].saturating_add(dx), c[1].saturating_add(dy), c[2].saturating_add(dz)];
                    let candidates = grid.get(&key).into_iter().flatten();
                    for &k in candidates.filter(|&&k| same_vertex(p, kept[k], v, tolerance)) {
                        found = Some(found.map_or(k, |f| f.min(k)));
                    }
                }
            }
        }
        let slot = found.unwrap_or_else(|| {
            kept.push(v);
            grid.entry(c).or_default().push(kept.len() - 1);
            kept.len() - 1
        });
        // A kept index is below the vertex count, which indices already fit in a u32.
        new_index.push(u32::try_from(slot).unwrap_or(u32::MAX));
    }
    if kept.len() == n {
        return;
    }
    let m = &mut p.mesh;
    gather(&mut m.positions, &kept, 1);
    gather(&mut m.normals, &kept, 1);
    gather(&mut m.uvs, &kept, 1);
    gather(&mut m.colours, &kept, 1);
    gather(&mut m.joints, &kept, 1);
    gather(&mut m.weights, &kept, 1);
    for attr in &mut m.extra {
        gather(&mut attr.data, &kept, usize::from(attr.components));
    }
    for t in &mut p.morph_targets {
        gather(&mut t.positions, &kept, 1);
        gather(&mut t.normals, &kept, 1);
    }
    for i in &mut m.indices {
        *i = new_index.get(*i as usize).copied().unwrap_or(*i);
    }
}

/// Keeps the `stride` items of each listed vertex, in the order listed; an empty array stays empty.
fn gather<T: Clone>(items: &mut Vec<T>, kept: &[usize], stride: usize) {
    if items.is_empty() {
        return;
    }
    *items = kept.iter().flat_map(|&v| items[v * stride..(v + 1) * stride].iter().cloned()).collect();
}

// ---- Merging ----------------------------------------------------------------------------------------------------

/// What must match for two primitives to share one vertex buffer: the material, which attributes exist, the extra
/// attributes' names and widths, and the shape of the morph targets.
#[derive(PartialEq)]
struct Layout<'a> {
    material: Option<MaterialIx>,
    present: [bool; 5],
    extras: Vec<(&'a str, u8)>,
    morph_normals: Vec<bool>,
}

impl<'a> Layout<'a> {
    fn of(p: &'a Primitive) -> Self {
        let m = &p.mesh;
        Self {
            material: p.material,
            present: [
                !m.normals.is_empty(),
                !m.uvs.is_empty(),
                !m.colours.is_empty(),
                !m.joints.is_empty(),
                !m.weights.is_empty(),
            ],
            extras: m.extra.iter().map(|a| (a.name.as_str(), a.components)).collect(),
            morph_normals: p.morph_targets.iter().map(|t| !t.normals.is_empty()).collect(),
        }
    }
}

/// Merges the mesh's compatible primitives; the merged primitive takes the place of the group's first.
fn merge_by_material(mesh: &mut Mesh) {
    let prims = std::mem::take(&mut mesh.primitives);
    let layouts: Vec<Option<Layout<'_>>> = prims.iter().map(|p| is_consistent(p).then(|| Layout::of(p))).collect();
    // Inconsistent primitives (no layout) always form a group of their own.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (i, layout) in layouts.iter().enumerate() {
        let home = layout.as_ref().and_then(|l| groups.iter().position(|g| layouts[g[0]].as_ref() == Some(l)));
        match home {
            Some(g) => groups[g].push(i),
            None => groups.push(vec![i]),
        }
    }
    let mut slots: Vec<Option<Primitive>> = prims.into_iter().map(Some).collect();
    for group in groups {
        let members: Vec<Primitive> = group.iter().filter_map(|&i| slots[i].take()).collect();
        mesh.primitives.extend(merge_group(members));
    }
}

/// One primitive out of a group of compatible ones, or the group unchanged when it is a single primitive or the
/// result would have more vertices than a `u32` index reaches.
fn merge_group(mut group: Vec<Primitive>) -> Vec<Primitive> {
    let total: usize = group.iter().map(|p| p.mesh.positions.len()).sum();
    if group.len() < 2 || u32::try_from(total).is_err() {
        return group;
    }
    let mut rest = group.split_off(1);
    let mut merged = group.swap_remove(0);
    for next in rest.drain(..) {
        // The checked conversion above covers the sum, so each running count fits as well.
        let offset = u32::try_from(merged.mesh.positions.len()).unwrap_or(u32::MAX);
        let (m, n) = (&mut merged.mesh, next.mesh);
        m.positions.extend(n.positions);
        m.normals.extend(n.normals);
        m.uvs.extend(n.uvs);
        m.colours.extend(n.colours);
        m.joints.extend(n.joints);
        m.weights.extend(n.weights);
        for (attr, other) in m.extra.iter_mut().zip(n.extra) {
            attr.data.extend(other.data);
        }
        m.indices.extend(n.indices.into_iter().map(|i| i + offset));
        for (t, other) in merged.morph_targets.iter_mut().zip(next.morph_targets) {
            extend_target(t, other);
        }
    }
    vec![merged]
}

fn extend_target(target: &mut MorphTarget, other: MorphTarget) {
    target.positions.extend(other.positions);
    target.normals.extend(other.normals);
}

// ---- Dropping unused data ---------------------------------------------------------------------------------------

/// Marks `ix` as used when it is inside the arena.
fn mark(used: &mut [bool], ix: usize) {
    if let Some(slot) = used.get_mut(ix) {
        *slot = true;
    }
}

/// Every texture reference a material holds, in any shading model, as mutable references.
fn texture_refs_mut(material: &mut omuzaki_io_api::Material) -> Vec<&mut TexRef> {
    let mut refs: Vec<&mut TexRef> = Vec::new();
    refs.extend(material.base_color_texture.as_mut());
    refs.extend(material.emissive_texture.as_mut());
    refs.extend(material.normal_texture.as_mut().map(|n| &mut n.texture));
    match &mut material.shading {
        Shading::Pbr(p) => refs.extend(p.metallic_roughness_texture.as_mut()),
        Shading::Unlit => {}
        Shading::Toon(t) => {
            refs.extend(t.shade_multiply_texture.as_mut());
            refs.extend(t.shading_shift_texture.as_mut().map(|s| &mut s.texture));
            refs.extend(t.matcap_texture.as_mut());
            refs.extend(t.rim_multiply_texture.as_mut());
            refs.extend(t.outline_width_texture.as_mut());
            refs.extend(t.uv_animation_mask.as_mut());
        }
    }
    refs
}

/// Removes the arena items not marked in `used` and returns, for every old index, its new one.
fn compact<T>(items: &mut Vec<T>, used: &[bool]) -> Vec<Option<u32>> {
    let mut next = 0u32;
    let map: Vec<Option<u32>> = used
        .iter()
        .map(|&u| {
            u.then(|| {
                next += 1;
                next - 1
            })
        })
        .collect();
    let mut flags = used.iter();
    items.retain(|_| flags.next().copied().unwrap_or(true));
    map
}

/// The new index for `old`. A scene that passes `check` never names an index outside its arenas; if one did, the
/// reference is left as it was.
fn remap(map: &[Option<u32>], old: usize) -> Option<u32> {
    let new = map.get(old).copied().flatten();
    debug_assert!(new.is_some(), "index {old} was not marked as used");
    new
}

fn drop_unused(scene: &mut IoScene, report: &mut CleanupReport) {
    // Materials are used by primitives and by expression binds; textures only by materials that stay; images only by
    // textures that stay and by the thumbnail. So a dropped material takes its textures and images with it.
    let mut used_materials = vec![false; scene.materials.len()];
    for p in scene.meshes.iter().flat_map(|m| &m.primitives) {
        p.material.iter().for_each(|m| mark(&mut used_materials, m.index()));
    }
    if let Some(avatar) = &scene.avatar {
        for e in avatar.expressions.values() {
            e.color_binds.iter().for_each(|b| mark(&mut used_materials, b.material.index()));
            e.uv_binds.iter().for_each(|b| mark(&mut used_materials, b.material.index()));
        }
    }
    let mut used_textures = vec![false; scene.textures.len()];
    let kept_materials = scene.materials.iter().zip(&used_materials).filter(|&(_, &u)| u).map(|(m, _)| m);
    for material in kept_materials {
        material.texture_refs().iter().for_each(|(_, t)| mark(&mut used_textures, t.texture.index()));
    }
    let mut used_images = vec![false; scene.images.len()];
    let kept_textures = scene.textures.iter().zip(&used_textures).filter(|&(_, &u)| u).map(|(t, _)| t);
    for texture in kept_textures {
        mark(&mut used_images, texture.image.index());
    }
    if let Some(thumbnail) = scene.meta.thumbnail_image {
        mark(&mut used_images, thumbnail.index());
    }

    let material_map = compact(&mut scene.materials, &used_materials);
    let texture_map = compact(&mut scene.textures, &used_textures);
    let image_map = compact(&mut scene.images, &used_images);
    report.dropped_materials = used_materials.len() - scene.materials.len();
    report.dropped_textures = used_textures.len() - scene.textures.len();
    report.dropped_images = used_images.len() - scene.images.len();

    let new_material = |ix: MaterialIx| remap(&material_map, ix.index()).map_or(ix, MaterialIx);
    for p in scene.meshes.iter_mut().flat_map(|m| &mut m.primitives) {
        p.material = p.material.map(new_material);
    }
    if let Some(avatar) = &mut scene.avatar {
        for e in avatar.expressions.values_mut() {
            e.color_binds.iter_mut().for_each(|b| b.material = new_material(b.material));
            e.uv_binds.iter_mut().for_each(|b| b.material = new_material(b.material));
        }
    }
    for material in &mut scene.materials {
        for t in texture_refs_mut(material) {
            t.texture = remap(&texture_map, t.texture.index()).map_or(t.texture, TextureIx);
        }
    }
    for texture in &mut scene.textures {
        texture.image = remap(&image_map, texture.image.index()).map_or(texture.image, ImageIx);
    }
    scene.meta.thumbnail_image = scene.meta.thumbnail_image.map(|ix| remap(&image_map, ix.index()).map_or(ix, ImageIx));
}
