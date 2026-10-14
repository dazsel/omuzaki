//! Meshes and skins: primitives decoded into the shared [`MeshData`], morph targets, skinning sets and strips/fans.
//!
//! **Budgets.** Every allocation here follows a count the file *declares*, so each is charged to a [`Pool`] first and
//! refused with [`IoError::Limit`] when the pool is spent. Vertices are capped by `Limits::max_vertices`; indices and
//! morph-target deltas get a multiple of it, because a triangle mesh has several indices per vertex and an expression
//! rig has dozens of targets. The multiples are generous for real models (a face with 52 targets over 60k vertices
//! uses 3 million deltas against a default budget of 32 million) and finite for hostile ones, where a few bytes of JSON
//! can name the same accessor thousands of times.

use omuzaki_core::glam::{Mat4, Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_io_api::{
    ExtraAttribute, IoError, Limits, MaterialIx, Mesh, MeshData, MorphTarget, NodeIx, Primitive, Severity, Skin,
};

use super::extensions;
use crate::accessor::{Accessors, Bound, Flavor};
use crate::hooks::{Carrier, ImportHooks};
use crate::ix32;
use crate::schema::{self as gltf, code};
use crate::tally::Tally;

/// Index budget as a multiple of the vertex budget: a mesh has about 6 indices per vertex.
const INDEX_FACTOR: u64 = 8;
/// Morph-delta budget as a multiple of the vertex budget.
const MORPH_FACTOR: u64 = 4;
/// `JOINTS_n` sets read per primitive (16 influences); more are reported and ignored.
const MAX_SKIN_SETS: usize = 4;

/// A counted allowance: how many elements may still be decoded for one purpose.
struct Pool {
    what: &'static str,
    limit: u64,
    used: u64,
}

impl Pool {
    fn new(what: &'static str, limit: u64) -> Self {
        Self { what, limit, used: 0 }
    }

    fn remaining(&self) -> usize {
        usize::try_from(self.limit.saturating_sub(self.used)).unwrap_or(usize::MAX)
    }

    /// The bound to hand an accessor reader: at most what is left.
    fn bound(&self) -> Bound {
        Bound::AtMost { max: self.remaining(), what: self.what }
    }

    fn take(&mut self, n: usize) -> Result<(), IoError> {
        let got = self.used.saturating_add(n as u64);
        if got > self.limit {
            return Err(IoError::Limit { what: self.what, limit: self.limit, got });
        }
        self.used = got;
        Ok(())
    }
}

/// Reads meshes and skins of one document.
pub(crate) struct Geometry<'a> {
    acc: Accessors<'a>,
    vertices: Pool,
    indices: Pool,
    morph: Pool,
    max_nodes: usize,
}

impl<'a> Geometry<'a> {
    pub(crate) fn new(acc: Accessors<'a>, limits: &Limits) -> Self {
        let vertices = u64::from(limits.max_vertices);
        Self {
            acc,
            vertices: Pool::new("vertex count", vertices),
            indices: Pool::new("index count", vertices.saturating_mul(INDEX_FACTOR)),
            morph: Pool::new("morph target delta count", vertices.saturating_mul(MORPH_FACTOR)),
            max_nodes: limits.max_nodes as usize,
        }
    }

    /// Builds mesh `index`. Primitives the scene can't hold are dropped with a finding; the mesh keeps its place so
    /// node references stay valid.
    pub(crate) fn mesh(
        &mut self,
        index: usize,
        def: &gltf::Mesh,
        hooks: &mut dyn ImportHooks,
        tally: &mut Tally,
    ) -> Result<Mesh, IoError> {
        let mut mesh = Mesh::new(def.name.clone());
        for (p, prim) in def.primitives.iter().enumerate() {
            let at = format!("/meshes/{index}/primitives/{p}");
            if let Some(primitive) = self.primitive(prim, &at, tally)? {
                mesh.primitives.push(primitive);
            }
        }
        if let Some(names) = def.target_names() {
            if names.len() == mesh.morph_target_count() {
                mesh.morph_target_names = names;
            } else {
                let msg = "the mesh names a different number of morph targets than it has; the names were dropped";
                tally.warn("gltf.target_names_dropped", msg, format!("/meshes/{index}/extras/targetNames"), None);
            }
        }
        report_mesh_extras(index, def, hooks, tally);
        Ok(mesh)
    }

