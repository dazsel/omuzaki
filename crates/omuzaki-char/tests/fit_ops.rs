//! `fit.reference` and `fit.apply` against the in-memory document, run the way the executor runs a job: `plan` under
//! the lock, `work` off it, `finish` under it again. The tests check that both are jobs (no fit in `plan`), that a
//! dry run fits nothing and nothing writes the asset store, that the fit starts from the stored body (so a second fit
//! starts where the first ended and an apply never makes the outline cost more), that the apply is one undoable step
//! touching only `proportions` and `shape` and is refused if the body changed meanwhile, that a cancel stops the
//! fit, that bad input is refused with typed errors, and that the ops recover the solver's seeded test characters
//! within the solver's own tolerances.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use common::{assert_error, at, build, mock_node};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use omuzaki_char::fit::solver::{FitInput, PARAM_ORDER, Param, iou_of};
use omuzaki_char::fit::{FitParams, FitView, Silhouette, Target, render_target};
use omuzaki_core::mesh::MeshData;
use omuzaki_core::rng::Rng;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{Change, DocPath, DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, Cancel, CancelFlag, ErrorCode, JobCtx, JobDoc, JobId, JobSignal, OpCtx, OpError, OpKind, Plan, Scope,
    Services, ServicesBuilder, Undo, find,
};
use omuzaki_store_api::{ContentHash, Provenance, Store, StoreError};
use serde_json::{Value, json};

/// A store that counts what is put in it (neither op may put anything) and what is read from it (planning, which runs
/// under the document lock, may read nothing).
#[derive(Default)]
struct MemStore {
    held: Mutex<HashMap<ContentHash, (Vec<u8>, Provenance)>>,
    reads: AtomicUsize,
}

impl Store for MemStore {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        let hash = ContentHash::of(bytes);
        self.held.lock().unwrap().entry(hash).or_insert_with(|| (bytes.to_vec(), provenance));
        Ok(hash)
    }
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.held.lock().unwrap().get(hash).map(|(b, _)| b.clone()).ok_or(StoreError::Missing(*hash))
    }
    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.held.lock().unwrap().get(hash).map(|(_, p)| p.clone()).ok_or(StoreError::Missing(*hash))
    }
    fn contains(&self, hash: &ContentHash) -> bool {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.held.lock().unwrap().contains_key(hash)
    }
}

const PARAMS: &str = "characters/mock/parts/body/params";
/// Rows of a synthetic view: small, so the fit is quick in a debug build.
const ROWS: usize = 48;
/// Rows of the evidence heatmaps.
const SHEET_ROWS: usize = 96;
/// How far the centre-sampled `IoU` of a body that already matches may move down while its cost goes down: under a
/// pixel's worth of outline on 112-row views.
const MATCHING_SLACK: f64 = 0.01;
/// The base64 of the PNG signature's first eight bytes: every heatmap starts with it.
const PNG_BASE64: &str = "iVBORw0KGgo";

/// The solver's recovery test (`tests/fit.rs`, #1958): its seeded draw, view height, views and tolerances.
const SEED: u64 = 2;
const SPREAD: f64 = 0.25;
const HEIGHT: usize = 384;
const VIEWS: [FitView; 3] = [FitView::Front, FitView::Side, FitView::Back];
const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

/// The mock character on `template` with a real body alone (stored at `params`, if given), so the fit draws nothing
/// else.
fn project_on(template: &str, params: Option<&FitParams>) -> MemDoc {
    with_parts(template, params, json!({}))
}

/// The mock character on `template` with a real body (stored at `params`, if given) and the `others` parts beside it.
fn with_parts(template: &str, params: Option<&FitParams>, others: Value) -> MemDoc {
    let mut node = mock_node();
    node["template"] = json!(template);
    node["parts"] = others;
    node["parts"]["body"] = json!({ "kind": "body" });
    if let Some(p) = params {
        node["parts"]["body"]["params"] = json!({ "proportions": p.proportions, "shape": p.shape });
    }
    MemDoc::from_value(json!({ "characters": { "mock": node } }))
}

/// A real head and hair beside the body, built by their own kinds.
fn head_and_hair() -> Value {
    json!({ "head": { "kind": "head" }, "hair": { "kind": "hair" } })
}

/// `mask` as a PNG: an opaque black figure on a transparent ground.
fn png_of(mask: &Silhouette) -> Vec<u8> {
    let data: Vec<u8> =
        mask.pixels().iter().flat_map(|&on| if on { [0, 0, 0, 255] } else { [255, 255, 255, 0] }).collect();
    let (w, h) = (u32::try_from(mask.width()).unwrap(), u32::try_from(mask.height()).unwrap());
    let mut out = Vec::new();
    PngEncoder::new(&mut out).write_image(&data, w, h, ExtendedColorType::Rgba8).unwrap();
    out
}

fn project() -> MemDoc {
    project_on("soft-sd", None)
}

/// A known body: longer legs, broader shoulders, a broader build.
fn truth() -> FitParams {
    let mut p = FitParams::default();
    p.proportions.leg_length = 1.25;
    p.proportions.shoulder_width = 1.3;
    p.shape.build = 0.4;
    p
}

fn rows_of(template: &str, truth: &FitParams, view: FitView, rows: usize) -> Vec<String> {
    let m = render_target(template, truth, &[], view, rows).unwrap();
    m.pixels().chunks(m.width()).map(|r| r.iter().map(|&on| if on { '#' } else { '.' }).collect()).collect()
}

fn view_name(view: FitView) -> &'static str {
    match view {
        FitView::Front => "front",
        FitView::Side => "side",
        FitView::Back => "back",
    }
}

fn views_of(template: &str, truth: &FitParams, views: &[FitView], rows: usize) -> Value {
    views.iter().map(|&v| json!({ "view": view_name(v), "rows": rows_of(template, truth, v, rows) })).collect()
}

