//! Opening a file: the container, the JSON, and the two gates every glTF reader needs (the version, and the
//! extensions a file requires).
//!
//! [`probe`] is the cheap sniff for `Importer::sniff`; [`parse`] is the real open. Both accept a binary GLB and a text
//! `.gltf`.

use std::collections::BTreeMap;

use omuzaki_io_api::{IoError, Limits};
use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::container;
use crate::schema::{self, Counts, Document};

/// Extensions this crate reads itself, which a file may therefore *require*.
pub const MODELLED_EXTENSIONS: [&str; 4] =
    ["KHR_materials_unlit", "KHR_materials_emissive_strength", "KHR_texture_transform", "KHR_mesh_quantization"];

/// Extensions whose JSON refers to accessors, buffer views or buffers by index. The writer re-packs all of those, so
/// a carried copy would point at the wrong data, and a file that *requires* one can't be read without implementing it.
pub const BUFFER_BOUND_EXTENSIONS: [&str; 5] = [
    "EXT_mesh_gpu_instancing",
    "KHR_draco_mesh_compression",
    "EXT_meshopt_compression",
    "EXT_mesh_features",
    "EXT_structural_metadata",
];

/// A parsed file: the typed document and, for a GLB, the binary chunk (buffer 0).
#[derive(Debug)]
pub struct Parsed<'a> {
    /// The glTF JSON.
    pub document: Document,
    /// The GLB's binary chunk, if the file is a GLB that has one.
    pub chunk: Option<&'a [u8]>,
}

impl Parsed<'_> {
    /// Refuses a file that requires an extension this crate doesn't model and `supported` doesn't name.
    ///
    /// # Errors
    /// [`IoError::Unsupported`] naming the extensions, with a hint (decompression for Draco and meshopt).
    pub fn check_required(&self, supported: &[&str]) -> Result<(), IoError> {
        let missing: Vec<&str> = self
            .document
            .extensions_required
            .iter()
            .map(String::as_str)
            .filter(|e| !MODELLED_EXTENSIONS.contains(e) && !supported.contains(e))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let hint = if missing.iter().any(|e| BUFFER_BOUND_EXTENSIONS.contains(e)) {
            "decompress or un-instance the model first (for example with gltf-transform) and import the result"
        } else {
            "remove the extension in the authoring tool, or add support"
        };
        Err(IoError::unsupported(format!("required extension(s) {}", missing.join(", ")), hint))
    }
}

/// Opens `bytes` as a GLB or a text glTF.
///
/// Sizes are checked before anything is allocated from them: the input against `limits.max_input_bytes`, GLB chunk
/// lengths against the file, and the length of every top-level array against the limits in a pass that builds none
/// of the arrays. Nodes and images have limits of their own; any other array may hold as many elements as fit in the
/// memory `limits.max_nodes` nodes take, so a heavier element gets fewer places.
///
/// # Errors
/// [`IoError::Limit`] when a size is over the limits, [`IoError::Malformed`] for a broken container or JSON or a
/// missing `asset.version`, [`IoError::Unsupported`] for a GLB or glTF version other than 2.
pub fn parse<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Parsed<'a>, IoError> {
    let size = bytes.len() as u64;
    if size > limits.max_input_bytes {
        return Err(IoError::Limit { what: "input size in bytes", limit: limits.max_input_bytes, got: size });
    }
    let (json, chunk) = if container::is_glb(bytes) {
        let parts = container::split(bytes)?;
        (parts.json, parts.bin)
    } else {
        (bytes, None)
    };
    let document = parse_document(trim_json(json), limits)?;
    check_version(&document)?;
    Ok(Parsed { document, chunk })
}

/// What a file's head says, without building the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// True for a binary GLB, false for text JSON.
    pub glb: bool,
    /// The names of the root-level extensions (empty if the head couldn't be read). A VRM importer asks for
    /// `VRMC_vrm` or `VRM`.
    pub root_extensions: Vec<String>,
}

