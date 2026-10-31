//! The `paint.*` ops end to end on the in-memory runner: a painting made, layered, drawn mark by mark, undone and
//! redone, rendered and sampled, edited, and exported as layered art; and the refusals an agent can act on.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, CancelFlag, ErrorCode, Grants, JobCtx, JobDoc, JobId, JobSignal, Mode, OpCtx, OpError, OpKind, ProjectDir,
    Request, Runner, Services, ServicesBuilder, find,
};
use omuzaki_ops_mock::MockRunner;
use omuzaki_store_api::{ContentHash, Store};
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

struct Doc(MemDoc);

impl JobDoc for Doc {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(&self.0);
    }
}

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

fn person() -> Actor {
    Actor::Person { name: "tester".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "painter".into(), model: None }
}

struct Rig {
    runner: MockRunner<MemDoc>,
}

impl Rig {
    fn new() -> Self {
        omuzaki_paint_ops::link();
        Self { runner: MockRunner::new(MemDoc::default()) }
    }

    fn run_as(&self, actor: Actor, op: &str, args: Value) -> Result<Value, OpError> {
        let grants = if matches!(actor, Actor::Person { .. }) { Grants::person() } else { Grants::agent_default() };
        let request =
            Request { op: op.into(), args, mode: Mode::Apply, actor, grants, cause: None, tx: None, base: None };
        Ok(self.runner.run(request)?.plan.output.unwrap_or(Value::Null))
    }

    fn run(&self, op: &str, args: Value) -> Result<Value, OpError> {
        self.run_as(person(), op, args)
    }

    /// The document as it is now, for a job to read.
    fn doc(&self) -> MemDoc {
        MemDoc::from_value(self.runner.get(&DocPath::root(), &Grants::person()).unwrap().unwrap_or(json!({})))
    }

    /// Plans then works a job op, as the executor does, with `services` and, if given, a project folder.
    fn job(&self, op: &str, args: Value, services: &Services, project: Option<&ProjectDir>) -> Result<Value, OpError> {
        let reg = find(op).expect("registered");
        assert_eq!((reg.meta)().kind, OpKind::Job, "{op} is a job");
        let decoded = (reg.decode)(args)?;
        let (doc, actor, clock, grants) = (self.doc(), person(), FakeClock::default(), Grants::person());
        let mut ctx = OpCtx::new(&doc, &actor, &clock).with_grants(&grants);
        if let Some(p) = project {
            ctx = ctx.with_project_dir(p);
        }
        let planned = decoded.plan(&ctx)?.output.expect("a job plan answers");
        let signal = Signal(CancelFlag::default());
        let jdoc = Doc(doc);
        let mut job = JobCtx::new(JobId(1), &actor, &jdoc, &signal)
            .with_services(services)
            .with_grants(&grants)
            .with_plan_output(&planned);
        if let Some(p) = project {
            job = job.with_project_dir(p);
        }
        decoded.work(&job)
    }
}

fn with_store() -> (Arc<MemStore>, Services) {
    let store = Arc::new(MemStore::new());
    (store.clone(), ServicesBuilder::new().insert::<dyn Store>(store).build())
}

/// A 64 by 64 painting with line art (a circle), flats bucket-filled inside it, and a multiply shadow clipped to them.
fn layered(rig: &Rig) {
    rig.run("paint.new", json!({ "id": "face", "name": "A face", "width": 64, "height": 64, "background": "#000000" }))
        .unwrap();
    rig.run("paint.add_layer", json!({ "painting": "face", "id": "lines", "name": "Line art" })).unwrap();
    rig.run("paint.add_layer", json!({ "painting": "face", "id": "flats", "below": "lines" })).unwrap();
    rig.run(
        "paint.add_layer",
        json!({ "painting": "face", "id": "shadow", "above": "flats", "clip": true, "blend": "multiply" }),
    )
    .unwrap();
    rig.run_as(
        agent(),
        "paint.draw",
        json!({ "painting": "face", "layer": "lines", "marks": [
        { "type": "shape", "geometry": { "kind": "ellipse", "cx": 32, "cy": 32, "rx": 24, "ry": 24 },
          "fill": "#00000000", "outline": { "colour": "#1b1420", "width": 2 }, "tag": "head" }
    ] }),
    )
    .unwrap();
    rig.run_as(
        agent(),
        "paint.draw",
        json!({ "painting": "face", "layer": "flats", "marks": [
        { "type": "bucket", "at": [32, 32], "fill": "#f3d9c8", "sample": ["lines"], "tag": "skin" }
    ] }),
    )
    .unwrap();
    rig.run_as(agent(), "paint.draw", json!({ "painting": "face", "layer": "shadow", "marks": [
        { "type": "shape", "geometry": { "kind": "rect", "x": 0, "y": 40, "w": 64, "h": 24 }, "fill": "#c08080", "tag": "jaw" }
    ] }))
    .unwrap();
}