fn views() -> Value {
    views_of("soft-sd", &truth(), &[FitView::Front, FitView::Side], ROWS)
}

/// The targets the ops see for `views_of(template, truth, views, rows)`, for scoring a body directly.
fn targets(template: &str, truth: &FitParams, views: &[FitView], rows: usize) -> Vec<Target> {
    views.iter().map(|&view| Target { view, mask: render_target(template, truth, &[], view, rows).unwrap() }).collect()
}

/// The job's view of the test document: read in place (the test holds no lock).
struct DocJob<'a>(&'a MemDoc);

impl JobDoc for DocJob<'_> {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(self.0);
    }
}

/// A job's signal that keeps the last progress and cancels itself after `cancel_after` progress reports.
#[derive(Default)]
struct Signal {
    cancel: CancelFlag,
    reports: AtomicUsize,
    cancel_after: Option<usize>,
    last: Mutex<Option<(String, f32)>>,
}

impl JobSignal for Signal {
    fn progress(&self, stage: &str, fraction: f32) {
        *self.last.lock().unwrap() = Some((stage.to_owned(), fraction));
        let n = self.reports.fetch_add(1, Ordering::SeqCst) + 1;
        if self.cancel_after.is_some_and(|after| n >= after) {
            self.cancel.cancel();
        }
    }
    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
    fn cancel_flag(&self) -> CancelFlag {
        self.cancel.clone()
    }
    fn spend(&self, _usd_micros: u64) -> Result<(), OpError> {
        Ok(())
    }
}

fn person() -> Actor {
    Actor::Person { name: "tester".into() }
}

fn services(store: &Arc<MemStore>) -> Services {
    ServicesBuilder::new().insert::<dyn Store>(store.clone()).build()
}

/// Plans `op` as a dry run with `store` as the asset store: what a dry run of a job is, the plan alone.
fn dry_run(doc: &MemDoc, store: &Arc<MemStore>, op: &str, args: Value) -> Result<Plan, OpError> {
    omuzaki_char::link();
    let op = (find(op).unwrap().decode)(args)?;
    let (actor, clock, services) = (person(), FakeClock::default(), services(store));
    op.plan(&OpCtx::new(doc, &actor, &clock).with_services(&services).with_dry_run(true))
}

/// The work of a job planned against `doc`: its result, for the finish.
fn work(doc: &MemDoc, store: &Arc<MemStore>, op: &str, args: &Value, signal: &Signal) -> Result<Value, OpError> {
    work_between(doc, doc, store, op, args, signal)
}

/// The work of a job planned against `planned` that reads `now`: the document as the executor's lock-free work sees it
/// when someone edited the character after the plan.
fn work_between(
    planned: &MemDoc,
    now: &MemDoc,
    store: &Arc<MemStore>,
    op: &str,
    args: &Value,
    signal: &Signal,
) -> Result<Value, OpError> {
    omuzaki_char::link();
    let op = (find(op).unwrap().decode)(args.clone())?;
    let (actor, clock, services) = (person(), FakeClock::default(), services(store));
    let plan = op.plan(&OpCtx::new(planned, &actor, &clock).with_services(&services))?;
    assert!(plan.patch.is_empty(), "a job's plan changes nothing");
    let output = plan.output.expect("the plan hands its choice to the work");
    let reader = DocJob(now);
    let job = JobCtx::new(JobId(1), &actor, &reader, signal).with_services(&services).with_plan_output(&output);
    op.work(&job)
}

/// The finish of `op` on `result` against `doc` as it is now.
fn finish(doc: &MemDoc, store: &Arc<MemStore>, op: &str, args: &Value, result: Value) -> Result<Plan, OpError> {
    let op = (find(op).unwrap().decode)(args.clone())?;
    let (actor, clock, services) = (person(), FakeClock::default(), services(store));
    op.finish(&OpCtx::new(doc, &actor, &clock).with_services(&services), result)
}

/// Runs `op` as the executor runs a job (plan, work, finish) and returns the finish's plan, unapplied.
fn run_job(doc: &MemDoc, store: &Arc<MemStore>, op: &str, args: &Value) -> Result<Plan, OpError> {
    let result = work(doc, store, op, args, &Signal::default())?;
    finish(doc, store, op, args, result)
}

fn answer(plan: &Plan) -> &Value {
    plan.output.as_ref().expect("the finish answers")
}

#[test]
fn both_ops_are_jobs_with_the_right_scopes() {
    omuzaki_char::link();
    let reference = (find("fit.reference").unwrap().meta)();
    assert_eq!((reference.kind, reference.scope, reference.undo), (OpKind::Job, Scope::Read, Undo::Auto));
    let apply = (find("fit.apply").unwrap().meta)();
    assert_eq!((apply.kind, apply.scope, apply.undo), (OpKind::Job, Scope::EditCharacter, Undo::Auto));
}

/// The plan runs under the executor's lock, so it must not fit: it names what it would fit and what that costs.
#[test]
fn a_dry_run_checks_the_arguments_and_fits_nothing() {
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let before = doc.root().clone();
    for op in ["fit.reference", "fit.apply"] {
        let start = Instant::now();
        let plan = dry_run(&doc, &store, op, json!({ "character": "mock", "views": views() })).unwrap();
        let took = start.elapsed();
        assert!(plan.patch.is_empty(), "{op}");
        let out = plan.output.unwrap();
        assert_eq!((out["character"].as_str(), out["part"].as_str()), (Some("mock"), Some("body")), "{op}: {out}");
        assert_eq!(out["views"], json!(["front", "side"]), "{op}");
        assert!(out.get("iou_after").is_none() && out.get("patch").is_none(), "{op} fitted in its plan: {out}");
        assert!(plan.cost.and_then(|c| c.seconds).is_some_and(|s| s > 0.0), "{op} gives no time estimate");
        eprintln!("{op}: plan {took:?}");
    }
    assert_eq!(doc.root(), &before);
    assert!(store.held.lock().unwrap().is_empty(), "a dry run put something in the store");
}

