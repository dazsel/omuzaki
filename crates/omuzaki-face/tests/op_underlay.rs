//! `face.underlay.set` (#668) against an in-memory store and a recording `UnderlayTarget`: what it plans, what its job
//! builds and hands the app, the opacity-only update, its dry run, and its refusals. Synthetic sheets only.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use image::{ImageFormat, Rgba, RgbaImage};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::{
    FaceProjection, FaceRect, UnderlayHead, UnderlayImage, UnderlayTarget, UnderlayTargetError,
};
use omuzaki_ops_api::{
    Actor, CancelFlag, ErrorCode, JobCtx, JobDoc, JobId, JobSignal, OpCtx, OpError, Plan, ServicesBuilder, find,
};
use omuzaki_store_api::{Commercial, ContentHash, Store};
use omuzaki_store_mock::{MemStore, conformance::sample_provenance};
use serde_json::{Value, json};

/// A 0.4 m face window at 20 px/m: the Base layer is 8 x 8 px.
const PX_PER_M: f32 = 20.0;

fn projection() -> FaceProjection {
    FaceProjection { rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.2, y1: 0.2 }, front_fade: [-1.0, -0.5] }
}

/// What the app was asked to do, in order.
#[derive(Debug, PartialEq)]
enum Call {
    Show(String, UnderlayImage),
    Opacity(String, f32),
}

#[derive(Default)]
struct Recording {
    calls: Mutex<Vec<Call>>,
    shown_key: Mutex<Option<String>>,
}

impl Recording {
    fn shows(&self) -> Vec<UnderlayImage> {
        let calls = self.calls.lock().unwrap();
        calls.iter().filter_map(|c| if let Call::Show(_, image) = c { Some(image.clone()) } else { None }).collect()
    }

    fn is_empty(&self) -> bool {
        self.calls.lock().unwrap().is_empty()
    }
}

struct App {
    log: Arc<Recording>,
    sheet_face: Option<[u32; 4]>,
    px_per_m: f32,
}

impl UnderlayTarget for App {
    fn head(&self, character: &str) -> Result<UnderlayHead, UnderlayTargetError> {
        if character != "mira" {
            return Err(UnderlayTargetError::NotFound(format!("the app shows no character {character}")));
        }
        Ok(UnderlayHead {
            projection: projection(),
            head_centre: Vec3::ZERO,
            px_per_m: self.px_per_m,
            sheet_face: self.sheet_face,
        })
    }

    fn shown(&self, _character: &str) -> Option<String> {
        self.log.shown_key.lock().unwrap().clone()
    }

    fn show(&self, character: &str, image: UnderlayImage) -> Result<(), UnderlayTargetError> {
        *self.log.shown_key.lock().unwrap() = Some(image.key.clone());
        self.log.calls.lock().unwrap().push(Call::Show(character.to_owned(), image));
        Ok(())
    }

    fn set_opacity(&self, character: &str, opacity: f32) -> Result<(), UnderlayTargetError> {
        self.log.calls.lock().unwrap().push(Call::Opacity(character.to_owned(), opacity));
        Ok(())
    }
}

/// A 16 x 16 sheet whose pixel (x, y) is (16x, 16y, 0, 255).
fn sheet_png() -> Vec<u8> {
    let image =
        RgbaImage::from_fn(16, 16, |x, y| Rgba([u8::try_from(x * 16).unwrap(), u8::try_from(y * 16).unwrap(), 0, 255]));
    let mut bytes = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
    bytes
}

fn face_rect() -> Value {
    json!({ "x": 0, "y": 0, "width": 8, "height": 8 })
}

struct Rig {
    store: Arc<MemStore>,
    log: Arc<Recording>,
    services: omuzaki_ops_api::Services,
    asset: ContentHash,
}

fn rig(locked_face: Option<[u32; 4]>) -> Rig {
    rig_at(locked_face, PX_PER_M)
}

fn rig_at(locked_face: Option<[u32; 4]>, px_per_m: f32) -> Rig {
    let store = Arc::new(MemStore::new());
    let asset = store.put(&sheet_png(), sample_provenance(Commercial::Ok)).unwrap();
    let log = Arc::new(Recording::default());
    let app: Arc<dyn UnderlayTarget> = Arc::new(App { log: log.clone(), sheet_face: locked_face, px_per_m });
    let dyn_store: Arc<dyn Store> = store.clone();
    let services = ServicesBuilder::new().insert::<dyn UnderlayTarget>(app).insert::<dyn Store>(dyn_store).build();
    Rig { store, log, services, asset }
}

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

