//! The readability measures through their registrations (VF-06, #578): `measure.silhouette` and `measure.readability`
//! on images in an in-memory store. Every image is drawn in code.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use ::image::ImageEncoder;
use ::image::codecs::png::PngEncoder;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, OpError, Services, ServicesBuilder, find};
use omuzaki_store_api::{Commercial, Store};
use omuzaki_store_mock::MemStore;
use omuzaki_store_mock::conformance::sample_provenance;
use serde_json::{Value, json};

struct Rig {
    store: Arc<MemStore>,
    services: Services,
}

impl Rig {
    fn new() -> Self {
        omuzaki_measure::link();
        let store = Arc::new(MemStore::new());
        let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
        Self { store, services }
    }

    /// Stores a `w` x `h` picture whose pixel at (x, y) is `paint(x, y)` (RGBA), and returns its image argument.
    fn image(&self, w: u32, h: u32, paint: impl Fn(f64, f64) -> [u8; 4]) -> Value {
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&paint(f64::from(x) + 0.5, f64::from(y) + 0.5));
            }
        }
        let mut png = Vec::new();
        PngEncoder::new(&mut png).write_image(&pixels, w, h, ::image::ExtendedColorType::Rgba8).unwrap();
        let asset = self.store.put(&png, sample_provenance(Commercial::Ok)).unwrap();
        json!({ "asset": asset.to_string() })
    }

    fn run(&self, op: &str, args: Value) -> Result<Value, OpError> {
        let op = (find(op).expect("registered").decode)(args)?;
        let (actor, clock) = (Actor::Agent { id: "agent".into(), model: None }, FakeClock::default());
        let doc = MemDoc::from_value(json!({}));
        let plan = op.plan(&OpCtx::new(&doc, &actor, &clock).with_services(&self.services))?;
        assert!(plan.patch.is_empty(), "a measure changes nothing");
        Ok(plan.output.unwrap())
    }
}

/// A figure on a transparent ground: a head of radius `head` on a body, plus `extra`.
fn figure(rig: &Rig, head: f64, extra: impl Fn(f64, f64) -> bool) -> Value {
    rig.image(200, 400, |x, y| {
        let on = (x - 100.0).hypot(y - 80.0) <= head
            || ((60.0..140.0).contains(&x) && (80.0 + head..390.0).contains(&y))
            || extra(x, y);
        if on { [30, 20, 40, 255] } else { [0, 0, 0, 0] }
    })
}

#[test]
fn silhouette_scores_a_signature_shape_against_look_alikes() {
    let rig = Rig::new();
    let plain = figure(&rig, 50.0, |_, _| false);
    let twin = figure(&rig, 52.0, |_, _| false);
    // Twin tails: two long locks either side of the head.
    let tails = figure(&rig, 50.0, |x, y| {
        ((10.0..45.0).contains(&x) || (155.0..190.0).contains(&x)) && (60.0..260.0).contains(&y)
    });
    let others = json!([{ "name": "twin", "image": twin }, { "name": "tails", "image": tails.clone() }]);
    let out = rig.run("measure.silhouette", json!({ "image": plain, "others": others })).unwrap();
    assert_eq!(out["size"], 64);
    assert_eq!(out["closest"]["name"], "twin");
    assert_eq!(out["within"], false, "{out}");
    assert!(out["hint"].as_str().unwrap().contains("twin"));
    assert_eq!(out["warnings"][0]["code"], "few_others");

    let others = json!([{ "name": "plain", "image": plain }, { "name": "twin", "image": twin }]);
    let out = rig.run("measure.silhouette", json!({ "image": tails, "others": others })).unwrap();
    assert_eq!(out["within"], true, "{out}");
    assert!(out["distinctiveness"].as_f64().unwrap() > 0.15, "{out}");
}

#[test]
fn silhouette_cuts_an_opaque_render_from_its_background_and_refuses_an_empty_one() {
    let rig = Rig::new();
    let on_grey =
        rig.image(
            100,
            100,
            |x, y| if (x - 50.0).hypot(y - 50.0) <= 30.0 { [200, 40, 40, 255] } else { [128, 128, 128, 255] },
        );
    let disc = rig.image(60, 60, |x, y| if (x - 30.0).hypot(y - 30.0) <= 25.0 { [0, 0, 0, 255] } else { [0, 0, 0, 0] });
    let out = rig
        .run("measure.silhouette", json!({ "image": on_grey, "others": [{ "name": "disc", "image": disc }] }))
        .unwrap();
    assert!(out["closest"]["overlap"].as_f64().unwrap() > 0.95, "{out}");
    let blank = rig.image(10, 10, |_, _| [0, 0, 0, 0]);
    let err = rig.run("measure.silhouette", json!({ "image": blank, "others": [{ "name": "disc", "image": disc }] }));
    assert_eq!(err.unwrap_err().field.as_deref(), Some("image"));
    let none = rig.run("measure.silhouette", json!({ "image": disc, "others": [] }));
    assert_eq!(none.unwrap_err().field.as_deref(), Some("others"));
}

