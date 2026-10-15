//! `io.inspect` of a `.gltf` reads its sidecars through [`DirResolver`](omuzaki_io_ops::ops::sidecar): the folder of the
//! file the person named, and nothing outside it. This binary holds its own probe importer, because the inventory
//! registry is per binary and `tests/ops.rs` asserts the exact importer set; `omuzaki-io-mock`'s importers read no
//! external files.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use omuzaki_core::glam::Vec3;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{
    Confidence, FormatInfo, ImportCtx, ImportInput, ImportOutcome, Importer, IoError, IoScene, LicenceEvidence, Mesh,
    MeshData, Node, Primitive, register_importer,
};
use omuzaki_io_ops::ops::inspect::Inspect;
use omuzaki_ops_api::{Actor, ErrorCode, Grants, Op, OpCtx, OpError, ProjectDir, Services, ServicesBuilder, find};
use omuzaki_store_api::{Commercial, Store};
use omuzaki_store_mock::MemStore;
use omuzaki_store_mock::conformance::sample_provenance;
use serde_json::{Value, json};
use tempfile::TempDir;

/// Reads a `.gltf` the way a real importer does: the first buffer comes through the resolver, the scene has one node per
/// glTF node, and the buffer's bytes are the positions of one mesh (`f32` xyz, 12 bytes a vertex).
#[derive(Default)]
struct ProbeImporter;

/// The `f32` at byte `at` of a 12-byte vertex.
fn f32_at(vertex: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([vertex[at], vertex[at + 1], vertex[at + 2], vertex[at + 3]])
}

impl Importer for ProbeImporter {
    fn info(&self) -> FormatInfo {
        FormatInfo { id: "probe", name: "Probe glTF", extensions: &["gltf"], media_type: "model/gltf+json" }
    }

    fn sniff(&self, input: &ImportInput<'_>) -> Confidence {
        if input.bytes.first() == Some(&b'{') { Confidence::Possible } else { Confidence::No }
    }

    fn import(&self, input: &ImportInput<'_>, ctx: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
        let json: Value = serde_json::from_slice(input.bytes).map_err(|e| IoError::malformed(e.to_string()))?;
        let mut scene = IoScene::new();
        let mut mesh_ix = None;
        if let Some(uri) = json["buffers"][0]["uri"].as_str() {
            let bytes = ctx.resolver.read(uri)?;
            let (vertices, _) = bytes.as_chunks::<12>();
            let positions: Vec<Vec3> =
                vertices.iter().map(|v| Vec3::new(f32_at(v, 0), f32_at(v, 4), f32_at(v, 8))).collect();
            // One triangle over the first three vertices, with a normal per vertex, as the scene checks require.
            let indices: Vec<u32> = (0..3).take(positions.len()).collect();
            let normals = vec![Vec3::Z; positions.len()];
            let mut mesh = Mesh::new("probe");
            mesh.primitives.push(Primitive::new(MeshData { positions, normals, indices, ..MeshData::default() }));
            mesh_ix = Some(scene.add_mesh(mesh));
        }
        for i in 0..json["nodes"].as_array().map_or(0, Vec::len) {
            let mut node = Node::new(format!("node {i}"));
            if i == 0 {
                node.mesh = mesh_ix;
            }
            scene.add_node(node);
        }
        Ok(ImportOutcome::new(scene, LicenceEvidence::Absent))
    }
}

register_importer!(ProbeImporter);

/// `models/hero.gltf` with its `models/hero.bin` (three vertices) beside it, and `outside.bin` one folder up from it, in
/// the project folder. The project folder is the folder the request runs in.
struct Project {
    _dir: TempDir,
    root: PathBuf,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Canonical, so the paths the checks return are the paths the tests name.
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(root.join("models")).unwrap();
        fs::write(root.join("models/hero.bin"), [0u8; 36]).unwrap();
        fs::write(root.join("outside.bin"), [1u8; 12]).unwrap();
        let project = Self { _dir: dir, root };
        project.write("models/hero.gltf", &gltf("hero.bin", 3));
        project
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.path(name), bytes).unwrap();
    }
}