fn decode(args: Value) -> Result<Box<dyn omuzaki_ops_api::DynOp>, OpError> {
    omuzaki_face::link();
    (find("face.underlay.set").expect("registered").decode)(args)
}

/// Only the plan: what the executor does under the lock, and all a dry run does.
fn plan(rig: &Rig, args: Value, dry_run: bool) -> Result<Plan, OpError> {
    let op = decode(args)?;
    let doc = MemDoc::from_value(json!({}));
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(&doc, &actor, &clock).with_services(&rig.services).with_dry_run(dry_run))
}

/// The plan, then the job's work, as the executor runs them. Returns what `work` answers.
fn run(rig: &Rig, args: Value) -> Result<Value, OpError> {
    plan(rig, args.clone(), false)?;
    let op = decode(args)?;
    let (actor, doc, signal) = (Actor::Person { name: "tester".into() }, Doc(MemDoc::from_value(json!({}))), Signal::default());
    op.work(&JobCtx::new(JobId(1), &actor, &doc, &signal).with_services(&rig.services))
}

fn args(rig: &Rig, extra: &Value) -> Value {
    let mut base = json!({
        "character": "mira",
        "asset": rig.asset.to_string(),
        "opacity": 0.5,
        "align": { "offset": [0.0, 0.0], "scale": 1.0, "rotation": 0.0 },
        "sheet_face": face_rect(),
    });
    for (key, value) in extra.as_object().unwrap() {
        base[key] = value.clone();
    }
    base
}

#[test]
fn the_op_is_a_job_and_the_job_builds_the_layer_and_hands_it_to_the_app() {
    let rig = rig(None);
    assert_eq!(decode(args(&rig, &json!({}))).unwrap().meta().kind, omuzaki_ops_api::OpKind::Job);
    let out = run(&rig, args(&rig, &json!({}))).expect("sets the underlay");
    assert_eq!(out["rebuilt"], json!(true));
    assert_eq!(out["view"]["opacity"], json!(0.5));
    let min = out["view"]["min"][0].as_f64().unwrap();
    assert!((min + 0.2).abs() < 1e-6, "the window starts at the Base layer's corner: {min}");

    let shown = rig.log.shows();
    assert_eq!(shown.len(), 1);
    let image = &shown[0];
    assert_eq!(image.opacity, 0.5);
    assert_eq!((image.size.x, image.size.y), (8, 8));
    assert_eq!(image.rgba.len(), 8 * 8 * 4);
    // Identity at the sheet's scale is a 1:1 copy of the face rectangle: layer pixel (3, 2) is sheet pixel (3, 2).
    let at = (2 * 8 + 3) * 4;
    assert_eq!(&image.rgba[at..at + 4], &[48, 32, 0, 255]);
}

#[test]
fn planning_reads_nothing_from_the_store_and_shows_nothing() {
    // The asset does not exist, and the plan still succeeds: the read, decode and resample belong to the job.
    let rig = rig(None);
    let missing = args(&rig, &json!({ "asset": "1".repeat(64) }));
    let planned = plan(&rig, missing.clone(), false).expect("plans without touching the store");
    assert_eq!(planned.output.expect("answers")["rebuild"], json!(true));
    assert!(rig.log.is_empty());
    // The job is what finds out.
    assert_eq!(run(&rig, missing).unwrap_err().code, ErrorCode::NotFound);
}

#[test]
fn a_dry_run_checks_what_it_can_and_shows_nothing() {
    let rig = rig(None);
    let planned = plan(&rig, args(&rig, &json!({})), true).expect("plans");
    assert_eq!(planned.output.expect("answers")["dry_run"], json!(true));
    assert_eq!(planned.patch.changes.len(), 0, "view state: the project is untouched");
    assert!(rig.log.is_empty());
    let bad = plan(&rig, args(&rig, &json!({ "opacity": 2.0 })), true).unwrap_err();
    assert_eq!(bad.field.as_deref(), Some("opacity"), "a dry run still refuses a bad request");
}

