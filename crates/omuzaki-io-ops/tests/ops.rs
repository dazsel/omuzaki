//! `io.list_formats` and `io.inspect` through the op registry, against the generated fixtures of `omuzaki-io-mock`.
//!
//! The ops are decoded from JSON and planned exactly as an executor would (`omuzaki-ops` is an implementation crate, so
//! it is not a dependency here); a query's plan is its answer, so planning is the dry run.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::path::Path;

use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{
    Confidence, ExportCtx, ImportCtx, ImportInput, ImportOptions, Report, SceneSummary, run_export, run_import,
};
use omuzaki_io_mock::fixtures::{glb_from_json, glb_parts, options_without_face_channels, tiny_avatar, tiny_glb};
use omuzaki_io_mock::{MockGlbImporter, MockVrm1Exporter, MockVrm1Importer};
use omuzaki_io_ops::ops::list_formats::Formats;
use omuzaki_ops_api::{
    Actor, ErrorCode, Grants, OpCtx, OpError, OpKind, ProjectDir, Scope, Services, ServicesBuilder, Undo, find,
    registrations,
};
use omuzaki_store_api::{Commercial, ContentHash, Store};
use omuzaki_store_mock::MemStore;
use omuzaki_store_mock::conformance::sample_provenance;
use serde_json::{Value, json};

/// What an `io.inspect` request runs against: who asks, which folder is the project, and the services.
struct Setup<'a> {
    project: Option<&'a Path>,
    grants: Grants,
    services: Services,
}

/// Decodes `args` for the op called `name` and plans it against an empty document, checking that it plans no change.
fn run_with(setup: &Setup<'_>, name: &str, args: Value) -> Result<Value, OpError> {
    let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
    let op = (registration.decode)(args)?;
    let (doc, actor, clock) = (MemDoc::new(), Actor::Person { name: "tester".into() }, FakeClock::default());
    let project = setup.project.map(|dir| ProjectDir::open(dir).expect("open the project folder"));
    let mut ctx = OpCtx::new(&doc, &actor, &clock).with_grants(&setup.grants).with_services(&setup.services);
    if let Some(project) = &project {
        ctx = ctx.with_project_dir(project);
    }
    let plan = op.plan(&ctx)?;
    assert!(plan.patch.changes.is_empty(), "{name} is a query and must not plan a change");
    Ok(plan.output.expect("a query answers with an output"))
}

/// An agent with the default grants (`read` only) and no services.
fn agent_in(project: Option<&Path>) -> Setup<'_> {
    Setup { project, grants: Grants::agent_default(), services: ServicesBuilder::new().build() }
}

fn run(name: &str, args: Value) -> Result<Value, OpError> {
    run_with(&agent_in(None), name, args)
}

/// Inspects `path` as an agent whose project folder is the folder the file is in.
fn inspect(path: &Path) -> Result<Value, OpError> {
    inspect_with(path, json!({}))
}

/// [`inspect`] with more arguments (`format`, `strict`).
fn inspect_with(path: &Path, extra: Value) -> Result<Value, OpError> {
    let mut args = extra;
    args["path"] = json!(path.to_str().unwrap());
    run_with(&agent_in(path.parent()), "io.inspect", args)
}

fn vrm_bytes() -> Vec<u8> {
    let options = options_without_face_channels();
    let outcome = run_export(&MockVrm1Exporter, &tiny_avatar(), &ExportCtx::new(&options)).expect("export the fixture");
    outcome.files.into_iter().next().expect("one file").bytes
}

/// Writes `bytes` to `name` in a fresh folder and returns the folder (kept alive by the caller) and the path.
fn file_with(name: &str, bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    // Canonical, because `fs_access` returns canonical paths and the tests compare against them.
    let path = std::fs::canonicalize(dir.path()).unwrap().join(name);
    std::fs::write(&path, bytes).unwrap();
    (dir, path)
}

fn summary_of(output: &Value) -> SceneSummary {
    serde_json::from_value(output["summary"].clone()).unwrap()
}

fn report_of(output: &Value) -> Report {
    serde_json::from_value(output["report"].clone()).unwrap()
}

#[test]
fn both_ops_register_with_the_scopes_the_design_gives_them() {
    let names: Vec<_> = registrations().map(|r| (r.meta)().name).collect();
    assert!(names.contains(&"io.list_formats") && names.contains(&"io.inspect"), "{names:?}");

    let list = (find("io.list_formats").unwrap().meta)();
    assert_eq!((list.kind, list.scope, list.undo), (OpKind::Query, Scope::Read, Undo::None));
    // `io.inspect` needs `read`; `fs_access` adds `fs` and a root for paths outside the project folder.
    let inspect = (find("io.inspect").unwrap().meta)();
    assert_eq!((inspect.kind, inspect.scope, inspect.undo), (OpKind::Query, Scope::Read, Undo::None));
}

