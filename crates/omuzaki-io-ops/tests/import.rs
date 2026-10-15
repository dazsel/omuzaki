//! `io.import` as a job: the three calls (`plan`, `work`, `finish`) driven the way the job manager drives them, against
//! `omuzaki-store-mock`. The mock formats carry no images, so this binary registers a probe importer that does.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::sync::{Arc, Mutex};

use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{
    Blob, CommercialUsage, Confidence, EncodedImage, ExportCtx, FormatInfo, ImageMime, ImportCtx, ImportInput,
    ImportOptions, ImportOutcome, Importer, IoError, IoScene, LicenceEvidence, Limits, Node, VrmMeta,
    register_importer, run_export,
};
use omuzaki_io_mock::MockVrm1Exporter;
use omuzaki_io_mock::fixtures::{glb_from_json, glb_parts, options_without_face_channels, tiny_avatar, tiny_glb};
use omuzaki_io_ops::ops::import::{Import, ImportAs};
use omuzaki_ops_api::{
    Actor, CancelFlag, ErrorCode, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, ProjectDir, Services,
    ServicesBuilder,
};
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store, StoreError};
use omuzaki_store_mock::MemStore;
use omuzaki_store_mock::conformance::sample_provenance;
use serde_json::{Value, json};

/// A file that starts `PROBE`, then the number of images, then how many of them are distinct. Its meta is a VRM 1.0
/// licence page with `PersonalProfit` terms, and it leaves the provenance for `run_import` to derive.
#[derive(Default)]
struct ProbeImporter;

impl Importer for ProbeImporter {
    fn info(&self) -> FormatInfo {
        FormatInfo { id: "probe", name: "Probe", extensions: &["probe"], media_type: "application/x-probe" }
    }

    fn sniff(&self, input: &ImportInput<'_>) -> Confidence {
        if input.bytes.starts_with(b"PROBE") { Confidence::Certain } else { Confidence::No }
    }

    fn import(&self, input: &ImportInput<'_>, ctx: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
        ctx.progress.checkpoint("probing", 0.0)?;
        if input.bytes[5] == SIDECAR {
            ctx.resolver.read("side.bin")?;
        }
        let (count, distinct) = (usize::from(input.bytes[5]), usize::from(input.bytes[6]).max(1));
        let mut scene = IoScene::new();
        let root = scene.add_node(Node::new("root"));
        scene.roots = vec![root];
        for ix in 0..count {
            let tag = u8::try_from(ix % distinct).unwrap();
            scene.add_image(EncodedImage::new(format!("img{ix}"), ImageMime::Png, Blob::new(vec![0x89, b'P', tag])));
        }
        let mut meta = VrmMeta::new("Probe", vec!["Probe Author".into()], omuzaki_io_api::VRM_LICENSE_URLS[0]);
        meta.terms.commercial_usage = if input.bytes.get(7) == Some(&NON_PROFIT) {
            CommercialUsage::PersonalNonProfit
        } else {
            CommercialUsage::PersonalProfit
        };
        scene.meta = meta;
        // Bytes after the flags say `licence|credit`: the importer sets the record itself, as a real one may.
        if let Some(text) = input.bytes.get(8..).filter(|text| !text.is_empty()) {
            let text = String::from_utf8_lossy(text);
            let (licence, credit) = text.split_once('|').unwrap_or((&text, ""));
            let mut record = scene.meta.imported_provenance(Some(input.name.unwrap_or("model")));
            licence.clone_into(&mut record.licence);
            record.attribution = (!credit.is_empty()).then(|| credit.to_owned());
            scene.provenance = Some(record);
        }
        Ok(ImportOutcome::new(scene, LicenceEvidence::Imported))
    }
}

register_importer!(ProbeImporter);

/// Reads a JSON model the way a `.gltf` importer does: the first buffer names the root node and the first image is the
/// picture, each from a `data:` URI (the text after the colon is the content) or through the resolver.
#[derive(Default)]
struct GltfProbe;

impl GltfProbe {
    fn load(ctx: &ImportCtx<'_>, uri: &str) -> Result<Vec<u8>, IoError> {
        match uri.strip_prefix("data:") {
            Some(content) => Ok(content.as_bytes().to_vec()),
            None => ctx.resolver.read(uri),
        }
    }
}

impl Importer for GltfProbe {
    fn info(&self) -> FormatInfo {
        FormatInfo { id: "gltf-probe", name: "Probe glTF", extensions: &["gltf"], media_type: "model/gltf+json" }
    }

    fn sniff(&self, input: &ImportInput<'_>) -> Confidence {
        if input.bytes.starts_with(b"{") { Confidence::Certain } else { Confidence::No }
    }

