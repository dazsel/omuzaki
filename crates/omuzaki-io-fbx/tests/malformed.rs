//! Never panics or aborts on hostile input (EX-09). A rich ASCII file (a skinned cube with a blend shape, a material and a
//! texture) and a binary node hierarchy are truncated and have bytes flipped, and hand-made attacks are thrown at the
//! importer: a header-only binary file, an array that declares a billion elements and holds none, and 10 000 levels of
//! nesting. Each must come back as `Ok` with a scene that passes the invariants, or as an `IoError`, within 5 seconds,
//! and with a 1 MiB input cap the process stays under 512 MiB resident. Every file is written by the test.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_possible_truncation)]

mod common;

use std::fmt::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use common::{Axis, FbxBuilder, SCENE_ROOT};
use omuzaki_core::rng::Rng;
use omuzaki_io_api::{ImportCtx, ImportInput, ImportOptions, IoError, Limits, run_import};
use omuzaki_io_fbx::FbxImporter;

/// `OMUZAKI_FUZZ_FULL=1` truncates at every offset and flips every byte (minutes; run it in release mode before a
/// release).
fn full() -> bool {
    std::env::var_os("OMUZAKI_FUZZ_FULL").is_some()
}

/// The longest one hostile file may take.
const PER_FILE: Duration = Duration::from_secs(5);

/// `values` as an FBX array: `*n { a: v,v,v }`.
fn array<T: std::fmt::Display>(values: &[T]) -> String {
    let joined = values.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
    format!("*{} {{\n\t\t\ta: {joined}\n\t\t}}", values.len())
}