const SKIN: [u8; 4] = [249, 211, 197, 255];
const INK: [u8; 4] = [40, 30, 40, 255];

/// A 400 px face: `eye` the eyes' half-height, `mouth` the mouth's half-width and half-height, `lash` the mouth line's
/// ink.
fn face(rig: &Rig, eye: f64, mouth: (f64, f64), ink: [u8; 4]) -> Value {
    rig.image(400, 400, |x, y| {
        let r = (x - 200.0).hypot(y - 200.0);
        let in_eye = [140.0, 260.0].iter().any(|cx| (x - cx).abs() <= 22.0 && (y - 170.0).abs() <= eye);
        let in_mouth = ((x - 200.0) / mouth.0).powi(2) + ((y - 280.0) / mouth.1).powi(2) <= 1.0;
        if r > 190.0 {
            [0, 0, 0, 0]
        } else if r > 184.0 || in_eye {
            INK
        } else if in_mouth {
            ink
        } else {
            SKIN
        }
    })
}

#[test]
fn readability_finds_the_pairs_that_blur_together_at_emote_size() {
    let rig = Rig::new();
    let expressions = json!([
        { "name": "rest", "image": face(&rig, 18.0, (40.0, 4.0), INK) },
        { "name": "surprised", "image": face(&rig, 26.0, (24.0, 30.0), INK) },
        { "name": "blink", "image": face(&rig, 3.0, (40.0, 4.0), INK) },
        { "name": "rest_wider", "image": face(&rig, 18.0, (41.5, 4.0), INK) },
    ]);
    let args = json!({
        "expressions": expressions,
        "eyes": [100, 120, 300, 220],
        "mouth": [130, 240, 270, 320],
        "skin": "#f9d3c5",
    });
    let out = rig.run("measure.readability", args.clone()).unwrap();
    let sizes = out["sizes"].as_array().unwrap();
    assert_eq!(sizes.iter().map(|s| s["size"].as_u64().unwrap()).collect::<Vec<_>>(), [112, 56, 28]);
    for s in sizes {
        let pairs = s["too_similar"].as_array().unwrap();
        assert!(pairs.iter().all(|p| p["a"] == "rest" && p["b"] == "rest_wider"), "{s}");
        assert_eq!(s["distinct"].as_u64().unwrap(), 4 - pairs.len() as u64, "{s}");
        assert!(s["eye_contrast"].as_f64().unwrap() > 15.0, "{s}");
    }
    let at_28 = &sizes[2];
    assert_eq!(at_28["too_similar"].as_array().unwrap().len(), 1, "the hairline change blurs at 28 px: {at_28}");
    assert_eq!(out["within"], false);
    assert_eq!(out["background"], "#18181b");

    // A pale mouth line holds at 112 px and fades against the skin at 28.
    let pale = [205, 150, 140, 255];
    let faint = json!({
        "expressions": [
            { "name": "rest", "image": face(&rig, 18.0, (40.0, 3.0), pale) },
            { "name": "blink", "image": face(&rig, 3.0, (40.0, 3.0), pale) },
        ],
        "eyes": [100, 120, 300, 220],
        "mouth": [130, 240, 270, 320],
        "skin": "#f9d3c5",
    });
    let out = rig.run("measure.readability", faint).unwrap();
    let mouth: Vec<f64> =
        out["sizes"].as_array().unwrap().iter().map(|s| s["mouth_contrast"].as_f64().unwrap()).collect();
    assert!(mouth[0] > mouth[2], "{mouth:?}");
    assert!(mouth[2] < 15.0 && out["within"] == false, "{out}");
}

#[test]
fn readability_refuses_mixed_sizes_and_a_single_expression() {
    let rig = Rig::new();
    let a = face(&rig, 18.0, (40.0, 4.0), INK);
    let small = rig.image(100, 100, |_, _| SKIN);
    let mixed = json!({ "expressions": [{ "name": "a", "image": a }, { "name": "b", "image": small }] });
    assert_eq!(rig.run("measure.readability", mixed).unwrap_err().field.as_deref(), Some("expressions.1.image"));
    let one = json!({ "expressions": [{ "name": "a", "image": a }] });
    assert_eq!(rig.run("measure.readability", one).unwrap_err().field.as_deref(), Some("expressions"));
    let bad_box = json!({
        "expressions": [{ "name": "a", "image": a }, { "name": "b", "image": a }],
        "eyes": [10, 10, 5, 20],
    });
    assert_eq!(rig.run("measure.readability", bad_box).unwrap_err().field.as_deref(), Some("eyes"));
}
