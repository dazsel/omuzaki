//! `face.underlay.set` through the executor (not the op's methods called by hand): a request runs as a job off the
//! document lock, reaches the app, and leaves no undo step, because it changes the view and not the project.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use image::{ImageFormat, Rgba, RgbaImage};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::SystemClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::underlay::{UnderlayHead, UnderlayImage, UnderlayTarget, UnderlayTargetError};
use omuzaki_face_api::{FaceProjection, FaceRect};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Actor, Grants, Mode, OpKind, Request, Runner, ServicesBuilder, find};
use omuzaki_store_api::{Commercial, Store};
use omuzaki_store_mock::{MemStore, conformance::sample_provenance};
use serde_json::{Value, json};

/// Records what the app is asked to show.
#[derive(Default)]
struct App {
    shown: Mutex<Vec<UnderlayImage>>,
}

impl UnderlayTarget for App {
    fn head(&self, _character: &str) -> Result<UnderlayHead, UnderlayTargetError> {
        Ok(UnderlayHead {
            projection: FaceProjection {
                rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.2, y1: 0.2 },
                front_fade: [-1.0, -0.5],
            },
            head_centre: Vec3::ZERO,
            px_per_m: 20.0,
            sheet_face: Some([0, 0, 8, 8]),
        })
    }

    fn shown(&self, _character: &str) -> Option<String> {
        self.shown.lock().unwrap().last().map(|image| image.key.clone())
    }

    fn set_opacity(&self, _c: &str, _ticket: u64, _key: &str, _opacity: f32) -> Result<bool, UnderlayTargetError> {
        Ok(false)
    }

    fn show(&self, _character: &str, _ticket: u64, image: UnderlayImage) -> Result<(), UnderlayTargetError> {
        self.shown.lock().unwrap().push(image);
        Ok(())
    }
}

fn request(op: &str, args: Value, mode: Mode, actor: &Actor) -> Request {
    Request {
        op: op.to_owned(),
        args,
        mode,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    }
}

#[test]
fn a_view_state_job_runs_through_the_executor_and_leaves_no_undo_step() {
    omuzaki_face::link();
    let registration = find("face.underlay.set").expect("registered");
    let meta = (registration.meta)();
    assert_eq!(meta.kind, OpKind::Job, "the read, decode and resample run off the document lock");

    let store = Arc::new(MemStore::new());
    let sheet = RgbaImage::from_pixel(16, 16, Rgba([10, 20, 30, 255]));
    let mut png = Vec::new();
    sheet.write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png).unwrap();
    let asset = store.put(&png, sample_provenance(Commercial::Ok)).unwrap();
    let app = Arc::new(App::default());
    let (target, dyn_store): (Arc<dyn UnderlayTarget>, Arc<dyn Store>) = (app.clone(), store);
    let services = ServicesBuilder::new().insert::<dyn UnderlayTarget>(target).insert::<dyn Store>(dyn_store).build();
    let runner = Executor::with_registrations(
        Box::new(MemDoc::from_value(json!({}))),
        Arc::new(SystemClock::new()),
        [registration],
    )
    .expect("registers")
    .with_services(services);
    let actor = Actor::Person { name: "tester".into() };
    let args = json!({
        "character": "mira",
        "asset": asset.to_string(),
        "opacity": 0.5,
        "align": { "offset": [0.0, 0.0], "scale": 1.0, "rotation": 0.0 },
    });

    // A dry run answers and shows nothing.
    let dry = runner.run(request("face.underlay.set", args.clone(), Mode::DryRun, &actor)).expect("dry-run");
    assert!(dry.applied.is_none());
    assert!(app.shown.lock().unwrap().is_empty());

    // Applying runs the job to its end and the app shows the layer.
    runner.run(request("face.underlay.set", args, Mode::Apply, &actor)).expect("runs");
    let shown = app.shown.lock().unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!((shown[0].size.x, shown[0].size.y), (8, 8));
    drop(shown);

    // The document did not change, so there is nothing to undo; `undo` must not reach back past this to another edit.
    assert_eq!(runner.undo(&actor, &Grants::person()).expect("undo answers"), None);
}
