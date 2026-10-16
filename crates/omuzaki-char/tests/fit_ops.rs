//! `fit.reference` and `fit.apply` against the in-memory document and a store: the dry run reports a proposal and stores
//! heatmaps without changing the document, the apply is one undoable edit that touches only the body's `proportions`
//! and `shape`, both are repeatable and refuse bad input with typed errors.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use common::{assert_error, at, mock_node, refused, round_trip};
use omuzaki_char::fit::{FitParams, FitView, render_target};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpKind, Scope, ServicesBuilder, Undo, find};
use omuzaki_store_api::{ContentHash, Provenance, Store, StoreError};
use serde_json::{Value, json};

/// The smallest store: bytes by hash.
#[derive(Default)]
struct MemStore(Mutex<HashMap<ContentHash, (Vec<u8>, Provenance)>>);

impl Store for MemStore {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        let hash = ContentHash::of(bytes);
        self.0.lock().unwrap().entry(hash).or_insert_with(|| (bytes.to_vec(), provenance));
        Ok(hash)
    }
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        self.0.lock().unwrap().get(hash).map(|(b, _)| b.clone()).ok_or(StoreError::Missing(*hash))
    }
    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        self.0.lock().unwrap().get(hash).map(|(_, p)| p.clone()).ok_or(StoreError::Missing(*hash))
    }
    fn contains(&self, hash: &ContentHash) -> bool {
        self.0.lock().unwrap().contains_key(hash)
    }
}

const PARAMS: &str = "characters/mock/parts/body/params";
/// Rows of a synthetic view: small, so the fit is quick in a debug build.
const ROWS: usize = 48;
/// Rows of the evidence heatmaps.
const SHEET_ROWS: usize = 96;

fn project() -> MemDoc {
    let mut node = mock_node();
    node["template"] = json!("soft-sd");
    node["parts"]["body"]["kind"] = json!("body");
    MemDoc::from_value(json!({ "characters": { "mock": node } }))
}

/// A known body: longer legs, broader shoulders, a broader build.
fn truth() -> FitParams {
    let mut p = FitParams::default();
    p.proportions.leg_length = 1.25;
    p.proportions.shoulder_width = 1.3;
    p.shape.build = 0.4;
    p
}

fn mask_rows(view: FitView, rows: usize) -> Vec<String> {
    let m = render_target("soft-sd", &truth(), &[], view, rows).unwrap();
    m.pixels().chunks(m.width()).map(|r| r.iter().map(|&on| if on { '#' } else { '.' }).collect()).collect()
}

fn views() -> Value {
    views_of(ROWS)
}

fn views_of(rows: usize) -> Value {
    json!([{ "view": "front", "rows": mask_rows(FitView::Front, rows) }, { "view": "side", "rows": mask_rows(FitView::Side, rows) }])
}

/// Plans `op` with `store` as the asset store, as a dry run.
fn plan_with_store(
    doc: &MemDoc,
    store: &Arc<MemStore>,
    op: &str,
    args: Value,
) -> Result<Value, omuzaki_ops_api::OpError> {
    omuzaki_char::link();
    let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
    let op = (find(op).unwrap().decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock).with_services(&services).with_dry_run(true)).map(|p| p.output.unwrap())
}

#[test]
fn both_ops_are_registered_with_the_right_kinds() {
    omuzaki_char::link();
    let reference = (find("fit.reference").unwrap().meta)();
    assert_eq!((reference.kind, reference.scope, reference.undo), (OpKind::Query, Scope::Read, Undo::None));
    let apply = (find("fit.apply").unwrap().meta)();
    assert_eq!((apply.kind, apply.scope, apply.undo), (OpKind::Edit, Scope::EditCharacter, Undo::Auto));
}

#[test]
fn the_dry_run_recovers_the_body_stores_heatmaps_and_changes_nothing() {
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let before = doc.root().clone();
    let out = plan_with_store(&doc, &store, "fit.reference", json!({ "character": "mock", "views": views() })).unwrap();
    assert_eq!(doc.root(), &before);
    let (b, a) = (out["iou_before"].as_f64().unwrap(), out["iou_after"].as_f64().unwrap());
    eprintln!("fit.reference: iou {b:.4} -> {a:.4}, patch {}", out["patch"]);
    assert!(a > b && a > 0.9, "{b} -> {a}");
    let per_view = out["per_view"].as_array().unwrap();
    assert_eq!(per_view.len(), 2);
    for v in per_view {
        let hash: ContentHash = v["heatmap"].as_str().unwrap().parse().unwrap();
        let png = store.get(&hash).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }
    // Repeatable: the same arguments, the same answer and the same stored bytes.
    let again =
        plan_with_store(&doc, &store, "fit.reference", json!({ "character": "mock", "views": views() })).unwrap();
    assert_eq!(out, again);
    assert_eq!(store.0.lock().unwrap().len(), 2);
}