    fn import(&self, input: &ImportInput<'_>, ctx: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
        let json: Value = serde_json::from_slice(input.bytes).map_err(|e| IoError::malformed(e.to_string()))?;
        let mut scene = IoScene::new();
        let buffer = Self::load(ctx, json["buffers"][0]["uri"].as_str().unwrap_or("data:"))?;
        let root = scene.add_node(Node::new(String::from_utf8_lossy(&buffer)));
        scene.roots = vec![root];
        if let Some(uri) = json["images"][0]["uri"].as_str() {
            scene.add_image(EncodedImage::new("img", ImageMime::Png, Blob::new(Self::load(ctx, uri)?)));
        }
        Ok(ImportOutcome::new(scene, LicenceEvidence::Absent))
    }
}

register_importer!(GltfProbe);

/// The image count that makes the probe read a file beside it, as a `.gltf` with a `.bin` does.
const SIDECAR: u8 = 255;

/// The byte that makes the probe's terms personal non-profit, so its record is `commercial: no`.
const NON_PROFIT: u8 = 1;

/// A probe file with `count` images of which `distinct` differ.
fn probe(count: u8, distinct: u8) -> Vec<u8> {
    let mut bytes = b"PROBE".to_vec();
    bytes.extend([count, distinct]);
    bytes
}

/// What the job manager gives a running job: the document to read, a cancel flag, and the stages reported.
struct Signal {
    flag: CancelFlag,
    stages: Mutex<Vec<String>>,
    /// Cancel when this stage is reported, as a person pressing cancel during it would.
    cancel_at: Option<&'static str>,
}

impl Signal {
    fn new(cancel_at: Option<&'static str>) -> Self {
        Self { flag: CancelFlag::new(), stages: Mutex::default(), cancel_at }
    }

    fn stages(&self) -> Vec<String> {
        self.stages.lock().unwrap().clone()
    }
}

impl JobSignal for Signal {
    fn progress(&self, stage: &str, _fraction: f32) {
        self.stages.lock().unwrap().push(stage.to_owned());
        if self.cancel_at == Some(stage) {
            self.flag.cancel();
        }
    }

    fn is_cancelled(&self) -> bool {
        omuzaki_ops_api::Cancel::is_cancelled(&self.flag)
    }

    fn cancel_flag(&self) -> CancelFlag {
        self.flag.clone()
    }

    fn spend(&self, _usd_micros: u64) -> Result<(), OpError> {
        Ok(())
    }
}

struct Doc<'a>(&'a MemDoc);

impl JobDoc for Doc<'_> {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(self.0);
    }
}

/// One project: a document, a store, and the actor's access.
struct Rig {
    doc: MemDoc,
    store: Arc<MemStore>,
    services: Services,
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let store = Arc::new(MemStore::new());
        let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
        let doc = MemDoc::from_value(json!({ "characters": { "mira": {} } }));
        Self { doc, store, services, dir: tempfile::tempdir().unwrap() }
    }

    fn op(args: Value) -> Import {
        serde_json::from_value(args).expect("decode io.import")
    }

    /// `plan`, as the executor runs it under the lock.
    fn plan(&self, op: &Import) -> Result<(), OpError> {
        let (actor, clock, grants) = (Actor::Person { name: "tester".into() }, FakeClock::default(), Grants::person());
        let project = ProjectDir::open(self.dir.path()).unwrap();
        let ctx = OpCtx::new(&self.doc, &actor, &clock)
            .with_grants(&grants)
            .with_project_dir(&project)
            .with_services(&self.services);
        let plan = op.plan(&ctx)?;
        assert!(plan.patch.changes.is_empty(), "a job's plan returns no patch");
        Ok(())
    }

    /// `work`, with `signal`.
    fn work(&self, op: &Import, signal: &Signal) -> Result<Value, OpError> {
        let (actor, grants) = (Actor::Person { name: "tester".into() }, Grants::person());
        let project = ProjectDir::open(self.dir.path()).unwrap();
        let doc = Doc(&self.doc);
        let job = JobCtx::new(JobId(7), &actor, &doc, signal)
            .with_grants(&grants)
            .with_project_dir(&project)
            .with_services(&self.services);
        op.work(&job)
    }

    /// `finish`, then the patch applied to the document, as the executor does.
    fn finish(&mut self, op: &Import, result: Value) -> Result<Patch, OpError> {
        let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
        let ctx = OpCtx::new(&self.doc, &actor, &clock).with_services(&self.services);
        let plan = op.finish(&ctx, result)?;
        self.doc.apply(&plan.patch).expect("the patch applies");
        Ok(plan.patch)
    }

    /// The whole job: plan, work, finish.
    fn run(&mut self, op: &Import) -> Result<(Value, Patch), OpError> {
        self.plan(op)?;
        let result = self.work(op, &Signal::new(None))?;
        let patch = self.finish(op, result.clone())?;
        Ok((result, patch))
    }
}

fn bytes_op(bytes: &[u8]) -> Import {
    Rig::op(json!({ "bytes": bytes, "name": "model.bin" }))
}

fn doc_path(text: &str) -> DocPath {
    DocPath::parse(text).expect("a document path")
}

