//! `face.channel.sweep` (#319) against an in-memory document and store: what it plans, what its job reports and stores,
//! that the report is byte-stable, and its refusals. The measure itself is tested in `coverage.rs`.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, CancelFlag, ErrorCode, JobCtx, JobDoc, JobId, JobSignal, OpCtx, OpError, OpKind, Services, ServicesBuilder,
    find,
};
use omuzaki_store_api::{ContentHash, Store};
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

const OP: &str = "face.channel.sweep";

struct Doc(MemDoc);

impl JobDoc for Doc {
    fn read(&self, f: &mut dyn FnMut(&dyn omuzaki_doc_api::DocRead)) {
        f(&self.0);
    }
}

#[derive(Default)]
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

/// A document with the character `mira` and no face of her own (the defaults).
fn doc() -> MemDoc {
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira" } } }))
}

fn decode(args: Value) -> Result<Box<dyn omuzaki_ops_api::DynOp>, OpError> {
    omuzaki_face::link();
    (find(OP).expect("registered").decode)(args)
}

fn plan(args: Value) -> Result<Value, OpError> {
    let op = decode(args)?;
    let (doc, actor, clock) = (doc(), Actor::Person { name: "tester".into() }, FakeClock::default());
    Ok(op.plan(&OpCtx::new(&doc, &actor, &clock))?.output.expect("answers"))
}

/// The plan, then the job, as the executor runs them.
fn run(services: &Services, args: Value, signal: &Signal) -> Result<Value, OpError> {
    let planned = plan(args.clone())?;
    let (op, actor, doc) = (decode(args)?, Actor::Person { name: "tester".into() }, Doc(doc()));
    op.work(&JobCtx::new(JobId(1), &actor, &doc, signal).with_services(services).with_plan_output(&planned))
}

fn with_store() -> (Arc<MemStore>, Services) {
    let store = Arc::new(MemStore::new());
    let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
    (store, services)
}

#[test]
fn it_is_a_read_only_job() {
    let registration = find(OP).expect("registered");
    let meta = (registration.meta)();
    assert_eq!(meta.kind, OpKind::Job);
}

#[test]
fn the_plan_says_what_the_job_will_sweep() {
    let all = plan(json!({ "character": "mira" })).unwrap();
    assert_eq!(all["channels"], 65);
    assert_eq!(all["width_px"], 774);
    assert_eq!(all["min_changed_px"], 10);
    let some = plan(json!({ "character": "mira", "channels": ["jawOpen", "blush"], "width_px": 1548 })).unwrap();
    assert_eq!((some["channels"].as_u64(), some["min_changed_px"].as_u64()), (Some(2), Some(40)));
}

#[test]
fn the_job_reports_every_channel_and_stores_a_labelled_sheet() {
    let (store, services) = with_store();
    let args = json!({ "character": "mira", "channels": ["jawOpen", "jawForward", "eyeBlinkLeft"], "width_px": 387 });
    let answer = run(&services, args, &Signal::default()).unwrap();
    assert_eq!(answer["summary"]["moves"], 2);
    assert_eq!(answer["summary"]["unmapped"], json!(["jawForward"]));
    assert_eq!(answer["summary"]["dead"], json!([]));
    let rows = answer["rows"].as_array().unwrap();
    assert_eq!(
        rows.iter().map(|r| r["channel"].as_str().unwrap()).collect::<Vec<_>>(),
        ["jawOpen", "jawForward", "eyeBlinkLeft"]
    );
    assert!(rows[2]["base"]["changed_px"].as_u64().unwrap() > 100, "a blink closes an eye: {}", rows[2]);
    let hash: ContentHash = answer["sheet"]["hash"].as_str().unwrap().parse().unwrap();
    let png = store.get(&hash).unwrap();
    let sheet = image::load_from_memory(&png).unwrap();
    // Four cells (the rest and three pushes) in one row.
    assert_eq!(u64::from(sheet.width()), answer["sheet"]["width"].as_u64().unwrap());
    assert!(sheet.width() > 4 * 160 && sheet.height() > 100);
    assert_eq!(answer["sheet"]["uri"], format!("omuzaki://render/{hash}.png"));
}

#[test]
fn the_report_is_byte_stable() {
    let (_, services) = with_store();
    let args = json!({ "character": "mira", "channels": ["mouthSmileLeft", "eyesStar", "fangs"], "width_px": 200 });
    let first = serde_json::to_string(&run(&services, args.clone(), &Signal::default()).unwrap()).unwrap();
    let second = serde_json::to_string(&run(&services, args, &Signal::default()).unwrap()).unwrap();
    assert_eq!(first, second);
}

#[test]
fn without_a_store_the_sheet_is_not_kept_and_says_so() {
    let args = json!({ "character": "mira", "channels": ["jawOpen"], "width_px": 100 });
    let answer = run(&Services::default(), args.clone(), &Signal::default()).unwrap();
    assert!(answer["sheet"]["note"].as_str().unwrap().contains("no asset store"));
    let mut no_sheet = args;
    no_sheet["sheet"] = json!(false);
    let answer = run(&Services::default(), no_sheet, &Signal::default()).unwrap();
    assert!(answer.get("sheet").is_none());
}

#[test]
fn a_cancelled_job_stops() {
    let signal = Signal::default();
    signal.0.cancel();
    let error = run(&Services::default(), json!({ "character": "mira" }), &signal).unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
}

#[test]
fn bad_arguments_are_refused_by_name() {
    let unknown = plan(json!({ "character": "mira", "channels": ["jawOpen", "smile"] })).unwrap_err();
    assert_eq!((unknown.code, unknown.field.as_deref()), (ErrorCode::Invalid, Some("channels/1")));
    let empty = plan(json!({ "character": "mira", "channels": [] })).unwrap_err();
    assert_eq!(empty.field.as_deref(), Some("channels"));
    let wide = plan(json!({ "character": "mira", "width_px": 5000 })).unwrap_err();
    assert_eq!(wide.field.as_deref(), Some("width_px"));
    let nobody = plan(json!({ "character": "nobody" })).unwrap_err();
    assert_eq!(nobody.code, ErrorCode::NotFound);
}