/// Recognises a glTF 2.x file cheaply: the GLB magic, or JSON whose `asset.version` starts with 2. Only the
/// *names* of the root extensions are read (their bodies are skipped), so this is fit for `Importer::sniff`.
pub fn probe(bytes: &[u8]) -> Option<Probe> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Head {
        asset: Version,
        // `IgnoredAny` skips each body: only the names are wanted.
        #[allow(clippy::zero_sized_map_values)]
        extensions: BTreeMap<String, IgnoredAny>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Version {
        version: String,
    }
    let names = |head: Head| head.extensions.into_keys().collect();
    if container::is_glb(bytes) {
        let head = container::split(bytes).ok().and_then(|c| serde_json::from_slice::<Head>(trim_json(c.json)).ok());
        return Some(Probe { glb: true, root_extensions: head.map(names).unwrap_or_default() });
    }
    let text = trim_json(bytes);
    if text.first() != Some(&b'{') {
        return None;
    }
    let head = serde_json::from_slice::<Head>(text).ok().filter(|h| h.asset.version.starts_with('2'))?;
    Some(Probe { glb: false, root_extensions: names(head) })
}

/// The JSON text of a document: a UTF-8 byte-order mark and trailing padding (spaces, and the NULs some exporters use)
/// removed.
pub fn trim_json(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let end = bytes.iter().rposition(|b| !matches!(b, b' ' | 0 | b'\n' | b'\r' | b'\t')).map_or(0, |i| i + 1);
    bytes.get(..end).unwrap_or(bytes)
}

fn parse_json<'de, T: Deserialize<'de>>(json: &'de [u8]) -> Result<T, IoError> {
    serde_json::from_slice(json).map_err(|e| IoError::Malformed {
        message: format!("invalid glTF JSON: {e}"),
        at: Some(format!("JSON line {} column {}", e.line(), e.column())),
    })
}

/// Counts first, so a file that names millions of objects is refused before the real parse allocates them.
fn parse_document(json: &[u8], limits: &Limits) -> Result<Document, IoError> {
    check_counts(&parse_json(json)?, limits)?;
    parse_json(json)
}

/// The fewest elements any top-level array may be held to, however small `max_nodes` is: a file with a handful of
/// nodes still needs a few dozen accessors, and 4096 of the largest element is under 4 MB.
const MIN_ARRAY_CAP: u64 = 4096;

/// The most elements a top-level array may hold, counted in `T`s: as many as fit in the memory the node limit allows
/// (`max_nodes` nodes), and at least [`MIN_ARRAY_CAP`]. [`Limits`] has no field per array and is the API's to grow, so
/// every array shares the one budget the node limit already implies. That keeps the worst a hostile file can make any
/// array cost at the size of a file full of nodes, however large its element is: the larger the element, the fewer
/// places.
fn array_cap<T>(limits: &Limits) -> u64 {
    let budget = u64::from(limits.max_nodes) * size_of::<schema::Node>() as u64;
    (budget / size_of::<T>().max(1) as u64).max(MIN_ARRAY_CAP)
}

/// Refuses a document with more of anything than the limits allow. Nodes and images have limits of their own; every
/// other top-level array is held to [`array_cap`].
fn check_counts(counts: &Counts, limits: &Limits) -> Result<(), IoError> {
    let len = |a: &[IgnoredAny]| a.len() as u64;
    for (what, limit, got) in [
        ("node count", u64::from(limits.max_nodes), len(&counts.nodes)),
        ("image count", u64::from(limits.max_images), len(&counts.images)),
        ("scene count", array_cap::<schema::Scene>(limits), len(&counts.scenes)),
        ("mesh count", array_cap::<schema::Mesh>(limits), len(&counts.meshes)),
        ("skin count", array_cap::<schema::Skin>(limits), len(&counts.skins)),
        ("material count", array_cap::<schema::Material>(limits), len(&counts.materials)),
        ("texture count", array_cap::<schema::Texture>(limits), len(&counts.textures)),
        ("sampler count", array_cap::<schema::Sampler>(limits), len(&counts.samplers)),
        ("accessor count", array_cap::<schema::Accessor>(limits), len(&counts.accessors)),
        ("buffer view count", array_cap::<schema::BufferView>(limits), len(&counts.buffer_views)),
        ("buffer count", array_cap::<schema::Buffer>(limits), len(&counts.buffers)),
        ("animation count", array_cap::<serde_json::Value>(limits), len(&counts.animations)),
        ("camera count", array_cap::<serde_json::Value>(limits), len(&counts.cameras)),
    ] {
        if got > limit {
            return Err(IoError::Limit { what, limit, got });
        }
    }
    Ok(())
}

