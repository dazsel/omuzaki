//! `io.export` of the kit character (#335): `character.create` makes it, `licence.derive` and `licence.set` record its
//! rights, and `io.export` writes it as VRM 1.0 through the real exporter; the file reads back through the real VRM
//! importer and becomes an avatar the stage loads, as a person's own model would. The face is baked by the mock baker
//! (every channel inert): the real one is the Face area's (#172, #173).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "the test reads the file the op wrote; the fence is for ops")]

use std::sync::Arc;

use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_char::ops::character::create::CharacterCreate;
use omuzaki_char_api::{BuildServices, CharId, Quality as BuildQuality, RegistryBuilder};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{ARKIT_52, ImportCtx, ImportInput, ImportOptions, PresetExpression, run_import};
use omuzaki_io_ops::ops::export::{Export, ExportResult};
use omuzaki_io_vrm::Vrm1Importer;
use omuzaki_ops_api::{
    Actor, CancelFlag, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, ProjectDir, ServicesBuilder, find,
};
use omuzaki_render_api::MtoonSupport;
use omuzaki_stage::body::{Body, BodyServices, load_body};
use omuzaki_store_api::{Commercial, Provenance, Source, Store};
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

// The mock face baker registers itself; linking the crate is what puts it in the registry.
use omuzaki_face_mock as _;

struct Signal(CancelFlag);

impl JobSignal for Signal {
    fn progress(&self, _stage: &str, _fraction: f32) {}