fn hash(value: &Value) -> ContentHash {
    value.as_str().expect("a hash is a string").parse().expect("a content hash")
}

/// A VRM 1.0 file whose meta says `url` and `usage`, from the mock exporter. The exporter refuses a blank or foreign
/// licence page, so the page is exported valid and then rewritten in the file.
fn vrm_with(url: &str, usage: CommercialUsage, credit: bool) -> Vec<u8> {
    let mut scene = tiny_avatar();
    scene.meta.terms.commercial_usage = usage;
    if credit {
        scene.meta.terms.credit_notation = omuzaki_io_api::CreditNotation::Required;
    }
    scene.provenance = Some(scene.meta.imported_provenance(Some("source.vrm")));
    let options = options_without_face_channels();
    let outcome = run_export(&MockVrm1Exporter, &scene, &ExportCtx::new(&options)).expect("export the fixture");
    let exported = outcome.files.into_iter().next().expect("one file").bytes;
    let (mut json, bin) = glb_parts(&exported).expect("a glb");
    json["extensions"]["VRMC_vrm"]["meta"]["licenseUrl"] = json!(url);
    glb_from_json(&json, &bin)
}

#[test]
fn the_original_and_its_images_are_stored_once_with_the_files_rights() {
    let mut rig = Rig::new();
    let file = probe(3, 2);
    let (result, patch) = rig.run(&bytes_op(&file)).unwrap();

    let original = hash(&result["asset"]["original"]);
    assert_eq!(original, ContentHash::of(&file));
    assert_eq!(rig.store.get(&original).unwrap(), file, "the original is stored unchanged");
    let images = result["asset"]["images"].as_array().unwrap();
    assert_eq!(images.len(), 2, "three images, two of them identical: stored once");
    assert_eq!(rig.store.len(), 1 + 2, "exactly one original and its distinct images");

    let record = rig.store.provenance(&original).unwrap();
    assert_eq!((record.commercial, record.licence.as_str()), (Commercial::Ok, "LicenseRef-VRM-Public-License-1.0"));
    assert_eq!(record.author.as_deref(), Some("Probe Author"));
    for image in images {
        let image = rig.store.provenance(&hash(image)).unwrap();
        assert_eq!((image.commercial, image.licence.as_str()), (record.commercial, record.licence.as_str()));
        assert!(matches!(image.source, Source::Imported { ref file, .. } if file.starts_with("model.bin#image")));
    }
    assert_eq!(result["summary"]["images"], 3);

    let entry = rig.doc.get(&doc_path(&format!("assets/{original}"))).expect("the asset is recorded").clone();
    assert_eq!(entry["original"], original.to_string());
    assert_eq!(entry["images"].as_array().unwrap().len(), 2);
    assert_eq!((entry["file"].as_str(), entry["format"].as_str()), (Some("model.bin"), Some("probe")));
    assert_eq!(patch.changes.len(), 1, "the document changes once");
}

/// One row of io-api D5, by the file's licence page and commercial terms.
struct Row {
    url: &'static str,
    usage: CommercialUsage,
    licence: &'static str,
    commercial: Commercial,
}

#[test]
fn commercial_follows_the_rights_table_row_by_row() {
    let page = omuzaki_io_api::VRM_LICENSE_URLS[0];
    let rows = [
        Row {
            url: page,
            usage: CommercialUsage::PersonalNonProfit,
            licence: "LicenseRef-VRM-Public-License-1.0",
            commercial: Commercial::No,
        },
        Row {
            url: page,
            usage: CommercialUsage::PersonalProfit,
            licence: "LicenseRef-VRM-Public-License-1.0",
            commercial: Commercial::Ok,
        },
        Row {
            url: page,
            usage: CommercialUsage::Corporation,
            licence: "LicenseRef-VRM-Public-License-1.0",
            commercial: Commercial::Ok,
        },
        Row {
            url: "https://example.org/terms",
            usage: CommercialUsage::Corporation,
            licence: "LicenseRef-VRM-Other",
            commercial: Commercial::Unknown,
        },
        Row {
            url: "",
            usage: CommercialUsage::Corporation,
            licence: "LicenseRef-Unknown",
            commercial: Commercial::Unknown,
        },
    ];
    for row in rows {
        let mut rig = Rig::new();
        let file = vrm_with(row.url, row.usage, false);
        let (result, _) = rig.run(&Rig::op(json!({ "bytes": file, "name": "hero.vrm" }))).unwrap();
        let record = rig.store.provenance(&hash(&result["asset"]["original"])).unwrap();
        assert_eq!(
            (record.licence.as_str(), record.commercial),
            (row.licence, row.commercial),
            "licence page {:?}, usage {:?}",
            row.url,
            row.usage
        );
        assert_eq!(result["asset"]["format"], "vrm1");
        assert!(!record.private_test && record.consent.is_none(), "only a person marks private material");
    }
}