#[test]
fn fit_reference_proposes_returns_heatmaps_and_changes_nothing_anywhere() {
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let before = doc.root().clone();
    let plan = run_job(&doc, &store, "fit.reference", &json!({ "character": "mock", "views": views() })).unwrap();
    assert!(plan.patch.is_empty(), "the query's finish changes nothing");
    assert_eq!(doc.root(), &before);
    assert!(store.held.lock().unwrap().is_empty(), "fit.reference put something in the store");
    let out = answer(&plan);
    let (b, a) = (out["iou_before"].as_f64().unwrap(), out["iou_after"].as_f64().unwrap());
    eprintln!("fit.reference: iou {b:.4} -> {a:.4}, patch {}", out["patch"]);
    assert!(a > b && a > 0.9, "{b} -> {a}");
    let per_view = out["per_view"].as_array().unwrap();
    assert_eq!(per_view.len(), 2);
    for v in per_view {
        let png = v["heatmap_png"].as_str().unwrap();
        assert!(png.starts_with(PNG_BASE64), "{}", &png[..16]);
        eprintln!("{} heatmap: {} base64 characters", v["view"], png.len());
    }
    // Repeatable: the same arguments, the same answer.
    let again = run_job(&doc, &store, "fit.reference", &json!({ "character": "mock", "views": views() })).unwrap();
    assert_eq!(out, answer(&again));
}

#[test]
fn heatmaps_can_be_left_out() {
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let args = json!({ "character": "mock", "views": views(), "heatmaps": false });
    let plan = run_job(&doc, &store, "fit.reference", &args).unwrap();
    assert!(answer(&plan)["per_view"].as_array().unwrap().iter().all(|v| v.get("heatmap_png").is_none()));
}

#[test]
fn apply_is_one_undoable_step_of_only_the_proportions_and_shape() {
    let store = Arc::new(MemStore::default());
    // The mock body has no params yet, so they are created whole.
    let mut doc = project();
    let original = doc.root().clone();
    let planned = run_job(&doc, &store, "fit.apply", &json!({ "character": "mock", "views": views() })).unwrap();
    assert_eq!(planned.patch.changes.len(), 1);
    assert_eq!(planned.patch.changes[0].path.to_string(), PARAMS);
    doc.apply(&planned.patch).unwrap();
    let applied = doc.root().clone();
    let leg = at(&doc, &format!("{PARAMS}/proportions/leg_length")).and_then(|v| v.as_f64()).unwrap();
    assert!(leg > 1.0, "the legs came out {leg}");
    doc.apply(&planned.patch.inverse()).unwrap();
    assert_eq!(doc.root(), &original, "undo restores the document exactly");
    doc.apply(&planned.patch).unwrap();
    assert_eq!(doc.root(), &applied, "redo gives the same document");
    assert!(store.held.lock().unwrap().is_empty(), "fit.apply put something in the store");

    // With params stored, only `proportions` and `shape` are written, and the rest stays.
    let mut node = mock_node();
    node["template"] = json!("soft-sd");
    node["parts"]["body"]["kind"] = json!("body");
    node["parts"]["body"]["params"] = json!({ "hand": { "palm_width": 1.1 } });
    let mut stored = MemDoc::from_value(json!({ "characters": { "mock": node } }));
    let planned = run_job(&stored, &store, "fit.apply", &json!({ "character": "mock", "views": views() })).unwrap();
    let paths: Vec<String> = planned.patch.changes.iter().map(|c| c.path.to_string()).collect();
    assert!(paths.iter().all(|p| p.ends_with("params/proportions") || p.ends_with("params/shape")), "{paths:?}");
    stored.apply(&planned.patch).unwrap();
    assert_eq!(at(&stored, &format!("{PARAMS}/hand/palm_width")), Some(json!(1.1)), "the hands are not touched");
}

#[test]
fn apply_writes_what_the_reference_proposed() {
    let store = Arc::new(MemStore::default());
    let proposed = run_job(&project(), &store, "fit.reference", &json!({ "character": "mock", "views": views() }))
        .unwrap()
        .output
        .unwrap();
    let mut applied = project();
    let plan = run_job(&applied, &store, "fit.apply", &json!({ "character": "mock", "views": views() })).unwrap();
    applied.apply(&plan.patch).unwrap();
    for (group, fields) in proposed["patch"].as_object().unwrap() {
        for (name, value) in fields.as_object().unwrap() {
            assert_eq!(at(&applied, &format!("{PARAMS}/{group}/{name}")).as_ref(), Some(value), "{group}/{name}");
        }
    }
    assert_eq!(answer(&plan)["iou_after"], proposed["iou_after"]);
}

/// The fit starts from the stored body: the `IoU` before is the stored body's own, and a second fit after an apply
/// starts exactly where the first ended and never ends at a higher cost than that.
#[test]
fn a_second_fit_starts_where_the_first_ended() {
    let store = Arc::new(MemStore::default());
    let args = json!({ "character": "mock", "views": views() });
    let mut doc = project();
    let first = run_job(&doc, &store, "fit.apply", &args).unwrap();
    doc.apply(&first.patch).unwrap();
    let second = run_job(&doc, &store, "fit.reference", &args).unwrap();
    let (first, second) = (answer(&first), answer(&second));
    eprintln!(
        "first {} -> {}, second {} -> {}",
        first["iou_before"], first["iou_after"], second["iou_before"], second["iou_after"]
    );
    assert_eq!(second["iou_before"], first["iou_after"], "the second fit's baseline is the applied body");
    assert_eq!(second["cost_before"], first["cost_after"]);
    assert!(second["cost_after"].as_f64().unwrap() <= second["cost_before"].as_f64().unwrap());
    for (v1, v2) in first["per_view"].as_array().unwrap().iter().zip(second["per_view"].as_array().unwrap()) {
        assert_eq!(v2["iou_before"], v1["iou_after"], "{}", v1["view"]);
    }
}

