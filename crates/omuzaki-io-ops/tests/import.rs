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
use omuzaki_store_api::{Commercial, ContentHash, Source, Store};
use omuzaki_store_mock::MemStore;
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
        Ok(ImportOutcome::new(scene, LicenceEvidence::Imported))
    }
}

register_importer!(ProbeImporter);

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

/// The probe file `count` images (all one image when `distinct` is 1) under non-profit terms.
fn restrictive_probe(count: u8, distinct: u8) -> Vec<u8> {
    let mut bytes = probe(count, distinct);
    bytes.push(NON_PROFIT);
    bytes
}

#[test]
fn an_image_already_stored_under_wider_rights_refuses_the_import() {
    let mut rig = Rig::new();
    // The first model is commercial-ok; the second is non-profit and shares its one image.
    rig.run(&bytes_op(&probe(1, 1))).unwrap();
    let (objects, doc) = (rig.store.len(), rig.doc.root().clone());

    let error = rig.run(&bytes_op(&restrictive_probe(1, 1))).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(error.message.contains("wider rights"), "{}", error.message);
    assert!(error.message.contains("Ok") && error.message.contains("No"), "names both: {}", error.message);
    assert_eq!(rig.store.len(), objects, "nothing of the refused model was stored");
    assert_eq!(rig.doc.root(), &doc);
}

#[test]
fn an_image_already_stored_under_equal_or_stricter_rights_is_fine() {
    let mut rig = Rig::new();
    let (strict, _) = rig.run(&bytes_op(&restrictive_probe(1, 1))).unwrap();
    let image = hash(&strict["asset"]["images"][0]);
    rig.run(&bytes_op(&probe(1, 1))).unwrap();
    assert_eq!(rig.store.provenance(&image).unwrap().commercial, Commercial::No, "the stricter record stays");
    rig.run(&bytes_op(&probe(1, 1))).unwrap();
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