#[test]
fn a_plain_glb_has_no_licence_and_so_an_unknown_commercial_status() {
    let mut rig = Rig::new();
    let (result, _) = rig.run(&bytes_op(&tiny_glb())).unwrap();
    let record = rig.store.provenance(&hash(&result["asset"]["original"])).unwrap();
    assert_eq!((record.licence.as_str(), record.commercial), ("LicenseRef-Unknown", Commercial::Unknown));
    assert_eq!(result["asset"]["format"], "glb");
}

#[test]
fn required_credit_travels_with_the_stored_record() {
    let mut rig = Rig::new();
    let file = vrm_with(omuzaki_io_api::VRM_LICENSE_URLS[0], CommercialUsage::PersonalProfit, true);
    let (result, _) = rig.run(&bytes_op(&file)).unwrap();
    let record = rig.store.provenance(&hash(&result["asset"]["original"])).unwrap();
    assert!(record.attribution.as_deref().is_some_and(|a| a.contains("Test Author")), "{:?}", record.attribution);
    assert_eq!(record.author.as_deref(), Some("Test Author"));
}

#[test]
fn importing_the_same_bytes_again_adds_nothing() {
    let mut rig = Rig::new();
    let op = bytes_op(&probe(2, 2));
    let (first, _) = rig.run(&op).unwrap();
    let (objects, doc) = (rig.store.len(), rig.doc.root().clone());

    let (second, patch) = rig.run(&op).unwrap();
    assert_eq!(second["asset"], first["asset"], "the same hashes");
    assert_eq!(rig.store.len(), objects, "content addressed: nothing new is stored");
    assert!(patch.changes.is_empty(), "the asset is already recorded");
    assert_eq!(rig.doc.root(), &doc);
}

#[test]
fn a_cancel_before_storing_leaves_no_objects_and_no_record() {
    let rig = Rig::new();
    let signal = Signal::new(Some("storing"));
    let error = rig.work(&bytes_op(&probe(3, 3)), &signal).unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
    assert!(signal.stages().contains(&"storing".to_owned()), "the cancel came at the last checkpoint");
    assert!(rig.store.is_empty(), "no partial objects");
}

#[test]
fn a_cancel_during_the_import_leaves_no_objects() {
    let rig = Rig::new();
    let signal = Signal::new(Some("probing"));
    let error = rig.work(&bytes_op(&probe(3, 3)), &signal).unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
    assert!(rig.store.is_empty());
    assert!(rig.doc.get(&doc_path("assets")).is_none());
}

#[test]
fn a_cancel_before_anything_is_read_stops_at_once() {
    let rig = Rig::new();
    let signal = Signal::new(Some("reading"));
    let error = rig.work(&bytes_op(&probe(1, 1)), &signal).unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
    assert!(rig.store.is_empty());
}

#[test]
fn progress_is_reported_through_the_job() {
    let rig = Rig::new();
    let signal = Signal::new(None);
    rig.work(&bytes_op(&probe(1, 1)), &signal).unwrap();
    assert_eq!(signal.stages(), ["reading", "probing", "storing", "done"]);
}

#[test]
fn a_file_over_the_limits_is_rejected_before_anything_is_stored() {
    let store = MemStore::new();
    let file = probe(3, 3);
    let op = bytes_op(&file);
    let hostile = [
        ("image count", Limits { max_images: 2, ..Limits::default() }),
        ("node count", Limits { max_nodes: 0, ..Limits::default() }),
        ("input size in bytes", Limits { max_input_bytes: 4, ..Limits::default() }),
    ];
    for (what, limits) in hostile {
        let options = ImportOptions::default().with_limits(limits);
        let error = op.import_bytes("model.bin", &file, &options, &store, &omuzaki_io_api::NoProgress).unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid, "{what}");
        assert!(error.message.contains(what), "{what}: {}", error.message);
    }
    assert!(store.is_empty());
}

