//! The CPU-side model: everything the viewport needs from an exported character GLB, decoded once at load.
//!
//! The prototype's GLB export script writes ordinary glTF plus one vendor extension,
//! `OMUZAKI_toon_spike`, on each material, and a `omuzaki` object in each mesh node's extras. This module reads both.

use std::collections::HashMap;
use std::path::Path;

use glam::Mat4;
use serde::Deserialize;

/// What can go wrong while loading a character GLB.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// The glTF itself is unreadable.
    #[error("glTF: {0}")]
    Gltf(#[from] gltf::Error),
    /// The vendor extension or node extras don't parse.
    #[error("bad toon data on {what}: {source}")]
    Toon { what: String, source: serde_json::Error },
    /// A primitive lacks an attribute the shader cannot do without.
    #[error("mesh `{mesh}` has no {attribute}")]
    MissingAttribute { mesh: String, attribute: &'static str },
    /// An image uses a pixel format the viewport does not convert.
    #[error("image {index} has an unsupported pixel format {format:?}")]
    ImageFormat { index: usize, format: gltf::image::Format },
}

/// One vertex as the GPU reads it. Every mesh uses this one layout, so a single pipeline family draws them all:
/// absent attributes get neutral defaults (white colour, a single full-weight joint).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    pub joints: [u16; 4],
    pub weights: [f32; 4],
    /// Hair lock UV (`_HAIRUV`): across the lock and along it.
    pub hair_uv: [f32; 2],
    /// Hair lock info (`_HAIRINFO`): per-lock seed, inner-layer shade, polar angle.
    pub hair_info: [f32; 3],
}

/// The shader hooks `toon.js` callers pass as GLSL, identified by the exporter's ids.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hooks(pub u32);

impl Hooks {
    /// Skin: keep the albedo hue under coloured light and floor the shadow side.
    pub const SKIN_OUT: u32 = 1;
    /// Head: composite the painted face canvas, projected from the front.
    pub const FACE_PROJECT: u32 = 2;
    /// Hair: strand colours, lock lines, angel ring, brows through the bangs.
    pub const HAIR: u32 = 4;
    /// Tights: faint sheen on faces that look at the camera.
    pub const NYLON_SHEEN: u32 = 8;
    /// Boots: a camera-fixed highlight in the leather.
    pub const LEATHER_HIGHLIGHT: u32 = 16;
    /// Brass and gold: a camera-fixed pale highlight.
    pub const BRASS_HIGHLIGHT: u32 = 32;

    fn from_ids(ids: &[String]) -> Self {
        let bits = ids.iter().fold(0, |bits, id| {
            bits | match id.as_str() {
                "skin_out" => Self::SKIN_OUT,
                "face_project" => Self::FACE_PROJECT,
                "hair" => Self::HAIR,
                "nylon_sheen" => Self::NYLON_SHEEN,
                "leather_highlight" => Self::LEATHER_HIGHLIGHT,
                "brass_highlight" => Self::BRASS_HIGHLIGHT,
                _ => 0,
            }
        });
        Self(bits)
    }
}

/// `toon.js` shading parameters of one material (colours are linear).
#[derive(Clone, Copy, Debug)]
pub struct Toon {
    pub shade: [f32; 3],
    pub shift: f32,
    pub soft: f32,
    pub rim: [f32; 3],
    pub rim_power: f32,
    pub rim_strength: f32,
}

/// Where the painted face canvas sits on the head, in bind-space metres.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct FaceRect {
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
}

/// How a material is lit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `toon.js` ramp shading.
    Toon,
    /// Flat colour, no lighting (`MeshBasicMaterial`).
    Basic,
}

/// Wrap mode of a texture axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    Repeat,
    Mirror,
    Clamp,
}

/// An sRGB RGBA8 image and how it is sampled.
#[derive(Clone, Debug)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub wrap_s: Wrap,
    pub wrap_t: Wrap,
}

/// One material: glTF base colour plus the toon extension.
#[derive(Clone, Debug)]
pub struct Material {
    pub name: String,
    pub kind: Kind,
    pub base_color: [f32; 4],
    pub base_texture: Option<usize>,
    pub double_sided: bool,
    pub vertex_colors: bool,
    pub toon: Toon,
    pub hooks: Hooks,
    /// `uSkinFloor`: skin never sinks below this fraction of its albedo.
    pub skin_floor: f32,
    /// `uHl` / `uHl2` / `uHlG`: the highlight colour of leather and brass.
    pub highlight: [f32; 3],
    /// Hair palette (`uRoot`, `uMid`, `uTip`, `uLine`, `uRingCol`), linear.
    pub hair_palette: [[f32; 3]; 5],
    pub face_texture: Option<usize>,
    pub brow_texture: Option<usize>,
    pub brow_on: bool,
    pub face_rect: Option<FaceRect>,
    /// `(factor, units)` of `polygonOffset`, which the JS uses to keep stitches and seams over the cloth.
    pub polygon_offset: Option<(f32, f32)>,
}

