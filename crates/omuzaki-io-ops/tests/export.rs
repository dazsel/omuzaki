//! `io.export` (#335): the mock character written to a file through the mock exporters and the mock face baker, and
//! read back. The op is planned and worked as the executor runs a job; files go to a temporary project folder.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::disallowed_methods, reason = "the test reads the files the op wrote; the fence is for ops")]

use std::sync::Arc;

use omuzaki_char_mock::mock_character;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{
    ARKIT_52, ExpressionName, ImportCtx, ImportInput, ImportOptions, IoScene, PresetExpression, run_import,
};
use omuzaki_io_mock::MockVrm1Importer;
use omuzaki_io_ops::gather::avatar::LicenceRecord;
use omuzaki_io_ops::ops::export::{Export, ExportResult, PlannedExport};
use omuzaki_io_ops::ops::licence::derive::DerivedLicence;
use omuzaki_io_ops::ops::licence::source::PartSource;
use omuzaki_ops_api::{
    Actor, CancelFlag, ErrorCode, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, Plan, ProjectDir,
    Scope, Services, ServicesBuilder, find,
};
use omuzaki_store_api::{Commercial, Provenance, Source, Store};
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

// The mock face baker registers itself; linking the crate is what puts it in the registry.
use omuzaki_face_mock as _;

struct Signal(CancelFlag);

impl JobSignal for Signal {
    fn progress(&self, _stage: &str, _fraction: f32) {}

    fn is_cancelled(&self) -> bool {
        omuzaki_ops_api::Cancel::is_cancelled(&self.0)
    }