    fn is_cancelled(&self) -> bool {
        false
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

fn plan(doc: &MemDoc, op: &str, args: Value) -> omuzaki_ops_api::Plan {
    let (actor, clock) = (Actor::Person { name: "test".into() }, FakeClock::default());
    let decoded = (find(op).unwrap().decode)(args).unwrap();
    decoded.plan(&OpCtx::new(doc, &actor, &clock)).unwrap_or_else(|e| panic!("{op}: {e:?}"))
}

/// A project with the kit character (`character.create`, template `standard`, seed 7) and its licence record, with
/// `look` as its look node if given.
fn kit_project(look: Option<Value>) -> MemDoc {
    let (actor, clock) = (Actor::Person { name: "test".into() }, FakeClock::default());
    let mut doc = MemDoc::from_value(json!({}));
    let create: CharacterCreate =
        serde_json::from_value(json!({ "id": "kit", "name": "Kit", "template": "standard", "seed": 7 })).unwrap();
    doc.apply(&create.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap().patch).unwrap();
    // The kit's generators are the project's own, so the derived record is complete; the authors are the person's.
    let mut record = plan(&doc, "licence.derive", json!({ "character": "kit" })).output.unwrap()["record"].clone();
    record["meta"]["authors"] = json!(["Omuzaki"]);
    doc.apply(&plan(&doc, "licence.set", json!({ "character": "kit", "record": record })).patch).unwrap();
    let Some(look) = look else { return doc };
    let mut root = doc.root().clone();
    root["characters"]["kit"]["look"] = look;
    MemDoc::from_value(root)
}

/// `io.export {character: kit, format: vrm1, purpose: publish}` planned and worked as the executor runs a job, into
/// `dir`; the result and the file's bytes.
fn export_kit(doc: &MemDoc, store: &Arc<MemStore>, dir: &std::path::Path) -> (ExportResult, Vec<u8>) {
    let (actor, clock) = (Actor::Person { name: "test".into() }, FakeClock::default());
    let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
    let project = ProjectDir::open(dir).unwrap();
    let grants = Grants::person();
    let args = json!({ "character": "kit", "format": "vrm1", "path": "exports/kit.vrm", "purpose": "publish" });
    let op: Export = serde_json::from_value(args).unwrap();
    let ctx = OpCtx::new(doc, &actor, &clock).with_grants(&grants).with_project_dir(&project).with_services(&services);
    op.plan(&ctx).unwrap_or_else(|e| panic!("{e:?}"));
    let (job_doc, signal) = (Doc(doc), Signal(CancelFlag::new()));
    let job = JobCtx::new(JobId(1), &actor, &job_doc, &signal)
        .with_grants(&grants)
        .with_project_dir(&project)
        .with_services(&services);
    let result: ExportResult = serde_json::from_value(op.work(&job).unwrap_or_else(|e| panic!("{e:?}"))).unwrap();
    let bytes = std::fs::read(dir.join("exports/kit.vrm")).unwrap();
    (result, bytes)
}

fn read_vrm(bytes: &[u8]) -> omuzaki_io_api::IoScene {
    run_import(&Vrm1Importer, &ImportInput::new(bytes), &ImportCtx::new(&ImportOptions::default()))
        .expect("the real importer reads the real exporter's file")
        .scene
}

#[test]
fn the_kit_character_exports_to_vrm_and_comes_back_as_an_avatar() {
    let clock = FakeClock::default();
    let doc = kit_project(None);
    let store = Arc::new(MemStore::new());
    let dir = tempfile::tempdir().unwrap();
    let (result, bytes) = export_kit(&doc, &store, dir.path());
    println!("{} bytes", result.files[0].bytes);
    for f in result.report.findings.iter().filter(|f| f.rule != "perfect_sync.inert_channel") {
        println!("{:?} {} {:?}: {}", f.severity, f.rule, f.location, f.message);
    }
    let inert = result.report.findings.iter().filter(|f| f.rule == "perfect_sync.inert_channel").count();
    println!("inert channels reported: {inert}");
    assert!(!result.report.has_errors());
    // The 52 channels, then the face's own named expressions (#771), each exported under `omuzaki_<id>`.
    let baked: Vec<&str> = result.bake.as_ref().unwrap().channels.iter().map(|c| c.channel.as_str()).collect();
    assert_eq!(baked[..52], ARKIT_52);
    assert!(baked[52..].iter().all(|n| n.starts_with("omuzaki_")), "{:?}", &baked[52..]);

    let scene = read_vrm(&bytes);
    let avatar = scene.avatar.as_ref().unwrap();
    assert!(avatar.humanoid.bones.len() >= 15, "the humanoid: {} bones", avatar.humanoid.bones.len());
    for preset in PresetExpression::ALL {
        assert!(avatar.expressions.keys().any(|e| e.as_str() == preset.as_str()), "{preset:?}");
    }
    let channels = ARKIT_52.iter().filter(|c| avatar.expressions.keys().any(|e| e.as_str() == **c)).count();
    assert_eq!(channels, 52);
    for named in &baked[52..] {
        assert!(avatar.expressions.keys().any(|e| e.as_str() == *named), "{named} is in the file");
    }
    assert_eq!(scene.meta.name, "Kit");

    // The file is a model a person could drop on the studio: it loads as an avatar and frames.
    let provenance = Provenance {
        source: Source::Authored,
        author: Some("Omuzaki".into()),
        licence: "LicenseRef-Omuzaki-User".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    };
    let hash = store.put(&bytes, provenance).unwrap();
    let def = AvatarDef::new(AvatarSource::new(hash, SourceFormat::Vrm1, "kit.vrm"));
    let mut root = doc.root().clone();
    root["characters"]["kit_vrm"] = json!({ "avatar": serde_json::to_value(&def).unwrap() });
    let doc = MemDoc::from_value(root);
    let build = BuildServices::default();
    let body_services = BodyServices::new(&RegistryBuilder, &build, store.as_ref(), MtoonSupport::NONE, &clock);
    let body = load_body(&doc, &body_services, &CharId::new("kit_vrm").unwrap(), None, BuildQuality::Live).unwrap();
    assert!(matches!(body, Body::Avatar(_)));
    let landmarks = body.landmarks().unwrap();
    assert!(landmarks.head.is_some() && (1.0..2.2).contains(&landmarks.height_m), "{landmarks:?}");
}

/// #713: the file carries the character's look. A look whose terminator is hard (`softness_scale` 0) exports every
/// toon material with `MToon`'s `shadingToony` 1; without a look node the export is what it always was.
#[test]
fn an_export_carries_the_characters_look() {
    let store = Arc::new(MemStore::new());
    let dir = tempfile::tempdir().unwrap();
    let toony = |bytes: &[u8]| -> Vec<f32> {
        read_vrm(bytes)
            .materials
            .iter()
            .filter_map(|m| match &m.shading {
                omuzaki_io_api::Shading::Toon(toon) => Some(toon.shading_toony),
                _ => None,
            })
            .collect()
    };
    let (_, plain) = export_kit(&kit_project(None), &store, dir.path());
    let (_, again) = export_kit(&kit_project(None), &store, dir.path());
    assert_eq!(plain, again, "the same project gives the same bytes");
    let hard = kit_project(Some(json!({ "overrides": { "all": { "softness_scale": 0.0 } } })));
    let (_, looked) = export_kit(&hard, &store, dir.path());
    let (before, after) = (toony(&plain), toony(&looked));
    println!("shadingToony without the look {before:?}, with it {after:?}");
    assert!(!after.is_empty() && after.iter().all(|t| (*t - 1.0).abs() < 1e-6), "{after:?}");
    assert!(before.iter().any(|t| *t < 1.0), "the parts' own look is softer: {before:?}");
}
