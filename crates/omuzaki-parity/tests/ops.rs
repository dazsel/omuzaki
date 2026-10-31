//! The `parity.*` ops through their registrations, against an in-memory store and document: the stop rule (done only
//! at a 0 map), regions and hints, camera alignment, overlays, the provenance refusals, targets, the loop over a
//! sequence with its timing and progress, and signal parity. Every image is a synthetic character drawn in code.
//!
//! Set `OMUZAKI_PARITY_EVIDENCE_DIR` to a folder (outside the repository) to keep the overlays the tests draw.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]
#![allow(
    clippy::cast_sign_loss,
    clippy::disallowed_methods,
    reason = "test pictures; evidence goes where the caller names"
)]

use std::sync::Arc;

use ::image::ImageEncoder;
use ::image::codecs::png::PngEncoder;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::DocRead;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::checks::apply_patch;
use omuzaki_ops_api::{Actor, OpCtx, OpError, Plan, Services, ServicesBuilder, find};
use omuzaki_store_api::{Commercial, ContentHash, Source, Store};
use omuzaki_store_mock::MemStore;
use omuzaki_store_mock::conformance::sample_provenance;
use serde_json::{Value, json};

const W: u32 = 160;
const H: u32 = 200;
const BACK: [u8; 3] = [40, 30, 28];

/// How the synthetic character is drawn.
#[derive(Clone, Copy)]
struct Look {
    shift: [f32; 2],
    hair: [u8; 3],
    /// Moves the head side to side over a sequence.
    sway: f32,
}

impl Default for Look {
    fn default() -> Self {
        Self { shift: [0.0, 0.0], hair: [230, 220, 190], sway: 0.0 }
    }
}

/// A bust: a body, a head, hair over the top of the head, two eyes and a mouth, on a dark backdrop, with a soft
/// shading ramp so the regions have texture.
fn draw(look: Look) -> Vec<u8> {
    let mut px = vec![0u8; (W * H * 4) as usize];
    let (cx, cy) = (80.0 + look.shift[0] + look.sway, 80.0 + look.shift[1]);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let inside =
                |ex: f32, ey: f32, rx: f32, ry: f32| ((fx - ex) / rx).powi(2) + ((fy - ey) / ry).powi(2) <= 1.0;
            let shade = |c: [u8; 3]| c.map(|v| (f32::from(v) * (0.8 + 0.2 * ((fx - cx + 80.0) / W as f32))) as u8);
            let mut c = BACK;
            if inside(80.0 + look.shift[0], 200.0 + look.shift[1], 60.0, 70.0) {
                c = shade([60, 80, 160]);
            }
            if inside(cx, cy, 34.0, 40.0) {
                c = shade([250, 220, 200]);
            }
            if inside(cx, cy - 22.0, 40.0, 26.0) && fy < cy - 8.0 {
                c = shade(look.hair);
            }
            if inside(cx - 12.0, cy + 2.0, 5.0, 6.0) || inside(cx + 12.0, cy + 2.0, 5.0, 6.0) {
                c = [40, 90, 200];
            }
            if inside(cx, cy + 22.0, 7.0, 2.5) {
                c = [180, 60, 70];
            }
            let i = ((y * W + x) * 4) as usize;
            px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    png(&px, W, H)
}

fn points(look: Look) -> Value {
    let (cx, cy) = (80.0 + look.shift[0] + look.sway, 80.0 + look.shift[1]);
    json!({ "eye_right": [cx - 12.0, cy + 2.0], "eye_left": [cx + 12.0, cy + 2.0], "mouth": [cx, cy + 22.0], "chin": [cx, cy + 40.0] })
}

fn png(px: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out).write_image(px, w, h, ::image::ExtendedColorType::Rgba8).unwrap();
    out
}

struct Rig {
    store: Arc<MemStore>,
    services: Services,
}

impl Rig {
    fn new() -> Self {
        omuzaki_parity::link();
        let store = Arc::new(MemStore::new());
        let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
        Self { store, services }
    }

    fn put(&self, bytes: &[u8]) -> ContentHash {
        self.store.put(bytes, sample_provenance(Commercial::Ok)).unwrap()
    }

    fn image(&self, look: Look) -> Value {
        json!({ "asset": self.put(&draw(look)).to_string() })
    }