#[test]
fn a_malformed_file_is_invalid_and_stores_nothing() {
    let rig = Rig::new();
    let mut file = tiny_glb();
    file.truncate(40);
    let error = rig.work(&bytes_op(&file), &Signal::new(None)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(rig.store.is_empty());
}

#[test]
fn a_file_no_importer_recognises_is_unsupported() {
    let rig = Rig::new();
    let error = rig.work(&bytes_op(b"not a model"), &Signal::new(None)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
}

#[test]
fn plan_refuses_what_the_job_could_not_do() {
    let rig = Rig::new();
    let refused = |args: Value| rig.plan(&Rig::op(args)).unwrap_err();

    let part = refused(json!({ "bytes": [1], "as": "part" }));
    assert_eq!(part.code, ErrorCode::Unsupported);
    assert_eq!(refused(json!({})).code, ErrorCode::Invalid, "neither path nor bytes");
    assert_eq!(refused(json!({ "path": "a.glb", "bytes": [1] })).code, ErrorCode::Invalid, "both");
    assert_eq!(refused(json!({ "bytes": [] })).field.as_deref(), Some("bytes"));
    assert_eq!(refused(json!({ "path": "" })).field.as_deref(), Some("path"));
    let format = refused(json!({ "bytes": [1], "format": "nope" }));
    assert_eq!((format.code, format.field.as_deref()), (ErrorCode::Invalid, Some("format")));
    let character = refused(json!({ "bytes": [1], "character": "ghost" }));
    assert_eq!((character.code, character.field.as_deref()), (ErrorCode::NotFound, None));
    assert_eq!(refused(json!({ "path": "missing.glb" })).code, ErrorCode::NotFound);
    assert!(serde_json::from_value::<Import>(json!({ "bytes": [1], "extra": 1 })).is_err(), "unknown field");
    rig.plan(&Rig::op(json!({ "bytes": [1], "as": "reference", "character": "mira" }))).unwrap();
    assert_eq!(Import { as_: ImportAs::default(), ..bytes_op(&[1]) }.as_, ImportAs::Reference);
}

#[test]
fn plan_needs_the_store() {
    let mut rig = Rig::new();
    rig.services = ServicesBuilder::new().build();
    assert_eq!(rig.plan(&bytes_op(&[1])).unwrap_err().code, ErrorCode::Unavailable);
}

#[test]
fn a_file_is_read_through_the_jobs_file_access() {
    let mut rig = Rig::new();
    std::fs::write(rig.dir.path().join("hero.vrm"), vrm_with("", CommercialUsage::Corporation, false)).unwrap();
    let (result, _) = rig.run(&Rig::op(json!({ "path": "hero.vrm", "character": "mira" }))).unwrap();
    assert_eq!(result["asset"]["file"], "hero.vrm", "the name defaults to the file's");
    assert_eq!(result["asset"]["character"], "mira");
    let original = hash(&result["asset"]["original"]);
    let entry = rig.doc.get(&doc_path(&format!("assets/{original}"))).unwrap();
    assert_eq!(entry["character"], "mira");

    let outside = rig.work(&Rig::op(json!({ "path": "/etc/hostname" })), &Signal::new(None)).unwrap_err();
    assert_eq!(outside.code, ErrorCode::Denied, "a path outside the project folder needs the fs grant");
}

#[test]
fn finish_refuses_a_character_removed_while_the_job_ran() {
    let mut rig = Rig::new();
    let op = Rig::op(json!({ "bytes": probe(1, 1), "character": "mira" }));
    rig.plan(&op).unwrap();
    let result = rig.work(&op, &Signal::new(None)).unwrap();
    rig.doc = MemDoc::from_value(json!({ "characters": {} }));
    assert_eq!(rig.finish(&op, result).unwrap_err().code, ErrorCode::NotFound);
}

#[test]
fn the_op_is_registered_as_a_job_that_needs_edit_character() {
    let meta = Import::meta();
    assert_eq!(meta.name, "io.import");
    assert_eq!((meta.kind, meta.scope), (omuzaki_ops_api::OpKind::Job, omuzaki_ops_api::Scope::EditCharacter));
    assert!(omuzaki_ops_api::find("io.import").is_some());
}

#[test]
fn a_model_that_refers_to_other_files_is_unsupported_and_says_to_use_a_glb() {
    let rig = Rig::new();
    let error = rig.work(&bytes_op(&probe(SIDECAR, 1)), &Signal::new(None)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported, "{}", error.message);
    assert!(error.message.contains("side.bin"), "{}", error.message);
    assert!(error.hint.as_deref().is_some_and(|h| h.contains(".glb")), "{:?}", error.hint);
    assert!(rig.store.is_empty());
}

/// The probe file with `count` images (all one image when `distinct` is 1) under non-profit terms.
fn restrictive_probe(count: u8, distinct: u8) -> Vec<u8> {
    let mut bytes = probe(count, distinct);
    bytes.push(NON_PROFIT);
    bytes
}

/// A probe file whose importer sets the record: `licence` and a credit line (empty for none), on commercial terms.
fn licensed_probe(count: u8, distinct: u8, licence: &str, credit: &str) -> Vec<u8> {
    let mut bytes = probe(count, distinct);
    bytes.push(0);
    bytes.extend(format!("{licence}|{credit}").bytes());
    bytes
}

/// Imports `file` into `rig` and returns the refusal, checking that it stored nothing and changed no document.
fn refused_without_a_trace(rig: &mut Rig, file: &[u8]) -> OpError {
    let (objects, doc) = (rig.store.len(), rig.doc.root().clone());
    let error = rig.run(&bytes_op(file)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict, "{}", error.message);
    assert_eq!(rig.store.len(), objects, "nothing of the refused model was stored");
    assert_eq!(rig.doc.root(), &doc);
    error
}

#[test]
fn an_image_stored_under_a_different_commercial_status_refuses_the_import_both_ways() {
    for (first, second) in [(probe(1, 1), restrictive_probe(1, 1)), (restrictive_probe(1, 1), probe(1, 1))] {
        let mut rig = Rig::new();
        rig.run(&bytes_op(&first)).unwrap();
        let error = refused_without_a_trace(&mut rig, &second);
        assert!(error.message.contains("different record"), "{}", error.message);
        assert!(error.message.contains("Ok") && error.message.contains("No"), "names both: {}", error.message);
        assert!(error.hint.as_deref().is_some_and(|h| h.contains("#1773")));
    }
}

#[test]
fn the_same_tuple_under_another_licence_or_credit_line_is_refused() {
    let cases = [
        (licensed_probe(1, 1, "CC-BY-4.0", ""), licensed_probe(1, 1, "CC-BY-SA-4.0", "")),
        (licensed_probe(1, 1, "CC-BY-4.0", "Credit A"), licensed_probe(1, 1, "CC-BY-4.0", "Credit B")),
        (licensed_probe(1, 1, "CC-BY-4.0", "Credit A"), licensed_probe(1, 1, "CC-BY-4.0", "")),
    ];
    for (first, second) in cases {
        let mut rig = Rig::new();
        rig.run(&bytes_op(&first)).unwrap();
        refused_without_a_trace(&mut rig, &second);
    }
}

#[test]
fn models_with_an_identical_record_share_an_image() {
    let mut rig = Rig::new();
    let (one, _) = rig.run(&bytes_op(&probe(1, 1))).unwrap();
    let (two, _) = rig.run(&bytes_op(&probe(2, 1))).unwrap();
    assert_eq!(one["asset"]["images"], two["asset"]["images"], "one shared image");
    assert_eq!(rig.store.len(), 3, "two originals and the one image");
}

/// A store that, on its first `put`, lets another import win the race: the object is stored first under `other`.
struct LosesTheRace {
    inner: Arc<MemStore>,
    other: Provenance,
}

impl Store for LosesTheRace {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        if !self.inner.contains(&ContentHash::of(bytes)) {
            self.inner.put(bytes, self.other.clone())?;
        }
        self.inner.put(bytes, provenance)
    }

    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        self.inner.get(hash)
    }

    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        self.inner.provenance(hash)
    }

    fn contains(&self, hash: &ContentHash) -> bool {
        self.inner.contains(hash)
    }
}

