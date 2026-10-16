//! A model as an avatar through the ops (#1293, #1333, #868): `io.inspect {as: avatar}` prepares it in memory with
//! the confirm token, `io.import {as: avatar}` checks the token and the acknowledgement and writes the character in
//! one patch, and `avatar.remap`, `avatar.map_channel` and `avatar.describe` work on the stored original. Every file is
//! generated here (the mock VRM 1.0 writer); no third-party model.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{add_skinned_mesh, humanoid, toon_material, vrm_bytes};
use omuzaki_avatar_api::def::AvatarDef;
use omuzaki_char_api::CharId;
use omuzaki_core::glam::Vec4;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{
    AvatarPermission, CommercialUsage, CreditNotation, Expression, ExpressionName, IoScene, Modification, Report,
};
use omuzaki_io_ops::ops::avatar::confirm::{Inputs, token};
use omuzaki_io_ops::ops::avatar::import::{ACKNOWLEDGEMENT_NEEDED, CONFIRM_STALE};
use omuzaki_io_ops::ops::import::Import;
use omuzaki_ops_api::{
    Actor, CancelFlag, ErrorCode, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, ProjectDir, Scope,
    Services, ServicesBuilder, find,
};
use omuzaki_render_api::MtoonSupport;
use omuzaki_store_api::{ContentHash, Store};
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

/// A generated avatar scene: a T-posed humanoid with a skinned body (two morph targets) and two expressions, whose
/// terms let anyone perform as it for profit.
fn model() -> IoScene {
    let mut f = humanoid(0.0);
    f.scene.meta.terms.avatar_permission = AvatarPermission::Everyone;
    f.scene.meta.terms.commercial_usage = CommercialUsage::PersonalProfit;
    f.scene.add_material(toon_material("Skin", Vec4::ONE));
    let joints: Vec<String> = f.nodes.keys().filter(|n| *n != "Root").cloned().collect();
    let joints: Vec<&str> = joints.iter().map(String::as_str).collect();
    let body = add_skinned_mesh(&mut f, "Body", &joints, 2, Some(0));
    let avatar = f.scene.avatar.as_mut().unwrap();
    avatar.expressions.insert(ExpressionName::parse("happy").unwrap(), Expression::morph(body, 0));
    avatar.expressions.insert(ExpressionName::parse("aa").unwrap(), Expression::morph(body, 1));
    f.scene
}

/// What a running job is told: never cancelled unless asked, at one stage.
struct Signal {
    flag: CancelFlag,
    cancel_at: Option<&'static str>,
}