/// A body stored at the very character the reference shows: the baseline is that body's own `IoU`, and the fit can
/// only lower what it minimises (the cost), so applying cannot make the stored body fit worse. The reported `IoU`s are
/// centre-sampled and may move by a little the other way; they stay close.
#[test]
fn applying_never_makes_a_matching_body_worse() {
    const SHARP: usize = 112; // enough rows that the true body is close to the best the masks allow
    let store = Arc::new(MemStore::default());
    let mut doc = project_on("soft-sd", Some(&truth()));
    let shown = [FitView::Front, FitView::Side];
    let tg = targets("soft-sd", &truth(), &shown, SHARP);
    let own = iou_of(&FitInput { template: "soft-sd", targets: &tg, fixed: &[] }, &truth()).unwrap();
    let args = json!({ "character": "mock", "views": views_of("soft-sd", &truth(), &shown, SHARP) });
    let plan = run_job(&doc, &store, "fit.apply", &args).unwrap();
    let out = answer(&plan);
    assert!((out["iou_before"].as_f64().unwrap() - own).abs() < 1e-12, "{} against {own}", out["iou_before"]);
    let (before, after) = (out["cost_before"].as_f64().unwrap(), out["cost_after"].as_f64().unwrap());
    assert!(after <= before, "the cost went from {before} to {after}");
    doc.apply(&plan.patch).unwrap();
    let params = at(&doc, PARAMS).unwrap();
    let stored = FitParams {
        proportions: serde_json::from_value(params["proportions"].clone()).unwrap(),
        shape: serde_json::from_value(params["shape"].clone()).unwrap(),
    };
    let applied = iou_of(&FitInput { template: "soft-sd", targets: &tg, fixed: &[] }, &stored).unwrap();
    eprintln!("a matching body: cost {before:.5} -> {after:.5}, IoU {own:.4} -> {applied:.4}, patch {}", out["patch"]);
    assert!((applied - out["iou_after"].as_f64().unwrap()).abs() < 1e-12, "the applied body is the one reported");
    assert!(applied >= own - MATCHING_SLACK, "{own} -> {applied}");
}

#[test]
fn a_body_changed_while_the_fit_ran_is_refused() {
    let store = Arc::new(MemStore::default());
    let args = json!({ "character": "mock", "views": views() });
    let mut doc = project();
    let result = work(&doc, &store, "fit.apply", &args, &Signal::default()).unwrap();
    // Someone edits the body between the work and the finish.
    let edit = Patch {
        changes: vec![Change {
            path: DocPath::parse(PARAMS).unwrap(),
            before: None,
            after: Some(json!({ "proportions": { "leg_length": 0.9 } })),
        }],
    };
    doc.apply(&edit).unwrap();
    let e = finish(&doc, &store, "fit.apply", &args, result).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict, "{e:?}");
    assert!(e.message.contains("changed while the fit ran"), "{}", e.message);
}

#[test]
fn a_cancel_stops_the_fit_and_progress_is_reported() {
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let args = json!({ "character": "mock", "views": views() });
    let signal = Signal { cancel_after: Some(5), ..Signal::default() };
    let e = work(&doc, &store, "fit.reference", &args, &signal).unwrap_err();
    assert_eq!(e.code, ErrorCode::Cancelled, "{e:?}");
    assert!(signal.reports.load(Ordering::SeqCst) <= 6, "the fit went on after the cancel");

    let signal = Signal::default();
    work(&doc, &store, "fit.reference", &args, &signal).unwrap();
    assert_eq!(*signal.last.lock().unwrap(), Some(("done".to_owned(), 1.0)));
    assert!(signal.reports.load(Ordering::SeqCst) > 10, "the fit reports its progress");
}

#[test]
fn bad_input_is_refused_with_the_field_at_fault() {
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let ok = rows_of("soft-sd", &truth(), FitView::Front, ROWS);
    let cases = [
        (json!({ "character": "mock", "views": [] }), "views", "0 views"),
        (json!({ "character": "mock", "views": [{ "view": "top", "rows": ok }] }), "views/0/view", "`top`"),
        (json!({ "character": "mock", "views": [{ "view": "front", "rows": ["#.", "#"] }] }), "views/0/rows", "row 1"),
        (json!({ "character": "mock", "views": [{ "view": "front", "rows": ["#x"] }] }), "views/0/rows", "`x`"),
        (json!({ "character": "mock", "views": [{ "view": "front", "rows": ["..", ".."] }] }), "views", "blank"),
        (
            json!({ "character": "mock", "views": [{ "view": "front", "rows": ok }, { "view": "front", "rows": ok }] }),
            "views",
            "twice",
        ),
        (json!({ "character": "mock", "views": views(), "evaluations": 0 }), "evaluations", "not from 1 to 600"),
        (json!({ "character": "mock", "views": views(), "evaluations": 601 }), "evaluations", "not from 1 to 600"),
    ];
    for op in ["fit.reference", "fit.apply"] {
        for (args, field, text) in &cases {
            let e = dry_run(&doc, &store, op, args.clone()).expect_err("refused");
            assert_error(&e, ErrorCode::Invalid, field, &[text], &[]);
        }
        let e = dry_run(&doc, &store, op, json!({ "character": "nobody", "views": views() })).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::NotFound, Some("character")));
    }
    // The fit starts from the stored body, so a stored body the body kind refuses is refused, by its field.
    let mut bad = project();
    let set = Patch {
        changes: vec![Change {
            path: DocPath::parse(PARAMS).unwrap(),
            before: None,
            after: Some(json!({ "proportions": { "leg_length": 5.0 } })),
        }],
    };
    bad.apply(&set).unwrap();
    let e = dry_run(&bad, &store, "fit.apply", json!({ "character": "mock", "views": views() })).unwrap_err();
    assert_error(
        &e,
        ErrorCode::Invalid,
        "parts/body/params/proportions/leg_length",
        &["out of range"],
        &["part.set_param"],
    );
}