#[test]
fn a_record_that_changes_between_the_check_and_the_put_fails_the_import() {
    let mut rig = Rig::new();
    let racer = LosesTheRace { inner: rig.store.clone(), other: sample_provenance(Commercial::Ok) };
    rig.services = ServicesBuilder::new().insert::<dyn Store>(Arc::new(racer)).build();
    let op = bytes_op(&probe(1, 1));
    rig.plan(&op).unwrap();

    let error = rig.work(&op, &Signal::new(None)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict, "{}", error.message);
    assert!(rig.doc.get(&doc_path("assets")).is_none(), "no asset record: `work` failed, so `finish` never runs");
    let file = probe(1, 1);
    assert_eq!(rig.store.get(&ContentHash::of(&file)).unwrap(), file, "what was put is whole");
}
/// What a TOML-backed project can store: no `null` anywhere.
fn assert_storable(value: &Value) {
    toml::Value::try_from(value).unwrap_or_else(|e| panic!("TOML cannot hold {value}: {e}"));
}

#[test]
fn what_the_import_writes_is_storable_in_a_toml_project() {
    assert!(toml::Value::try_from(json!({ "a": null })).is_err(), "the check bites on a null");
    let mut rig = Rig::new();
    for args in [json!({ "bytes": probe(2, 2) }), json!({ "bytes": probe(0, 1), "character": "mira" })] {
        let op = Rig::op(args);
        rig.plan(&op).unwrap();
        let result = rig.work(&op, &Signal::new(None)).unwrap();
        assert_storable(&result);
        let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
        let ctx = OpCtx::new(&rig.doc, &actor, &clock).with_services(&rig.services);
        let plan = op.finish(&ctx, result).unwrap();
        assert_eq!(plan.patch.changes.len(), 1, "one change records the asset");
        for change in &plan.patch.changes {
            change.before.iter().chain(&change.after).for_each(assert_storable);
        }
        rig.doc.apply(&plan.patch).unwrap();
    }
}

/// A store that flags the job cancelled when its first `put` begins, as a person pressing cancel mid-write would.
struct CancelOnPut {
    inner: Arc<MemStore>,
    flag: CancelFlag,
}

impl Store for CancelOnPut {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        self.flag.cancel();
        self.inner.put(bytes, provenance)
    }

    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        self.inner.get(hash)
    }

    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        self.inner.provenance(hash)
    }

    fn contains(&self, hash: &ContentHash) -> bool {
        self.inner.contains(hash)
    }
}