#[test]
fn a_new_opacity_for_the_same_sheet_and_alignment_sets_only_the_opacity() {
    let rig = rig(None);
    run(&rig, args(&rig, &json!({}))).expect("first call builds");
    let planned = plan(&rig, args(&rig, &json!({ "opacity": 0.25 })), false).unwrap();
    assert_eq!(planned.output.unwrap()["rebuild"], json!(false), "the plan says nothing is rebuilt");

    let out = run(&rig, args(&rig, &json!({ "opacity": 0.25 }))).expect("second call");
    assert_eq!(out["rebuilt"], json!(false));
    assert_eq!(out["view"]["opacity"], json!(0.25));
    assert_eq!(rig.log.shows().len(), 1, "no second build");
    assert_eq!(rig.log.calls.lock().unwrap().last(), Some(&Call::Opacity("mira".into(), 0.25)));

    // Another alignment changes the pixels, so it builds again.
    let moved = json!({ "align": { "offset": [1.0, 0.0], "scale": 1.0, "rotation": 0.0 }, "opacity": 0.25 });
    assert_eq!(run(&rig, args(&rig, &moved)).unwrap()["rebuilt"], json!(true));
    assert_eq!(rig.log.shows().len(), 2);
}

#[test]
fn the_sheet_face_comes_from_the_locked_sheet_unless_the_request_gives_one() {
    let rig = rig(Some([8, 8, 8, 8]));
    let mut without = args(&rig, &json!({}));
    without.as_object_mut().unwrap().remove("sheet_face");
    run(&rig, without).expect("the locked sheet's rectangle is used");
    run(&rig, args(&rig, &json!({}))).expect("the request's rectangle wins");
    let shown = rig.log.shows();
    // Layer pixel (0, 0) reads sheet (8, 8) from the locked rectangle, and sheet (0, 0) from the request's.
    assert_eq!(&shown[0].rgba[..4], &[128, 128, 0, 255]);
    assert_eq!(&shown[1].rgba[..4], &[0, 0, 0, 255]);
}

#[test]
fn the_op_refuses_what_it_cannot_build_and_names_the_field() {
    let rig = rig(None);
    let mut none = args(&rig, &json!({}));
    none.as_object_mut().unwrap().remove("sheet_face");
    assert_eq!(plan(&rig, none, false).unwrap_err().field.as_deref(), Some("sheet_face"));

    let not_an_image = rig.store.put(b"not an image", sample_provenance(Commercial::Ok)).unwrap();
    let err = run(&rig, args(&rig, &json!({ "asset": not_an_image.to_string() }))).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("asset"), "{err:?}");

    let err = plan(&rig, args(&rig, &json!({ "character": "nobody" })), false).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(rig.log.is_empty(), "a refused request shows nothing");
}

#[test]
fn the_failing_field_is_kept_not_flattened() {
    let rig = rig(None);
    // A rectangle outside the sheet is found once the sheet is read, in the job.
    let outside = json!({ "x": 10, "y": 0, "width": 8, "height": 8 });
    let err = run(&rig, args(&rig, &json!({ "sheet_face": outside }))).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("sheet_face"), "{err:?}");
    assert!(err.message.contains("sheet_face"), "{err:?}");

    // An empty rectangle too.
    let empty = json!({ "x": 0, "y": 0, "width": 0, "height": 8 });
    let err = run(&rig, args(&rig, &json!({ "sheet_face": empty }))).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("sheet_face"), "{err:?}");

    // The app's density is the argument to fix for a layer that would be too large; a plan already says so.
    let dense = self::rig_at(None, 1.0e6);
    let err = plan(&dense, args(&dense, &json!({})), false).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("px_per_m"), "{err:?}");

    // Alignment fields keep their names.
    let bad = json!({ "align": { "offset": [0.0, 0.0], "scale": 0.0, "rotation": 0.0 } });
    assert_eq!(plan(&rig, args(&rig, &bad), false).unwrap_err().field.as_deref(), Some("align.scale"));
    assert!(rig.log.is_empty());
}

#[test]
fn the_op_needs_the_app_to_provide_its_services() {
    let op = decode(json!({
        "character": "mira",
        "asset": "0".repeat(64),
        "opacity": 1.0,
        "align": { "offset": [0.0, 0.0], "scale": 1.0, "rotation": 0.0 },
    }))
    .unwrap();
    let doc = MemDoc::from_value(json!({}));
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    let err = op.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap_err();
    assert_eq!(err.code, ErrorCode::Unavailable);
}