#[test]
fn the_dry_run_needs_the_store() {
    let doc = project();
    let e = common::plan(&doc, "fit.reference", json!({ "character": "mock", "views": views() })).unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable);
}

#[test]
fn apply_is_one_undoable_edit_of_only_the_proportions_and_shape() {
    // The mock body has no params yet, so they are created whole.
    let mut doc = project();
    let planned = round_trip(&mut doc, "fit.apply", json!({ "character": "mock", "views": views() }));
    assert_eq!(planned.patch.changes.len(), 1);
    assert_eq!(planned.patch.changes[0].path.to_string(), PARAMS);
    let leg = at(&doc, &format!("{PARAMS}/proportions/leg_length")).and_then(|v| v.as_f64()).unwrap();
    assert!(leg > 1.0, "the legs came out {leg}");

    // With params stored, only `proportions` and `shape` are written, and the rest stays.
    let mut node = mock_node();
    node["template"] = json!("soft-sd");
    node["parts"]["body"]["kind"] = json!("body");
    node["parts"]["body"]["params"] = json!({ "hand": { "palm_width": 1.1 } });
    let mut stored = MemDoc::from_value(json!({ "characters": { "mock": node } }));
    let planned = round_trip(&mut stored, "fit.apply", json!({ "character": "mock", "views": views() }));
    let paths: Vec<String> = planned.patch.changes.iter().map(|c| c.path.to_string()).collect();
    assert!(paths.iter().all(|p| p.ends_with("params/proportions") || p.ends_with("params/shape")), "{paths:?}");
    assert_eq!(at(&stored, &format!("{PARAMS}/hand/palm_width")), Some(json!(1.1)), "the hands are not touched");
}

#[test]
fn apply_writes_what_the_dry_run_proposed() {
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let proposed =
        plan_with_store(&doc, &store, "fit.reference", json!({ "character": "mock", "views": views() })).unwrap();
    let mut applied = project();
    common::apply(&mut applied, "fit.apply", json!({ "character": "mock", "views": views() }));
    for (group, fields) in proposed["patch"].as_object().unwrap() {
        for (name, value) in fields.as_object().unwrap() {
            assert_eq!(at(&applied, &format!("{PARAMS}/{group}/{name}")).as_ref(), Some(value), "{group}/{name}");
        }
    }
}

#[test]
fn bad_input_is_refused_with_the_field_at_fault() {
    let doc = project();
    let ok = mask_rows(FitView::Front, ROWS);
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
    ];
    for (args, field, text) in cases {
        let e = refused(&doc, "fit.apply", args);
        assert_error(&e, ErrorCode::Invalid, field, &[text], &[]);
    }
    let e = refused(&doc, "fit.apply", json!({ "character": "nobody", "views": views() }));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::NotFound, Some("character")));
}

/// Writes the front and side heatmaps of a synthetic character, for the evidence sheet (`evidence.toml`, case
/// `fit-heatmap`): the fit starts from the default body, so the maps show the offset the fit then closes.
#[allow(clippy::disallowed_methods)] // a test writing its evidence images, not an op reaching a file
#[test]
fn write_the_heatmaps_when_asked() {
    let Ok(dir) = std::env::var("OMUZAKI_FIT_HEATMAP_DIR") else { return };
    let (doc, store) = (project(), Arc::new(MemStore::default()));
    let out =
        plan_with_store(&doc, &store, "fit.reference", json!({ "character": "mock", "views": views_of(SHEET_ROWS) }))
            .unwrap();
    for v in out["per_view"].as_array().unwrap() {
        let hash: ContentHash = v["heatmap"].as_str().unwrap().parse().unwrap();
        std::fs::write(format!("{dir}/heatmap-{}.png", v["view"].as_str().unwrap()), store.get(&hash).unwrap())
            .unwrap();
    }
}