impl JobSignal for Signal {
    fn progress(&self, stage: &str, _fraction: f32) {
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

/// A project: a document, a store, a folder for files, and who acts.
struct Project {
    doc: MemDoc,
    store: Arc<MemStore>,
    services: Services,
    dir: tempfile::TempDir,
    actor: Actor,
    grants: Grants,
}

impl Project {
    fn new() -> Self {
        let store = Arc::new(MemStore::new());
        let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
        Self {
            doc: MemDoc::from_value(json!({ "characters": {} })),
            store,
            services,
            dir: tempfile::tempdir().unwrap(),
            actor: Actor::Person { name: "tester".into() },
            grants: Grants::person(),
        }
    }

    /// Writes `bytes` to `name` in the project folder.
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// Runs `f` with an op context for this project.
    fn with_ctx<T>(&self, f: impl FnOnce(&OpCtx<'_>) -> T) -> T {
        let clock = FakeClock::default();
        let project = ProjectDir::open(self.dir.path()).unwrap();
        let ctx = OpCtx::new(&self.doc, &self.actor, &clock)
            .with_grants(&self.grants)
            .with_project_dir(&project)
            .with_services(&self.services);
        f(&ctx)
    }

    /// Plans the op called `name` with `args`.
    fn plan(&self, name: &str, args: Value) -> Result<omuzaki_ops_api::Plan, OpError> {
        let op = (find(name).unwrap().decode)(args)?;
        self.with_ctx(|ctx| op.plan(ctx))
    }

    /// `io.inspect {path, as: avatar}` plus `extra`.
    fn inspect(&self, path: &str, extra: &Value) -> Result<Value, OpError> {
        let mut args = json!({ "path": path, "as": "avatar" });
        args.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        Ok(self.plan("io.inspect", args)?.output.unwrap())
    }

    /// The whole `io.import` job, cancelled at `cancel_at` if given; the patch is applied as the executor does.
    fn import(&mut self, args: Value, cancel_at: Option<&'static str>) -> Result<(Value, Patch), OpError> {
        let op: Import = serde_json::from_value(args).unwrap();
        let plan = self.with_ctx(|ctx| op.plan(ctx))?;
        assert!(plan.patch.changes.is_empty(), "a job's plan returns no patch");
        let project = ProjectDir::open(self.dir.path()).unwrap();
        let signal = Signal { flag: CancelFlag::new(), cancel_at };
        let doc = Doc(&self.doc);
        let output = plan.output.clone().unwrap_or(Value::Null);
        let job = JobCtx::new(JobId(1), &self.actor, &doc, &signal)
            .with_grants(&self.grants)
            .with_project_dir(&project)
            .with_services(&self.services)
            .with_plan_output(&output);
        let result = op.work(&job)?;
        let plan = self.with_ctx(|ctx| op.finish(ctx, result.clone()))?;
        self.doc.apply(&plan.patch).unwrap();
        Ok((result, plan.patch))
    }

    /// Writes `scene` as a VRM, inspects it and imports it as an avatar with the token; returns the character's id.
    fn imported(&mut self, scene: &IoScene, extra: &Value) -> CharId {
        let path = self.file("Avatar.vrm", &vrm_bytes(scene));
        let path = path.to_str().unwrap();
        let confirm = self.inspect(path, &json!({})).unwrap()["avatar"]["confirm"].clone();
        let mut args = json!({ "path": path, "as": "avatar", "confirm": confirm });
        args.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        let (result, _) = self.import(args, None).unwrap();
        serde_json::from_value(result["id"].clone()).unwrap()
    }

    fn value(&self, path: &str) -> Option<Value> {
        self.doc.get(&DocPath::parse(path).unwrap()).cloned()
    }
}

fn report_of(output: &Value) -> Report {
    serde_json::from_value(output["report"].clone()).unwrap()
}

#[test]
fn inspecting_as_an_avatar_prepares_in_memory_and_touches_nothing() {
    let project = Project::new();
    let path = project.file("Avatar.vrm", &vrm_bytes(&model()));
    let before = project.value("").unwrap();
    let out = project.inspect(path.to_str().unwrap(), &json!({})).unwrap();
    assert_eq!(project.value("").unwrap(), before, "the document is unchanged");
    assert_eq!(project.store.len(), 0, "nothing is stored");
    let summary = &out["avatar"]["summary"];
    assert_eq!(summary["id_suggestion"], "avatar", "from the file's name");
    let stages: Vec<&str> = summary["timings"].as_array().unwrap().iter().map(|t| t[0].as_str().unwrap()).collect();
    assert_eq!(stages, ["read", "prepare", "decode"], "the stages this op runs");
    assert_eq!(summary["texture_bytes"], 0, "the mock writer carries no textures");
    assert_eq!(out["avatar"]["confirm"].as_str().unwrap().len(), 64);
    let report = report_of(&out);
    assert!(report.has_rule("avatar.expression_custom"), "the preparation's findings are in the one report");
}

#[test]
fn the_token_is_stable_and_changes_with_any_option_dependency_or_byte() {
    let project = Project::new();
    let path = project.file("Avatar.vrm", &vrm_bytes(&model()));
    let path = path.to_str().unwrap();
    let inspected = |extra: &Value| project.inspect(path, extra).unwrap()["avatar"]["confirm"].clone();
    let first = inspected(&json!({}));
    assert_eq!(first, inspected(&json!({})), "two runs give one token");
    assert_ne!(first, inspected(&json!({ "strict": true })), "an option changes it");
    let mut moved = model();
    moved.meshes[0].primitives[0].mesh.positions[0].x += 1e-3;
    let other = project.file("Avatar.vrm", &vrm_bytes(&moved));
    assert_ne!(first, project.inspect(other.to_str().unwrap(), &json!({})).unwrap()["avatar"]["confirm"]);

    // A file read beside the model (a `.gltf`'s buffer) changes it too.
    let def = AvatarDef::new(omuzaki_avatar_api::def::AvatarSource::new(
        ContentHash([1; 32]),
        omuzaki_avatar_api::def::SourceFormat::Gltf,
        "a.gltf",
    ));
    let report = Report::new();
    let with = |hash: ContentHash| {
        let dependencies = [("a.bin".to_owned(), hash)];
        token(&Inputs {
            source: ContentHash([1; 32]),
            dependencies: &dependencies,
            def: &def,
            mtoon: MtoonSupport::NONE,
            strict: false,
            importer: "glb",
            report: &report,
            suggestion: "avatar",
        })
        .unwrap()
    };
    assert_ne!(with(ContentHash([2; 32])), with(ContentHash([3; 32])), "one dependency byte");
}

#[test]
fn every_rights_row_reaches_the_report() {
    let project = Project::new();
    let mut scene = model();
    let terms = &mut scene.meta.terms;
    terms.avatar_permission = AvatarPermission::OnlyAuthor;
    terms.commercial_usage = CommercialUsage::PersonalNonProfit;
    terms.credit_notation = CreditNotation::Required;
    terms.allow_excessively_violent_usage = false;
    terms.modification = Modification::Prohibited;
    let path = project.file("Avatar.vrm", &vrm_bytes(&scene));
    let report = report_of(&project.inspect(path.to_str().unwrap(), &json!({})).unwrap());
    for rule in [
        "rights.avatar_permission",
        "rights.perform_non_profit",
        "rights.credit_required",
        "rights.content_restricted",
        "rights.import_restricted",
    ] {
        assert!(report.has_rule(rule), "{rule}: {report:?}");
    }
}

#[test]
fn a_stale_token_is_refused_even_when_the_findings_read_the_same() {
    let mut project = Project::new();
    let path = project.file("Avatar.vrm", &vrm_bytes(&model()));
    let path = path.to_str().unwrap().to_owned();
    let inspected = project.inspect(&path, &json!({})).unwrap();
    let mut moved = model();
    moved.meshes[0].primitives[0].mesh.positions[0].x += 1e-3;
    project.file("Avatar.vrm", &vrm_bytes(&moved));
    let reinspected = project.inspect(&path, &json!({})).unwrap();
    assert_eq!(report_of(&inspected), report_of(&reinspected), "the findings read the same");
    let args = json!({ "path": path, "as": "avatar", "confirm": inspected["avatar"]["confirm"] });
    let error = project.import(args, None).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Conflict, Some("confirm")));
    assert!(error.message.starts_with(CONFIRM_STALE), "{}", error.message);
    assert_eq!(project.store.len(), 0, "nothing stored");
}

#[test]
fn an_import_writes_the_character_in_one_patch_that_one_undo_removes() {
    let mut project = Project::new();
    let before = project.value("").unwrap();
    let path = project.file("Avatar.vrm", &vrm_bytes(&model()));
    let path = path.to_str().unwrap().to_owned();
    let confirm = project.inspect(&path, &json!({})).unwrap()["avatar"]["confirm"].clone();
    let (result, undo) = project.import(json!({ "path": path, "as": "avatar", "confirm": confirm }), None).unwrap();
    assert_eq!(result["id"], "avatar");
    assert_eq!(project.value("characters/avatar/name"), Some(json!("Generated Test Avatar")));
    let def: AvatarDef = serde_json::from_value(project.value("characters/avatar/avatar").unwrap()).unwrap();
    assert_eq!(def.source.file, "Avatar.vrm");
    assert_eq!(def.acknowledged, None, "no acknowledgement was needed");
    let row = project.value("characters/avatar/sources/avatar").unwrap();
    let terms: omuzaki_io_api::UseTerms = serde_json::from_value(row["terms"].clone()).unwrap();
    assert_eq!(terms, model().meta.terms, "the row carries the file's own terms");
    let objects = project.store.len();
    assert!(objects >= 1);

    project.doc.apply(&undo.inverse()).unwrap();
    assert_eq!(project.value("").unwrap(), before, "one undo removes the character, its node and its row");

    let (again, _) =
        project.import(json!({ "path": path, "as": "avatar", "confirm": confirm, "id": "again" }), None).unwrap();
    assert_eq!(again["id"], "again");
    assert_eq!(project.store.len(), objects, "the same bytes are stored once");
}

#[test]
fn only_author_terms_need_a_persons_acknowledgement() {
    let mut scene = model();
    scene.meta.terms.avatar_permission = AvatarPermission::OnlyAuthor;
    let mut project = Project::new();
    let path = project.file("Avatar.vrm", &vrm_bytes(&scene));
    let path = path.to_str().unwrap().to_owned();
    let confirm = project.inspect(&path, &json!({})).unwrap()["avatar"]["confirm"].clone();
    let args = json!({ "path": path, "as": "avatar", "confirm": confirm });
    let error = project.import(args.clone(), None).unwrap_err();
    assert_eq!(error.field.as_deref(), Some("acknowledge"));
    assert!(error.message.starts_with(ACKNOWLEDGEMENT_NEEDED), "{}", error.message);
    assert_eq!(project.store.len(), 0);

    project.actor = Actor::Agent { id: "agent".into(), model: None };
    project.grants = Grants::agent_default();
    let mut acknowledged = args.clone();
    acknowledged["acknowledge"] = json!(true);
    let error = project.import(acknowledged.clone(), None).unwrap_err();
    assert!(
        matches!(error.detail.as_deref(), Some(omuzaki_ops_api::ErrorDetail::NeedsPerson { .. })),
        "an agent can't acknowledge for the person: {error:?}"
    );

    project.actor = Actor::Person { name: "tester".into() };
    project.grants = Grants::person();
    project.import(acknowledged, None).unwrap();
    let def: AvatarDef = serde_json::from_value(project.value("characters/avatar/avatar").unwrap()).unwrap();
    let ack = def.acknowledged.expect("recorded");
    assert_eq!(ack.by, "tester");
    assert_eq!(ack.perform.len(), 10, "a date: {}", ack.perform);

    let mut scene = model();
    scene.meta.terms.commercial_usage = CommercialUsage::PersonalNonProfit;
    let mut project = Project::new();
    project.imported(&scene, &json!({}));
    assert!(project.value("characters/avatar").is_some(), "non-profit terms warn and need nothing");
}

#[test]
fn a_cancelled_import_stores_and_writes_nothing() {
    let mut project = Project::new();
    let before = project.value("").unwrap();
    let path = project.file("Avatar.vrm", &vrm_bytes(&model()));
    let path = path.to_str().unwrap().to_owned();
    let confirm = project.inspect(&path, &json!({})).unwrap()["avatar"]["confirm"].clone();
    let error =
        project.import(json!({ "path": path, "as": "avatar", "confirm": confirm }), Some("storing")).unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
    assert_eq!(project.store.len(), 0);
    assert_eq!(project.value("").unwrap(), before);
}

#[test]
fn a_remap_that_breaks_the_parent_rule_is_refused_and_a_valid_one_undoes_in_one_step() {
    let mut project = Project::new();
    let scene = model();
    let id = project.imported(&scene, &json!({}));
    let node = |name: &str| scene.nodes.iter().position(|n| n.name == name).unwrap();
    let before = project.value("").unwrap();

    let error =
        project.plan("avatar.remap", json!({ "character": id, "bone": "leftHand", "node": node("head") })).unwrap_err();
    assert!(error.message.contains("humanoid.override_invalid"), "{}", error.message);
    assert_eq!(error.field.as_deref(), Some("node"));

    let plan =
        project.plan("avatar.remap", json!({ "character": id, "bone": "leftHand", "node": node("leftHand") })).unwrap();
    project.doc.apply(&plan.patch).unwrap();
    let def: AvatarDef = serde_json::from_value(project.value("characters/avatar/avatar").unwrap()).unwrap();
    assert_eq!(def.humanoid.values().next().map(|c| c.name.as_str()), Some("leftHand"));
    project.doc.apply(&plan.patch.inverse()).unwrap();
    assert_eq!(project.value("").unwrap(), before);

    let error = project.plan("avatar.remap", json!({ "character": id, "bone": "leftHnad", "node": 1 })).unwrap_err();
    assert_eq!(error.hint.as_deref(), Some("did you mean leftHand?"));
}

#[test]
fn a_channel_map_undoes_in_one_step_and_an_unknown_expression_names_the_nearest() {
    let mut project = Project::new();
    let id = project.imported(&model(), &json!({}));
    let before = project.value("").unwrap();
    let plan = project
        .plan("avatar.map_channel", json!({ "character": id, "channel": "cheekPuff", "expression": "happy" }))
        .unwrap();
    assert!(!plan.output.as_ref().unwrap()["driven"].is_null(), "the new driven channels come back");
    project.doc.apply(&plan.patch).unwrap();
    assert_eq!(project.value("characters/avatar/avatar/channels/cheekPuff"), Some(json!("happy")));
    project.doc.apply(&plan.patch.inverse()).unwrap();
    assert_eq!(project.value("").unwrap(), before);

    let error = project
        .plan("avatar.map_channel", json!({ "character": id, "channel": "cheekPuff", "expression": "hapy" }))
        .unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("expression")));
    assert_eq!(error.hint.as_deref(), Some("did you mean happy?"));
    assert_eq!(project.value("").unwrap(), before, "a refused map changes nothing");
}