/// An inverted-hull outline: colour (linear) and width in metres at 2 m.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Outline {
    pub color: [f32; 3],
    pub width: f32,
}

/// A drawable: one glTF primitive on one node.
#[derive(Clone, Debug)]
pub struct Mesh {
    pub name: String,
    pub node: usize,
    pub skin: Option<usize>,
    pub material: usize,
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub cast_shadow: bool,
    pub receive_shadow: bool,
    pub render_order: i32,
    pub outline: Option<Outline>,
}

/// A skin: joint nodes and their inverse bind matrices.
#[derive(Clone, Debug, PartialEq)]
pub struct Skin {
    pub joints: Vec<usize>,
    pub inverse_bind: Vec<Mat4>,
}

/// A node of the scene hierarchy.
#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<usize>,
    pub local: Mat4,
}

/// A decoded character.
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub nodes: Vec<Node>,
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    pub textures: Vec<Texture>,
    pub skins: Vec<Skin>,
}

// ---- the extension payload, as the exporter writes it ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToonExt {
    kind: String,
    #[serde(default)]
    vertex_colors: bool,
    polygon_offset: Option<(f32, f32)>,
    #[serde(default)]
    shade: Option<[f32; 3]>,
    #[serde(default)]
    shift: f32,
    #[serde(default)]
    soft: f32,
    #[serde(default)]
    rim: Option<[f32; 3]>,
    #[serde(default)]
    rim_pow: f32,
    #[serde(default)]
    rim_str: f32,
    #[serde(default)]
    hooks: Vec<String>,
    #[serde(default)]
    uniforms: HashMap<String, serde_json::Value>,
    #[serde(default)]
    textures: HashMap<String, usize>,
    face: Option<FaceRect>,
}

#[derive(Deserialize, Default)]
struct NodeExtras {
    omuzaki: Option<MeshExtras>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MeshExtras {
    cast_shadow: bool,
    receive_shadow: bool,
    render_order: i32,
    outline: Option<Outline>,
}

const EXTENSION: &str = "OMUZAKI_toon_spike";

impl Model {
    /// Loads a GLB (or glTF) file.
    ///
    /// # Errors
    /// Returns an error when the file is unreadable, malformed, or its toon data does not parse.
    pub fn load(path: &Path) -> Result<Self, ModelError> {
        let (document, buffers, images) = gltf::import(path)?;
        Self::from_gltf(&document, &buffers, &images)
    }

    /// Loads a GLB held in memory.
    ///
    /// # Errors
    /// As [`Model::load`].
    pub fn from_slice(bytes: &[u8]) -> Result<Self, ModelError> {
        let (document, buffers, images) = gltf::import_slice(bytes)?;
        Self::from_gltf(&document, &buffers, &images)
    }

    fn from_gltf(
        document: &gltf::Document,
        buffers: &[gltf::buffer::Data],
        images: &[gltf::image::Data],
    ) -> Result<Self, ModelError> {
        let mut model = Self { nodes: read_nodes(document), ..Self::default() };
        model.textures = document.textures().map(|t| read_texture(&t, images)).collect::<Result<_, _>>()?;
        model.materials = document.materials().map(|m| read_material(&m)).collect::<Result<_, _>>()?;
        model.skins = document
            .skins()
            .map(|skin| {
                let reader = skin.reader(|b| Some(&buffers[b.index()]));
                let joints: Vec<usize> = skin.joints().map(|j| j.index()).collect();
                let inverse_bind = reader.read_inverse_bind_matrices().map_or_else(
                    || vec![Mat4::IDENTITY; joints.len()],
                    |it| it.map(|m| Mat4::from_cols_array_2d(&m)).collect(),
                );
                Skin { joints, inverse_bind }
            })
            .collect();
        for node in document.nodes() {
            let Some(mesh) = node.mesh() else { continue };
            let extras = read_extras::<NodeExtras>(node.extras(), || format!("node `{}`", node.name().unwrap_or("?")))?
                .omuzaki
                .unwrap_or(MeshExtras { cast_shadow: true, receive_shadow: true, render_order: 0, outline: None });
            for primitive in mesh.primitives() {
                let name = node.name().unwrap_or("mesh").to_owned();
                let (vertices, indices) = read_primitive(&primitive, buffers, &name)?;
                model.meshes.push(Mesh {
                    name,
                    node: node.index(),
                    skin: node.skin().map(|s| s.index()),
                    material: primitive.material().index().unwrap_or(0),
                    vertices,
                    indices,
                    cast_shadow: extras.cast_shadow,
                    receive_shadow: extras.receive_shadow,
                    render_order: extras.render_order,
                    outline: extras.outline,
                });
            }
        }
        Ok(model)
    }