/// A glTF with `nodes` nodes and one buffer at `uri`.
fn gltf(uri: &str, nodes: usize) -> Vec<u8> {
    let nodes: Vec<Value> = (0..nodes).map(|_| json!({})).collect();
    json!({ "asset": { "version": "2.0" }, "nodes": nodes, "buffers": [{ "uri": uri, "byteLength": 36 }] })
        .to_string()
        .into_bytes()
}

/// `io.inspect` is registered by this crate; naming its type links the crate into this binary, so its registration runs.
#[test]
fn the_op_is_registered() {
    assert_eq!(Inspect::meta().name, "io.inspect");
}

/// Inspects `path` as an agent (read only) in the project folder, with `services`.
fn inspect_in(project: &Project, args: Value, services: &Services) -> Result<Value, OpError> {
    let registration = find("io.inspect").expect("io.inspect is registered");
    let op = (registration.decode)(args)?;
    let (doc, actor, clock) = (MemDoc::new(), Actor::Person { name: "tester".into() }, FakeClock::default());
    let grants = Grants::agent_default();
    let project_dir = ProjectDir::open(&project.root).expect("open the project folder");
    let ctx =
        OpCtx::new(&doc, &actor, &clock).with_project_dir(&project_dir).with_grants(&grants).with_services(services);
    let plan = op.plan(&ctx)?;
    plan.output.ok_or_else(|| OpError::internal("a query answers with an output"))
}

/// `io.inspect` of the file at `path`, relative to the project folder, with no services.
fn inspect(project: &Project, path: &str) -> Result<Value, OpError> {
    inspect_in(project, json!({ "path": project.path(path).to_str().unwrap() }), &ServicesBuilder::new().build())
}

#[test]
fn a_gltf_with_its_bin_beside_it_inspects_with_the_right_counts() {
    let project = Project::new();
    let output = inspect(&project, "models/hero.gltf").unwrap();
    assert_eq!(output["source"], json!("path"));
    let summary = &output["summary"];
    assert_eq!(
        (
            summary["nodes"].as_u64(),
            summary["meshes"].as_u64(),
            summary["primitives"].as_u64(),
            summary["vertices"].as_u64()
        ),
        (Some(3), Some(1), Some(1), Some(3)),
        "{output}"
    );
}

#[test]
fn a_sidecar_that_climbs_out_of_the_folder_is_refused_even_when_the_file_exists() {
    let project = Project::new();
    project.write("models/climb.gltf", &gltf("../outside.bin", 1));
    assert!(project.path("outside.bin").exists(), "the refusal must come from the form, not from a missing file");
    let error = inspect(&project, "models/climb.gltf").unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "{error:?}");
    assert!(error.message.contains("refused:"), "{}", error.message);
    assert!(error.message.contains("../outside.bin"), "{}", error.message);
}

#[test]
fn an_absolute_sidecar_uri_is_refused() {
    // A portable absolute URI: a native path would carry backslashes on Windows, which are refused for another reason.
    let project = Project::new();
    project.write("models/absolute.gltf", &gltf("/hero.bin", 1));
    let error = inspect(&project, "models/absolute.gltf").unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "{error:?}");
    assert!(error.message.contains("refused:") && error.message.contains("the path is absolute"), "{}", error.message);
}

#[test]
fn a_drive_letter_sidecar_uri_is_refused() {
    // Only the refusal is asserted: which rule catches it depends on the platform's path handling.
    let project = Project::new();
    project.write("models/drive.gltf", &gltf("C:/hero.bin", 1));
    let error = inspect(&project, "models/drive.gltf").unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "{error:?}");
    assert!(error.message.contains("refused:"), "{}", error.message);
}

#[test]
fn an_asset_sourced_gltf_still_has_no_access_to_other_files() {
    let project = Project::new();
    let store = Arc::new(MemStore::new());
    let hash = store.put(&gltf("hero.bin", 3), sample_provenance(Commercial::Ok)).unwrap();
    let services = ServicesBuilder::new().insert::<dyn Store>(store).build();
    let error = inspect_in(&project, json!({ "asset": hash.to_string() }), &services).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "{error:?}");
    assert!(error.message.contains("no access to other files"), "{}", error.message);
}