fn decode_png(store: &MemStore, answer: &Value) -> image::RgbaImage {
    let hash: ContentHash = answer["hash"].as_str().unwrap().parse().unwrap();
    image::load_from_memory(&store.get(&hash).unwrap()).unwrap().to_rgba8()
}

#[test]
fn every_op_is_registered_with_its_kind() {
    omuzaki_paint_ops::link();
    let jobs = ["paint.render", "paint.sample", "paint.export_layers"];
    let queries = ["paint.list", "paint.get", "paint.brushes"];
    let edits = [
        "paint.new",
        "paint.delete",
        "paint.add_layer",
        "paint.set_layer",
        "paint.remove_layer",
        "paint.move_layer",
        "paint.draw",
        "paint.replace_mark",
        "paint.remove_marks",
        "paint.set_mask",
    ];
    for (names, kind) in [(&jobs[..], OpKind::Job), (&queries[..], OpKind::Query), (&edits[..], OpKind::Edit)] {
        for name in names {
            assert_eq!((find(name).unwrap_or_else(|| panic!("{name}")).meta)().kind, kind, "{name}");
        }
    }
}

#[test]
fn a_painting_drawn_mark_by_mark_renders_its_layers() {
    let rig = Rig::new();
    layered(&rig);
    let (store, services) = with_store();
    let answer = rig.job("paint.render", json!({ "painting": "face" }), &services, None).unwrap();
    assert_eq!(answer["uri"].as_str().unwrap(), format!("omuzaki://render/{}.png", answer["hash"].as_str().unwrap()));
    let png = decode_png(&store, &answer);
    assert_eq!(png.dimensions(), (64, 64));
    // Inside the head: the flat skin; inside the head and the jaw rect: skin times the shadow; outside: the backdrop.
    assert_eq!(png.get_pixel(32, 25).0, [0xf3, 0xd9, 0xc8, 255]);
    let jaw = png.get_pixel(32, 50).0;
    let want = |a: u8, b: u8| u8::try_from((u16::from(a) * u16::from(b) + 127) / 255).unwrap();
    for (c, (a, b)) in jaw.iter().zip([(0xf3, 0xc0), (0xd9, 0x80), (0xc8, 0x80)]) {
        assert!((i16::from(*c) - i16::from(want(a, b))).abs() <= 1, "{jaw:?}");
    }
    assert_eq!(png.get_pixel(2, 50).0, [0, 0, 0, 255], "the shadow is clipped to the flats");
    assert_eq!(png.get_pixel(32, 8).0[..3], [0x1b, 0x14, 0x20], "the line art");

    let sampled =
        rig.job("paint.sample", json!({ "painting": "face", "points": [[32, 25], [2, 2]] }), &services, None).unwrap();
    assert_eq!(sampled["colours"], json!(["#f3d9c8ff", "#000000ff"]));
    let flats_only = rig
        .job("paint.sample", json!({ "painting": "face", "points": [[2, 2]], "layer": "flats" }), &services, None)
        .unwrap();
    assert_eq!(flats_only["colours"], json!(["#00000000"]));
}

#[test]
fn a_render_crops_and_scales() {
    let rig = Rig::new();
    layered(&rig);
    let (store, services) = with_store();
    let a = rig
        .job("paint.render", json!({ "painting": "face", "crop": [16, 16, 8, 4], "scale": 4 }), &services, None)
        .unwrap();
    assert_eq!(decode_png(&store, &a).dimensions(), (32, 16));
    let b = rig.job("paint.render", json!({ "painting": "face", "scale": 0.25 }), &services, None).unwrap();
    assert_eq!(decode_png(&store, &b).dimensions(), (16, 16));
    let e =
        rig.job("paint.render", json!({ "painting": "face", "crop": [60, 60, 8, 8] }), &services, None).unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
}