fn check_version(doc: &Document) -> Result<(), IoError> {
    let major = |v: &str| v.split('.').next().and_then(|m| m.parse::<u32>().ok());
    let version = &doc.asset.version;
    if version.is_empty() {
        return Err(IoError::Malformed {
            message: "asset.version is required".into(),
            at: Some("/asset/version".into()),
        });
    }
    if major(version) != Some(2) || doc.asset.min_version.as_deref().and_then(major).is_some_and(|m| m > 2) {
        return Err(IoError::unsupported(format!("glTF version {version:?}"), "only glTF 2.x is supported"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn glb(doc: &serde_json::Value) -> Vec<u8> {
        container::join(doc.to_string().as_bytes(), &[1, 2, 3, 4]).unwrap()
    }

    #[test]
    fn json_is_trimmed_of_bom_and_padding() {
        assert_eq!(trim_json(b"\xEF\xBB\xBF{}  \0\0\n"), b"{}");
        assert_eq!(trim_json(b"   "), b"");
        assert_eq!(trim_json(b""), b"");
    }

    #[test]
    fn a_glb_and_a_text_gltf_open_to_the_same_document() {
        let doc = json!({ "asset": { "version": "2.0" }, "nodes": [{ "name": "a" }] });
        let limits = Limits::default();
        let binary = glb(&doc);
        let from_glb = parse(&binary, &limits).unwrap();
        assert_eq!(from_glb.chunk, Some(&[1, 2, 3, 4][..]));
        let text = doc.to_string();
        let from_text = parse(text.as_bytes(), &limits).unwrap();
        assert_eq!(from_text.chunk, None);
        assert_eq!(from_glb.document, from_text.document);
        assert_eq!(from_text.document.nodes[0].name, "a");
    }

    #[test]
    fn the_probe_recognises_a_file_and_reads_only_the_root_extension_names() {
        let doc = json!({
            "asset": { "version": "2.0" },
            "extensions": { "VRMC_vrm": { "specVersion": "1.0", "humanoid": { "big": [1, 2, 3] } }, "KHR_x": {} },
            "nodes": [{}]
        });
        let want = vec!["KHR_x".to_owned(), "VRMC_vrm".to_owned()];
        assert_eq!(probe(&glb(&doc)), Some(Probe { glb: true, root_extensions: want.clone() }));
        assert_eq!(probe(doc.to_string().as_bytes()), Some(Probe { glb: false, root_extensions: want }));
        assert_eq!(
            probe(b"glTF\x02\0\0\0").map(|p| (p.glb, p.root_extensions)),
            Some((true, vec![])),
            "magic is enough"
        );
        for no in [&br#"{"asset":{"version":"1.0"}}"#[..], br#"{"hello":1}"#, b"[]", b""] {
            assert_eq!(probe(no), None, "{}", String::from_utf8_lossy(no));
        }
    }

    #[test]
    fn versions_other_than_two_are_refused() {
        let with = |v: &str, min: Option<&str>| {
            let mut d = Document::default();
            d.asset.version = v.into();
            d.asset.min_version = min.map(str::to_owned);
            check_version(&d)
        };
        assert!(with("2.0", None).is_ok() && with("2.1", Some("2.0")).is_ok());
        assert!(matches!(with("1.0", None), Err(IoError::Unsupported { .. })));
        assert!(matches!(with("2.0", Some("3.0")), Err(IoError::Unsupported { .. })));
        assert!(matches!(with("", None), Err(IoError::Malformed { .. })));
    }

    #[test]
    fn sizes_are_refused_before_the_real_parse() {
        let tight = Limits { max_input_bytes: 100, max_nodes: 2, max_vertices: 10, max_images: 1 };
        let big = json!({ "asset": { "version": "2.0" }, "extras": "x".repeat(200) }).to_string();
        assert!(matches!(parse(big.as_bytes(), &tight), Err(IoError::Limit { what: "input size in bytes", .. })));
        let nodes = json!({ "asset": { "version": "2.0" }, "nodes": [{}, {}, {}] }).to_string();
        assert!(matches!(
            parse(nodes.as_bytes(), &tight),
            Err(IoError::Limit { what: "node count", limit: 2, got: 3 })
        ));
        let images = json!({ "asset": { "version": "2.0" }, "images": [{}, {}] }).to_string();
        assert!(matches!(parse(images.as_bytes(), &tight), Err(IoError::Limit { what: "image count", .. })));
        assert!(matches!(parse(b"{ nope", &Limits::default()), Err(IoError::Malformed { .. })));
    }

    /// A text glTF with `entries` empty objects in `array`.
    fn with_empty(array: &str, entries: u64) -> String {
        let body = vec!["{}"; usize::try_from(entries).unwrap()].join(",");
        format!(r#"{{"asset":{{"version":"2.0"}},"{array}":[{body}]}}"#)
    }

    #[test]
    fn every_top_level_array_is_held_to_its_cap_before_the_real_parse() {
        let limits = Limits { max_nodes: 1000, ..Limits::default() };
        let arrays = [
            ("scenes", "scene count", array_cap::<schema::Scene>(&limits)),
            ("meshes", "mesh count", array_cap::<schema::Mesh>(&limits)),
            ("skins", "skin count", array_cap::<schema::Skin>(&limits)),
            ("materials", "material count", array_cap::<schema::Material>(&limits)),
            ("textures", "texture count", array_cap::<schema::Texture>(&limits)),
            ("samplers", "sampler count", array_cap::<schema::Sampler>(&limits)),
            ("accessors", "accessor count", array_cap::<schema::Accessor>(&limits)),
            ("bufferViews", "buffer view count", array_cap::<schema::BufferView>(&limits)),
            ("buffers", "buffer count", array_cap::<schema::Buffer>(&limits)),
            ("animations", "animation count", array_cap::<serde_json::Value>(&limits)),
            ("cameras", "camera count", array_cap::<serde_json::Value>(&limits)),
        ];
        for (array, name, cap) in arrays {
            assert!(parse(with_empty(array, cap).as_bytes(), &limits).is_ok(), "{array}: {cap} is allowed");
            let Err(IoError::Limit { what, limit, got }) = parse(with_empty(array, cap + 1).as_bytes(), &limits) else {
                panic!("{array}: {} is over the cap", cap + 1)
            };
            assert_eq!((what, limit, got), (name, cap, cap + 1));
        }
    }

    #[test]
    fn the_caps_are_the_memory_of_the_node_limit_with_a_floor() {
        let limits = Limits::default();
        assert_eq!(array_cap::<schema::Node>(&limits), u64::from(limits.max_nodes), "nodes are the unit");
        let heavy = array_cap::<schema::Material>(&limits);
        let light = array_cap::<schema::Texture>(&limits);
        assert!(
            heavy < u64::from(limits.max_nodes) && u64::from(limits.max_nodes) < light,
            "bigger elements, fewer places"
        );
        let (material, node) = (size_of::<schema::Material>() as u64, size_of::<schema::Node>() as u64);
        assert!(
            heavy * material <= u64::from(limits.max_nodes) * node,
            "the memory of an array never passes the nodes'"
        );
        let one = Limits { max_nodes: 1, ..Limits::default() };
        assert_eq!(array_cap::<schema::Material>(&one), MIN_ARRAY_CAP, "a tiny node limit still leaves room to work");
    }

    #[test]
    fn required_extensions_are_refused_unless_modelled_or_supported() {
        let doc = |required: &[&str]| Parsed {
            document: Document {
                extensions_required: required.iter().map(|s| (*s).to_owned()).collect(),
                ..Document::default()
            },
            chunk: None,
        };
        assert!(doc(&["KHR_texture_transform"]).check_required(&[]).is_ok());
        assert!(doc(&["VRMC_vrm"]).check_required(&["VRMC_vrm"]).is_ok(), "the caller extends the list");
        let Err(IoError::Unsupported { what, hint }) =
            doc(&["KHR_texture_transform", "KHR_draco_mesh_compression"]).check_required(&[])
        else {
            panic!("should refuse")
        };
        assert!(what.contains("KHR_draco_mesh_compression") && !what.contains("KHR_texture"));
        assert!(hint.is_some_and(|h| h.contains("gltf-transform")));
        let Err(IoError::Unsupported { hint, .. }) = doc(&["X_other"]).check_required(&[]) else { panic!("refuse") };
        assert!(hint.is_some_and(|h| h.contains("remove the extension")));
    }
}