    fn primitive(&mut self, def: &gltf::Primitive, at: &str, tally: &mut Tally) -> Result<Option<Primitive>, IoError> {
        let mode = def.mode.unwrap_or(code::TRIANGLES);
        match mode {
            code::TRIANGLES..=code::TRIANGLE_FAN => {}
            0..=3 => {
                let msg = format!("primitive mode {mode} (points, lines) can't be represented and was dropped");
                tally.warn("gltf.primitive_dropped", msg, at, None);
                return Ok(None);
            }
            _ => {
                let message = format!("unknown primitive mode {mode}");
                return Err(IoError::Malformed { message, at: Some(at.into()) });
            }
        }
        for ext in def.extensions.keys() {
            tally.info(
                "gltf.extension_dropped",
                format!("primitive extension {ext} has no place in the scene"),
                format!("{at}/extensions/{ext}"),
            );
        }
        let position = *def.attributes.get("POSITION").ok_or_else(|| IoError::Malformed {
            message: "a primitive needs a POSITION attribute".into(),
            at: Some(format!("{at}/attributes")),
        })?;
        let n = self.acc.count(position)?;
        if n == 0 {
            tally.warn("gltf.primitive_dropped", "a primitive with no vertices was dropped", at, None);
            return Ok(None);
        }
        self.vertices.take(n)?;
        let positions = self.acc.floats::<3>(position, Bound::Exactly(n), Flavor::Attribute)?;
        let mut mesh =
            MeshData { positions: positions.into_iter().map(Vec3::from_array).collect(), ..MeshData::default() };
        mesh.indices = self.triangle_list(def, mode, n, at, tally)?;
        if mesh.indices.is_empty() {
            tally.warn("gltf.primitive_dropped", "a primitive with no triangles was dropped", at, None);
            return Ok(None);
        }
        if let Some(bad) = mesh.indices.iter().find(|&&i| i as usize >= n) {
            let message = format!("index {bad} is past the last vertex ({})", n - 1);
            return Err(IoError::Malformed { message, at: Some(format!("{at}/indices")) });
        }
        self.attributes(def, n, &mut mesh, at, tally)?;
        let mut out = Primitive::new(mesh);
        out.material = def.material.map(|m| MaterialIx(ix32(m)));
        self.morph_targets(def, n, &mut out, at, tally)?;
        self.ensure_normals(&mut out, at, tally)?;
        Ok(Some(out))
    }

    /// The primitive's triangles as a plain list, whatever the mode.
    fn triangle_list(
        &mut self,
        def: &gltf::Primitive,
        mode: u32,
        vertices: usize,
        at: &str,
        tally: &mut Tally,
    ) -> Result<Vec<u32>, IoError> {
        let indices = if let Some(ix) = def.indices {
            let found = self.acc.indices(ix, self.indices.bound())?;
            self.indices.take(found.len())?;
            found
        } else {
            self.indices.take(vertices)?;
            (0..ix32(vertices)).collect()
        };
        let mut list = match mode {
            code::TRIANGLE_STRIP => {
                self.indices.take(3 * indices.len().saturating_sub(2))?;
                tally.info("gltf.primitive_converted", "a triangle strip was converted to a triangle list", at);
                strip_to_list(&indices)
            }
            code::TRIANGLE_FAN => {
                self.indices.take(3 * indices.len().saturating_sub(2))?;
                tally.info("gltf.primitive_converted", "a triangle fan was converted to a triangle list", at);
                fan_to_list(&indices)
            }
            _ => indices,
        };
        let whole = list.len() / 3 * 3;
        if whole != list.len() {
            list.truncate(whole);
            tally.warn(
                "gltf.indices_truncated",
                "trailing indices that don't make a whole triangle were dropped",
                at,
                None,
            );
        }
        Ok(list)
    }