#[test]
fn each_draw_is_one_step_that_undoes_and_redoes() {
    let rig = Rig::new();
    layered(&rig);
    let marks = DocPath::parse("paintings/face/layers/lines/marks").unwrap();
    let count = |rig: &Rig| rig.runner.get(&marks, &Grants::person()).unwrap().unwrap().as_array().unwrap().len();
    let answer = rig
        .run("paint.draw", json!({ "painting": "face", "layer": "lines", "marks": [
            { "type": "stroke", "brush": "g_pen", "colour": "#1b1420", "size": 3, "points": [[20, 30], [32, 34, 0.5], [44, 30]], "tag": "mouth" },
            { "type": "stroke", "colour": "#1b1420", "size": 2, "points": [[22, 22]], "tag": "dot" }
        ] }))
        .unwrap();
    assert_eq!((answer["first"].as_u64(), answer["count"].as_u64()), (Some(1), Some(2)));
    assert_eq!(count(&rig), 3);
    assert!(rig.runner.undo(&person(), &Grants::person()).unwrap().is_some());
    assert_eq!(count(&rig), 1);
    assert!(rig.runner.redo(&person(), &Grants::person()).unwrap().is_some());
    assert_eq!(count(&rig), 3);
}

#[test]
fn marks_are_replaced_and_removed_by_index_or_tag() {
    let rig = Rig::new();
    layered(&rig);
    rig.run(
        "paint.draw",
        json!({ "painting": "face", "layer": "lines", "marks": [
        { "type": "stroke", "colour": "#ff0000", "size": 2, "points": [[10, 10], [20, 10]], "tag": "a" },
        { "type": "stroke", "colour": "#ff0000", "size": 2, "points": [[10, 20], [20, 20]], "tag": "b" },
        { "type": "stroke", "colour": "#ff0000", "size": 2, "points": [[10, 30], [20, 30]], "tag": "b" }
    ] }),
    )
    .unwrap();
    rig.run(
        "paint.replace_mark",
        json!({ "painting": "face", "layer": "lines", "index": 2, "mark":
        { "type": "stroke", "colour": "#00ff00", "size": 2, "points": [[10, 20], [20, 20]], "tag": "c" } }),
    )
    .unwrap();
    let removed = rig.run("paint.remove_marks", json!({ "painting": "face", "layer": "lines", "tag": "b" })).unwrap();
    assert_eq!((removed["removed"].as_u64(), removed["left"].as_u64()), (Some(1), Some(3)));
    let got = rig.run("paint.get", json!({ "painting": "face", "layer": "lines" })).unwrap();
    let tags: Vec<&str> = got["marks"].as_array().unwrap().iter().map(|m| m["mark"]["tag"].as_str().unwrap()).collect();
    assert_eq!(tags, ["head", "a", "c"]);
    rig.run("paint.remove_marks", json!({ "painting": "face", "layer": "lines", "from": 1, "count": 2 })).unwrap();
    let e = rig
        .run("paint.remove_marks", json!({ "painting": "face", "layer": "lines", "from": 1, "count": 1 }))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
}

#[test]
fn layers_are_set_moved_grouped_masked_and_removed() {
    let rig = Rig::new();
    layered(&rig);
    rig.run("paint.add_layer", json!({ "painting": "face", "id": "hair", "kind": "group" })).unwrap();
    rig.run("paint.add_layer", json!({ "painting": "face", "id": "fringe", "parent": "hair", "role": "hair_front" }))
        .unwrap();
    rig.run("paint.move_layer", json!({ "painting": "face", "layer": "lines", "parent": "hair" })).unwrap();
    let got = rig.run("paint.get", json!({ "painting": "face" })).unwrap();
    let order: Vec<&str> = got["layers"].as_array().unwrap().iter().map(|l| l["id"].as_str().unwrap()).collect();
    assert_eq!(order, ["flats", "shadow", "hair", "fringe", "lines"]);
    assert_eq!(got["layers"][4]["parent"], "hair");
    rig.run("paint.set_layer", json!({ "painting": "face", "layer": "shadow", "opacity": 0.5, "visible": false }))
        .unwrap();
    rig.run(
        "paint.set_mask",
        json!({ "painting": "face", "layer": "flats", "marks": [
        { "type": "shape", "geometry": { "kind": "rect", "x": 0, "y": 0, "w": 32, "h": 64 }, "fill": "#ffffff" }
    ] }),
    )
    .unwrap();
    let (_, services) = with_store();
    let s = rig
        .job("paint.sample", json!({ "painting": "face", "points": [[24, 32], [40, 32]] }), &services, None)
        .unwrap();
    assert_eq!(s["colours"], json!(["#f3d9c8ff", "#000000ff"]), "the mask hides the right half");
    let removed = rig.run("paint.remove_layer", json!({ "painting": "face", "layer": "hair" })).unwrap_err();
    assert_eq!(removed.code, ErrorCode::Invalid, "the flats' bucket samples the line art inside the group");
    rig.run("paint.move_layer", json!({ "painting": "face", "layer": "lines" })).unwrap();
    let removed = rig.run("paint.remove_layer", json!({ "painting": "face", "layer": "hair" })).unwrap();
    assert_eq!(removed["removed"], json!(["hair", "fringe"]));
}