    fn plan(&self, doc: &MemDoc, op: &str, args: Value) -> Result<Plan, OpError> {
        let op = (find(op).expect("registered").decode)(args)?;
        let (actor, clock) = (Actor::Agent { id: "agent".into(), model: None }, FakeClock::default());
        op.plan(&OpCtx::new(doc, &actor, &clock).with_services(&self.services))
    }

    fn answer(&self, doc: &MemDoc, op: &str, args: Value) -> Value {
        self.plan(doc, op, args).unwrap().output.unwrap()
    }

    /// Keeps a stored overlay in the evidence folder, when one is named.
    fn keep(&self, asset: &Value, name: &str) {
        if let Ok(dir) = std::env::var("OMUZAKI_PARITY_EVIDENCE_DIR") {
            let hash: ContentHash = asset.as_str().unwrap().parse().unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(format!("{dir}/{name}.png"), self.store.get(&hash).unwrap()).unwrap();
        }
    }
}

fn metric(out: &Value, name: &str) -> f64 {
    out["metrics"].as_array().unwrap().iter().find(|m| m["name"] == name).unwrap()["value"].as_f64().unwrap()
}

fn region<'a>(out: &'a Value, name: &str) -> &'a Value {
    out["regions"].as_array().unwrap().iter().find(|r| r["name"] == name).unwrap_or_else(|| panic!("no {name}"))
}

#[test]
fn the_same_picture_is_done_and_every_metric_is_at_its_target() {
    let rig = Rig::new();
    let doc = MemDoc::new();
    let img = rig.image(Look::default());
    let out = rig.answer(&doc, "parity.compare", json!({ "ours": img, "reference": img }));
    assert_eq!(out["done"], true, "{out}");
    assert_eq!(out["pixels_off"], 0);
    assert!(out["metrics"].as_array().unwrap().iter().all(|m| m["pass"] == true), "{}", out["metrics"]);
    assert_eq!(out["backdrop"], "#281e1c", "the backdrop is the reference's border colour");
    assert_eq!(out["provenance"]["clean"], true);
}

#[test]
fn one_level_on_one_pixel_is_not_parity() {
    let rig = Rig::new();
    let doc = MemDoc::new();
    let reference = draw(Look::default());
    let mut decoded = ::image::load_from_memory(&reference).unwrap().to_rgba8().into_raw();
    let at = ((120 * W + 33) * 4) as usize;
    decoded[at] = decoded[at].wrapping_add(1);
    let ours = json!({ "asset": rig.put(&png(&decoded, W, H)).to_string() });
    let reference = json!({ "asset": rig.put(&reference).to_string() });
    let out = rig.answer(&doc, "parity.compare", json!({ "ours": ours, "reference": reference }));
    assert_eq!(out["done"], false);
    assert_eq!(out["pixels_off"], 1, "{out}");
    assert_eq!(out["max_level"], 1);
    let tile = &out["worst_tiles"][0]["rect"];
    let [x, y, w, h] = [0, 1, 2, 3].map(|k| tile[k].as_u64().unwrap());
    assert!((x..x + w).contains(&33) && (y..y + h).contains(&120), "the worst tile holds the pixel: {tile}");
    // A tolerance of one level calls it a match, and says so.
    let loose =
        rig.answer(&doc, "parity.compare", json!({ "ours": ours, "reference": reference, "tolerance_levels": 1 }));
    assert_eq!(loose["done"], true);
}

#[test]
fn recoloured_hair_is_the_worst_region_and_the_hint_says_colour() {
    let rig = Rig::new();
    let doc = MemDoc::new();
    let look = Look::default();
    let ours = rig.image(Look { hair: [200, 150, 110], ..look });
    let reference = rig.image(look);
    let out = rig.answer(
        &doc,
        "parity.compare",
        json!({ "ours": ours, "reference": reference, "ours_points": points(look), "reference_points": points(look) }),
    );
    assert_eq!(out["worst"][0]["scope"], "hair", "{}", out["worst"]);
    let hair = region(&out, "hair");
    assert!(hair["colour"]["shift_explains"].as_f64().unwrap() > 0.4, "{hair}");
    assert!(hair["colour"]["delta_lightness"].as_f64().unwrap() < -5.0, "ours is darker: {hair}");
    let hint = out["worst"][0]["hint"].as_str().unwrap();
    assert!(hint.contains("darker") && hint.contains("aim its mean at #d3c6ad"), "{hint}");
    for r in ["eye_right", "eye_left", "mouth"] {
        assert_eq!(region(&out, r)["pixels_off"], 0, "{r} is untouched");
    }
}