    /// Reads every vertex attribute the shared mesh holds; reports the ones it doesn't.
    fn attributes(
        &mut self,
        def: &gltf::Primitive,
        n: usize,
        mesh: &mut MeshData,
        at: &str,
        tally: &mut Tally,
    ) -> Result<(), IoError> {
        let exactly = Bound::Exactly(n);
        for (name, &ix) in &def.attributes {
            let place = format!("{at}/attributes/{name}");
            match classify(name) {
                Attribute::Position | Attribute::Joints | Attribute::Weights => {}
                Attribute::Normal => {
                    mesh.normals = named(name, self.acc.floats::<3>(ix, exactly, Flavor::Attribute))?
                        .into_iter()
                        .map(Vec3::from_array)
                        .collect();
                }
                Attribute::Texcoord(0) => {
                    mesh.uvs = named(name, self.acc.floats::<2>(ix, exactly, Flavor::Attribute))?
                        .into_iter()
                        .map(Vec2::from_array)
                        .collect();
                }
                Attribute::Color(0) => mesh.colours = named(name, self.acc.colours(ix, exactly))?,
                Attribute::Custom if mesh.extra.len() < MeshData::MAX_EXTRA_ATTRIBUTES => {
                    match named(name, self.custom(name, ix, exactly))? {
                        Some(extra) => mesh.extra.push(extra),
                        None => tally.info(
                            "gltf.attribute_dropped",
                            format!("attribute {name} isn't a float scalar or vector with a usable name"),
                            place,
                        ),
                    }
                }
                Attribute::Texcoord(_)
                | Attribute::Color(_)
                | Attribute::Tangent
                | Attribute::Custom
                | Attribute::Other => {
                    tally.info("gltf.attribute_dropped", dropped_message(name), place);
                }
            }
        }
        self.skinning(def, n, mesh, at, tally)
    }

    /// An application-specific attribute (`_STRAND_UV`) as a named extra, or `None` if its type or name doesn't fit.
    fn custom(&self, name: &str, ix: usize, bound: Bound) -> Result<Option<ExtraAttribute>, IoError> {
        let Some(slug) = custom_slug(name) else { return Ok(None) };
        let Some((components, data)) = self.acc.flat_floats(ix, bound)? else { return Ok(None) };
        Ok(Some(ExtraAttribute { name: slug, components, data }))
    }

    /// `JOINTS_n` / `WEIGHTS_n`: one set is kept as is, several are folded into the four strongest influences.
    fn skinning(
        &mut self,
        def: &gltf::Primitive,
        n: usize,
        mesh: &mut MeshData,
        at: &str,
        tally: &mut Tally,
    ) -> Result<(), IoError> {
        let mut sets = Vec::new();
        for set in 0..MAX_SKIN_SETS {
            let (joints, weights) = (format!("JOINTS_{set}"), format!("WEIGHTS_{set}"));
            let (Some(&j), Some(&w)) = (def.attributes.get(&joints), def.attributes.get(&weights)) else {
                // No more sets, or a half of a pair: either way the rest are reported as unread below.
                break;
            };
            let joints = named(&joints, self.acc.joints(j, Bound::Exactly(n)))?;
            let weights = named(&weights, self.acc.floats::<4>(w, Bound::Exactly(n), Flavor::Weight))?;
            sets.push((joints, weights));
        }
        report_unread_sets(def, sets.len(), at, tally);
        match sets.len() {
            0 => {}
            1 => (mesh.joints, mesh.weights) = sets.remove(0),
            _ => {
                (mesh.joints, mesh.weights) = fold_influences(&sets);
                let msg =
                    format!("{} skinning sets were folded into the four strongest influences per vertex", sets.len());
                tally.warn(
                    "gltf.joints_folded",
                    msg,
                    format!("{at}/attributes"),
                    Some("re-export the model with 4 influences per vertex to avoid the loss"),
                );
            }
        }
        renormalise(&mut mesh.weights);
        Ok(())
    }