    /// World matrices of every node, from the rest pose stored in the file.
    #[must_use]
    pub fn world_matrices(&self) -> Vec<Mat4> {
        // Parents may be listed after their children, so resolve each chain on demand and memoise it.
        fn resolve(i: usize, nodes: &[Node], world: &mut [Option<Mat4>]) -> Mat4 {
            if let Some(m) = world[i] {
                return m;
            }
            let parent = nodes[i].parent.map_or(Mat4::IDENTITY, |p| resolve(p, nodes, world));
            let m = parent * nodes[i].local;
            world[i] = Some(m);
            m
        }
        let mut world = vec![None; self.nodes.len()];
        (0..self.nodes.len()).map(|i| resolve(i, &self.nodes, &mut world)).collect()
    }
}

fn read_nodes(document: &gltf::Document) -> Vec<Node> {
    let mut nodes: Vec<Node> = document
        .nodes()
        .map(|n| Node { parent: None, local: Mat4::from_cols_array_2d(&n.transform().matrix()) })
        .collect();
    for node in document.nodes() {
        for child in node.children() {
            nodes[child.index()].parent = Some(node.index());
        }
    }
    nodes
}

fn read_extras<T: for<'de> Deserialize<'de> + Default>(
    extras: &gltf::json::Extras,
    what: impl Fn() -> String,
) -> Result<T, ModelError> {
    match extras {
        Some(raw) => serde_json::from_str(raw.get()).map_err(|source| ModelError::Toon { what: what(), source }),
        None => Ok(T::default()),
    }
}

fn wrap(mode: gltf::texture::WrappingMode) -> Wrap {
    match mode {
        gltf::texture::WrappingMode::ClampToEdge => Wrap::Clamp,
        gltf::texture::WrappingMode::MirroredRepeat => Wrap::Mirror,
        gltf::texture::WrappingMode::Repeat => Wrap::Repeat,
    }
}

fn read_texture(texture: &gltf::Texture<'_>, images: &[gltf::image::Data]) -> Result<Texture, ModelError> {
    let index = texture.source().index();
    let image = &images[index];
    let rgba = match image.format {
        gltf::image::Format::R8G8B8A8 => image.pixels.clone(),
        gltf::image::Format::R8G8B8 => {
            image.pixels.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect()
        }
        format => return Err(ModelError::ImageFormat { index, format }),
    };
    let sampler = texture.sampler();
    Ok(Texture {
        width: image.width,
        height: image.height,
        rgba,
        wrap_s: wrap(sampler.wrap_s()),
        wrap_t: wrap(sampler.wrap_t()),
    })
}

fn read_material(material: &gltf::Material<'_>) -> Result<Material, ModelError> {
    let name = material.name().unwrap_or("material").to_owned();
    let pbr = material.pbr_metallic_roughness();
    let ext: Option<ToonExt> = material
        .extension_value(EXTENSION)
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .map_err(|source| ModelError::Toon { what: format!("material `{name}`"), source })?;
    let ext = ext.unwrap_or(ToonExt {
        kind: "basic".to_owned(),
        vertex_colors: false,
        polygon_offset: None,
        shade: None,
        shift: 0.0,
        soft: 0.0,
        rim: None,
        rim_pow: 3.0,
        rim_str: 0.0,
        hooks: Vec::new(),
        uniforms: HashMap::new(),
        textures: HashMap::new(),
        face: None,
    });
    // JSON numbers are f64; the shader wants f32, and the values are small colours and factors.
    #[allow(clippy::cast_possible_truncation)]
    let float = |key: &str, default: f32| {
        ext.uniforms.get(key).and_then(serde_json::Value::as_f64).map_or(default, |v| v as f32)
    };
    let color = |key: &str| -> [f32; 3] {
        let value = ext.uniforms.get(key).and_then(|v| serde_json::from_value::<[f32; 3]>(v.clone()).ok());
        value.unwrap_or([0.0; 3])
    };
    let highlight = ["uHl", "uHl2", "uHlG"]
        .iter()
        .find_map(|k| ext.uniforms.get(*k).and_then(|v| serde_json::from_value::<[f32; 3]>(v.clone()).ok()))
        .unwrap_or([0.0; 3]);
    Ok(Material {
        kind: if ext.kind == "toon" { Kind::Toon } else { Kind::Basic },
        base_color: pbr.base_color_factor(),
        base_texture: pbr.base_color_texture().map(|t| t.texture().index()),
        double_sided: material.double_sided(),
        vertex_colors: ext.vertex_colors,
        toon: Toon {
            shade: ext.shade.unwrap_or([1.0; 3]),
            shift: ext.shift,
            soft: ext.soft,
            rim: ext.rim.unwrap_or([0.0; 3]),
            rim_power: ext.rim_pow,
            rim_strength: ext.rim_str,
        },
        hooks: Hooks::from_ids(&ext.hooks),
        skin_floor: float("uSkinFloor", 0.0),
        highlight,
        hair_palette: [color("uRoot"), color("uMid"), color("uTip"), color("uLine"), color("uRingCol")],
        face_texture: ext.textures.get("uFace").copied(),
        brow_texture: ext.textures.get("uBrow").copied(),
        brow_on: float("uBrowOn", 0.0) > 0.5,
        face_rect: ext.face,
        polygon_offset: ext.polygon_offset,
        name,
    })
}

fn read_primitive(
    primitive: &gltf::Primitive<'_>,
    buffers: &[gltf::buffer::Data],
    mesh: &str,
) -> Result<(Vec<Vertex>, Vec<u32>), ModelError> {
    let get = |b: gltf::Buffer<'_>| Some(&buffers[b.index()][..]);
    let reader = primitive.reader(get);
    let positions: Vec<[f32; 3]> = reader
        .read_positions()
        .ok_or_else(|| ModelError::MissingAttribute { mesh: mesh.to_owned(), attribute: "positions" })?
        .collect();
    let normals: Vec<[f32; 3]> = reader
        .read_normals()
        .ok_or_else(|| ModelError::MissingAttribute { mesh: mesh.to_owned(), attribute: "normals" })?
        .collect();
    let uvs: Option<Vec<[f32; 2]>> = reader.read_tex_coords(0).map(|t| t.into_f32().collect());
    let colors: Option<Vec<[f32; 4]>> = reader.read_colors(0).map(|c| c.into_rgba_f32().collect());
    let joints: Option<Vec<[u16; 4]>> = reader.read_joints(0).map(|j| j.into_u16().collect());
    let weights: Option<Vec<[f32; 4]>> = reader.read_weights(0).map(|w| w.into_f32().collect());
    // Custom attributes (the exporter prefixes them with `_`): glTF has no semantic for hair locks.
    let custom = |name: &str| primitive.get(&gltf::Semantic::Extras(name.to_owned()));
    let hair_uv = custom("HAIRUV").and_then(|a| read_f32s::<2>(&a, buffers));
    let hair_info = custom("HAIRINFO").and_then(|a| read_f32s::<3>(&a, buffers));

    let vertices = positions
        .iter()
        .enumerate()
        .map(|(i, &position)| Vertex {
            position,
            normal: normals[i],
            uv: uvs.as_ref().map_or([0.0; 2], |u| u[i]),
            color: colors.as_ref().map_or([1.0; 4], |c| c[i]),
            joints: joints.as_ref().map_or([0; 4], |j| j[i]),
            weights: weights.as_ref().map_or([1.0, 0.0, 0.0, 0.0], |w| w[i]),
            hair_uv: hair_uv.as_ref().map_or([0.0; 2], |h| h[i]),
            hair_info: hair_info.as_ref().map_or([0.0; 3], |h| h[i]),
        })
        .collect();
    let indices = reader
        .read_indices()
        .map_or_else(|| (0..u32::try_from(positions.len()).unwrap_or(u32::MAX)).collect(), |i| i.into_u32().collect());
    Ok((vertices, indices))
}

fn read_f32s<const N: usize>(accessor: &gltf::Accessor<'_>, buffers: &[gltf::buffer::Data]) -> Option<Vec<[f32; N]>>
where
    [f32; N]: gltf::accessor::Item,
{
    gltf::accessor::util::Iter::<[f32; N]>::new(accessor.clone(), |b| Some(&buffers[b.index()][..]))
        .map(Iterator::collect)
}

/// Skinning matrices for one pose, laid out for a single GPU storage buffer.
///
/// Every mesh draws with `joint_base`: skinned meshes index their skin's joint matrices, unskinned meshes a single
/// matrix (their node's world matrix), so one vertex shader serves both. Identical skins (the exporter writes one per
/// mesh) share one run of matrices.
#[derive(Clone, Debug, PartialEq)]
pub struct JointTable {
    /// `world(joint) * inverse_bind` for each skin, and the world matrix of each unskinned mesh.
    pub matrices: Vec<Mat4>,
    /// First matrix of each mesh, in `Model::meshes` order.
    pub mesh_base: Vec<u32>,
}

impl JointTable {
    /// Builds the table for the given node world matrices.
    #[must_use]
    pub fn build(model: &Model, world: &[Mat4]) -> Self {
        let mut matrices = vec![Mat4::IDENTITY];
        let mut skin_base: Vec<Option<u32>> = vec![None; model.skins.len()];
        let mesh_base = model
            .meshes
            .iter()
            .map(|mesh| {
                if let Some(s) = mesh.skin {
                    Self::skin_run(model, world, s, &mut skin_base, &mut matrices)
                } else {
                    matrices.push(world[mesh.node]);
                    u32::try_from(matrices.len() - 1).unwrap_or(u32::MAX)
                }
            })
            .collect();
        Self { matrices, mesh_base }
    }

    /// The first matrix of skin `s`, appending its joint matrices unless an identical skin already has a run.
    fn skin_run(model: &Model, world: &[Mat4], s: usize, bases: &mut [Option<u32>], matrices: &mut Vec<Mat4>) -> u32 {
        if let Some(base) = bases[s] {
            return base;
        }
        let twin = (0..s).find(|&t| model.skins[t] == model.skins[s]).and_then(|t| bases[t]);
        let base = twin.unwrap_or_else(|| {
            let base = u32::try_from(matrices.len()).unwrap_or(u32::MAX);
            let skin = &model.skins[s];
            matrices.extend(skin.joints.iter().zip(&skin.inverse_bind).map(|(&j, &ibm)| world[j] * ibm));
            base
        });
        bases[s] = Some(base);
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(parent: Option<usize>, x: f32) -> Node {
        Node { parent, local: Mat4::from_translation(glam::Vec3::new(x, 0.0, 0.0)) }
    }

    #[test]
    fn world_matrices_resolve_parents_listed_after_children() {
        let model =
            Model { nodes: vec![node(Some(1), 1.0), node(Some(2), 10.0), node(None, 100.0)], ..Model::default() };
        let x: Vec<f32> = model.world_matrices().iter().map(|m| m.w_axis.x).collect();
        assert_eq!(x, vec![111.0, 110.0, 100.0]);
    }

    fn mesh(node: usize, skin: Option<usize>) -> Mesh {
        Mesh {
            name: String::new(),
            node,
            skin,
            material: 0,
            vertices: Vec::new(),
            indices: Vec::new(),
            cast_shadow: true,
            receive_shadow: true,
            render_order: 0,
            outline: None,
        }
    }

    #[test]
    fn identical_skins_share_one_run_of_matrices() {
        let skin = Skin { joints: vec![0, 1], inverse_bind: vec![Mat4::IDENTITY; 2] };
        let model = Model {
            nodes: vec![node(None, 1.0), node(None, 2.0), node(None, 3.0)],
            skins: vec![skin.clone(), skin],
            meshes: vec![mesh(2, Some(0)), mesh(2, Some(1)), mesh(2, None)],
            ..Model::default()
        };
        let table = JointTable::build(&model, &model.world_matrices());
        assert_eq!(table.mesh_base, vec![1, 1, 3]);
        assert_eq!(table.matrices.len(), 4);
        assert!((table.matrices[3].w_axis.x - 3.0).abs() < 1e-6);
    }

    #[test]
    fn hooks_parse_the_exporter_ids() {
        let hooks = Hooks::from_ids(&["skin_out".to_owned(), "face_project".to_owned(), "unknown".to_owned()]);
        assert_eq!(hooks.0, Hooks::SKIN_OUT | Hooks::FACE_PROJECT);
    }
}
