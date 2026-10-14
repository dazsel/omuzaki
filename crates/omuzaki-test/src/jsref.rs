//! Reader for the JS prototype's reference dumps (written by the prototype's dump script): the oracle the
//! Rust character port is compared against.
//!
//! The dumps are the sample character's, so they are private test material and live in a local assets folder, never in git. [`JsRef::open`]
//! finds them and returns `None` (with a printed reason) when the drive is absent, so a test that needs them skips
//! instead of failing. The file formats are described in `geometry/README.md` and `state/README.md` next to the dumps.
//!
//! What it loads:
//! - [`GeometryDump`]: every mesh as a [`MeshData`] (positions, normals, UVs, vertex colours, skin data, the shader
//!   attributes as named extras), its material and outline records, and the skeleton.
//! - [`HairDump`]: the hair clumps, their simulated particles at rest, settled and after a head turn, and the bones.
//! - [`FaceStates`]: the face parameters, gaze and camera of every golden case.
//!
//! Every binary section is checked against the FNV-1a hash the dump wrote beside it, so a truncated or edited file is an
//! error instead of quietly wrong numbers.
//!
//! **UV convention:** the dumps hold three.js UVs (origin bottom-left, for canvas textures that three.js flips on
//! upload). [`MeshData`] uses glTF's top-left origin, so `v` is loaded as `1 - v`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use omuzaki_core::glam::{Mat4, Quat, Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::mesh::{ExtraAttribute, MeshData};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use omuzaki_core::assets::{ASSETS_ENV, assets_root_with};

use crate::dirs::GOLDENS_ENV;

/// The geometry sets the dump holds: the two golden variants, and the head part alone with the analytic head.
pub const GEOMETRY_SETS: [&str; 4] = ["game", "close-up", "game-trace0", "close-up-trace0"];

/// The golden variants that have hair and face state.
pub const VARIANTS: [&str; 2] = ["game", "close-up"];

/// Why a dump could not be read.
#[derive(Debug, thiserror::Error)]
pub enum JsRefError {
    /// A file could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// A JSON file does not have the expected shape.
    #[error("cannot parse {}: {source}", path.display())]
    Json {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: serde_json::Error,
    },
    /// The dump contradicts itself: a section outside its file, a wrong item size, a hash that does not match.
    #[error("{}: {reason}", path.display())]
    Corrupt {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        reason: String,
    },
}

/// The dump directory. Cheap to hold; each `load` call reads what it needs.
#[derive(Debug, Clone)]
pub struct JsRef {
    root: PathBuf,
}

impl JsRef {
    /// The dump named by `OMUZAKI_GOLDENS` (`<OMUZAKI_GOLDENS>/js`, the same store root as the golden images), else
    /// `$OMUZAKI_ASSETS/goldens/js`. `None`, after printing why, when it is not there: the caller skips its test.
    pub fn open() -> Option<Self> {
        Self::open_at(&locate(|key| std::env::var_os(key)))
    }

    /// The dump at `root`, or `None` (after printing why) when `root/manifest.json` is not there.
    pub fn open_at(root: &Path) -> Option<Self> {
        if root.join("manifest.json").is_file() {
            Some(Self { root: root.to_path_buf() })
        } else {
            eprintln!("skipped: no JS reference dump at {} (set OMUZAKI_ASSETS to the assets folder)", root.display());
            None
        }
    }

    /// The dump's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Loads the meshes of one of [`GEOMETRY_SETS`].
    ///
    /// # Errors
    /// [`JsRefError`] when a file is missing, malformed or fails its hash check.
    pub fn geometry(&self, set: &str) -> Result<GeometryDump, JsRefError> {
        let dir = self.root.join("geometry").join(set);
        let index_path = dir.join("index.json");
        let index: GeometryIndex = read_json(&index_path)?;
        let corrupt = |reason: String| JsRefError::Corrupt { path: index_path.clone(), reason };
        let bone_inverses =
            index.skeleton.bone_inverses.as_chunks::<16>().0.iter().map(Mat4::from_cols_array).collect::<Vec<_>>();
        if bone_inverses.len() != index.skeleton.bones.len() {
            return Err(corrupt(format!(
                "{} bones but {} inverse matrices",
                index.skeleton.bones.len(),
                bone_inverses.len()
            )));
        }
        let meshes = index.meshes.iter().map(|m| load_mesh(&dir, m)).collect::<Result<Vec<_>, _>>()?;
        Ok(GeometryDump {
            variant: index.variant,
            trace_mix: index.trace_mix,
            three: index.three,
            group_matrix_world: Mat4::from_cols_array(&index.group.matrix_world),
            bones: index.skeleton.bones,
            bone_inverses,
            bind_matrix: Mat4::from_cols_array(&index.skeleton.bind_matrix),
            group_geometry_hash: index.group_geometry_hash,
            meshes,
        })
    }

    /// Loads the hair clumps and their simulated state for one of [`VARIANTS`].
    ///
    /// # Errors
    /// [`JsRefError`] when a file is missing, malformed or fails its hash check.
    pub fn hair(&self, variant: &str) -> Result<HairDump, JsRefError> {
        let dir = self.root.join("state").join(variant);
        let json: HairJson = read_json(&dir.join("hair.json"))?;
        let blob = Blob::read(&dir.join("hair.bin"))?;
        let clumps = json.clumps.iter().map(|c| load_clump(&blob, c)).collect::<Result<Vec<_>, _>>()?;
        Ok(HairDump {
            variant: json.variant,
            sim: json.sim,
            pose_t: json.pose.t,
            tube_ring: json.tube_ring,
            curtain_columns: json.curtain_columns,
            static_tail_vertices: json.static_tail_vertices,
            yaw_deg: json.motion.yaw_deg,
            motion: json.motion.steps,
            bones_settled: json.bones.settled,
            bones_yawed: json.bones.yawed,
            clumps,
        })
    }