#[test]
fn refusals_name_what_to_fix() {
    let rig = Rig::new();
    let e = rig.run("paint.draw", json!({ "painting": "nope", "layer": "x", "marks": [] })).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    layered(&rig);
    let bad_point = rig
        .run(
            "paint.draw",
            json!({ "painting": "face", "layer": "lines", "marks": [
            { "type": "stroke", "colour": "#000000", "size": 2, "points": [[1, 2], [3]] }
        ] }),
        )
        .unwrap_err();
    assert_eq!(bad_point.code, ErrorCode::Invalid);
    assert!(format!("{bad_point:?}").contains("marks/0/points/1"), "{bad_point:?}");
    let bad_brush = rig
        .run(
            "paint.draw",
            json!({ "painting": "face", "layer": "lines", "marks": [
            { "type": "stroke", "brush": "crayon", "colour": "#000000", "size": 2, "points": [[1, 2]] }
        ] }),
        )
        .unwrap_err();
    assert!(format!("{bad_brush:?}").contains("g_pen"), "the refusal lists the presets: {bad_brush:?}");
    let looped = rig
        .run(
            "paint.draw",
            json!({ "painting": "face", "layer": "lines", "marks": [
            { "type": "bucket", "at": [1, 1], "fill": "#ffffff", "sample": ["flats"] }
        ] }),
        )
        .unwrap_err();
    assert_eq!(looped.code, ErrorCode::Invalid, "lines would sample flats, which samples lines");
    let dup = rig.run("paint.new", json!({ "id": "face", "name": "again", "width": 8, "height": 8 })).unwrap_err();
    assert_eq!(dup.code, ErrorCode::Conflict);
    let huge = rig.run("paint.new", json!({ "id": "big", "name": "big", "width": 9000, "height": 8 })).unwrap_err();
    assert_eq!(huge.code, ErrorCode::Invalid);
}

#[test]
fn brushes_lists_every_preset_and_blend() {
    let rig = Rig::new();
    let b = rig.run("paint.brushes", json!({})).unwrap();
    let names: Vec<&str> = b["brushes"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    for want in ["ink", "g_pen", "airbrush", "pencil", "blender", "eraser"] {
        assert!(names.contains(&want), "{want}");
    }
    assert_eq!(b["blend_modes"].as_array().unwrap().len(), 18);
}

#[test]
#[allow(clippy::disallowed_methods)] // reads back the files the export wrote into a temporary folder; no op runs here
fn layered_art_exports_for_the_puppet() {
    let rig = Rig::new();
    layered(&rig);
    rig.run("paint.set_layer", json!({ "painting": "face", "layer": "flats", "role": "face" })).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let project = ProjectDir::open(dir.path()).unwrap();
    let (_, services) = with_store();
    let answer = rig
        .job("paint.export_layers", json!({ "painting": "face", "folder": "art" }), &services, Some(&project))
        .unwrap();
    assert_eq!(answer["parts"], 2, "flats with its clipped shadow, then the line art");
    let manifest: toml::Value =
        toml::from_str(&std::fs::read_to_string(dir.path().join("art/face.layers.toml")).unwrap()).unwrap();
    assert_eq!(manifest["canvas"].as_array().unwrap().len(), 2);
    let layers = manifest["layer"].as_array().unwrap();
    assert_eq!(layers[0]["role"].as_str(), Some("face"));
    assert_eq!(layers[0]["file"].as_str(), Some("flats.png"));
    let png = image::open(dir.path().join("art/flats.png")).unwrap().to_rgba8();
    let offset: Vec<i64> = layers[0]["offset"].as_array().unwrap().iter().map(|v| v.as_integer().unwrap()).collect();
    // The flats are cropped to the head: the fill grows one pixel under the 2 px line of a radius-24 circle.
    assert!(png.width() < 64 && offset[0] > 0, "{} {offset:?}", png.width());
}