#[test]
fn describe_gives_the_imports_report_and_what_the_model_drives() {
    let mut project = Project::new();
    let path = project.file("Avatar.vrm", &vrm_bytes(&model()));
    let path = path.to_str().unwrap().to_owned();
    let inspected = project.inspect(&path, &json!({})).unwrap();
    project.import(json!({ "path": path, "as": "avatar", "confirm": inspected["avatar"]["confirm"] }), None).unwrap();
    let described = project.plan("avatar.describe", json!({ "character": "avatar" })).unwrap();
    let described = described.output.unwrap();
    assert_eq!(report_of(&described), report_of(&inspected), "an unedited avatar describes as it imported");
    assert_eq!(described["driven"], inspected["avatar"]["summary"]["driven"]);
}

#[test]
fn the_edits_need_edit_character_and_describe_needs_read() {
    for (name, scope) in [
        ("avatar.remap", Scope::EditCharacter),
        ("avatar.map_channel", Scope::EditCharacter),
        ("avatar.describe", Scope::Read),
    ] {
        let registration = find(name).unwrap();
        assert_eq!((registration.meta)().scope, scope, "{name}");
    }
    assert!(Grants::person().allows(Scope::EditCharacter), "a person may edit");
    assert!(Grants::agent_default().allows(Scope::EditCharacter), "an agent with edit:character may");
    assert!(!Grants::of([Scope::Read]).allows(Scope::EditCharacter), "an agent without it may not");
}