    /// Loads the face state of every golden case of one of [`VARIANTS`].
    ///
    /// # Errors
    /// [`JsRefError`] when the file is missing or malformed.
    pub fn face_states(&self, variant: &str) -> Result<FaceStates, JsRefError> {
        read_json(&self.root.join("state").join(variant).join("face-state.json"))
    }
}

/// The dump root for a given environment: `<OMUZAKI_GOLDENS>/js` when that is set to something, else
/// `<assets folder>/goldens/js`.
fn locate(env: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    if let Some(store) = env(GOLDENS_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(store).join("js");
    }
    let start = std::env::current_dir().unwrap_or_default();
    assets_root_with(env(ASSETS_ENV), &start).join("goldens/js")
}

// ---------------------------------------------------------------------------------------------------------------------
// Geometry

/// One golden variant's meshes, as built by the prototype and dumped in bind space (before skinning).
#[derive(Debug, Clone)]
pub struct GeometryDump {
    /// `game` or `close-up`.
    pub variant: String,
    /// The head radius map mix (`Some(0.0)` in the `-trace0` sets, which hold the analytic head).
    pub trace_mix: Option<f64>,
    /// The three.js revision that built it.
    pub three: String,
    /// The character group's world matrix. Meshes that are not skinned (the dynamic hair) are in this group's space.
    pub group_matrix_world: Mat4,
    /// Bone names; a mesh's joint indices index this list.
    pub bones: Vec<String>,
    /// One inverse bind matrix per bone.
    pub bone_inverses: Vec<Mat4>,
    /// The bind matrix shared by every skinned mesh.
    pub bind_matrix: Mat4,
    /// The prototype's FNV hash over every drawn mesh's positions, in scene-graph order.
    pub group_geometry_hash: String,
    /// Every mesh, in scene-graph order.
    pub meshes: Vec<JsMesh>,
}

impl GeometryDump {
    /// The mesh called `name`, if the dump has it.
    pub fn mesh(&self, name: &str) -> Option<&JsMesh> {
        self.meshes.iter().find(|m| m.name == name)
    }
}

/// One mesh of the prototype with its material.
#[derive(Debug, Clone)]
pub struct JsMesh {
    /// The mesh name (`head`, `hair-static`, `robe-cloth`, ...).
    pub name: String,
    /// The part module that built it (`body`, `head`, `face`, `hair`, `uniform`, `robe`, `boots`).
    pub part: String,
    /// The geometry. Vertex colours are the prototype's `color` attribute (alpha 1); the shader attributes `aux`,
    /// `pid`, `hairInfo` and `hairUV` are extras named `robe-aux`, `robe-pid`, `hair-info` and `hair-uv`.
    pub mesh: MeshData,
    /// Minimum and maximum of the positions, as the prototype computed them.
    pub bbox: (Vec3, Vec3),
    /// The prototype's FNV hash of the positions.
    pub hash: String,
    /// Draw order.
    pub render_order: i32,
    /// Whether it casts shadows.
    pub cast_shadow: bool,
    /// Whether it receives shadows.
    pub receive_shadow: bool,
    /// The material.
    pub material: MaterialRecord,
    /// The inverted-hull outline drawn around it, if any.
    pub outline: Option<OutlineRecord>,
}

/// A prototype material: three.js settings plus the toon shader's options.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterialRecord {
    /// The material name.
    pub name: String,
    /// `MeshToonMaterial` or `MeshBasicMaterial`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The shader variant key from toon.js, if any.
    pub program_key: Option<String>,
    /// Base colour, sRGB hex.
    pub color: String,
    /// Emissive colour, sRGB hex; `None` for basic materials.
    pub emissive: Option<String>,
    /// `front`, `back` or `double`.
    pub side: String,
    /// Whether the vertex colours multiply the base colour.
    pub vertex_colors: bool,
    /// Whether it blends.
    pub transparent: bool,
    /// Alpha-test threshold (0 = off).
    pub alpha_test: f64,
    /// Whether it has a base-colour texture.
    pub has_map: bool,
    /// The `toon()` options; `None` for basic materials.
    pub toon: Option<ToonRecord>,
    /// Every other shader uniform by name: hex strings for colours, numbers, arrays, or `{ texture, width, height }`.
    pub other_uniforms: BTreeMap<String, serde_json::Value>,
}

/// The toon shader's options.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToonRecord {
    /// Shade colour, sRGB hex.
    pub shade: String,
    /// Where the terminator sits.
    pub shift: f64,
    /// How soft it is.
    pub soft: f64,
    /// Rim colour, sRGB hex.
    pub rim: String,
    /// Rim strength.
    pub rim_strength: f64,
    /// Rim falloff exponent.
    pub rim_power: f64,
}