/// The solver's seeded random character (`tests/fit.rs`): every parameter drawn within `SPREAD` of its default.
fn random_params(seed: u64) -> FitParams {
    let mut rng = Rng::new(seed);
    let mut p = FitParams::default();
    for param in PARAM_ORDER {
        let (lo, hi) = param.range();
        let half = (hi - lo) / 2.0 * SPREAD;
        let v = param.get(&p) + f64::from(rng.range_f32(-1.0, 1.0)) * half;
        param.set(&mut p, v.clamp(lo, hi));
    }
    p
}

/// `fit.reference`'s proposal applied to the default body.
fn proposed(out: &Value) -> FitParams {
    let mut p = FitParams::default();
    let patch = &out["patch"];
    for param in PARAM_ORDER {
        let group = if param.range() == (-1.0, 1.0) { "shape" } else { "proportions" };
        if let Some(v) = patch[group][param.name()].as_f64() {
            param.set(&mut p, v);
        }
    }
    p
}

/// #2158's acceptance: through the op, on the solver's own synthetic silhouettes (#1958's seed, three 384-row views),
/// the proposal recovers what a silhouette identifies within the solver test's tolerances: the `IoU`, the widths, arm
/// length, foot size, curves and the leg's share. All four templates in a release build; one in a debug build, where
/// the solver's test fits all four.
#[test]
fn fit_reference_recovers_the_solvers_test_character_within_its_tolerances() {
    let truth = random_params(SEED);
    let ratio = |p: &FitParams| (p.proportions.leg_length / p.proportions.torso_length).ln();
    let templates: &[&str] = if cfg!(debug_assertions) { &TEMPLATES[1..2] } else { &TEMPLATES };
    let store = Arc::new(MemStore::default());
    for template in templates {
        let doc = project_on(template, None);
        let args =
            json!({ "character": "mock", "views": views_of(template, &truth, &VIEWS, HEIGHT), "heatmaps": false });
        let start = Instant::now();
        let plan = run_job(&doc, &store, "fit.reference", &args).unwrap();
        let took = start.elapsed();
        let out = answer(&plan);
        let fitted = proposed(out);
        let iou = out["iou_after"].as_f64().unwrap();
        let mut errors = Vec::new();
        for p in [Param::ShoulderWidth, Param::HipWidth, Param::ArmLength, Param::FootSize, Param::Curves] {
            let (lo, hi) = p.range();
            errors.push((p, (p.get(&fitted) - p.get(&truth)).abs() / (hi - lo)));
        }
        let share = (ratio(&fitted) - ratio(&truth)).abs();
        eprintln!(
            "{template}: {took:?}, IoU {} -> {iou:.4}, errors {errors:?}, leg/torso {share:.4}",
            out["iou_before"]
        );
        assert!(iou >= 0.985, "{template}: IoU {iou}");
        for (p, err) in errors {
            assert!(err < 0.03, "{template}: {p:?} off by {err}");
        }
        assert!(share < 0.08, "{template}: leg to torso ratio off by {share}");
    }
}

/// The three forms of a view give the same fit: text rows, a base64 PNG, and a PNG in the asset store.
#[test]
fn rows_a_png_and_a_stored_png_fit_alike() {
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let shown = [FitView::Front, FitView::Side];
    let masks: Vec<Silhouette> =
        shown.iter().map(|&v| render_target("soft-sd", &truth(), &[], v, ROWS).unwrap()).collect();
    let as_png: Vec<Value> = shown
        .iter()
        .zip(&masks)
        .map(|(&v, m)| json!({ "view": view_name(v), "png": BASE64.encode(png_of(m)) }))
        .collect();
    let as_asset: Vec<Value> = shown
        .iter()
        .zip(&masks)
        .map(|(&v, m)| {
            let hash = store.put(&png_of(m), common_provenance()).unwrap();
            json!({ "view": view_name(v), "asset": hash })
        })
        .collect();
    let answer_of = |views: Value| {
        // The three forms decode to one mask, so a short fit tells them apart as well as a full one.
        let args = json!({ "character": "mock", "views": views, "heatmaps": false, "evaluations": 40 });
        answer(&run_job(&doc, &store, "fit.reference", &args).unwrap()).clone()
    };
    let from_rows = answer_of(views());
    assert_eq!(answer_of(Value::Array(as_png)), from_rows);
    assert_eq!(answer_of(Value::Array(as_asset)), from_rows);
}

/// What a store the test fills itself records: the test's own material.
fn common_provenance() -> Provenance {
    Provenance {
        source: omuzaki_store_api::Source::Authored,
        author: None,
        licence: "LicenseRef-Omuzaki-User".into(),
        attribution: None,
        commercial: omuzaki_store_api::Commercial::Unknown,
        consent: None,
        private_test: false,
    }
}