/// Prints the evidence #2087 asks for: a generated VRM's report in its four sections, with each stage's time on the
/// real clock. The model is the original test head (`tests/common/head.rs`) written by the mock VRM 1.0 writer.
#[test]
#[ignore = "prints evidence for the pull request; run by hand with --nocapture"]
fn evidence_report() {
    use omuzaki_io_ops::report_sections::{Section, section};
    let mut scene = common::head::head().scene;
    scene.meta.terms.avatar_permission = AvatarPermission::Everyone;
    scene.meta.terms.credit_notation = CreditNotation::Required;
    let project = Project::new();
    let path = project.file("Head.vrm", &vrm_bytes(&scene));
    let clock = omuzaki_core::time::SystemClock::new();
    let op = (find("io.inspect").unwrap().decode)(json!({ "path": path.to_str().unwrap(), "as": "avatar" })).unwrap();
    let dir = ProjectDir::open(project.dir.path()).unwrap();
    let ctx = OpCtx::new(&project.doc, &project.actor, &clock)
        .with_grants(&project.grants)
        .with_project_dir(&dir)
        .with_services(&project.services);
    let out = op.plan(&ctx).unwrap().output.unwrap();
    let report = report_of(&out);
    for wanted in [Section::Mapped, Section::MadeForYou, Section::NotUsed, Section::Rights] {
        let rows: Vec<String> = report
            .findings
            .iter()
            .filter(|f| section(&f.rule) == wanted)
            .map(|f| format!("  - {} ({}): {}", f.rule, f.location.as_deref().unwrap_or("-"), f.message))
            .collect();
        println!("{wanted:?}: {} findings\n{}", rows.len(), rows.join("\n"));
    }
    println!("summary: {}", serde_json::to_string_pretty(&out["avatar"]["summary"]).unwrap());
}