    fn morph_targets(
        &mut self,
        def: &gltf::Primitive,
        n: usize,
        out: &mut Primitive,
        at: &str,
        tally: &mut Tally,
    ) -> Result<(), IoError> {
        for (t, target) in def.targets.iter().enumerate() {
            self.morph.take(n)?;
            let place = format!("{at}/targets/{t}");
            let mut morph = MorphTarget::new(Vec::new());
            for (name, &ix) in target {
                let deltas = || {
                    let read = self.acc.floats::<3>(ix, Bound::Exactly(n), Flavor::Attribute);
                    named(&format!("morph target {t} {name}"), read)
                };
                match name.as_str() {
                    "POSITION" => morph.positions = deltas()?.into_iter().map(Vec3::from_array).collect(),
                    "NORMAL" => morph.normals = deltas()?.into_iter().map(Vec3::from_array).collect(),
                    other => tally.info(
                        "gltf.attribute_dropped",
                        format!("morph target attribute {other} is not kept"),
                        format!("{place}/{other}"),
                    ),
                }
            }
            if morph.positions.is_empty() {
                // A target may move only normals; the scene wants one position delta per vertex.
                morph.positions = vec![Vec3::ZERO; n];
            }
            out.morph_targets.push(morph);
        }
        Ok(())
    }

    /// The shared mesh always has normals; glTF lets a file omit them, and then clients must use flat ones.
    fn ensure_normals(&mut self, prim: &mut Primitive, at: &str, tally: &mut Tally) -> Result<(), IoError> {
        if !prim.mesh.normals.is_empty() {
            return Ok(());
        }
        // Flat normals give every triangle corner its own vertex, so the primitive grows to `indices` vertices.
        let grown = prim.mesh.indices.len().saturating_sub(prim.mesh.positions.len());
        self.vertices.take(grown)?;
        if !prim.generate_flat_normals() {
            return Err(IoError::Malformed {
                message: "vertex attributes are inconsistent, so normals can't be generated".into(),
                at: Some(at.into()),
            });
        }
        let msg = "the file has no normals, so flat normals were generated (vertices are no longer shared)";
        tally.info("gltf.normals_generated", msg, format!("{at}/attributes/NORMAL"));
        Ok(())
    }

    /// Builds skin `index`.
    pub(crate) fn skin(&self, index: usize, def: &gltf::Skin) -> Result<Skin, IoError> {
        if def.joints.len() > self.max_nodes {
            return Err(IoError::Limit {
                what: "joint count",
                limit: self.max_nodes as u64,
                got: def.joints.len() as u64,
            });
        }
        let inverse_bind = match def.inverse_bind_matrices {
            None => vec![Mat4::IDENTITY; def.joints.len()],
            Some(ix) => {
                let bound = Bound::AtMost { max: self.max_nodes, what: "joint count" };
                let mut matrices: Vec<Mat4> =
                    self.acc.floats::<16>(ix, bound, Flavor::Matrix)?.iter().map(Mat4::from_cols_array).collect();
                if matrices.len() < def.joints.len() {
                    let message = format!("{} inverse bind matrices for {} joints", matrices.len(), def.joints.len());
                    return Err(IoError::Malformed { message, at: Some(format!("/skins/{index}")) });
                }
                matrices.truncate(def.joints.len());
                matrices
            }
        };
        let mut skin = Skin::new(def.name.clone(), def.joints.iter().map(|&j| NodeIx(ix32(j))).collect(), inverse_bind);
        skin.skeleton = def.skeleton.map(|s| NodeIx(ix32(s)));
        Ok(skin)
    }
}

/// Names the attribute in a malformed-file error, so a mismatch reads "`JOINTS_1`: 2 elements, but the primitive has 3
/// vertices" rather than naming only an accessor.
fn named<T>(name: &str, result: Result<T, IoError>) -> Result<T, IoError> {
    result.map_err(|e| match e {
        IoError::Malformed { message, at } => IoError::Malformed { message: format!("{name}: {message}"), at },
        other => other,
    })
}

/// What a vertex attribute name means to the scene.
enum Attribute {
    Position,
    Normal,
    Tangent,
    Texcoord(u32),
    Color(u32),
    Joints,
    Weights,
    /// `_NAME`: application-specific.
    Custom,
    Other,
}