#[test]
fn bad_pngs_and_assets_are_refused_by_their_field() {
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let mask = render_target("soft-sd", &truth(), &[], FitView::Front, ROWS).unwrap();
    let ok = rows_of("soft-sd", &truth(), FitView::Front, ROWS);
    let cases = [
        (json!({ "view": "front", "png": "not base64!" }), ErrorCode::Invalid, Some("views/0/png"), "base64"),
        (
            json!({ "view": "front", "png": BASE64.encode(b"not a png") }),
            ErrorCode::Invalid,
            Some("views/0/png"),
            "doesn't decode",
        ),
        (
            json!({ "view": "front", "rows": ok, "png": BASE64.encode(png_of(&mask)) }),
            ErrorCode::Invalid,
            Some("views/0/rows"),
            "exactly one",
        ),
        (json!({ "view": "front" }), ErrorCode::Invalid, Some("views/0/rows"), "exactly one"),
    ];
    for (view, code, field, text) in cases {
        let e = dry_run(&doc, &store, "fit.reference", json!({ "character": "mock", "views": [view] })).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (code, field), "{e:?}");
        assert!(e.message.contains(text), "{}", e.message);
    }
    // A missing asset is found when the work loads it, not when the arguments are checked (planning reads nothing).
    let missing =
        json!({ "character": "mock", "views": [{ "view": "front", "asset": ContentHash::of(b"never stored") }] });
    dry_run(&doc, &store, "fit.reference", missing.clone()).unwrap();
    let e = work(&doc, &store, "fit.reference", &missing, &Signal::default()).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
    assert!(e.message.contains("can't be read"), "{}", e.message);
    // An asset needs the store: an app that wired none is told so.
    let hash = store.put(&png_of(&mask), common_provenance()).unwrap();
    let args = json!({ "character": "mock", "views": [{ "view": "front", "asset": hash }] });
    let e = common::plan(&doc, "fit.reference", args).unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable, "{e:?}");
}

/// The meshes of every part but the body, as built at `params`: what a reference of the whole character shows.
fn other_parts(doc: &MemDoc) -> Vec<MeshData> {
    build(doc, "mock").meshes.into_iter().filter(|m| m.part.as_str() != "body").map(|m| m.mesh.geometry).collect()
}

/// A reference of the whole character (body, head and hair) fits the body inside it when the other parts are drawn
/// with the body, following its joints; fitting the body alone against it reaches less.
#[test]
fn the_other_parts_are_drawn_with_the_body() {
    const PART_ROWS: usize = 96;
    let store = Arc::new(MemStore::default());
    let fixed = other_parts(&with_parts("soft-sd", Some(&truth()), head_and_hair()));
    assert!(!fixed.is_empty(), "the head and hair build");
    let shown = [FitView::Front, FitView::Side];
    let views: Value = shown
        .iter()
        .map(|&v| {
            let m = render_target("soft-sd", &truth(), &fixed, v, PART_ROWS).unwrap();
            let rows: Vec<String> = m
                .pixels()
                .chunks(m.width())
                .map(|r| r.iter().map(|&on| if on { '#' } else { '.' }).collect())
                .collect();
            json!({ "view": view_name(v), "rows": rows })
        })
        .collect();
    let doc = with_parts("soft-sd", None, head_and_hair());
    let fit = |parts: Option<Value>| {
        let mut args = json!({ "character": "mock", "views": views, "heatmaps": false });
        if let Some(parts) = parts {
            args["parts"] = parts;
        }
        answer(&run_job(&doc, &store, "fit.reference", &args).unwrap()).clone()
    };
    let (drawn, alone) = (fit(None), fit(Some(json!([]))));
    assert_eq!(drawn["parts"], json!(["hair", "head"]), "every enabled part by default, in the character's order");
    assert_eq!(alone["parts"], json!([]));
    let (with, without) = (drawn["iou_after"].as_f64().unwrap(), alone["iou_after"].as_f64().unwrap());
    let leg = |a: &Value| a["patch"]["proportions"]["leg_length"].as_f64().unwrap_or(1.0);
    eprintln!(
        "whole-character reference: IoU {with:.4} with the parts drawn, {without:.4} without; legs {} / {} (truth 1.25)",
        leg(&drawn),
        leg(&alone)
    );
    assert!(with >= 0.97, "{with}");
    assert!(with > without, "{with} against {without}");
    assert!((leg(&drawn) - 1.25).abs() < (leg(&alone) - 1.25).abs(), "drawing the parts recovers the legs better");
}

#[test]
fn the_parts_asked_for_are_checked() {
    let store = Arc::new(MemStore::default());
    let mut doc = with_parts("standard", None, head_and_hair());
    let plan = |doc: &MemDoc, parts: Value| {
        dry_run(doc, &store, "fit.reference", json!({ "character": "mock", "views": views(), "parts": parts }))
    };
    let e = plan(&doc, json!(["tail"])).unwrap_err();
    assert_error(&e, ErrorCode::Invalid, "parts/0", &["no part `tail`"], &["head", "hair"]);
    let e = plan(&doc, json!(["body"])).unwrap_err();
    assert_error(&e, ErrorCode::Invalid, "parts/0", &["the body"], &[]);
    assert_eq!(plan(&doc, json!(["hair", "hair"])).unwrap().output.unwrap()["parts"], json!(["hair"]));
    let off = Patch {
        changes: vec![Change {
            path: DocPath::parse("characters/mock/parts/head/enabled").unwrap(),
            before: None,
            after: Some(json!(false)),
        }],
    };
    doc.apply(&off).unwrap();
    let e = plan(&doc, json!(["head"])).unwrap_err();
    assert_error(&e, ErrorCode::Invalid, "parts/0", &["disabled"], &["part.set_enabled"]);
    let default = dry_run(&doc, &store, "fit.reference", json!({ "character": "mock", "views": views() })).unwrap();
    assert_eq!(default.output.unwrap()["parts"], json!(["hair"]), "a disabled part is left out by default");
}

/// Writes the front and side heatmaps of a synthetic character, for the evidence sheet
/// (`evidence/fit-reference.toml`): the fit starts from the default body and the maps show what it could not close.
#[allow(clippy::disallowed_methods)] // a test writing its evidence images, not an op reaching a file
#[test]
fn write_the_heatmaps_when_asked() {
    let Ok(dir) = std::env::var("OMUZAKI_FIT_HEATMAP_DIR") else { return };
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let views = views_of("soft-sd", &truth(), &[FitView::Front, FitView::Side], SHEET_ROWS);
    let plan = run_job(&doc, &store, "fit.reference", &json!({ "character": "mock", "views": views })).unwrap();
    for v in answer(&plan)["per_view"].as_array().unwrap() {
        let png = BASE64.decode(v["heatmap_png"].as_str().unwrap()).unwrap();
        std::fs::write(format!("{dir}/heatmap-{}.png", v["view"].as_str().unwrap()), png).unwrap();
    }
}