#[test]
fn the_argument_schemas_are_flat_and_closed() {
    let schema = |name: &str| serde_json::to_value((find(name).unwrap().schema)()).unwrap();

    let inspect = schema("io.inspect");
    assert!(inspect.get("required").is_none(), "path and asset are each optional: the op checks that one is given");
    let properties = inspect["properties"].as_object().unwrap();
    assert_eq!(properties.keys().cloned().collect::<Vec<_>>(), ["as", "asset", "format", "path", "strict"]);
    assert_eq!(inspect["additionalProperties"], json!(false));
    assert!(!inspect.to_string().contains("$ref"), "MCP tool schemas are inlined");

    let list = schema("io.list_formats");
    assert_eq!(list["additionalProperties"], json!(false));
    assert!(list["properties"].as_object().is_none_or(serde_json::Map::is_empty));
}

#[test]
fn arguments_that_do_not_fit_the_schema_are_invalid_with_a_hint() {
    // The field is the argument that is wrong: a missing or unknown key names itself.
    for (name, args, field) in [
        ("io.inspect", json!({ "path": 3 }), "path"),
        ("io.inspect", json!({ "asset": 3 }), "asset"),
        ("io.inspect", json!({ "unknown": 1 }), "unknown"),
        ("io.inspect", json!({ "path": "a.glb", "format": "VRM 1" }), "format"),
        ("io.inspect", json!({ "path": "a.glb", "asset": "abc" }), "asset"),
        ("io.list_formats", json!({ "format": "glb" }), "format"),
    ] {
        let Err(error) = (find(name).unwrap().decode)(args.clone()) else { panic!("{name} accepted {args}") };
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some(field)), "{args}");
        assert!(error.hint.is_some());
    }
}

#[test]
fn list_formats_merges_importers_and_exporters() {
    let output = run("io.list_formats", json!({})).unwrap();
    let formats: Formats = serde_json::from_value(output).unwrap();
    assert_eq!(formats, Formats::registered());

    let ids: Vec<&str> = formats.formats.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["glb", "vrm1"], "sorted by id, one entry per format");
    for format in &formats.formats {
        assert!(format.import && format.export, "{} has an importer and an exporter", format.id);
        assert_eq!(format.media_type, "model/gltf-binary");
    }
    assert_eq!(formats.formats[1].extensions, ["vrm"]);
    // The mock's motion format, apart from the models: `io.export_animation` takes these.
    let motions: Vec<(&str, &[String])> =
        formats.animations.iter().map(|f| (f.id.as_str(), f.extensions.as_slice())).collect();
    assert_eq!(motions, [("vrma", &["vrma".to_owned()][..])]);
}

#[test]
fn inspecting_a_glb_gives_the_summary_run_import_gives() {
    let bytes = tiny_glb();
    let (_dir, path) = file_with("triangle.glb", &bytes);
    let output = inspect(&path).unwrap();

    let options = ImportOptions::default();
    let direct =
        run_import(&MockGlbImporter, &ImportInput::new(&bytes).named("triangle.glb"), &ImportCtx::new(&options))
            .unwrap();
    assert_eq!(summary_of(&output), direct.scene.summary());
    assert_eq!(report_of(&output), direct.report);
    assert_eq!(summary_of(&output).triangles, 1);
    assert_eq!(output["format"], "glb");
    assert_eq!(output["file"], "triangle.glb");
    assert_eq!(output["bytes"], bytes.len());
    assert_eq!(output["detected"], json!([{ "format": "glb", "confidence": "possible" }]));
}

#[test]
fn inspecting_a_vrm_gives_the_summary_run_import_gives_and_ranks_the_vrm_importer_first() {
    let bytes = vrm_bytes();
    let (_dir, path) = file_with("tiny.vrm", &bytes);
    let output = inspect(&path).unwrap();

    let options = ImportOptions::default();
    let direct =
        run_import(&MockVrm1Importer, &ImportInput::new(&bytes).named("tiny.vrm"), &ImportCtx::new(&options)).unwrap();
    assert_eq!(summary_of(&output), direct.scene.summary());
    assert_eq!(report_of(&output), direct.report);
    assert_eq!(summary_of(&output).humanoid_bones, 15);
    assert_eq!(output["format"], "vrm1");
    // Every VRM is also a glTF binary: both importers recognise it, the VRM one is certain.
    let certain = serde_json::to_value(Confidence::Certain).unwrap();
    assert_eq!(output["detected"][0], json!({ "format": "vrm1", "confidence": certain }));
    assert_eq!(output["detected"][1]["format"], "glb");
}