    fn cancel_flag(&self) -> CancelFlag {
        self.0.clone()
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

/// A part's source row: a library item under `licence`.
fn row(licence: &str, commercial: Commercial) -> PartSource {
    PartSource::new(Provenance {
        source: Source::Library { item: format!("{licence}@1") },
        author: Some("Ann".into()),
        licence: licence.into(),
        attribution: None,
        commercial,
        consent: None,
        private_test: false,
    })
}

/// The project: the mock character `mira`, every part from a library item under `licence`, with the licence record
/// derived from them and stored as `licence.set` stores it.
struct Project {
    doc: MemDoc,
    services: Services,
    dir: tempfile::TempDir,
}

impl Project {
    fn new(licence: &str, commercial: Commercial) -> Self {
        let mut root = json!({ "characters": { "mira": serde_json::to_value(mock_character().unwrap()).unwrap() } });
        for part in ["body", "hair", "head", "shirt", "vest"] {
            root["characters"]["mira"]["sources"][part] = serde_json::to_value(row(licence, commercial)).unwrap();
        }
        let mut project = Self {
            doc: MemDoc::from_value(root),
            services: ServicesBuilder::new().insert::<dyn Store>(Arc::new(MemStore::new())).build(),
            dir: tempfile::tempdir().unwrap(),
        };
        let derived: DerivedLicence = answer(project.query("licence.derive", json!({ "character": "mira" })));
        let mut record: LicenceRecord = derived.record;
        record.meta.authors = vec!["Ann".into()];
        let set = project.query_plan("licence.set", json!({ "character": "mira", "record": record })).unwrap();
        project.doc.apply(&set.patch).unwrap();
        project
    }

    fn query_plan(&self, op: &str, args: Value) -> Result<Plan, OpError> {
        let decoded = (find(op).unwrap().decode)(args)?;
        let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
        decoded.plan(&OpCtx::new(&self.doc, &actor, &clock).with_services(&self.services).with_dry_run(true))
    }

    fn query(&self, op: &str, args: Value) -> Plan {
        self.query_plan(op, args).unwrap_or_else(|e| panic!("{op}: {e:?}"))
    }

    /// `plan`, as the executor runs it (a dry run when `dry`), with the person's grants.
    fn plan(&self, op: &Export, dry: bool) -> Result<Plan, OpError> {
        let (actor, clock, grants) = (Actor::Person { name: "tester".into() }, FakeClock::default(), Grants::person());
        let project = ProjectDir::open(self.dir.path()).unwrap();
        let ctx = OpCtx::new(&self.doc, &actor, &clock)
            .with_grants(&grants)
            .with_project_dir(&project)
            .with_services(&self.services)
            .with_dry_run(dry);
        op.plan(&ctx)
    }

    /// The whole job: plan, then work.
    fn export(&self, op: &Export) -> Result<ExportResult, OpError> {
        let plan = self.plan(op, false)?;
        assert!(plan.patch.changes.is_empty(), "an export changes nothing in the document");
        let (actor, grants) = (Actor::Person { name: "tester".into() }, Grants::person());
        let project = ProjectDir::open(self.dir.path()).unwrap();
        let (doc, signal) = (Doc(&self.doc), Signal(CancelFlag::new()));
        let job = JobCtx::new(JobId(3), &actor, &doc, &signal)
            .with_grants(&grants)
            .with_project_dir(&project)
            .with_services(&self.services);
        Ok(serde_json::from_value(op.work(&job)?).unwrap())
    }

    fn file(&self, path: &str) -> Vec<u8> {
        std::fs::read(self.dir.path().join(path)).unwrap()
    }
}

fn answer<T: serde::de::DeserializeOwned>(plan: Plan) -> T {
    serde_json::from_value(plan.output.expect("an answer")).unwrap()
}

fn export(args: Value) -> Export {
    serde_json::from_value(args).unwrap()
}

fn vrm(path: &str, purpose: &str) -> Export {
    export(json!({ "character": "mira", "format": "vrm1", "path": path, "purpose": purpose }))
}

fn read_back(bytes: &[u8]) -> IoScene {
    run_import(&MockVrm1Importer, &ImportInput::new(bytes), &ImportCtx::new(&ImportOptions::default()))
        .expect("the file reads back")
        .scene
}

#[test]
fn a_dry_run_writes_nothing_and_says_what_the_export_will_do() {
    let project = Project::new("CC0-1.0", Commercial::Ok);
    let planned: PlannedExport = answer(project.plan(&vrm("out/mira.vrm", "publish"), true).unwrap());
    assert_eq!(planned.path, "out/mira.vrm");
    assert!(planned.bakes_face, "the mock baker is registered and the mock head has a painted face");
    assert!(!planned.report.has_errors(), "{:#?}", planned.report);
    assert!(!project.dir.path().join("out").exists(), "nothing is written");
}

#[test]
fn the_written_file_reimports_with_the_52_perfect_sync_expressions_and_the_presets() {
    let project = Project::new("CC0-1.0", Commercial::Ok);
    let result = project.export(&vrm("out/mira.vrm", "private")).unwrap();
    assert_eq!(result.files.len(), 1);
    let bytes = project.file("out/mira.vrm");
    assert_eq!(result.files[0].bytes, bytes.len() as u64);
    assert_eq!(result.files[0].hash, omuzaki_store_api::ContentHash::of(&bytes).to_string());
    let bake = result.bake.expect("the face was baked");
    assert_eq!(bake.channels.len(), 52);

    let scene = read_back(&bytes);
    let avatar = scene.avatar.expect("a VRM has an avatar");
    for channel in ARKIT_52 {
        assert!(avatar.expressions.contains_key(&ExpressionName::parse(channel).unwrap()), "{channel}");
    }
    for preset in PresetExpression::ALL {
        assert!(avatar.expressions.contains_key(&ExpressionName::Preset(preset)), "{preset:?}");
    }
    assert_eq!((scene.meta.name.as_str(), scene.meta.authors.as_slice()), ("Mock", ["Ann".to_owned()].as_slice()));
}

#[test]
fn publishing_needs_a_clear_record_and_a_private_file_does_not() {
    let clear = Project::new("CC0-1.0", Commercial::Ok);
    clear.export(&vrm("mira.vrm", "publish")).expect("a clear record publishes");

    let non_commercial = Project::new("CC-BY-NC-4.0", Commercial::No);
    for dry in [true, false] {
        let error = non_commercial.plan(&vrm("mira.vrm", "publish"), dry).unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid, "{error:?}");
        assert!(error.message.contains("rights.gate_non_commercial"), "the gate's rule is named: {error:?}");
        assert!(error.hint.as_deref().is_some_and(|h| h.contains("private")), "{error:?}");
    }
    assert!(!non_commercial.dir.path().join("mira.vrm").exists(), "a refused export writes nothing");
    non_commercial.export(&vrm("mira.vrm", "private")).expect("a private file is the person's own business");
}

#[test]
fn the_same_inputs_give_the_same_bytes() {
    let project = Project::new("CC0-1.0", Commercial::Ok);
    project.export(&vrm("a.vrm", "private")).unwrap();
    project.export(&vrm("b.vrm", "private")).unwrap();
    assert_eq!(project.file("a.vrm"), project.file("b.vrm"));
    // Again over the first: the file is replaced, not appended to.
    project.export(&vrm("a.vrm", "private")).unwrap();
    assert_eq!(project.file("a.vrm"), project.file("b.vrm"));
}

#[test]
fn perfect_sync_off_writes_the_presets_only_and_glb_has_no_avatar() {
    let project = Project::new("CC0-1.0", Commercial::Ok);
    let args = json!({ "character": "mira", "format": "vrm1", "path": "p.vrm", "perfect_sync": "off" });
    let result = project.export(&export(args)).unwrap();
    assert!(result.bake.is_none());
    let avatar = read_back(&project.file("p.vrm")).avatar.unwrap();
    assert!(!avatar.expressions.keys().any(|e| ARKIT_52.contains(&e.as_str())));

    let args = json!({ "character": "mira", "format": "glb", "path": "m.glb", "target_height": 1.2 });
    let result = project.export(&export(args)).unwrap();
    assert!(result.bake.is_none(), "glTF has no expressions to bake into");
    assert!(project.file("m.glb").starts_with(b"glTF"));
}

#[test]
fn refusals_name_the_argument_and_how_to_fix_it() {
    let project = Project::new("CC0-1.0", Commercial::Ok);
    let refused = |args: Value, field: &str| {
        let error = project.plan(&export(args), true).unwrap_err();
        assert_eq!(error.field.as_deref(), Some(field), "{error:?}");
        assert!(error.hint.is_some(), "{error:?}");
    };
    refused(json!({ "character": "mira", "format": "vrm1", "path": "mira.glb" }), "path");
    refused(json!({ "character": "mira", "format": "fbx", "path": "mira.fbx" }), "format");
    refused(json!({ "character": "mira", "format": "vrm1", "path": "m.vrm", "target_height": 0.0 }), "target_height");
    let error = project
        .plan(&export(json!({ "character": "mira", "format": "vrm1", "path": "m.vrm", "variant": "nope" })), true)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "{error:?}");
    let error =
        project.plan(&export(json!({ "character": "nobody", "format": "vrm1", "path": "m.vrm" })), true).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "{error:?}");

    // An agent with no `fs` grant may not write outside the project folder.
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    let grants = Grants::of([Scope::Read, Scope::Publish]);
    let dir = ProjectDir::open(project.dir.path()).unwrap();
    let ctx = OpCtx::new(&project.doc, &actor, &clock).with_grants(&grants).with_project_dir(&dir).with_dry_run(true);
    let error = vrm("m.vrm", "private").plan(&ctx).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied, "{error:?}");
}

#[test]
fn the_op_is_a_registered_publish_job() {
    let entry = find("io.export").expect("registered");
    let meta = (entry.meta)();
    assert_eq!((meta.kind, meta.scope), (omuzaki_ops_api::OpKind::Job, Scope::Publish));
}