/// Planning runs under the document lock, so it reads nothing from the asset store (a disk store hashes a whole asset
/// per read): an asset view is loaded and decoded in the work.
#[test]
fn planning_reads_nothing_from_the_store_and_the_work_does() {
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let shown = [FitView::Front, FitView::Side];
    let views: Vec<Value> = shown
        .iter()
        .map(|&v| {
            let mask = render_target("soft-sd", &truth(), &[], v, ROWS).unwrap();
            json!({ "view": view_name(v), "asset": store.put(&png_of(&mask), common_provenance()).unwrap() })
        })
        .collect();
    let reads = || store.reads.load(Ordering::SeqCst);
    for op in ["fit.reference", "fit.apply"] {
        let args = json!({ "character": "mock", "views": views, "heatmaps": false, "evaluations": 4 });
        let args =
            if op == "fit.apply" { json!({ "character": "mock", "views": views, "evaluations": 4 }) } else { args };
        let plan = dry_run(&doc, &store, op, args.clone()).unwrap();
        assert!(plan.patch.is_empty() && plan.output.is_some(), "{op}");
        assert_eq!(reads(), 0, "{op}: planning read the store");
        // The same plan as a job's, then the work: the work is what reads (every view, once).
        let result = work(&doc, &store, op, &args, &Signal::default()).unwrap();
        assert!(reads() >= shown.len(), "{op}: the work did not load the assets");
        store.reads.store(0, Ordering::SeqCst);
        assert!(result.is_object(), "{op}");
    }
}

/// The parts drawn with the body are rebuilt for every candidate: head and hair (and a tail or wings) are built on the
/// body's joints and stature, so after the fit the character the document now describes must draw what the fit
/// reported. Reported `IoU` against the `IoU` of the rebuilt character, which a frozen-parts fit does not match.
#[test]
fn the_reported_fit_is_what_the_rebuilt_character_draws() {
    const PART_ROWS: usize = 64;
    let store = Arc::new(MemStore::default());
    let shown = [FitView::Front, FitView::Side];
    let fixed = other_parts(&with_parts("soft-sd", Some(&truth()), head_and_hair()));
    let masks: Vec<Silhouette> =
        shown.iter().map(|&v| render_target("soft-sd", &truth(), &fixed, v, PART_ROWS).unwrap()).collect();
    let views: Value = shown
        .iter()
        .zip(&masks)
        .map(|(&v, m)| {
            let rows: Vec<String> = m
                .pixels()
                .chunks(m.width())
                .map(|r| r.iter().map(|&on| if on { '#' } else { '.' }).collect())
                .collect();
            json!({ "view": view_name(v), "rows": rows })
        })
        .collect();
    let doc = with_parts("soft-sd", None, head_and_hair());
    // Any candidate the fit settles on must match its rebuild, so a short fit tests it as well as a full one.
    let args = json!({ "character": "mock", "views": views, "heatmaps": false, "evaluations": 24 });
    let out = answer(&run_job(&doc, &store, "fit.reference", &args).unwrap()).clone();
    let proposed = proposed(&out);
    // The character as the document would hold it after an apply: built again, its other parts drawn with its body.
    let rebuilt = other_parts(&with_parts("soft-sd", Some(&proposed), head_and_hair()));
    let tg: Vec<Target> = shown.iter().zip(&masks).map(|(&view, m)| Target { view, mask: m.clone() }).collect();
    let drawn = iou_of(&FitInput { template: "soft-sd", targets: &tg, fixed: &rebuilt }, &proposed).unwrap();
    let reported = out["iou_after"].as_f64().unwrap();
    eprintln!("reported IoU {reported:.5}, the rebuilt character draws {drawn:.5}");
    assert!((reported - drawn).abs() < 1e-9, "reported {reported}, rebuilt {drawn}");
}

/// Writes `after` at `path` (`None` removes it), whatever is there now.
fn set(doc: &mut MemDoc, path: &str, after: Option<Value>) {
    let path = DocPath::parse(path).unwrap();
    let before = doc.get(&path).cloned();
    doc.apply(&Patch { changes: vec![Change { path, before, after }] }).unwrap();
}

/// What the fit drew the body with is part of what it fitted: editing, disabling or removing a drawn part while the
/// fit ran leaves a result fitted to geometry that is gone, so the apply is refused. An edit that cannot change the
/// geometry (the character's name) is not, and neither is any edit when the body is fitted alone.
#[test]
fn parts_changed_while_the_fit_ran_refuse_the_apply() {
    const HAIR: &str = "characters/mock/parts/hair";
    const HEAD_PARAMS: &str = "characters/mock/parts/head/params";
    type Edit = Box<dyn Fn(&mut MemDoc)>;
    let store = Arc::new(MemStore::default());
    let doc = with_parts("soft-sd", None, head_and_hair());
    let apply = |args: &Value, result: &Value, edit: &dyn Fn(&mut MemDoc)| {
        let mut edited = MemDoc::from_value(doc.root().clone());
        edit(&mut edited);
        finish(&edited, &store, "fit.apply", args, result.clone())
    };
    // One fit (the slow part), finished against several documents; a short one, as the guard does not depend on how
    // well it converged (#2272).
    let args = json!({ "character": "mock", "views": views(), "evaluations": 12 });
    let result = work(&doc, &store, "fit.apply", &args, &Signal::default()).unwrap();
    let edits: [(&str, Edit); 4] = [
        ("hair edited", Box::new(|d| set(d, &format!("{HAIR}/params"), Some(json!({ "rng": "legacy_lcg" }))))),
        ("hair disabled", Box::new(|d| set(d, &format!("{HAIR}/enabled"), Some(json!(false))))),
        ("hair removed", Box::new(|d| set(d, HAIR, None))),
        ("head edited", Box::new(|d| set(d, HEAD_PARAMS, Some(json!({ "window": { "half_width": 0.2 } }))))),
    ];
    for (what, edit) in &edits {
        let e = apply(&args, &result, edit.as_ref()).unwrap_err();
        assert_eq!(e.code, ErrorCode::Conflict, "{what}: {e:?}");
        assert!(e.message.contains("parts drawn with the body"), "{what}: {}", e.message);
    }
    let renamed = apply(&args, &result, &|d| set(d, "characters/mock/name", Some(json!("Renamed")))).unwrap();
    assert!(!renamed.patch.is_empty(), "an edit that leaves the geometry alone still applies");
    // With the body alone nothing else is drawn, so editing the other parts changes nothing the fit used.
    let alone = json!({ "character": "mock", "views": views(), "parts": [], "evaluations": 4 });
    let result = work(&doc, &store, "fit.apply", &alone, &Signal::default()).unwrap();
    let edit = |d: &mut MemDoc| set(d, &format!("{HAIR}/enabled"), Some(json!(false)));
    assert!(apply(&alone, &result, &edit).is_ok());
}