#[test]
fn a_cancel_during_a_put_leaves_only_whole_objects_and_no_record() {
    let mut rig = Rig::new();
    let signal = Signal::new(None);
    let slow = CancelOnPut { inner: rig.store.clone(), flag: signal.flag.clone() };
    rig.services = ServicesBuilder::new().insert::<dyn Store>(Arc::new(slow)).build();
    let file = probe(2, 2);
    let op = bytes_op(&file);
    rig.plan(&op).unwrap();

    // The manager applies `finish` for work that returns `Ok`, even after a cancel request, so `work` must not.
    let error = rig.work(&op, &signal).unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);

    assert!(omuzaki_ops_api::Cancel::is_cancelled(&signal.flag));
    let expected = [file.clone(), vec![0x89, b'P', 0], vec![0x89, b'P', 1]];
    let present: Vec<&Vec<u8>> = expected.iter().filter(|bytes| rig.store.contains(&ContentHash::of(bytes))).collect();
    assert_eq!(rig.store.len(), present.len(), "nothing else is in the store");
    for bytes in present {
        assert_eq!(&rig.store.get(&ContentHash::of(bytes)).unwrap(), bytes, "every object present is whole");
    }
    assert!(rig.doc.get(&doc_path("assets")).is_none(), "no asset record unless `finish` is applied");
}

#[test]
fn errors_blame_the_argument_the_file_came_from() {
    let rig = Rig::new();
    let mut malformed = tiny_glb();
    malformed.truncate(40);

    let from_bytes = rig.work(&bytes_op(&malformed), &Signal::new(None)).unwrap_err();
    assert_eq!((from_bytes.code, from_bytes.field.as_deref()), (ErrorCode::Invalid, Some("bytes")));

    std::fs::write(rig.dir.path().join("broken.glb"), &malformed).unwrap();
    let from_path = rig.work(&Rig::op(json!({ "path": "broken.glb" })), &Signal::new(None)).unwrap_err();
    assert_eq!((from_path.code, from_path.field.as_deref()), (ErrorCode::Invalid, Some("path")));

    let file = probe(1, 1);
    let options = ImportOptions::default().with_limits(Limits { max_input_bytes: 4, ..Limits::default() });
    let over = bytes_op(&file).import_bytes("m.bin", &file, &options, &MemStore::new(), &omuzaki_io_api::NoProgress);
    assert_eq!(over.unwrap_err().field.as_deref(), Some("bytes"));
}

#[test]
fn a_conflict_names_the_consent_of_both_records() {
    let mut rig = Rig::new();
    rig.run(&bytes_op(&probe(1, 1))).unwrap();
    let error = refused_without_a_trace(&mut rig, &restrictive_probe(1, 1));
    assert_eq!(error.message.matches("consent None").count(), 2, "{}", error.message);
}

/// A rig whose document has the characters `mira` and `kai`.
fn rig_with_two_characters() -> Rig {
    let mut rig = Rig::new();
    rig.doc = MemDoc::from_value(json!({ "characters": { "mira": {}, "kai": {} } }));
    rig
}

fn import_for(rig: &mut Rig, character: Option<&str>) -> Result<(Value, Patch), OpError> {
    let mut args = json!({ "bytes": probe(1, 1), "name": "model.bin" });
    if let Some(character) = character {
        args["character"] = json!(character);
    }
    rig.run(&Rig::op(args))
}

fn recorded_character(rig: &Rig, result: &Value) -> Option<String> {
    let path = doc_path(&format!("assets/{}", hash(&result["asset"]["original"])));
    rig.doc.get(&path).and_then(|entry| entry["character"].as_str()).map(str::to_owned)
}

#[test]
fn a_reimport_without_a_character_keeps_the_recorded_association() {
    let mut rig = rig_with_two_characters();
    let (first, _) = import_for(&mut rig, Some("mira")).unwrap();
    let before = rig.doc.root().clone();
    let (_, patch) = import_for(&mut rig, None).unwrap();
    assert_eq!(patch, Patch::new(), "nothing is overwritten");
    assert_eq!(rig.doc.root(), &before);
    assert_eq!(recorded_character(&rig, &first).as_deref(), Some("mira"));
}

#[test]
fn a_reimport_with_the_same_character_is_a_no_op() {
    let mut rig = rig_with_two_characters();
    import_for(&mut rig, Some("mira")).unwrap();
    let before = rig.doc.root().clone();
    let (_, patch) = import_for(&mut rig, Some("mira")).unwrap();
    assert_eq!(patch, Patch::new());
    assert_eq!(rig.doc.root(), &before);
}

#[test]
fn a_reimport_with_another_character_is_a_conflict_naming_the_existing_one() {
    let mut rig = rig_with_two_characters();
    let (first, _) = import_for(&mut rig, Some("mira")).unwrap();
    let before = rig.doc.root().clone();
    let error = import_for(&mut rig, Some("kai")).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(error.message.contains("mira") && error.message.contains("kai"), "{}", error.message);
    assert_eq!(rig.doc.root(), &before, "the record is unchanged");
    assert_eq!(recorded_character(&rig, &first).as_deref(), Some("mira"));
}

