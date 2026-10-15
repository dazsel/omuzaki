//! `face.underlay.set` (#668) against an in-memory store and a recording `UnderlayTarget`: what it builds, what it hands
//! the app, its dry run, and its refusals. Synthetic sheets only.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use image::{ImageFormat, Rgba, RgbaImage};
use omuzaki_char_api::CharId;
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::underlay::{UnderlayHead, UnderlayLayer, UnderlayTarget};
use omuzaki_face_api::{FaceProjection, FaceRect};
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, Plan, ServicesBuilder, find};
use omuzaki_render_api::PixelRect;
use omuzaki_store_api::{Commercial, ContentHash, Store};
use omuzaki_store_mock::{MemStore, conformance::sample_provenance};
use serde_json::{Value, json};

/// A 0.4 m face window at 20 px/m: the Base layer is 8 x 8 px.
const PX_PER_M: f32 = 20.0;

fn projection() -> FaceProjection {
    FaceProjection { rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.2, y1: 0.2 }, front_fade: [-1.0, -0.5] }
}

/// What the app was asked to show.
#[derive(Default)]
struct Recording(Mutex<Vec<(CharId, UnderlayLayer)>>);

struct App {
    shown: Arc<Recording>,
    sheet_face: Option<PixelRect>,
}

impl UnderlayTarget for App {
    fn head(&self, character: &CharId) -> Result<UnderlayHead, OpError> {
        if character.as_str() != "mira" {
            return Err(OpError::not_found(format!("the app shows no character {character}")));
        }
        Ok(UnderlayHead {
            projection: projection(),
            head_centre: Vec3::ZERO,
            px_per_m: PX_PER_M,
            sheet_face: self.sheet_face,
        })
    }

    fn show(&self, character: &CharId, layer: UnderlayLayer) -> Result<(), OpError> {
        self.shown.0.lock().unwrap().push((character.clone(), layer));
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
    shown: Arc<Recording>,
    services: omuzaki_ops_api::Services,
    asset: ContentHash,
}

fn rig(locked_face: Option<PixelRect>) -> Rig {
    let store = Arc::new(MemStore::new());
    let asset = store.put(&sheet_png(), sample_provenance(Commercial::Ok)).unwrap();
    let shown = Arc::new(Recording::default());
    let app: Arc<dyn UnderlayTarget> = Arc::new(App { shown: shown.clone(), sheet_face: locked_face });
    let dyn_store: Arc<dyn Store> = store.clone();
    let services = ServicesBuilder::new().insert::<dyn UnderlayTarget>(app).insert::<dyn Store>(dyn_store).build();
    Rig { store, shown, services, asset }
}

fn run(rig: &Rig, args: Value, dry_run: bool) -> Result<Plan, OpError> {
    omuzaki_face::link();
    let op = (find("face.underlay.set").expect("registered").decode)(args)?;
    let doc = MemDoc::from_value(json!({}));
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(&doc, &actor, &clock).with_services(&rig.services).with_dry_run(dry_run))
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
fn the_op_builds_the_layer_and_hands_it_to_the_app() {
    let rig = rig(None);
    let plan = run(&rig, args(&rig, &json!({})), false).expect("sets the underlay");
    assert_eq!(plan.patch.changes.len(), 0, "view state: the project is untouched");
    let out = plan.output.expect("answers");
    assert_eq!(out["view"]["opacity"], json!(0.5));
    let min = out["view"]["min"][0].as_f64().unwrap();
    assert!((min + 0.2).abs() < 1e-6, "the window starts at the Base layer's corner: {min}");

    let shown = rig.shown.0.lock().unwrap();
    assert_eq!(shown.len(), 1);
    let (who, layer) = &shown[0];
    assert_eq!(who.as_str(), "mira");
    assert_eq!(layer.opacity(), 0.5);
    // Identity at the sheet's scale is a 1:1 copy of the face rectangle: layer pixel (3, 2) is sheet pixel (3, 2).
    let at = (2 * 8 + 3) * 4;
    assert_eq!(&layer.rgba()[at..at + 4], &[48, 32, 0, 255]);
}

#[test]
fn a_dry_run_checks_everything_and_shows_nothing() {
    let rig = rig(None);
    let plan = run(&rig, args(&rig, &json!({})), true).expect("plans");
    assert_eq!(plan.output.expect("answers")["dry_run"], json!(true));
    assert!(rig.shown.0.lock().unwrap().is_empty());
    let bad = run(&rig, args(&rig, &json!({ "opacity": 2.0 })), true).unwrap_err();
    assert_eq!(bad.field.as_deref(), Some("opacity"), "a dry run still refuses a bad request");
}

#[test]
fn the_sheet_face_comes_from_the_locked_sheet_unless_the_request_gives_one() {
    let locked = PixelRect { x: 8, y: 8, width: 8, height: 8 };
    let rig = rig(Some(locked));
    let mut without = args(&rig, &json!({}));
    without.as_object_mut().unwrap().remove("sheet_face");
    run(&rig, without, false).expect("the locked sheet's rectangle is used");
    run(&rig, args(&rig, &json!({})), false).expect("the request's rectangle wins");
    let shown = rig.shown.0.lock().unwrap();
    // Layer pixel (0, 0) reads sheet (8, 8) from the locked rectangle, and sheet (0, 0) from the request's.
    assert_eq!(&shown[0].1.rgba()[..4], &[128, 128, 0, 255]);
    assert_eq!(&shown[1].1.rgba()[..4], &[0, 0, 0, 255]);
}

#[test]
fn the_op_refuses_what_it_cannot_build_and_names_the_field() {
    let rig = rig(None);
    let mut none = args(&rig, &json!({}));
    none.as_object_mut().unwrap().remove("sheet_face");
    assert_eq!(run(&rig, none, false).unwrap_err().field.as_deref(), Some("sheet_face"));

    let missing = run(&rig, args(&rig, &json!({ "asset": "1".repeat(64) })), false).unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);

    let not_an_image = rig.store.put(b"not an image", sample_provenance(Commercial::Ok)).unwrap();
    let err = run(&rig, args(&rig, &json!({ "asset": not_an_image.to_string() })), false).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("asset"), "{err:?}");

    let outside = json!({ "x": 10, "y": 0, "width": 8, "height": 8 });
    let err = run(&rig, args(&rig, &json!({ "sheet_face": outside })), false).unwrap_err();
    assert!(err.message.contains("sheet_face"), "{err:?}");

    let err = run(&rig, args(&rig, &json!({ "character": "nobody" })), false).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(rig.shown.0.lock().unwrap().is_empty(), "a refused request shows nothing");
}

#[test]
fn the_op_needs_the_app_to_provide_its_services() {
    omuzaki_face::link();
    let op = (find("face.underlay.set").expect("registered").decode)(json!({
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