#[test]
fn a_camera_off_by_a_few_pixels_is_found_and_said_in_words() {
    let rig = Rig::new();
    let doc = MemDoc::new();
    let ours_look = Look { shift: [-6.0, 4.0], ..Look::default() };
    let out = rig.answer(
        &doc,
        "parity.compare",
        json!({ "ours": rig.image(ours_look), "reference": rig.image(Look::default()), "align": "auto" }),
    );
    let a = &out["alignment"];
    let shift = &a["camera"]["subject_shift_px"];
    assert!((shift[0].as_f64().unwrap() - 6.0).abs() < 1.0 && (shift[1].as_f64().unwrap() + 4.0).abs() < 1.0, "{a}");
    assert!(a["framing_explains"].as_f64().unwrap() > 0.6, "{a}");
    assert!(a["camera"]["summary"].as_str().unwrap().contains("right"), "{a}");
    assert_eq!(out["done"], false, "alignment is a diagnosis; the render as rendered is what is scored");
    // By points, exactly.
    let by_points = rig.answer(
        &doc,
        "parity.align",
        json!({ "ours": rig.image(ours_look), "reference": rig.image(Look::default()), "method": "points",
                "ours_points": points(ours_look), "reference_points": points(Look::default()) }),
    );
    assert_eq!(by_points["transform"]["shift"], json!([6.0, -4.0]), "{by_points}");
    assert!(
        by_points["residual_after"].as_f64().unwrap() < 0.05 * by_points["residual_before"].as_f64().unwrap(),
        "{by_points}"
    );
}

#[test]
fn every_overlay_is_stored_as_evidence_made_from_the_reference() {
    let rig = Rig::new();
    let doc = MemDoc::new();
    let look = Look::default();
    let reference = rig.image(look);
    let ours = rig.image(Look { hair: [210, 170, 120], shift: [3.0, 0.0], ..look });
    let modes = [
        json!({"mode": "onion"}),
        json!({"mode": "exact"}),
        json!({"mode": "difference"}),
        json!({"mode": "delta_e"}),
        json!({"mode": "signed"}),
        json!({"mode": "wipe", "position": 0.4}),
        json!({"mode": "checker", "cell": 10}),
        json!({"mode": "edges"}),
        json!({"mode": "silhouette"}),
        json!({"mode": "regions"}),
        json!({"mode": "sheet"}),
    ];
    for mode in modes {
        let out = rig.answer(
            &doc,
            "parity.overlay",
            json!({ "ours": ours, "reference": reference, "overlay": mode, "ours_points": points(look), "reference_points": points(look) }),
        );
        let name = mode["mode"].as_str().unwrap();
        rig.keep(&out["asset"], &format!("overlay-{name}"));
        let hash: ContentHash = out["asset"].as_str().unwrap().parse().unwrap();
        let Source::Generated { inputs, tool, .. } = rig.store.provenance(&hash).unwrap().source else {
            panic!("an overlay is generated evidence")
        };
        assert_eq!(tool, "parity");
        assert!(inputs.contains(&reference["asset"].as_str().unwrap().parse().unwrap()), "{name}");
        if name == "sheet" {
            assert_eq!(out["size"], json!([W * 3 + 8, H * 2 + 4]));
        } else {
            assert_eq!(out["size"], json!([W, H]), "{name}");
        }
    }
}