#[test]
fn a_character_is_added_to_a_record_that_has_none() {
    let mut rig = rig_with_two_characters();
    let (first, _) = import_for(&mut rig, None).unwrap();
    assert_eq!(recorded_character(&rig, &first), None);
    import_for(&mut rig, Some("kai")).unwrap();
    assert_eq!(recorded_character(&rig, &first).as_deref(), Some("kai"));
}

/// Writes `file` under the rig's project folder, creating its folders.
fn write_project_file(rig: &Rig, file: &str, bytes: &[u8]) {
    let path = rig.dir.path().join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn a_gltf_with_a_bin_and_a_png_imports_like_the_self_contained_one() {
    let mut rig = Rig::new();
    write_project_file(&rig, "models/hero.bin", b"buf");
    write_project_file(&rig, "models/tex/hero.png", b"PNGDATA");
    let external = json!({ "buffers": [{ "uri": "hero.bin" }], "images": [{ "uri": "tex/hero.png" }] });
    let inline = json!({ "buffers": [{ "uri": "data:buf" }], "images": [{ "uri": "data:PNGDATA" }] });
    write_project_file(&rig, "models/hero.gltf", external.to_string().as_bytes());
    write_project_file(&rig, "models/inline.gltf", inline.to_string().as_bytes());

    let (split, _) = rig.run(&Rig::op(json!({ "path": "models/hero.gltf", "name": "hero.gltf" }))).unwrap();
    let (joined, _) = rig.run(&Rig::op(json!({ "path": "models/inline.gltf", "name": "hero.gltf" }))).unwrap();
    assert_eq!(split["asset"]["images"].as_array().unwrap().len(), 1);
    assert_eq!(split["asset"]["images"], joined["asset"]["images"], "the same picture is the same object");
    assert_eq!(split["summary"], joined["summary"]);
    assert_eq!(split["report"], joined["report"]);
    assert_eq!(split["provenance"], joined["provenance"], "the same record");
    assert_eq!(rig.store.get(&hash(&split["asset"]["images"][0])).unwrap(), b"PNGDATA");

    // Nothing the import used is lost: both files are stored and the asset names them. The picture is the same object
    // as the scene's image, so it is stored once.
    let sidecars = split["asset"]["sidecars"].as_array().unwrap();
    let uris: Vec<&str> = sidecars.iter().map(|s| s["uri"].as_str().unwrap()).collect();
    assert_eq!(uris, ["hero.bin", "tex/hero.png"]);
    assert_eq!(rig.store.get(&hash(&sidecars[0]["hash"])).unwrap(), b"buf");
    assert_eq!(sidecars[1]["hash"], split["asset"]["images"][0]);
    assert!(joined["asset"].get("sidecars").is_none(), "a self-contained model has none");
    let record = rig.store.provenance(&hash(&sidecars[0]["hash"])).unwrap();
    assert_eq!(record.licence, split_original_record(&rig, &split).licence);
}

/// The record the original was stored with.
fn split_original_record(rig: &Rig, result: &Value) -> Provenance {
    rig.store.provenance(&hash(&result["asset"]["original"])).unwrap()
}

#[test]
fn a_sidecar_outside_the_files_folder_is_refused_and_nothing_is_stored() {
    let mut rig = Rig::new();
    write_project_file(&rig, "x.bin", b"outside the folder");
    write_project_file(&rig, "models/hero.gltf", json!({ "buffers": [{ "uri": "../x.bin" }] }).to_string().as_bytes());
    let error = rig.run(&Rig::op(json!({ "path": "models/hero.gltf" }))).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "{}", error.message);
    assert!(error.message.contains("refused"), "{}", error.message);
    assert!(rig.store.is_empty());
}

#[test]
fn a_missing_sidecar_is_not_found_and_nothing_is_stored() {
    let mut rig = Rig::new();
    write_project_file(&rig, "models/hero.gltf", json!({ "buffers": [{ "uri": "gone.bin" }] }).to_string().as_bytes());
    let error = rig.run(&Rig::op(json!({ "path": "models/hero.gltf" }))).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "{}", error.message);
    assert!(rig.store.is_empty());
}

#[test]
fn a_reimport_after_a_sidecar_changed_is_a_conflict_and_an_unchanged_one_is_not() {
    let mut rig = Rig::new();
    write_project_file(&rig, "hero.bin", b"v1");
    write_project_file(&rig, "hero.gltf", json!({ "buffers": [{ "uri": "hero.bin" }] }).to_string().as_bytes());
    let op = Rig::op(json!({ "path": "hero.gltf" }));
    rig.run(&op).unwrap();
    let (_, patch) = rig.run(&op).unwrap();
    assert!(patch.changes.is_empty(), "the same files again change nothing");

    write_project_file(&rig, "hero.bin", b"v2");
    let error = rig.run(&op).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict, "{}", error.message);
    assert!(error.message.contains("sidecars"), "{}", error.message);
}