#[test]
fn a_named_format_overrides_detection() {
    let (_dir, path) = file_with("tiny.vrm", &vrm_bytes());
    let output = inspect_with(&path, json!({ "format": "glb" })).unwrap();
    assert_eq!(output["format"], "glb");
    assert_eq!(summary_of(&output).humanoid_bones, 0, "read as plain glTF, the avatar block is dropped");
    assert!(report_of(&output).has_rule("gltf.extension_dropped"));
    assert_eq!(output["detected"][0]["format"], "vrm1", "detection is still reported");
}

#[test]
fn strict_turns_warnings_into_a_refusal() {
    // A file with an image: the mock importer drops images and says so with a warning.
    let (mut doc, bin) = glb_parts(&tiny_glb()).unwrap();
    doc["images"] = json!([{ "uri": "data:image/png;base64,AAAA" }]);
    let (_dir, path) = file_with("images.glb", &glb_from_json(&doc, &bin));
    let args = |strict: bool| json!({ "strict": strict });

    let lenient = inspect_with(&path, args(false)).unwrap();
    assert!(report_of(&lenient).has_rule("gltf.images_dropped"));

    let error = inspect_with(&path, args(true)).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("path")));
    assert!(error.message.contains("gltf.images_dropped"), "{}", error.message);
}

#[test]
fn bytes_no_importer_recognises_are_unsupported_with_the_formats_that_would_work() {
    let (_dir, path) = file_with("notes.txt", b"this is not a model file at all");
    let error = inspect(&path).unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert!(error.hint.as_deref().unwrap().contains("importable formats: glb, vrm1"), "{error:?}");
}

#[test]
fn an_empty_file_and_an_empty_path_are_invalid() {
    let (_dir, path) = file_with("empty.glb", b"");
    let error = inspect(&path).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("path")));
    let error = run("io.inspect", json!({ "path": "" })).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("path")));
}

#[test]
fn a_missing_file_is_not_found_and_a_folder_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir(base.join("folder")).unwrap();
    let setup = agent_in(Some(&base));
    let at = |name: &str| run_with(&setup, "io.inspect", json!({ "path": base.join(name).to_str().unwrap() }));
    assert_eq!(at("gone.glb").unwrap_err().code, ErrorCode::NotFound);
    let error = at("folder").unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("path")));
}

/// A store holding `bytes`, as the services of a request, with the hash they are stored under.
fn store_with(bytes: &[u8]) -> (Services, ContentHash) {
    let store = std::sync::Arc::new(MemStore::new());
    let hash = store.put(bytes, sample_provenance(Commercial::Ok)).unwrap();
    (ServicesBuilder::new().insert::<dyn Store>(store).build(), hash)
}

#[test]
fn a_stored_asset_inspects_like_the_same_file() {
    let bytes = vrm_bytes();
    let (_dir, path) = file_with("tiny.vrm", &bytes);
    let from_file = inspect(&path).unwrap();

    let (services, hash) = store_with(&bytes);
    let setup = Setup { services, ..agent_in(None) };
    let from_store = run_with(&setup, "io.inspect", json!({ "asset": hash.to_string() })).unwrap();
    assert_eq!((&from_file["source"], &from_store["source"]), (&json!("path"), &json!("asset")));
    assert_eq!(from_store["file"], json!(hash.to_string()));
    for key in ["bytes", "format", "detected", "summary", "report"] {
        assert_eq!(from_file[key], from_store[key], "{key}");
    }
}

#[test]
fn a_stored_asset_that_is_missing_or_has_no_store_to_come_from_fails_cleanly() {
    let (services, _) = store_with(&tiny_glb());
    let other = ContentHash::of(b"never stored");
    let setup = Setup { services, ..agent_in(None) };
    let error = run_with(&setup, "io.inspect", json!({ "asset": other.to_string() })).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::NotFound, Some("asset")));
    assert!(error.message.contains(&other.to_string()), "{}", error.message);

    let error = run("io.inspect", json!({ "asset": other.to_string() })).unwrap_err();
    assert_eq!(error.code, ErrorCode::Unavailable, "no store is registered");
}

#[test]
fn import_errors_blame_the_argument_the_bytes_came_from() {
    let whole = tiny_glb();
    let (doc, bin) = glb_parts(&whole).unwrap();
    let mut with_image = doc.clone();
    with_image["images"] = json!([{ "uri": "data:image/png;base64,AAAA" }]);
    for (what, bytes, extra, rule) in [
        ("a truncated glb", whole[..whole.len() - 8].to_vec(), json!({ "format": "glb" }), "malformed"),
        ("a strict warning", glb_from_json(&with_image, &bin), json!({ "strict": true }), "gltf.images_dropped"),
    ] {
        let (services, hash) = store_with(&bytes);
        let setup = Setup { services, ..agent_in(None) };
        let mut args = extra;
        args["asset"] = json!(hash.to_string());
        let error = run_with(&setup, "io.inspect", args).unwrap_err();
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("asset")), "{what}: {error:?}");
        assert!(error.message.contains(rule), "{what}: {}", error.message);
    }
}