#[test]
fn a_render_built_from_reference_pixels_is_refused() {
    let rig = Rig::new();
    let look = Look::default();
    let reference_png = draw(look);
    let reference = rig.put(&reference_png);
    let ours = rig.image(Look { hair: [120, 80, 60], ..look });
    let args = json!({ "ours": ours, "reference": { "asset": reference.to_string() } });

    // The reference used as a texture.
    let doc = MemDoc::from_value(json!({ "characters": { "a": { "texture": reference.to_string() } } }));
    let e = rig.plan(&doc, "parity.compare", args.clone()).unwrap_err();
    assert!(e.message.contains("refused") && e.message.contains("characters/a/texture"), "{}", e.message);

    // A crop of it, as its own asset.
    let full = ::image::load_from_memory(&reference_png).unwrap().to_rgba8();
    let crop = ::image::imageops::crop_imm(&full, 0, 0, W / 2, H / 2).to_image();
    let cut = rig.put(&png(crop.as_raw(), W / 2, H / 2));
    let doc = MemDoc::from_value(json!({ "characters": { "a": { "decal": cut.to_string() } } }));
    let e = rig.plan(&doc, "parity.compare", args.clone()).unwrap_err();
    assert!(e.message.contains("near-copy"), "{}", e.message);

    // An overlay (it holds reference pixels).
    let overlay = rig.answer(
        &MemDoc::new(),
        "parity.overlay",
        json!({ "ours": ours, "reference": args["reference"], "overlay": {"mode": "onion"} }),
    );
    let doc = MemDoc::from_value(json!({ "characters": { "a": { "skin": overlay["asset"] } } }));
    let e = rig.plan(&doc, "parity.compare", args.clone()).unwrap_err();
    assert!(e.message.contains("parity made from the reference"), "{}", e.message);

    // A clean project scores, and the target itself (under parity/) is not a use.
    let doc = MemDoc::from_value(
        json!({ "parity": { "targets": { "t": { "reference": { "asset": reference.to_string() } } } } }),
    );
    assert!(rig.plan(&doc, "parity.compare", args).is_ok());
}

#[test]
fn targets_set_and_remove_undoably() {
    let rig = Rig::new();
    let mut doc = MemDoc::new();
    let reference = rig.image(Look::default());
    let set = rig
        .plan(&doc, "parity.target.set", json!({ "name": "bust", "target": { "reference": reference, "points": points(Look::default()), "note": "a synthetic bust" } }))
        .unwrap();
    apply_patch(&mut doc, &set.patch).unwrap();
    assert!(doc.get(&omuzaki_doc_api::DocPath::parse("parity/targets/bust").unwrap()).is_some());
    let undo = set.patch.inverse();
    let remove = rig.plan(&doc, "parity.target.remove", json!({ "name": "bust" })).unwrap();
    apply_patch(&mut doc, &remove.patch).unwrap();
    assert!(doc.get(&omuzaki_doc_api::DocPath::parse("parity/targets/bust").unwrap()).is_none());
    apply_patch(&mut doc, &remove.patch.inverse()).unwrap();
    apply_patch(&mut doc, &undo).unwrap();
    assert!(
        doc.get(&omuzaki_doc_api::DocPath::parse("parity/targets").unwrap())
            .is_none_or(|v| v.as_object().is_some_and(serde_json::Map::is_empty))
    );
    assert!(
        rig.plan(&doc, "parity.target.set", json!({ "name": "Not a slug!", "target": { "reference": reference } }))
            .is_err()
    );
}

#[test]
fn the_loop_lines_a_late_sequence_up_scores_it_and_reports_progress() {
    let rig = Rig::new();
    let mut doc = MemDoc::new();
    let sway = |k: usize| 18.0 * ((k as f32) * 0.5).sin() + (k as f32) * 0.8;
    let reference: Vec<Value> = (0..14).map(|k| rig.image(Look { sway: sway(k), ..Look::default() })).collect();
    let ref_points: Vec<Value> = (0..14).map(|k| points(Look { sway: sway(k), ..Look::default() })).collect();
    let set = rig
        .plan(&doc, "parity.target.set", json!({ "name": "sway", "target": { "reference": { "frames": reference }, "points": ref_points, "fps": 30.0 } }))
        .unwrap();
    apply_patch(&mut doc, &set.patch).unwrap();

    // Ours starts three frames in, with the hair the wrong colour.
    let ours: Vec<Value> =
        (3..11).map(|k| rig.image(Look { sway: sway(k), hair: [150, 110, 90], ..Look::default() })).collect();
    let first = rig.answer(
        &doc,
        "parity.loop",
        json!({ "target": { "name": "sway" }, "ours": { "frames": ours }, "made_with": "test drawing" }),
    );
    assert_eq!(first["done"], false);
    assert_eq!(first["timing"]["offset_frames"], 3, "{}", first["timing"]);
    assert_eq!(first["frames"][0]["reference"], 3);
    assert_eq!(first["worst"][0]["scope"], "hair", "{}", first["worst"]);
    assert!(
        first["next"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().starts_with("hair:")),
        "{}",
        first["next"]
    );
    assert!(first["report"].is_string() && first["evidence"][0]["asset"].is_string());
    rig.keep(&first["evidence"][0]["asset"], "loop-sheet-first");

    // Fixed: every frame matches, and the progress says so.
    let fixed: Vec<Value> = (3..11).map(|k| rig.image(Look { sway: sway(k), ..Look::default() })).collect();
    let second = rig.answer(
        &doc,
        "parity.loop",
        json!({ "target": { "name": "sway" }, "ours": { "frames": fixed }, "made_with": "test drawing", "previous": first["report"] }),
    );
    assert_eq!(second["done"], true, "{}", second["metrics"]);
    assert_eq!(metric(&second, "frames_at_zero"), 1.0);
    assert_eq!(second["progress"]["better"], true);
    assert_eq!(second["progress"]["pixels_off_now"], 0.0);
    assert!(metric(&first, "pixels_off") > 0.0);

    // No made_with, no score.
    assert!(
        rig.plan(
            &doc,
            "parity.loop",
            json!({ "target": { "name": "sway" }, "ours": { "frames": fixed }, "made_with": " " })
        )
        .is_err()
    );
}