/// Sets one field of the face layout. The fixture has no `face` table, so the whole table is written.
fn set_layout(doc: &mut MemDoc, field: &str, value: f64) {
    set(doc, "characters/mock/face", Some(json!({ "layout": { field: value } })));
}

/// The head, hair and glasses (which place their frames by the face layout) beside the body.
fn head_hair_and_glasses() -> Value {
    json!({ "head": { "kind": "head" }, "hair": { "kind": "hair" }, "glasses": { "kind": "glasses" } })
}

/// A glasses part reads the character's face layout (eye spacing and height), so a layout edit while the fit ran leaves
/// a result fitted to frames that moved. The face profile is in the guard whole, and so is the seed.
#[test]
fn a_face_layout_changed_while_the_fit_ran_refuses_the_apply() {
    type Edit = Box<dyn Fn(&mut MemDoc)>;
    let store = Arc::new(MemStore::default());
    let doc = with_parts("soft-sd", None, head_hair_and_glasses());
    // The guard does not depend on how well the fit converged, so a few candidates are enough (#2272).
    let args = json!({ "character": "mock", "views": views(), "evaluations": 4 });
    let result = work(&doc, &store, "fit.apply", &args, &Signal::default()).unwrap();
    let edits: [(&str, Edit); 3] = [
        ("eye spacing", Box::new(|d| set_layout(d, "eye_spacing", 0.12))),
        ("eye height", Box::new(|d| set_layout(d, "eye_y", -0.05))),
        ("seed", Box::new(|d| set(d, "characters/mock/seed", Some(json!(8))))),
    ];
    for (what, edit) in &edits {
        let mut edited = MemDoc::from_value(doc.root().clone());
        edit(&mut edited);
        let e = finish(&edited, &store, "fit.apply", &args, result.clone()).unwrap_err();
        assert_eq!(e.code, ErrorCode::Conflict, "{what}: {e:?}");
        assert!(e.message.contains("parts drawn with the body"), "{what}: {}", e.message);
    }
    // The body alone does not look at the face.
    let alone = json!({ "character": "mock", "views": views(), "parts": [], "evaluations": 4 });
    let result = work(&doc, &store, "fit.apply", &alone, &Signal::default()).unwrap();
    let mut edited = MemDoc::from_value(doc.root().clone());
    set_layout(&mut edited, "eye_spacing", 0.12);
    assert!(finish(&edited, &store, "fit.apply", &alone, result).is_ok());
}

/// The plan chose the parts from the character as it was; `work` reads the character again. A chosen part that is gone
/// or changed by then is a `Conflict`, not a fit that leaves it out while the answer lists it.
#[test]
fn parts_changed_between_the_plan_and_the_work_are_refused() {
    const HAIR: &str = "characters/mock/parts/hair";
    type Edit = Box<dyn Fn(&mut MemDoc)>;
    let store = Arc::new(MemStore::default());
    let planned = with_parts("soft-sd", None, head_hair_and_glasses());
    let edits: [(&str, &str, Edit); 5] = [
        ("hair removed", "hair", Box::new(|d| set(d, HAIR, None))),
        ("hair disabled", "hair", Box::new(|d| set(d, &format!("{HAIR}/enabled"), Some(json!(false))))),
        (
            "hair edited",
            "changed before",
            Box::new(|d| set(d, &format!("{HAIR}/params"), Some(json!({ "rng": "legacy_lcg" })))),
        ),
        ("face layout edited", "changed before", Box::new(|d| set_layout(d, "eye_spacing", 0.12))),
        ("seed edited", "changed before", Box::new(|d| set(d, "characters/mock/seed", Some(json!(8))))),
    ];
    for op in ["fit.reference", "fit.apply"] {
        // Hair asked for by name, as well as by default.
        for args in [
            json!({ "character": "mock", "views": views(), "parts": ["hair"] }),
            json!({ "character": "mock", "views": views() }),
        ] {
            for (what, says, edit) in &edits {
                let mut now = MemDoc::from_value(planned.root().clone());
                edit(&mut now);
                let e = work_between(&planned, &now, &store, op, &args, &Signal::default()).unwrap_err();
                assert_eq!(e.code, ErrorCode::Conflict, "{op}, {what}: {e:?}");
                assert!(e.message.contains(says), "{op}, {what}: {}", e.message);
            }
        }
    }
    // Parts the fit does not draw may change meanwhile.
    let args = json!({ "character": "mock", "views": views(), "parts": [] });
    let mut now = MemDoc::from_value(planned.root().clone());
    set(&mut now, HAIR, None);
    assert!(work_between(&planned, &now, &store, "fit.reference", &args, &Signal::default()).is_ok());
}