/// A Maya-style ASCII file with every kind of object the importer reads: a cube mesh with normals, UVs and a material
/// layer, a two-bone skin, a blend shape, a Lambert material and a texture on its diffuse colour.
fn rich_ascii() -> Vec<u8> {
    let mut file = FbxBuilder::new(Axis::Y, Axis::Z, Axis::X, 1.0);
    let hips = file.model("Hips", SCENE_ROOT, [0.0, 100.0, 0.0], [0.0; 3]);
    let spine = file.model("Spine", hips, [0.0, 20.0, 0.0], [0.0, 0.0, 10.0]);
    file.kind(hips, "LimbNode");
    file.kind(spine, "LimbNode");
    let body = file.model("Body", SCENE_ROOT, [0.0; 3], [0.0; 3]);
    file.kind(body, "Mesh");
    let points: Vec<i32> = (0..8).flat_map(|i| [i & 1, (i >> 1) & 1, (i >> 2) & 1].map(|b| b * 50)).collect();
    let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
    let indices: Vec<i32> = quads.iter().flat_map(|q| [q[0], q[1], q[2], -q[3] - 1]).collect();
    let normals: Vec<i32> = quads.iter().flat_map(|_| [0, 0, 1].repeat(4)).collect();
    let uvs: Vec<i32> = quads.iter().flat_map(|_| [0, 0, 1, 0, 1, 1, 0, 1]).collect();
    let geometry = file.id();
    let mut text = format!(
        "\tGeometry: {geometry}, \"Geometry::\", \"Mesh\" {{\n\t\tVertices: {}\n\t\tPolygonVertexIndex: {}\n\
         \t\tLayerElementNormal: 0 {{\n\t\t\tVersion: 101\n\t\t\tMappingInformationType: \"ByPolygonVertex\"\n\
         \t\t\tReferenceInformationType: \"Direct\"\n\t\t\tNormals: {}\n\t\t}}\n\
         \t\tLayerElementUV: 0 {{\n\t\t\tVersion: 101\n\t\t\tName: \"map1\"\n\t\t\tMappingInformationType: \"ByPolygonVertex\"\n\
         \t\t\tReferenceInformationType: \"Direct\"\n\t\t\tUV: {}\n\t\t}}\n\
         \t\tLayerElementMaterial: 0 {{\n\t\t\tVersion: 101\n\t\t\tMappingInformationType: \"AllSame\"\n\
         \t\t\tReferenceInformationType: \"IndexToDirect\"\n\t\t\tMaterials: *1 {{\n\t\t\t\ta: 0\n\t\t\t}}\n\t\t}}\n\
         \t\tLayer: 0 {{\n\t\t\tVersion: 100\n",
        array(&points),
        array(&indices),
        array(&normals),
        array(&uvs)
    );
    for kind in ["Normal", "UV", "Material"] {
        write!(
            text,
            "\t\t\tLayerElement:  {{\n\t\t\t\tType: \"LayerElement{kind}\"\n\t\t\t\tTypedIndex: 0\n\t\t\t}}\n"
        )
        .unwrap();
    }
    text.push_str("\t\t}\n\t}\n");
    file.raw(text, vec![(geometry, body, None)]);
    // The skin: the bottom four points follow the hips, the top four the spine.
    let skin = file.id();
    let skin_text = format!("\tDeformer: {skin}, \"Deformer::Skin\", \"Skin\" {{\n\t\tVersion: 101\n\t}}\n");
    file.raw(skin_text, vec![(skin, geometry, None)]);
    let identity = array(&[1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
    for (bone, points) in [(hips, [0, 1, 4, 5]), (spine, [2, 3, 6, 7])] {
        let cluster = file.id();
        let text = format!(
            "\tDeformer: {cluster}, \"SubDeformer::Cluster\", \"Cluster\" {{\n\t\tVersion: 100\n\t\tIndexes: {}\n\
             \t\tWeights: {}\n\t\tTransform: {identity}\n\t\tTransformLink: {identity}\n\t}}\n",
            array(&points),
            array(&[1; 4]),
        );
        file.raw(text, vec![(cluster, skin, None), (bone, cluster, None)]);
    }
    // A blend shape that lifts the top face.
    let (blend, channel, shape) = (file.id(), file.id(), file.id());
    file.raw(
        format!("\tDeformer: {blend}, \"Deformer::Blend\", \"BlendShape\" {{\n\t\tVersion: 100\n\t}}\n"),
        vec![(blend, geometry, None)],
    );
    file.raw(
        format!(
            "\tDeformer: {channel}, \"SubDeformer::Lift\", \"BlendShapeChannel\" {{\n\t\tVersion: 100\n\
             \t\tDeformPercent: 0\n\t\tFullWeights: *1 {{\n\t\t\ta: 100\n\t\t}}\n\t}}\n"
        ),
        vec![(channel, blend, None)],
    );
    file.raw(
        format!(
            "\tGeometry: {shape}, \"Geometry::Lift\", \"Shape\" {{\n\t\tVersion: 100\n\t\tIndexes: {}\n\t\tVertices: {}\n\t}}\n",
            array(&[2, 3, 6, 7]),
            array(&[0, 10, 0].repeat(4))
        ),
        vec![(shape, channel, None)],
    );
    // A material with a diffuse texture.
    let material = file.id();
    file.raw(
        format!(
            "\tMaterial: {material}, \"Material::skin\", \"\" {{\n\t\tVersion: 102\n\t\tShadingModel: \"lambert\"\n\
             \t\tProperties70:  {{\n\t\t\tP: \"DiffuseColor\", \"Color\", \"\", \"A\",0.9,0.7,0.6\n\t\t}}\n\t}}\n"
        ),
        vec![(material, body, None)],
    );
    let texture = file.id();
    file.raw(
        format!(
            "\tTexture: {texture}, \"Texture::skin.png\", \"\" {{\n\t\tType: \"TextureVideoClip\"\n\t\tVersion: 202\n\
             \t\tFileName: \"skin.png\"\n\t\tRelativeFilename: \"skin.png\"\n\t}}\n"
        ),
        vec![(texture, material, Some("DiffuseColor".to_owned()))],
    );
    file.bytes()
}

/// A binary file: a three-node chain and a sibling.
fn binary() -> Vec<u8> {
    let mut file = FbxBuilder::new(Axis::Y, Axis::Z, Axis::X, 1.0);
    let root = file.model("Root", SCENE_ROOT, [0.0, 1.0, 0.0], [0.0; 3]);
    let arm = file.model("Arm", root, [10.0, 0.0, 0.0], [0.0, 0.0, 45.0]);
    file.model("Hand", arm, [10.0, 0.0, 0.0], [0.0; 3]);
    file.model("Prop", SCENE_ROOT, [0.0; 3], [90.0, 0.0, 0.0]);
    file.binary()
}

/// Imports `bytes` with `options`: a panic fails the test naming `what`, an `Ok` must pass the scene invariants, and
/// either outcome must come within [`PER_FILE`]. Returns the error, if any.
fn survives(bytes: &[u8], options: &ImportOptions, what: &str) -> Option<IoError> {
    let start = Instant::now();
    let result =
        catch_unwind(AssertUnwindSafe(|| run_import(&FbxImporter, &ImportInput::new(bytes), &ImportCtx::new(options))));
    let took = start.elapsed();
    assert!(took < PER_FILE, "{what} took {took:?}");
    match result {
        Err(panic) => {
            eprintln!("{what} made the importer panic");
            std::panic::resume_unwind(panic);
        }
        Ok(Ok(out)) => {
            assert!(!out.scene.check().has_errors(), "{what} gave a scene that breaks the invariants");
            None
        }
        Ok(Err(e)) => Some(e),
    }
}

/// Truncations at evenly spaced offsets (every offset in full mode), then seeded single-byte flips (every byte set to
/// three values in full mode).
fn mutate(name: &str, base: &[u8], options: &ImportOptions) {
    let total = base.len();
    let cuts: Vec<usize> = if full() { (0..total).collect() } else { (0..64).map(|k| k * total / 64).collect() };
    let (mut tried, mut imported) = (0, 0);
    let mut count = |error: Option<IoError>| {
        tried += 1;
        imported += usize::from(error.is_none());
    };
    for len in cuts {
        count(survives(&base[..len], options, &format!("{name}: truncation to {len} of {total} bytes")));
    }
    if full() {
        for at in 0..total {
            for value in [0x00, 0xFF, 0x7F] {
                let mut bytes = base.to_vec();
                bytes[at] = value;
                count(survives(&bytes, options, &format!("{name}: byte {at} set to {value:#x}")));
            }
        }
    } else {
        let mut rng = Rng::new(0x0FB8_4A11);
        for _ in 0..256 {
            let at = (rng.next_u64() % total as u64) as usize;
            let value = (rng.next_u64() & 0xFF) as u8;
            let mut bytes = base.to_vec();
            bytes[at] = value;
            count(survives(&bytes, options, &format!("{name}: byte {at} set to {value:#x}")));
        }
    }
    eprintln!("{name}: {total} bytes, {tried} truncations and byte flips, {imported} still imported");
}

/// Hand-made attacks: a binary header and nothing after it, an array that declares a billion elements and holds none,
/// and 10 000 levels of nesting in both forms.
fn attacks() -> Vec<(&'static str, Vec<u8>)> {
    let header = [b"Kaydara FBX Binary  \0\x1A\x00".as_slice(), &7400_u32.to_le_bytes()].concat();
    let empty_array =
        String::from_utf8(rich_ascii()).unwrap().replacen("Vertices: *24 {", "Vertices: *1000000000 {", 1);
    let mut nested = "; FBX 7.4.0 project file\n".to_owned();
    nested.push_str(&"A: {\n".repeat(10_000));
    nested.push_str(&"}\n".repeat(10_000));
    let mut deep_binary = header.clone();
    let mut ends = Vec::new();
    for _ in 0..10_000 {
        ends.push(deep_binary.len());
        // A record with no properties and one child: its end offset is patched once the children are written.
        deep_binary.extend([0_u8; 12]);
        deep_binary.extend([1, b'A']);
    }
    for start in ends.into_iter().rev() {
        deep_binary.extend([0_u8; 13]);
        let end = u32::try_from(deep_binary.len()).unwrap();
        deep_binary[start..start + 4].copy_from_slice(&end.to_le_bytes());
    }
    deep_binary.extend([0_u8; 13]);
    assert!(empty_array.contains("*1000000000"), "the fixture's vertex array was found");
    vec![
        ("a header-only binary file", header),
        ("an ASCII array declaring 1 000 000 000 elements and holding 24", empty_array.into_bytes()),
        ("10 000 nested ASCII nodes", nested.into_bytes()),
        ("10 000 nested binary records", deep_binary),
    ]
}

#[test]
fn the_fixtures_import_before_they_are_broken() {
    for (name, bytes) in [("ascii", rich_ascii()), ("binary", binary())] {
        let out = run_import(&FbxImporter, &ImportInput::new(&bytes), &ImportCtx::new(&ImportOptions::default()))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!out.scene.nodes.is_empty(), "{name}: nodes");
    }
    let out =
        run_import(&FbxImporter, &ImportInput::new(&rich_ascii()), &ImportCtx::new(&ImportOptions::default())).unwrap();
    let mesh = &out.scene.meshes[0];
    assert!(mesh.primitives[0].mesh.positions.len() >= 8, "the cube's points");
    assert!(!mesh.primitives[0].morph_targets.is_empty(), "the blend shape");
    assert!(out.scene.skins.first().is_some_and(|s| s.joints.len() == 2), "the two-bone skin");
    assert!(!out.scene.materials.is_empty(), "the material");
}

#[test]
fn truncated_and_flipped_files_never_panic() {
    let options = ImportOptions::default();
    mutate("ascii", &rich_ascii(), &options);
    mutate("binary", &binary(), &options);
}

#[test]
fn hostile_files_come_back_quickly_and_under_half_a_gigabyte() {
    let mut options = ImportOptions::default();
    options.limits = Limits { max_input_bytes: 1 << 20, ..Limits::default() };
    for (what, bytes) in attacks() {
        let outcome = survives(&bytes, &options, what);
        eprintln!("{what}: {}", outcome.map_or_else(|| "imported".to_owned(), |e| e.to_string()));
    }
    mutate("ascii with a 1 MiB cap", &rich_ascii(), &options);
    // The whole test process's peak resident memory, so the other tests running beside this one count against it too.
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let peak_kib: u64 = status
            .lines()
            .find_map(|l| l.strip_prefix("VmHWM:"))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
            .expect("VmHWM in /proc/self/status");
        eprintln!("peak resident memory: {} MiB", peak_kib / 1024);
        assert!(peak_kib < 512 * 1024, "peak resident memory {peak_kib} KiB");
    }
}