#[test]
fn timing_finds_the_offset_of_two_sequences() {
    let rig = Rig::new();
    let doc = MemDoc::new();
    let sway = |k: usize| 20.0 * ((k as f32) * 0.45).sin() + (k as f32);
    let reference: Vec<Value> = (0..16).map(|k| rig.image(Look { sway: sway(k), ..Look::default() })).collect();
    let ours: Vec<Value> = (5..13).map(|k| rig.image(Look { sway: sway(k), ..Look::default() })).collect();
    let out = rig.answer(
        &doc,
        "parity.timing",
        json!({ "ours": { "frames": ours }, "reference": { "frames": reference }, "max_lag": 10 }),
    );
    assert_eq!(out["offset_frames"], 5, "{out}");
    assert_eq!(out["map"][0], 5);
    assert_eq!(out["drift_max_frames"], 0);
}

#[test]
fn signal_parity_scores_curves_in_the_shared_shape() {
    let rig = Rig::new();
    let doc = MemDoc::new();
    let curve = |gain: f64, lag: usize| -> Vec<f64> {
        (0..90).map(|i| gain * (0.003 * ((i + lag) as f64).powi(2)).sin()).collect()
    };
    let reference = json!({ "rate_hz": 30.0, "channels": { "jawOpen": curve(1.0, 0), "head.yaw": curve(10.0, 0) } });
    let same = rig.answer(&doc, "parity.series", json!({ "ours": reference, "reference": reference }));
    assert_eq!(same["done"], true, "{same}");
    let ours = json!({ "rate_hz": 30.0, "channels": { "jawOpen": curve(0.6, 0), "head.yaw": curve(10.0, 0), "extra": [0.0] } });
    let out = rig.answer(&doc, "parity.series", json!({ "ours": ours, "reference": reference }));
    assert_eq!(out["done"], false);
    assert_eq!(out["missing"], json!(["extra"]));
    assert_eq!(out["worst"][0]["scope"], "jawOpen", "{}", out["worst"]);
    assert!(out["worst"][0]["hint"].as_str().unwrap().contains("amplitude 60%"), "{}", out["worst"][0]);
    for m in out["metrics"].as_array().unwrap() {
        for key in ["name", "value", "unit", "target", "better", "pass", "scope"] {
            assert!(m.get(key).is_some(), "{m} has {key}");
        }
    }
}

#[test]
fn a_size_mismatch_is_refused_with_the_size_to_render_at() {
    let rig = Rig::new();
    let doc = MemDoc::new();
    let small = json!({ "asset": rig.put(&png(&vec![255u8; 4 * 10 * 10], 10, 10)).to_string() });
    let e = rig
        .plan(&doc, "parity.compare", json!({ "ours": small, "reference": rig.image(Look::default()) }))
        .unwrap_err();
    assert!(e.hint.as_deref().unwrap_or_default().contains("[160, 200]"), "{e:?}");
}