/// The inverted-hull line around a mesh.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineRecord {
    /// Line colour, sRGB hex.
    pub color: String,
    /// Line width in metres at 2 m from the camera.
    pub width: f64,
    /// How much the light dims it.
    pub light_scale: f64,
    /// Draw order.
    pub render_order: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeometryIndex {
    variant: String,
    trace_mix: Option<f64>,
    three: String,
    group: GroupJson,
    skeleton: SkeletonJson,
    group_geometry_hash: String,
    meshes: Vec<MeshJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupJson {
    matrix_world: [f32; 16],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkeletonJson {
    bones: Vec<String>,
    bone_inverses: Vec<f32>,
    bind_matrix: [f32; 16],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MeshJson {
    name: String,
    part: String,
    file: String,
    vertex_count: usize,
    bbox: BboxJson,
    hash: String,
    render_order: i32,
    cast_shadow: bool,
    receive_shadow: bool,
    material: MaterialRecord,
    outline: Option<OutlineRecord>,
    sections: BTreeMap<String, Section>,
}

#[derive(Deserialize)]
struct BboxJson {
    min: Vec3,
    max: Vec3,
}

/// The shader attributes the prototype's meshes carry, and the names they get as [`MeshData`] extras (`aux` is a
/// reserved Windows device name, so it cannot be a slug as it is).
const EXTRA_NAMES: [(&str, &str); 4] =
    [("aux", "robe-aux"), ("pid", "robe-pid"), ("hairInfo", "hair-info"), ("hairUV", "hair-uv")];

fn load_mesh(dir: &Path, json: &MeshJson) -> Result<JsMesh, JsRefError> {
    let blob = Blob::read(&dir.join(&json.file))?;
    let corrupt = |reason: String| blob.corrupt(&format!("mesh {}: {reason}", json.name));
    let section = |name: &str| json.sections.get(name);
    let required = |name: &str| section(name).ok_or_else(|| corrupt(format!("no `{name}` section")));

    let position = required("position")?;
    if position.fnv != json.hash || position.count != json.vertex_count {
        return Err(corrupt(format!(
            "position section ({} items, fnv {}) disagrees with the index",
            position.count, position.fnv
        )));
    }
    let mut mesh = MeshData {
        positions: vec3s(&blob.floats(position, 3)?),
        normals: vec3s(&blob.floats(required("normal")?, 3)?),
        indices: blob.uints(required("index")?, 1)?,
        ..MeshData::default()
    };
    if let Some(uv) = section("uv") {
        mesh.uvs = blob.floats(uv, 2)?.as_chunks::<2>().0.iter().map(|&[u, v]| Vec2::new(u, 1.0 - v)).collect();
    }
    if let Some(color) = section("color") {
        mesh.colours = blob.floats(color, 3)?.as_chunks::<3>().0.iter().map(|&[r, g, b]| [r, g, b, 1.0]).collect();
    }
    if let (Some(joints), Some(weights)) = (section("skinIndex"), section("skinWeight")) {
        // Joint indices are u16 in `MeshData`; the dump widened them to u32.
        let joint = |i: u32| u16::try_from(i).map_err(|_| corrupt(format!("joint index {i} is past u16")));
        mesh.joints = blob
            .uints(joints, 4)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[a, b, c, d]| Ok([joint(a)?, joint(b)?, joint(c)?, joint(d)?]))
            .collect::<Result<_, _>>()?;
        mesh.weights = blob.floats(weights, 4)?.as_chunks::<4>().0.to_vec();
    }
    for (js_name, name) in EXTRA_NAMES {
        if let Some(extra) = section(js_name) {
            let slug = Slug::new(name).map_err(|e| corrupt(e.to_string()))?;
            let components = u8::try_from(extra.item_size).map_err(|_| corrupt(format!("`{js_name}` is too wide")))?;
            mesh.extra.push(ExtraAttribute { name: slug, components, data: blob.floats(extra, extra.item_size)? });
        }
    }
    let known = ["position", "normal", "uv", "color", "index", "skinIndex", "skinWeight"];
    if let Some(unknown) =
        json.sections.keys().find(|k| !known.contains(&k.as_str()) && !EXTRA_NAMES.iter().any(|(n, _)| n == k))
    {
        return Err(corrupt(format!("unknown section `{unknown}`: teach jsref about it")));
    }
    mesh.validate().map_err(|e| corrupt(e.0))?;
    Ok(JsMesh {
        name: json.name.clone(),
        part: json.part.clone(),
        mesh,
        bbox: (json.bbox.min, json.bbox.max),
        hash: json.hash.clone(),
        render_order: json.render_order,
        cast_shadow: json.cast_shadow,
        receive_shadow: json.receive_shadow,
        material: json.material.clone(),
        outline: json.outline.clone(),
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// Binary sections

/// A binary section of a dump: where it is in the `.bin` and what it must hash to.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Section {
    #[serde(rename = "type")]
    kind: String,
    item_size: usize,
    count: usize,
    offset: usize,
    fnv: String,
}

/// A `.bin` file read into memory.
struct Blob {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl Blob {
    fn read(path: &Path) -> Result<Self, JsRefError> {
        let bytes = fs::read(path).map_err(|source| JsRefError::Io { path: path.to_path_buf(), source })?;
        Ok(Self { path: path.to_path_buf(), bytes })
    }

    fn corrupt(&self, reason: &str) -> JsRefError {
        JsRefError::Corrupt { path: self.path.clone(), reason: reason.to_owned() }
    }

    /// The little-endian 32-bit words of `section`, after checking its type, item size, bounds and hash.
    fn words(&self, section: &Section, kind: &str, item_size: usize) -> Result<Vec<u32>, JsRefError> {
        if section.kind != kind || section.item_size != item_size {
            return Err(self.corrupt(&format!(
                "section at {} is {}x{}, expected {kind}x{item_size}",
                section.offset, section.kind, section.item_size
            )));
        }
        let range = section
            .count
            .checked_mul(item_size)
            .and_then(|n| n.checked_mul(4))
            .and_then(|len| section.offset.checked_add(len).map(|end| section.offset..end))
            .filter(|range| section.offset.is_multiple_of(4) && range.end <= self.bytes.len())
            .ok_or_else(|| self.corrupt(&format!("section at {} lies outside the file", section.offset)))?;
        let words: Vec<u32> = self.bytes[range].as_chunks::<4>().0.iter().map(|&w| u32::from_le_bytes(w)).collect();
        let hash = format!("{:08x}", fnv1a_words(&words));
        if hash != section.fnv {
            return Err(self
                .corrupt(&format!("section at {} hashes to {hash}, the index says {}", section.offset, section.fnv)));
        }
        Ok(words)
    }

    fn floats(&self, section: &Section, item_size: usize) -> Result<Vec<f32>, JsRefError> {
        Ok(self.words(section, "f32", item_size)?.into_iter().map(f32::from_bits).collect())
    }

    fn uints(&self, section: &Section, item_size: usize) -> Result<Vec<u32>, JsRefError> {
        self.words(section, "u32", item_size)
    }

    fn vec3s(&self, section: &Section) -> Result<Vec<Vec3>, JsRefError> {
        Ok(vec3s(&self.floats(section, 3)?))
    }
}

/// Three floats at a time as vectors (a trailing partial triple cannot occur: [`Blob::words`] checks the section length).
fn vec3s(floats: &[f32]) -> Vec<Vec3> {
    floats.as_chunks::<3>().0.iter().map(|&c| Vec3::from_array(c)).collect()
}

/// FNV-1a over 32-bit words (offset basis 0x811c9dc5, prime 0x01000193): the dump's section hash.
fn fnv1a_words(words: &[u32]) -> u32 {
    words.iter().fold(0x811c_9dc5_u32, |h, &w| (h ^ w).wrapping_mul(0x0100_0193))
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, JsRefError> {
    let text = fs::read(path).map_err(|source| JsRefError::Io { path: path.to_path_buf(), source })?;
    serde_json::from_slice(&text).map_err(|source| JsRefError::Json { path: path.to_path_buf(), source })
}

// ---------------------------------------------------------------------------------------------------------------------
// Hair

/// The hair simulation of one variant: clump shapes, particle positions at rest, after settling and after a head turn.
#[derive(Debug, Clone)]
pub struct HairDump {
    /// `game` or `close-up`.
    pub variant: String,
    /// The simulation setup: settle for `settle_steps` steps of `dt` from the styled shape.
    pub sim: HairSim,
    /// The animation time of the pose the hair was settled in.
    pub pose_t: f64,
    /// Vertices per tube ring (hair.js).
    pub tube_ring: u32,
    /// Columns per curtain strip (hair.js).
    pub curtain_columns: u32,
    /// How many vertices at the end of mesh `hair-static` are the braid, after the clump tubes.
    pub static_tail_vertices: u32,
    /// How far the head turned over `motion`, in degrees.
    pub yaw_deg: f64,
    /// The head turn, one entry per simulation step.
    pub motion: Vec<MotionStep>,
    /// The 17 normalised bones' local transforms at the settled pose.
    pub bones_settled: BTreeMap<String, BoneState>,
    /// The same after the head turn.
    pub bones_yawed: BTreeMap<String, BoneState>,
    /// The clumps, in the hair part's order.
    pub clumps: Vec<HairClump>,
}

/// How the hair was settled.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HairSim {
    /// Simulation time at the start.
    pub t: f64,
    /// Seconds per step.
    pub dt: f64,
    /// Steps taken from the styled shape to the settled state.
    pub settle_steps: u32,
}

/// One step of the head turn: the yaw asked for and the neck and head rotations the animator produced.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionStep {
    /// The look yaw in radians.
    pub look_yaw: f64,
    /// The neck bone's local rotation.
    pub neck: Quat,
    /// The head bone's local rotation.
    pub head: Quat,
}

/// A bone's local transform.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct BoneState {
    /// Local position in metres.
    pub position: Vec3,
    /// Local rotation.
    pub quaternion: Quat,
}

/// One hair clump.
#[derive(Debug, Clone)]
pub struct HairClump {
    /// Position in the hair part's list.
    pub index: usize,
    /// Whether it is simulated.
    pub dynamic: bool,
    /// `body` when it follows the body's motion, else none.
    pub follow: Option<String>,
    /// Whether it is a long-hair guide (a strip of the curtain).
    pub shell: bool,
    /// For shell clumps, the position across the back, 0 to 1.
    pub u: Option<f32>,
    /// Shading brightness.
    pub shade: f32,
    /// Where its tube vertices are.
    pub tube: TubeRange,
    /// The rest shape, sampled at `i / (len - 1)` (bind space).
    pub rest: Vec<Vec3>,
    /// Width along the clump.
    pub width: Vec<f32>,
    /// Thickness along the clump.
    pub thick: Vec<f32>,
    /// Stiffness along the clump; simulated clumps only.
    pub stiff: Option<Vec<f32>>,
    /// The simulation state; simulated clumps only.
    pub particles: Option<Particles>,
}

/// The vertices of one clump's tube within a mesh.
#[derive(Debug, Clone, Deserialize)]
pub struct TubeRange {
    /// The mesh holding them.
    pub mesh: String,
    /// The first vertex.
    pub first: usize,
    /// How many vertices.
    pub count: usize,
    /// For curtain clumps, the strip number.
    pub strip: Option<usize>,
}

/// A simulated clump's particles.
#[derive(Debug, Clone)]
pub struct Particles {
    /// The rest-point index each particle sits at.
    pub rest_index: Vec<u32>,
    /// Particle rest positions (bind space).
    pub rest: Vec<Vec3>,
    /// Rest length of each segment between particles (one fewer than particles).
    pub segment_len: Vec<f32>,
    /// Spring stiffness per particle.
    pub stiffness: Vec<f32>,
    /// Positions in **world space** after settling.
    pub settled: Vec<Vec3>,
    /// Positions in **world space** after the head turn.
    pub yawed: Vec<Vec3>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HairJson {
    variant: String,
    sim: HairSim,
    pose: PoseJson,
    tube_ring: u32,
    curtain_columns: u32,
    static_tail_vertices: u32,
    motion: MotionJson,
    bones: BonesJson,
    clumps: Vec<ClumpJson>,
}

#[derive(Deserialize)]
struct PoseJson {
    t: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MotionJson {
    yaw_deg: f64,
    steps: Vec<MotionStep>,
}

#[derive(Deserialize)]
struct BonesJson {
    settled: BTreeMap<String, BoneState>,
    yawed: BTreeMap<String, BoneState>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClumpJson {
    index: usize,
    #[serde(rename = "dyn")]
    dynamic: bool,
    follow: Option<String>,
    shell: bool,
    u: Option<f32>,
    shade: f32,
    tube: TubeRange,
    rest: Section,
    width: Section,
    thick: Section,
    stiff: Option<Section>,
    pidx: Option<Section>,
    p_rest: Option<Section>,
    len: Option<Section>,
    k: Option<Section>,
    settled: Option<Section>,
    yawed: Option<Section>,
}

fn load_clump(blob: &Blob, c: &ClumpJson) -> Result<HairClump, JsRefError> {
    let particles = match (&c.pidx, &c.p_rest, &c.len, &c.k, &c.settled, &c.yawed) {
        (Some(pidx), Some(p_rest), Some(len), Some(k), Some(settled), Some(yawed)) => Some(Particles {
            rest_index: blob.uints(pidx, 1)?,
            rest: blob.vec3s(p_rest)?,
            segment_len: blob.floats(len, 1)?,
            stiffness: blob.floats(k, 1)?,
            settled: blob.vec3s(settled)?,
            yawed: blob.vec3s(yawed)?,
        }),
        (None, None, None, None, None, None) => None,
        _ => return Err(blob.corrupt(&format!("clump {} has some particle sections but not all", c.index))),
    };
    Ok(HairClump {
        index: c.index,
        dynamic: c.dynamic,
        follow: c.follow.clone(),
        shell: c.shell,
        u: c.u,
        shade: c.shade,
        tube: c.tube.clone(),
        rest: blob.vec3s(&c.rest)?,
        width: blob.floats(&c.width, 1)?,
        thick: blob.floats(&c.thick, 1)?,
        stiff: c.stiff.as_ref().map(|s| blob.floats(s, 1)).transpose()?,
        particles,
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// Face state

/// The face part's state after painting each golden case of a variant.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceStates {
    /// `game` or `close-up`.
    pub variant: String,
    /// The constants of face.js's `update()` that turn a camera position into a gaze.
    pub face_view: FaceViewConstants,
    /// One entry per golden case.
    pub cases: Vec<FaceCase>,
}

impl FaceStates {
    /// The case with this id (`game/blink__viewer-studio__face__1024__nobloom`).
    pub fn case(&self, id: &str) -> Option<&FaceCase> {
        self.cases.iter().find(|c| c.id == id)
    }
}

/// The constants behind [`FaceView`].
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceViewConstants {
    /// How far the eyes sit below the head joint.
    pub eye_lift: f64,
    /// The camera counts as in front when `z / distance` is above this.
    pub front_gate: f64,
    /// Yaw (radians) that turns the eyes fully.
    pub look_yaw_scale: f64,
    /// Pitch (radians) that turns the eyes fully.
    pub look_pitch_scale: f64,
    /// Reflection offset per radian.
    pub refl_gain: f64,
    /// Largest horizontal reflection offset.
    pub refl_max_x: f64,
    /// Largest vertical reflection offset.
    pub refl_max_y: f64,
}

/// A pair of numbers: a gaze or an offset.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Xy {
    /// Horizontal.
    pub x: f64,
    /// Vertical.
    pub y: f64,
}

/// The face state for one golden case.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceCase {
    /// The case id, as in the golden manifest.
    pub id: String,
    /// The expression axis of the case.
    pub expression: String,
    /// The camera axis of the case.
    pub camera: String,
    /// What the face part was given.
    pub input: FaceInput,
    /// Every face parameter it ended up with.
    pub p: BTreeMap<String, f64>,
    /// How the camera turned into a gaze.
    pub view: FaceView,
    /// The camera-following gaze after the dt = 0 snap.
    pub look: Xy,
    /// The driven gaze: `look_add` clamped to -1..1.
    pub look_d: Xy,
    /// The eye reflection offset.
    pub refl_v: Xy,
    /// The twinkle clock.
    pub tw_t: f64,
    /// The eye dart offset (zero: a driven gaze owns small eye movements).
    pub dart: Xy,
    /// The head joint's world matrix.
    pub head_matrix_world: Mat4,
    /// The camera's world matrix.
    pub camera_matrix_world: Mat4,
}

/// The inputs of a face case.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceInput {
    /// Absolute face channel values given to the face part.
    pub channels: BTreeMap<String, f64>,
    /// Extra gaze on top of the camera-following gaze.
    pub look_add: Xy,
    /// The animation time.
    pub t: f64,
    /// The camera.
    pub camera_spec: CameraSpec,
}

/// A perspective camera.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct CameraSpec {
    /// Vertical field of view in degrees.
    pub fov: f64,
    /// Near plane in metres.
    pub near: f64,
    /// Far plane in metres.
    pub far: f64,
    /// Position in metres.
    pub position: Vec3,
    /// The point it looks at.
    pub target: Vec3,
}

/// How a case's camera became a gaze (face.js `update()`).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceView {
    /// The camera in head-joint space.
    pub camera_in_head: Vec3,
    /// The same, lowered by the eye lift.
    pub eye_relative: Vec3,
    /// Distance from the eyes to the camera.
    pub distance: f64,
    /// `atan2(x, z)` of the camera as the eyes see it.
    pub azimuth: f64,
    /// `asin(y / distance)`.
    pub elevation: f64,
    /// Whether the camera is in front of the face.
    pub facing: bool,
    /// The gaze target from azimuth and elevation, zero when not facing.
    pub look_t: Xy,
    /// The reflection offset, facing or not.
    pub refl_v: Xy,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// A binary file under construction: sections back to back, each described the way the dump writes them.
    #[derive(Default)]
    struct BlobBuilder {
        bytes: Vec<u8>,
    }

    impl BlobBuilder {
        fn section(&mut self, kind: &str, item_size: usize, words: &[u32]) -> Value {
            let offset = self.bytes.len();
            self.bytes.extend(words.iter().flat_map(|w| w.to_le_bytes()));
            json!({ "type": kind, "itemSize": item_size, "count": words.len() / item_size, "offset": offset,
                    "fnv": format!("{:08x}", fnv1a_words(words)) })
        }

        fn floats(&mut self, item_size: usize, values: &[f32]) -> Value {
            self.section("f32", item_size, &values.iter().map(|v| v.to_bits()).collect::<Vec<_>>())
        }
    }

    /// A made-up dump (a quad with every kind of attribute, two hair clumps, one face case) laid out like the real one.
    /// Nothing here is the sample character's.
    fn write_dump(root: &Path) {
        let write = |rel: &str, bytes: &[u8]| {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        };
        write("manifest.json", b"{}");

        let mut blob = BlobBuilder::default();
        let position = blob.floats(3, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0]);
        let normal = blob.floats(3, &[0.0, 0.0, 1.0].repeat(4));
        let uv = blob.floats(2, &[0.0, 0.0, 1.0, 0.0, 1.0, 0.25, 0.0, 1.0]);
        let color = blob.floats(3, &[0.1, 0.2, 0.3].repeat(4));
        let aux = blob.floats(2, &[5.0; 8]);
        let pid = blob.floats(1, &[0.0, 1.0, 2.0, 0.0]);
        let index = blob.section("u32", 1, &[0, 1, 2, 0, 2, 3]);
        let skin_index = blob.section("u32", 4, &[3, 0, 0, 0].repeat(4));
        let skin_weight = blob.floats(4, &[1.0, 0.0, 0.0, 0.0].repeat(4));
        let hash = position["fnv"].clone();
        let material = json!({ "name": "skin", "type": "MeshToonMaterial", "programKey": "dtoon:head", "color": "#f9d3c5",
            "emissive": "#000000", "side": "front", "vertexColors": true, "transparent": false, "alphaTest": 0,
            "hasMap": false, "toon": { "shade": "#f2b4ab", "shift": -0.28, "soft": 0.28, "rim": "#ffd9cf",
            "rimStrength": 0.1, "rimPower": 3 }, "otherUniforms": { "uBlush": "#f6a6a0", "uFace": { "texture": true } } });
        let mesh = json!({ "name": "quad", "part": "head", "type": "SkinnedMesh", "file": "quad.bin", "vertexCount": 4,
            "triangleCount": 2, "bbox": { "min": [0, 0, 0], "max": [1, 1, 0] }, "hash": hash, "skinned": true,
            "renderOrder": 2, "castShadow": true, "receiveShadow": false, "material": material,
            "outline": { "color": "#d49a8e", "width": 0.0014, "lightScale": 1, "renderOrder": 0 },
            "sections": { "position": position, "normal": normal, "uv": uv, "color": color, "aux": aux, "pid": pid,
                          "index": index, "skinIndex": skin_index, "skinWeight": skin_weight } });
        let identity: Vec<f32> = Mat4::IDENTITY.to_cols_array().to_vec();
        let inverses = [identity.clone(), identity.clone()].concat();
        let index_json = json!({ "schema": 1, "kind": "geometry", "variant": "game", "three": "170",
            "group": { "matrixWorld": identity }, "groupGeometryHash": "7a83faea",
            "skeleton": { "bones": ["hips", "spine"], "boneInverses": inverses,
                          "bindMatrix": identity },
            "meshes": [mesh] });
        write("geometry/game/index.json", index_json.to_string().as_bytes());
        write("geometry/game/quad.bin", &blob.bytes);

        let mut hair = BlobBuilder::default();
        let rest = hair.floats(3, &[0.0, 0.0, 0.0, 0.0, -1.0, 0.0]);
        let width = hair.floats(1, &[0.1, 0.05]);
        let thick = hair.floats(1, &[0.02, 0.01]);
        let stiff = hair.floats(1, &[1.0, 0.5]);
        let pidx = hair.section("u32", 1, &[0, 1]);
        let p_rest = hair.floats(3, &[0.0, 0.0, 0.0, 0.0, -1.0, 0.0]);
        let len = hair.floats(1, &[1.0]);
        let k = hair.floats(1, &[1.0, 0.5]);
        let settled = hair.floats(3, &[0.0, 0.5, 0.0, 0.0, -0.5, 0.0]);
        let yawed = hair.floats(3, &[0.1, 0.5, 0.0, 0.1, -0.5, 0.0]);
        let still = json!({ "index": 0, "dyn": false, "follow": null, "shell": false, "u": null, "shade": 0.97,
            "tube": { "mesh": "hair-static", "first": 0, "count": 16 }, "restCount": 2,
            "rest": rest, "width": width, "thick": thick, "stiff": null });
        let moving = json!({ "index": 1, "dyn": true, "follow": "body", "shell": true, "u": 0.25, "shade": 1,
            "tube": { "mesh": "hair-curtain", "first": 0, "count": 10, "strip": 3 }, "restCount": 2,
            "rest": rest, "width": width, "thick": thick, "stiff": stiff, "particleCount": 2,
            "pidx": pidx, "pRest": p_rest, "len": len, "k": k, "settled": settled, "yawed": yawed });
        let bone = json!({ "position": [0, 0.5, 0], "quaternion": [0, 0, 0, 1], "rotation": [0, 0, 0, "XYZ"] });
        let hair_json = json!({ "schema": 1, "kind": "hair", "variant": "game",
            "sim": { "t": 0, "dt": 0.0111, "settleSteps": 480 }, "pose": { "t": 0, "par": {} },
            "motion": { "yawDeg": 25, "stepCount": 1, "steps": [{ "lookYaw": 0.4, "neck": [0, 0.1, 0, 0.99], "head": [0, 0.2, 0, 0.98] }] },
            "tubeRing": 8, "curtainColumns": 5, "staticTailVertices": 3840,
            "bones": { "settled": { "hips": bone }, "yawed": { "hips": bone } }, "clumps": [still, moving] });
        write("state/game/hair.json", hair_json.to_string().as_bytes());
        write("state/game/hair.bin", &hair.bytes);

        let xy = json!({ "x": 0.5, "y": -0.25 });
        let face_json = json!({ "schema": 1, "kind": "face-state", "variant": "game",
            "faceView": { "eyeLift": 0.22, "frontGate": 0.25, "lookYawScale": 0.7, "lookPitchScale": 0.6,
                          "reflGain": 0.35, "reflMaxX": 0.25, "reflMaxY": 0.2 },
            "cases": [{ "id": "game/blink", "expression": "blink", "camera": "face",
                "input": { "channels": { "winkL": 1 }, "lookAdd": xy, "t": 0.5,
                           "cameraSpec": { "fov": 30, "near": 0.05, "far": 80, "position": [0, 0.75, 1.1], "target": [0, 0.75, 0] } },
                "p": { "open": 1, "mSmile": -0.45 },
                "view": { "cameraInHead": [0, 0.12, 1.1], "eyeRelative": [0, -0.09, 1.1], "distance": 1.1, "azimuth": 0,
                          "elevation": -0.08, "facing": true, "lookT": xy, "reflV": xy },
                "look": xy, "lookD": xy, "reflV": xy, "twT": 0.5, "dart": xy,
                "headMatrixWorld": identity, "cameraMatrixWorld": identity }] });
        write("state/game/face-state.json", face_json.to_string().as_bytes());
    }

    fn dump() -> (tempfile::TempDir, JsRef) {
        let dir = tempfile::tempdir().unwrap();
        write_dump(dir.path());
        let js = JsRef::open_at(dir.path()).unwrap();
        (dir, js)
    }

    #[test]
    fn a_missing_dump_is_a_skip_not_an_error() {
        let empty = tempfile::tempdir().unwrap();
        assert!(JsRef::open_at(empty.path()).is_none());
        assert!(JsRef::open_at(&empty.path().join("nowhere")).is_none());
    }

    #[test]
    fn the_dump_is_found_under_the_golden_store_or_at_the_default() {
        let env = |goldens: Option<&'static str>, assets: Option<&'static str>| {
            move |key: &str| match key {
                GOLDENS_ENV => goldens.map(OsString::from),
                ASSETS_ENV => assets.map(OsString::from),
                _ => None,
            }
        };
        assert_eq!(locate(env(Some("/store"), None)), Path::new("/store/js"));
        assert_eq!(locate(env(Some(""), Some("/assets"))), Path::new("/assets/goldens/js"));
        assert_eq!(locate(env(None, Some("/assets"))), Path::new("/assets/goldens/js"));
        assert!(locate(env(None, None)).ends_with("assets-local/goldens/js"));
    }

    #[test]
    fn a_mesh_loads_into_mesh_data() {
        let (_dir, js) = dump();
        let dump = js.geometry("game").unwrap();
        assert_eq!((dump.variant.as_str(), dump.three.as_str(), dump.trace_mix), ("game", "170", None));
        assert_eq!((dump.bones.len(), dump.bone_inverses.len(), dump.bind_matrix), (2, 2, Mat4::IDENTITY));
        let m = dump.mesh("quad").unwrap();
        assert_eq!((m.part.as_str(), m.render_order, m.cast_shadow, m.receive_shadow), ("head", 2, true, false));
        assert_eq!(m.bbox, (Vec3::ZERO, Vec3::new(1.0, 1.0, 0.0)));

        let mesh = &m.mesh;
        assert_eq!(mesh.positions[2], Vec3::new(1.0, 1.0, 0.0));
        assert_eq!(mesh.normals[0], Vec3::Z);
        assert_eq!(mesh.indices, [0, 1, 2, 0, 2, 3]);
        // three.js has the origin bottom-left, MeshData top-left: v is flipped.
        assert_eq!(mesh.uvs[2], Vec2::new(1.0, 0.75));
        assert_eq!(mesh.colours[1], [0.1, 0.2, 0.3, 1.0]);
        assert_eq!(mesh.joints[0], [3, 0, 0, 0]);
        assert_eq!(mesh.weights[0], [1.0, 0.0, 0.0, 0.0]);
        let extras: Vec<(&str, u8)> = mesh.extra.iter().map(|e| (e.name.as_str(), e.components)).collect();
        assert_eq!(extras, [("robe-aux", 2), ("robe-pid", 1)]);
        assert_eq!(mesh.extra_attribute("robe-pid").unwrap().data, [0.0, 1.0, 2.0, 0.0]);
        mesh.validate().unwrap();
    }

    #[test]
    fn material_and_outline_records_come_through() {
        let (_dir, js) = dump();
        let m = js.geometry("game").unwrap().meshes.remove(0);
        assert_eq!((m.material.name.as_str(), m.material.kind.as_str()), ("skin", "MeshToonMaterial"));
        assert_eq!(m.material.program_key.as_deref(), Some("dtoon:head"));
        assert!(m.material.vertex_colors && !m.material.has_map);
        let toon = m.material.toon.unwrap();
        assert_eq!((toon.shade.as_str(), toon.shift, toon.rim_power), ("#f2b4ab", -0.28, 3.0));
        assert_eq!(m.material.other_uniforms["uBlush"], "#f6a6a0");
        assert_eq!(m.material.other_uniforms["uFace"]["texture"], true);
        assert_eq!(m.outline.unwrap().width, 0.0014);
    }

    #[test]
    fn a_changed_byte_is_a_hash_error_naming_the_file() {
        let (dir, js) = dump();
        let bin = dir.path().join("geometry/game/quad.bin");
        let mut bytes = fs::read(&bin).unwrap();
        bytes[5] ^= 0x40; // inside the position section
        fs::write(&bin, bytes).unwrap();
        let err = js.geometry("game").unwrap_err();
        assert!(matches!(&err, JsRefError::Corrupt { path, .. } if *path == bin), "{err}");
        assert!(err.to_string().contains("hashes to"), "{err}");
    }

    #[test]
    fn a_truncated_file_and_a_missing_file_are_errors() {
        let (dir, js) = dump();
        let bin = dir.path().join("geometry/game/quad.bin");
        let bytes = fs::read(&bin).unwrap();
        fs::write(&bin, &bytes[..bytes.len() - 8]).unwrap();
        assert!(js.geometry("game").unwrap_err().to_string().contains("lies outside the file"));
        fs::remove_file(&bin).unwrap();
        assert!(matches!(js.geometry("game"), Err(JsRefError::Io { .. })));
        assert!(matches!(js.geometry("nowhere"), Err(JsRefError::Io { .. })));
    }

    #[test]
    fn a_section_of_the_wrong_shape_is_an_error() {
        let (dir, js) = dump();
        let index = dir.path().join("geometry/game/index.json");
        let text = fs::read_to_string(&index).unwrap();
        // The index claims the position section has two items per vertex.
        fs::write(&index, text.replacen("\"itemSize\":3", "\"itemSize\":2", 1)).unwrap();
        assert!(js.geometry("game").unwrap_err().to_string().contains("expected f32x3"));
        fs::write(&index, "{ not json").unwrap();
        assert!(matches!(js.geometry("game"), Err(JsRefError::Json { .. })));
    }

    #[test]
    fn hair_clumps_and_particles_load() {
        let (_dir, js) = dump();
        let hair = js.hair("game").unwrap();
        assert_eq!((hair.sim.settle_steps, hair.tube_ring, hair.static_tail_vertices), (480, 8, 3840));
        assert_eq!((hair.yaw_deg, hair.motion.len(), hair.motion[0].look_yaw), (25.0, 1, 0.4));
        assert_eq!(hair.motion[0].head, Quat::from_xyzw(0.0, 0.2, 0.0, 0.98));
        assert_eq!(hair.bones_settled["hips"].position, Vec3::new(0.0, 0.5, 0.0));
        let [still, moving] = &hair.clumps[..] else { panic!("two clumps") };
        assert!(!still.dynamic && still.particles.is_none() && still.stiff.is_none() && still.u.is_none());
        assert_eq!(still.rest, [Vec3::ZERO, Vec3::NEG_Y]);
        assert_eq!(
            (moving.dynamic, moving.follow.as_deref(), moving.shell, moving.u),
            (true, Some("body"), true, Some(0.25))
        );
        assert_eq!((moving.tube.mesh.as_str(), moving.tube.count, moving.tube.strip), ("hair-curtain", 10, Some(3)));
        let particles = moving.particles.as_ref().unwrap();
        assert_eq!(particles.rest_index, [0, 1]);
        assert_eq!(particles.segment_len, [1.0]);
        assert_eq!(particles.stiffness, [1.0, 0.5]);
        assert_eq!(particles.settled[0], Vec3::new(0.0, 0.5, 0.0));
        assert_eq!(particles.yawed[1], Vec3::new(0.1, -0.5, 0.0));
        assert_eq!(moving.stiff.as_deref(), Some(&[1.0, 0.5][..]));
    }

    #[test]
    fn face_state_cases_load_and_can_be_looked_up() {
        let (_dir, js) = dump();
        let faces = js.face_states("game").unwrap();
        assert_eq!(faces.face_view.eye_lift, 0.22);
        let case = faces.case("game/blink").unwrap();
        assert!(faces.case("game/other").is_none());
        assert_eq!((case.expression.as_str(), case.camera.as_str()), ("blink", "face"));
        assert_eq!(case.input.channels["winkL"], 1.0);
        assert_eq!(case.input.camera_spec.position, Vec3::new(0.0, 0.75, 1.1));
        assert_eq!(case.p["mSmile"], -0.45);
        assert_eq!((case.look_d, case.tw_t), (Xy { x: 0.5, y: -0.25 }, 0.5));
        assert!(case.view.facing);
        assert_eq!(case.head_matrix_world, Mat4::IDENTITY);
    }
}