#[test]
fn a_strict_inspection_confirms_a_strict_import() {
    let mut project = Project::new();
    let path = project.file("Avatar.vrm", &vrm_bytes(&model()));
    let path = path.to_str().unwrap().to_owned();
    let confirm = project.inspect(&path, &json!({ "strict": true })).unwrap()["avatar"]["confirm"].clone();
    let lax = project.import(json!({ "path": path, "as": "avatar", "confirm": confirm }), None).unwrap_err();
    assert!(lax.message.starts_with(CONFIRM_STALE), "the options differ: {}", lax.message);
    project.import(json!({ "path": path, "as": "avatar", "confirm": confirm, "strict": true }), None).unwrap();
}

#[test]
fn the_token_covers_the_character_id_the_person_saw() {
    let mut project = Project::new();
    let path = project.file("Avatar.vrm", &vrm_bytes(&model()));
    let path = path.to_str().unwrap().to_owned();
    let confirm = project.inspect(&path, &json!({})).unwrap()["avatar"]["confirm"].clone();
    // Someone makes a character called `avatar` before the import runs: the id the person saw is gone.
    let taken = DocPath::parse("characters/avatar").unwrap();
    project.doc.apply(&Patch::new().with(taken, None, Some(json!({ "name": "Other" })))).unwrap();
    let error = project.import(json!({ "path": path, "as": "avatar", "confirm": confirm }), None).unwrap_err();
    assert!(error.message.starts_with(CONFIRM_STALE), "{}", error.message);
}

#[test]
fn an_acknowledgement_needs_a_named_person() {
    let mut scene = model();
    scene.meta.terms.avatar_permission = AvatarPermission::OnlyAuthor;
    let mut project = Project::new();
    project.actor = Actor::Person { name: "  ".into() };
    let path = project.file("Avatar.vrm", &vrm_bytes(&scene));
    let path = path.to_str().unwrap().to_owned();
    let confirm = project.inspect(&path, &json!({})).unwrap()["avatar"]["confirm"].clone();
    let args = json!({ "path": path, "as": "avatar", "confirm": confirm, "acknowledge": true });
    let error = project.import(args, None).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("acknowledge")));
}

#[test]
fn the_first_character_of_a_new_project_is_an_imported_avatar() {
    let mut project = Project::new();
    project.doc = MemDoc::from_value(json!({}));
    let id = project.imported(&model(), &json!({}));
    assert!(project.value(&format!("characters/{id}/avatar")).is_some(), "the character table is made with it");
}
