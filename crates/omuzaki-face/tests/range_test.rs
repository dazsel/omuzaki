//! The expression range test (#170): the measure on the default face with the starter set, a look-alike pair it must
//! flag, an expression that reads as the rest face, determinism, and the `face.expression.range_test` job with its
//! sheets and refusals.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_anim_api::Channel;
use omuzaki_char_api::HeadParams;
use omuzaki_core::colour::Colour;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::preview::FaceSubject;
use omuzaki_face::preview::range::{RangeOptions, compare, paint_all};
use omuzaki_face_api::{ExpressionId, ExpressionSet, FaceProfile, Palette};
use omuzaki_ops_api::{
    Actor, CancelFlag, ErrorCode, JobCtx, JobDoc, JobId, JobSignal, OpCtx, OpError, Services, ServicesBuilder, find,
};
use omuzaki_store_api::{ContentHash, Store};
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

fn id(name: &str) -> ExpressionId {
    ExpressionId::new(name).unwrap()
}

/// The default face with `set` as its expressions.
fn subject(set: ExpressionSet) -> FaceSubject {
    let profile = FaceProfile { expressions: set, ..FaceProfile::default() };
    FaceSubject {
        profile: profile.resolve_colours(&Palette::default()).unwrap(),
        projection: HeadParams::default().face_projection(),
        skin: Colour::rgb(0xf9, 0xd3, 0xc5),
    }
}

fn options() -> RangeOptions {
    RangeOptions { sizes: vec![28, 56], ..RangeOptions::default() }
}

/// The starter set plus `angry-nudge` (angry with both outer brows raised a tenth more) and `flat` (a blush of 0.02:
/// nothing a viewer sees).
fn test_set() -> ExpressionSet {
    let mut set = ExpressionSet::starter();
    let mut nudge = set.0[&id("angry")].clone();
    for brow in [Channel::BROW_OUTER_UP_LEFT, Channel::BROW_OUTER_UP_RIGHT] {
        *nudge.deltas.0.entry(brow).or_insert(0.0) += 0.1;
    }
    set.0.insert(id("angry-nudge"), nudge);
    let mut flat = set.0[&id("happy")].clone();
    flat.deltas = omuzaki_face_api::ChannelValues::default().with(Channel::BLUSH, 0.02);
    set.0.insert(id("flat"), flat);
    set
}

#[test]
fn the_starter_moods_read_apart_and_look_alikes_are_flagged() {
    let face = subject(test_set());
    let painted = paint_all(&face, &options(), &|| false).unwrap().unwrap();
    let report = compare(&painted, &options(), &face);
    let pairs_at = |size: u32| -> Vec<(String, String)> {
        report.similar.iter().filter(|p| p.size == size).map(|p| (p.a.clone(), p.b.clone())).collect()
    };
    // A brow nudge of a tenth is not a new expression, at either size.
    assert!(pairs_at(56).contains(&("angry".into(), "angry-nudge".into())), "{:?}", report.similar);
    assert!(pairs_at(28).contains(&("angry".into(), "angry-nudge".into())));
    // A 0.02 blush reads as the rest face.
    assert!(pairs_at(56).contains(&("flat".into(), "rest".into())));
    assert_eq!(report.neutral, ["flat"]);
    // Happy, angry, sad, relaxed, surprised and blink all read apart at a small stream size.
    let at_56 = report.sizes.iter().find(|s| s.size == 56).unwrap();
    assert!(at_56.distinct >= 6, "{:?}", report.sizes);
    for mood in ["happy", "angry", "sad", "relaxed", "surprised", "blink"] {
        let as_rest = pairs_at(56).iter().any(|(a, b)| (a == mood && b == "rest") || (a == "rest" && b == mood));
        assert!(!as_rest, "{mood} reads as the rest face");
    }
    // Visemes are only compared with visemes, never with a mood.
    let viseme = |n: &str| ["aa", "ih", "ou", "ee", "oh"].contains(&n);
    assert!(report.similar.iter().all(|p| viseme(&p.a) == viseme(&p.b)));
    // The eyes keep their contrast when the face is an emote.
    assert!(report.sizes[0].eye_contrast > 0.4, "{:?}", report.sizes);
}

#[test]
fn the_measure_is_deterministic() {
    let face = subject(ExpressionSet::starter());
    let run = || {
        let painted = paint_all(&face, &options(), &|| false).unwrap().unwrap();
        serde_json::to_string(&compare(&painted, &options(), &face)).unwrap()
    };
    assert_eq!(run(), run());
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

const OP: &str = "face.expression.range_test";

/// `mira` with the starter expressions, and `bare` with none.
fn doc() -> MemDoc {
    let face = json!({ "expressions": ExpressionSet::starter() });
    MemDoc::from_value(
        json!({ "characters": { "mira": { "name": "Mira", "face": face }, "bare": { "name": "Bare" } } }),
    )
}

fn decode(args: Value) -> Result<Box<dyn omuzaki_ops_api::DynOp>, OpError> {
    omuzaki_face::link();
    (find(OP).expect("registered").decode)(args)
}

fn plan(args: Value) -> Result<Value, OpError> {
    let (op, doc) = (decode(args)?, doc());
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    Ok(op.plan(&OpCtx::new(&doc, &actor, &clock))?.output.expect("answers"))
}

fn run(services: &Services, args: Value) -> Result<Value, OpError> {
    let planned = plan(args.clone())?;
    let (op, actor, doc, signal) =
        (decode(args)?, Actor::Person { name: "tester".into() }, Doc(doc()), Signal::default());
    op.work(&JobCtx::new(JobId(1), &actor, &doc, &signal).with_services(services).with_plan_output(&planned))
}

#[test]
fn the_job_reports_each_size_and_stores_a_sheet_per_size() {
    let store = Arc::new(MemStore::new());
    let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
    let answer = run(&services, json!({ "character": "mira", "sizes": [56, 28] })).unwrap();
    assert_eq!(answer["expressions"], 11);
    let sizes: Vec<u64> = answer["sizes"].as_array().unwrap().iter().map(|s| s["size"].as_u64().unwrap()).collect();
    assert_eq!(sizes, [28, 56], "sorted, smallest first");
    assert_eq!(answer["min_distinct"], 6);
    let sheets = answer["sheets"].as_array().unwrap();
    assert_eq!(sheets.len(), 2);
    for sheet in sheets {
        let hash: ContentHash = sheet["hash"].as_str().unwrap().parse().unwrap();
        let png = image::load_from_memory(&store.get(&hash).unwrap()).unwrap();
        assert!(png.width() >= 6 * 160, "a row of six cells, each at least 160 px");
    }
}

#[test]
fn bad_arguments_are_refused_by_name() {
    let refused = |args: Value| plan(args).unwrap_err();
    assert_eq!(refused(json!({ "character": "mira", "sizes": [] })).field.as_deref(), Some("sizes"));
    assert_eq!(refused(json!({ "character": "mira", "sizes": [28, 4] })).field.as_deref(), Some("sizes/1"));
    let threshold = refused(json!({ "character": "mira", "similar_threshold": 1.5 }));
    assert_eq!(threshold.field.as_deref(), Some("similar_threshold"));
    let bare = refused(json!({ "character": "bare" }));
    assert_eq!((bare.code, bare.field.as_deref()), (ErrorCode::Invalid, Some("character")));
    assert_eq!(refused(json!({ "character": "nobody" })).code, ErrorCode::NotFound);
}