fn classify(name: &str) -> Attribute {
    let numbered = |prefix: &str| name.strip_prefix(prefix).and_then(|n| n.parse::<u32>().ok());
    match name {
        "POSITION" => Attribute::Position,
        "NORMAL" => Attribute::Normal,
        "TANGENT" => Attribute::Tangent,
        _ if name.starts_with('_') => Attribute::Custom,
        _ => numbered("TEXCOORD_")
            .map(Attribute::Texcoord)
            .or_else(|| numbered("COLOR_").map(Attribute::Color))
            .or_else(|| numbered("JOINTS_").map(|_| Attribute::Joints))
            .or_else(|| numbered("WEIGHTS_").map(|_| Attribute::Weights))
            .unwrap_or(Attribute::Other),
    }
}

fn dropped_message(name: &str) -> String {
    match classify(name) {
        Attribute::Tangent => "TANGENT isn't kept: tangents are recomputed (MikkTSpace) where needed".into(),
        Attribute::Texcoord(_) => format!("{name} isn't kept: only the first UV set is"),
        Attribute::Color(_) => format!("{name} isn't kept: only the first colour set is"),
        _ => format!("attribute {name} is not kept"),
    }
}

/// `_STRAND_UV` becomes the slug `strand_uv`. Only the names the exporter writes back exactly (`[A-Z0-9_]`) convert,
/// so a round trip restores them.
fn custom_slug(name: &str) -> Option<Slug> {
    let rest = name.strip_prefix('_')?;
    let canonical = rest.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !canonical {
        return None;
    }
    Slug::new(rest.to_ascii_lowercase()).ok()
}

/// Tolerance on a vertex's weight sum: the scene check (`omuzaki-io-api`) refuses sums further than this from 1.
const WEIGHT_SUM_TOLERANCE: f32 = 1e-3;

/// Reports `JOINTS_n`/`WEIGHTS_n` attributes that were not read: sets past the supported number (a warning: influences
/// are lost), and a half of a pair or a set after a gap in the numbering (a note).
fn report_unread_sets(def: &gltf::Primitive, read: usize, at: &str, tally: &mut Tally) {
    let place = format!("{at}/attributes");
    let number = |name: &str| {
        name.strip_prefix("JOINTS_").or_else(|| name.strip_prefix("WEIGHTS_")).and_then(|n| n.parse::<usize>().ok())
    };
    let unread: Vec<(&String, usize)> =
        def.attributes.keys().filter_map(|name| number(name).map(|k| (name, k))).filter(|&(_, k)| k >= read).collect();
    if unread.iter().any(|&(_, k)| k >= MAX_SKIN_SETS) {
        let msg = format!("skinning sets beyond JOINTS_{} were ignored", MAX_SKIN_SETS - 1);
        tally.warn("gltf.joints_folded", msg, &place, None);
    }
    for (name, _) in unread.into_iter().filter(|&(_, k)| k < MAX_SKIN_SETS) {
        let msg = format!("{name} was ignored: it has no JOINTS/WEIGHTS partner, or follows a gap in the numbering");
        tally.info("gltf.attribute_dropped", msg, format!("{place}/{name}"));
    }
}

/// Scales a vertex's weights to sum to 1 when they are further off than the scene check tolerates (8-bit weights
/// often are). Sums within the tolerance are left bit for bit, so a file this crate wrote reads back identically.
fn renormalise(weights: &mut [[f32; 4]]) {
    for w in weights {
        let sum: f32 = w.iter().sum();
        if sum > 0.0 && (sum - 1.0).abs() > WEIGHT_SUM_TOLERANCE {
            *w = w.map(|x| x / sum);
        }
    }
}

/// Keeps the triangles of a strip or fan that have three distinct vertices. Exporters join strips with repeated
/// vertices (zero-area triangles that draw nothing).
fn without_degenerate(list: &[u32]) -> Vec<u32> {
    list.as_chunks::<3>().0.iter().filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2]).flatten().copied().collect()
}

/// `(v0, v1, v2), (v2, v1, v3), (v2, v3, v4)…`: every other triangle is flipped to keep the winding. Degenerate
/// triangles are dropped, after the flip is decided, so the winding of the rest is unchanged.
fn strip_to_list(indices: &[u32]) -> Vec<u32> {
    let list: Vec<u32> = indices
        .windows(3)
        .enumerate()
        .flat_map(|(i, w)| if i % 2 == 0 { [w[0], w[1], w[2]] } else { [w[1], w[0], w[2]] })
        .collect();
    without_degenerate(&list)
}