#[test]
fn exactly_one_of_path_and_asset_is_required() {
    let hash = ContentHash::of(b"x").to_string();
    for args in [json!({}), json!({ "path": "a.glb", "asset": hash })] {
        let error = run("io.inspect", args.clone()).unwrap_err();
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("path")), "{args}");
        assert!(error.message.contains("exactly one of path or asset"), "{}", error.message);
        assert!(error.hint.is_some());
    }
}

#[test]
fn a_path_outside_the_project_needs_fs_and_a_root() {
    let (_dir, outside) = file_with("tiny.glb", &tiny_glb());
    let project = tempfile::tempdir().unwrap();
    let project_path = std::fs::canonicalize(project.path()).unwrap();
    let folder = outside.parent().unwrap().to_str().unwrap().to_owned();
    let args = json!({ "path": outside.to_str().unwrap() });
    let with = |grants: Grants| Setup { grants, ..agent_in(Some(&project_path)) };

    for (who, grants) in [("an agent", Grants::agent_default()), ("a person without a root", Grants::person())] {
        let error = run_with(&with(grants), "io.inspect", args.clone()).unwrap_err();
        assert_eq!(error.code, ErrorCode::Denied, "{who}: {error:?}");
        assert!(error.message.contains(outside.to_str().unwrap()), "{who}: names the path: {}", error.message);
    }
    let error = run_with(&with(Grants::person()), "io.inspect", args.clone()).unwrap_err();
    assert!(error.message.contains("root") || error.hint.as_deref().unwrap_or("").contains("root"), "{error:?}");

    let output = run_with(&with(Grants::person().with_fs_root(folder)), "io.inspect", args).unwrap();
    assert_eq!(output["format"], "glb");
}

#[test]
fn the_project_folder_needs_only_read() {
    let (_dir, path) = file_with("tiny.glb", &tiny_glb());
    let grants = Grants::agent_default();
    assert!(!grants.allows(Scope::Fs), "the default agent has no file grant");
    let setup = Setup { grants, ..agent_in(path.parent()) };
    // Absolute inside the project, and relative to it.
    for given in [path.to_str().unwrap(), "tiny.glb"] {
        let output = run_with(&setup, "io.inspect", json!({ "path": given })).unwrap();
        assert_eq!(output["file"], "tiny.glb", "{given}");
    }
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_project_is_denied() {
    let (_outside_dir, outside) = file_with("tiny.glb", &tiny_glb());
    let project = tempfile::tempdir().unwrap();
    let project_path = std::fs::canonicalize(project.path()).unwrap();
    std::os::unix::fs::symlink(&outside, project_path.join("link.glb")).unwrap();
    let setup = agent_in(Some(&project_path));
    let error = run_with(&setup, "io.inspect", json!({ "path": "link.glb" })).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied, "{error:?}");
}

#[test]
fn a_format_nobody_registered_is_invalid_and_names_the_ones_that_exist() {
    let (_dir, path) = file_with("tiny.glb", &tiny_glb());
    let error = inspect_with(&path, json!({ "format": "fbx" })).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("format")));
    assert!(error.hint.as_deref().unwrap().contains("glb, vrm1"));
}

#[test]
fn truncating_a_file_anywhere_gives_a_structured_error_and_never_a_panic() {
    for (what, bytes) in [("glb", tiny_glb()), ("vrm", vrm_bytes())] {
        let inspect = |bytes: &[u8]| {
            let op: omuzaki_io_ops::ops::inspect::Inspect = serde_json::from_value(json!({ "path": "x" })).unwrap();
            op.inspect_bytes("cut", bytes)
        };
        assert!(inspect(&bytes).is_ok(), "the whole {what} inspects");
        let mut malformed = 0;
        for len in 0..bytes.len() {
            let error = inspect(&bytes[..len]).expect_err(&format!("{what} cut to {len} bytes"));
            assert!(matches!(error.code, ErrorCode::Invalid | ErrorCode::Unsupported), "{what} at {len}: {error:?}");
            malformed += usize::from(error.code == ErrorCode::Invalid);
        }
        assert!(malformed > 0, "a cut file that still starts like a {what} is reported as malformed");
    }
}

#[test]
fn inspecting_changes_nothing_on_disk() {
    let bytes = tiny_glb();
    let (dir, path) = file_with("triangle.glb", &bytes);
    inspect(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no file was added beside it");
}