/// `(v0, v1, v2), (v0, v2, v3)…`, without degenerate triangles.
fn fan_to_list(indices: &[u32]) -> Vec<u32> {
    let Some((&hub, rest)) = indices.split_first() else { return Vec::new() };
    let list: Vec<u32> = rest.windows(2).flat_map(|w| [hub, w[0], w[1]]).collect();
    without_degenerate(&list)
}

/// One `JOINTS_n` / `WEIGHTS_n` pair: four joints and four weights per vertex.
type SkinSet = (Vec<[u16; 4]>, Vec<[f32; 4]>);

/// Folds several `JOINTS_n`/`WEIGHTS_n` sets into one: per vertex the four heaviest influences (ties keep file
/// order), renormalised to sum to 1. A vertex whose influences sum to 0 keeps weight 0.
fn fold_influences(sets: &[SkinSet]) -> SkinSet {
    let vertices = sets.first().map_or(0, |(j, _)| j.len());
    let mut joints = Vec::with_capacity(vertices);
    let mut weights = Vec::with_capacity(vertices);
    for v in 0..vertices {
        let influences = |(j, w): &SkinSet| {
            let joint = j.get(v).copied().unwrap_or_default();
            joint.into_iter().zip(w.get(v).copied().unwrap_or_default()).collect::<Vec<_>>()
        };
        let mut all: Vec<(u16, f32)> = sets.iter().flat_map(influences).collect();
        all.sort_by(|a, b| b.1.total_cmp(&a.1));
        all.truncate(4);
        let sum: f32 = all.iter().map(|&(_, w)| w).sum();
        let scale = if sum > 0.0 { 1.0 / sum } else { 0.0 };
        let mut j = [0u16; 4];
        let mut w = [0f32; 4];
        for (slot, &(joint, weight)) in all.iter().enumerate() {
            j[slot] = joint;
            w[slot] = weight * scale;
        }
        joints.push(j);
        weights.push(w);
    }
    (joints, weights)
}

/// Anything on a mesh the scene has no place for: extensions, extras other than `targetNames`, default morph weights.
fn report_mesh_extras(index: usize, def: &gltf::Mesh, hooks: &mut dyn ImportHooks, tally: &mut Tally) {
    let at = format!("/meshes/{index}");
    extensions::report_without_bag(&def.extensions, Carrier::Mesh, &at, hooks, tally);
    let other_extras =
        def.extras.as_ref().and_then(|e| e.as_object()).is_some_and(|o| o.keys().any(|k| k != "targetNames"));
    if other_extras {
        tally.info("gltf.extras_dropped", "mesh extras other than targetNames were dropped", format!("{at}/extras"));
    }
    if def.weights.as_ref().is_some_and(|w| w.iter().any(|&x| x != 0.0)) {
        let msg = "default morph target weights were dropped";
        tally.note(Severity::Warning, "gltf.morph_weights_dropped", msg, format!("{at}/weights"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_keep_their_winding_and_fans_pivot_on_the_first_vertex() {
        assert_eq!(strip_to_list(&[0, 1, 2, 3, 4]), [0, 1, 2, 2, 1, 3, 2, 3, 4]);
        assert_eq!(strip_to_list(&[0, 1]), [] as [u32; 0]);
        assert_eq!(fan_to_list(&[0, 1, 2, 3, 4]), [0, 1, 2, 0, 2, 3, 0, 3, 4]);
        assert_eq!(fan_to_list(&[]), [] as [u32; 0]);
    }

    #[test]
    fn degenerate_triangles_from_joined_strips_and_fans_are_dropped() {
        // The repeated vertex 2 joins two strips; the winding of the triangles after it is unchanged.
        assert_eq!(strip_to_list(&[0, 1, 2, 2, 3, 4]), [0, 1, 2, 3, 2, 4]);
        assert_eq!(strip_to_list(&[0, 1, 2, 3]), [0, 1, 2, 2, 1, 3]);
        assert_eq!(fan_to_list(&[0, 1, 2, 2, 3]), [0, 1, 2, 0, 2, 3]);
        assert_eq!(fan_to_list(&[0, 1, 2, 0, 3]), [0, 1, 2], "a repeat of the hub");
    }

    #[test]
    fn weights_are_scaled_only_when_the_scene_check_would_refuse_them() {
        let exact = [0.25, 0.25, 0.25, 0.25];
        let near = [0.5, 0.25, 0.1255, 0.1245]; // sums to 1.0 within a ten-thousandth
        let eight_bit = [0.5, 0.25, 0.125, 0.1204]; // 255-step quantisation lands a little under 1
        let unweighted = [0.0; 4];
        let mut w = [exact, near, eight_bit, unweighted];
        renormalise(&mut w);
        assert_eq!((w[0], w[1]), (exact, near), "within tolerance: untouched");
        assert!((w[2].iter().sum::<f32>() - 1.0).abs() < 1e-6, "{:?}", w[2]);
        assert_eq!(w[3], unweighted, "a zero sum has nothing to scale");
    }

    #[test]
    fn folding_keeps_the_four_heaviest_influences_and_renormalises() {
        let set0 = (vec![[1u16, 2, 3, 4]], vec![[0.4f32, 0.1, 0.1, 0.1]]);
        let set1 = (vec![[5u16, 6, 7, 8]], vec![[0.2f32, 0.05, 0.05, 0.0]]);
        let (j, w) = fold_influences(&[set0, set1]);
        assert_eq!(j[0], [1, 5, 2, 3], "0.4, 0.2, then the two 0.1s in file order");
        let sum: f32 = w[0].iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
        assert!((w[0][0] - 0.4 / 0.8).abs() < 1e-6);
        let (_, none) = fold_influences(&[(vec![[0u16; 4]], vec![[0.0f32; 4]]), (vec![[0; 4]], vec![[0.0; 4]])]);
        assert_eq!(none[0], [0.0; 4], "no influence stays no influence");
    }

    #[test]
    fn custom_attribute_names_convert_only_when_they_convert_back() {
        assert_eq!(custom_slug("_STRAND_UV").unwrap().as_str(), "strand_uv");
        assert!(custom_slug("_Strand").is_none(), "lower case wouldn't survive a round trip");
        assert!(custom_slug("_").is_none());
        assert!(custom_slug("_CON").is_none(), "reserved device name");
        assert!(custom_slug("POSITION").is_none());
    }

    #[test]
    fn attribute_names_are_classified() {
        assert!(matches!(classify("TEXCOORD_1"), Attribute::Texcoord(1)));
        assert!(matches!(classify("COLOR_0"), Attribute::Color(0)));
        assert!(matches!(classify("JOINTS_2"), Attribute::Joints));
        assert!(matches!(classify("_HAIR"), Attribute::Custom));
        assert!(matches!(classify("TEXCOORD_x"), Attribute::Other));
    }

    #[test]
    fn malformed_errors_name_the_attribute() {
        let bad: Result<(), _> =
            Err(IoError::Malformed { message: "2 elements".into(), at: Some("/accessors/4".into()) });
        let Err(IoError::Malformed { message, at }) = named("JOINTS_1", bad) else { panic!("still malformed") };
        assert_eq!((message.as_str(), at.as_deref()), ("JOINTS_1: 2 elements", Some("/accessors/4")));
        let limit: Result<(), _> = Err(IoError::Limit { what: "x", limit: 1, got: 2 });
        assert!(matches!(named("JOINTS_1", limit), Err(IoError::Limit { .. })), "other errors pass unchanged");
    }

    #[test]
    fn pools_refuse_what_they_cannot_cover() {
        let mut p = Pool::new("things", 10);
        assert!(p.take(6).is_ok());
        assert_eq!(p.remaining(), 4);
        assert!(matches!(p.take(5), Err(IoError::Limit { what: "things", limit: 10, got: 11 })));
        assert!(matches!(p.bound(), Bound::AtMost { max: 4, .. }));
    }
}
