//! The measure ops through their registrations, against an in-memory store and document (tasks T8 to T10): what
//! `measure.face_outline`, `measure.feature_spacing` and `measure.likeness` answer, the evidence image of
//! `measure.face_overlay`, and likeness targets (set by name and by asset, undo, dry run, the three cache cases, private
//! provenance, remove). Every image is a synthetic face drawn in code.
//!
//! Set `OMUZAKI_MEASURE_EVIDENCE_DIR` to a folder to keep the overlays the tests draw.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "the evidence images are written to the folder the caller names")]

mod support;

use std::sync::Arc;

use ::image::ImageEncoder;
use ::image::codecs::png::PngEncoder;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead};
use omuzaki_doc_mock::MemDoc;
use omuzaki_measure::image::Rgba8;
use omuzaki_measure::target::Target;
use omuzaki_ops_api::checks::apply_patch;
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, Plan, Services, ServicesBuilder, find};
use omuzaki_store_api::{Commercial, ContentHash, Store};
use omuzaki_store_mock::MemStore;
use omuzaki_store_mock::conformance::sample_provenance;
use serde_json::{Value, json};
use support::synth::{Drawn, FACE_A, FACE_B, Shot, Style, SynthFace, draw};

fn png(image: &Rgba8) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&image.pixels, image.width, image.height, ::image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

/// A drawn face stored as a PNG asset.
struct Face {
    drawn: Drawn,
    asset: ContentHash,
}

impl Face {
    /// The `face` argument: the asset and its true landmarks.
    fn json(&self) -> Value {
        json!({
            "image": { "asset": self.asset.to_string() },
            "landmarks": {
                "eye_right": self.drawn.eye_right(),
                "eye_left": self.drawn.eye_left(),
                "mouth": self.drawn.mouth(),
            },
        })
    }
}

struct Rig {
    store: Arc<MemStore>,
    services: Services,
}

impl Rig {
    fn new() -> Self {
        let store = Arc::new(MemStore::new());
        let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
        Self { store, services }
    }

    fn face(&self, face: &SynthFace, style: Style, private: bool) -> Face {
        let drawn = draw(face, style, Shot::new(160.0));
        let mut provenance = sample_provenance(Commercial::Ok);
        provenance.private_test = private;
        let asset = self.store.put(&png(&drawn.image), provenance).unwrap();
        Face { drawn, asset }
    }

    fn plan(&self, doc: &MemDoc, op: &str, args: Value, dry_run: bool) -> Result<Plan, OpError> {
        let op = (find(op).expect("registered").decode)(args)?;
        let (actor, clock) = (Actor::Agent { id: "agent".into(), model: None }, FakeClock::default());
        op.plan(&OpCtx::new(doc, &actor, &clock).with_services(&self.services).with_dry_run(dry_run))
    }

    fn answer(&self, doc: &MemDoc, op: &str, args: Value) -> Value {
        self.plan(doc, op, args, false).unwrap().output.unwrap()
    }

    /// Runs an edit op: plans it and applies its patch, returning the patch's inverse (its undo) and its output.
    fn edit(&self, doc: &mut MemDoc, op: &str, args: Value) -> (omuzaki_doc_api::Patch, Value) {
        let plan = self.plan(doc, op, args, false).unwrap();
        apply_patch(doc, &plan.patch).unwrap();
        (plan.patch.inverse(), plan.output.unwrap())
    }
}

fn mira() -> MemDoc {
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira" } } }))
}

fn close(a: &Value, b: f64, within: f64) -> bool {
    a.as_f64().is_some_and(|a| (a - b).abs() <= within)
}

#[test]
fn face_outline_answers_the_profile_frame_and_landmarks() {
    let rig = Rig::new();
    let face = rig.face(&FACE_A, Style::RenderLike, false);
    let out = rig.answer(&mira(), "measure.face_outline", face.json());
    assert_eq!(out["measure_version"], 1);
    assert_eq!(out["private"], false);
    assert_eq!(out["image"]["hash"], face.asset.to_string());
    assert_eq!(out["outline"]["r"].as_array().unwrap().len(), 85);
    assert!(out["outline"]["state"].as_array().unwrap().iter().all(|s| s == "ok"), "{}", out["outline"]["state"]);
    let truth = FACE_A.truth_profile();
    assert!(close(&out["chin"]["drop"], truth[42], 0.01), "{} against {}", out["chin"]["drop"], truth[42]);
    assert!(close(&out["frame"]["iod_px"], 160.0, 4.0), "{}", out["frame"]["iod_px"]);
    assert_eq!(out["landmarks"]["eye_right"]["snapped"], true);
    assert_eq!(out["landmarks"]["mouth"]["snapped"], true);
    assert!(out["asymmetry"].as_f64().unwrap() < 0.01, "a front view is symmetric");
    assert!(out["warnings"].as_array().unwrap().is_empty(), "{}", out["warnings"]);
}

#[test]
fn feature_spacing_answers_the_fields_and_the_layout_ratios() {
    let rig = Rig::new();
    let face = rig.face(&FACE_A, Style::RenderLike, false);
    let out = rig.answer(&mira(), "measure.feature_spacing", face.json());
    let s = &out["spacing"];
    let iod = FACE_A.iod();
    assert!(close(&s["eye_to_chin"], (FACE_A.eye_y - FACE_A.chin_y()) / iod, 0.015), "{s}");
    assert!(close(&s["eye_to_mouth"], (FACE_A.eye_y - FACE_A.mouth_y) / iod, 0.03), "{s}");
    // The widest row is the eye line, where the jaw has begun to blend into the cranium.
    let at_eyes = 2.0 * FACE_A.half_width_at(FACE_A.eye_y).unwrap() / iod;
    assert!(close(&s["width_max"], at_eyes, 0.02) && close(&s["width_at_eyes"], at_eyes, 0.02), "{s}");
    assert!(s["width_at_mouth"].is_number() && s["width_at_eyes"].is_number(), "{s}");
    assert!(out.to_string().contains("drop_to_iod"), "the layout ratios face.layout.measure takes: {out}");
}

#[test]
fn likeness_passes_the_same_face_across_styles_and_fails_another_face() {
    let rig = Rig::new();
    let (doc, render) = (mira(), rig.face(&FACE_A, Style::RenderLike, false));
    let drawing = rig.face(&FACE_A, Style::Drawing, false);
    let other = rig.face(&FACE_B, Style::RenderLike, false);
    let same = rig.answer(&doc, "measure.likeness", json!({ "reference": drawing.json(), "candidate": render.json() }));
    assert_eq!(same["compare"], "cross_look");
    assert_eq!(same["within"], true, "{same}");
    assert!(same["score"].as_f64().unwrap() < 1.0, "{same}");
    assert!(same["notes"].as_array().unwrap().is_empty(), "{same}");
    let codes: Vec<&str> = same["warnings"].as_array().unwrap().iter().filter_map(|w| w["code"].as_str()).collect();
    assert!(codes.contains(&"expression"), "the drawing's open mouth: {codes:?}");
    let different =
        rig.answer(&doc, "measure.likeness", json!({ "reference": render.json(), "candidate": other.json() }));
    assert_eq!(different["within"], false);
    assert!(different["score"].as_f64().unwrap() > 1.0, "{different}");
    let notes = different["notes"].as_array().unwrap();
    assert!(notes.iter().any(|n| n.as_str().unwrap().contains("lower face")), "B's long jaw is named: {notes:?}");
    assert!(different["outline"]["worst"]["from_deg"].is_number(), "{}", different["outline"]["worst"]);
    // The same faces with same_look: tighter.
    let strict = rig.answer(
        &doc,
        "measure.likeness",
        json!({ "reference": drawing.json(), "candidate": render.json(), "compare": "same_look" }),
    );
    assert_eq!(strict["within"], false, "a drawing is not a render: {strict}");
}

#[test]
fn refusals_name_the_field_at_fault() {
    let rig = Rig::new();
    let face = rig.face(&FACE_A, Style::RenderLike, false);
    let mut outside = face.json();
    outside["landmarks"]["eye_left"] = json!([5000.0, 10.0]);
    let e = rig
        .plan(&mira(), "measure.likeness", json!({ "reference": face.json(), "candidate": outside }), false)
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
    assert_eq!(e.field.as_deref(), Some("candidate.landmarks.eye_left"), "{e:?}");
    let mut swapped = face.json();
    swapped["landmarks"]["eye_left"] = face.json()["landmarks"]["eye_right"].clone();
    swapped["landmarks"]["eye_right"] = face.json()["landmarks"]["eye_left"].clone();
    let e = rig.plan(&mira(), "measure.face_outline", swapped, false).unwrap_err();
    assert!(e.message.contains("swapped") || e.message.contains("upside down"), "{e:?}");
    let mut missing = face.json();
    missing["image"] = json!({ "asset": ContentHash::of(b"nothing").to_string() });
    let e = rig.plan(&mira(), "measure.face_outline", missing, false).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
}

/// Keeps an overlay under the evidence folder, when one is named.
fn keep(name: &str, bytes: &[u8]) {
    if let Some(dir) = std::env::var_os("OMUZAKI_MEASURE_EVIDENCE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), bytes).unwrap();
    }
}

fn overlay(rig: &Rig, args: Value, name: &str) -> (Rgba8, Value) {
    let out = rig.answer(&mira(), "measure.face_overlay", args);
    let asset: ContentHash = out["asset"].as_str().unwrap().parse().unwrap();
    let bytes = rig.store.get(&asset).unwrap();
    keep(name, &bytes);
    let image = omuzaki_measure::image::decode(&bytes, asset, "overlay").unwrap();
    (image, out)
}

/// How many pixels differ between two images of one size.
fn changed(a: &Rgba8, b: &Rgba8) -> usize {
    a.pixels.as_chunks::<4>().0.iter().zip(b.pixels.as_chunks::<4>().0).filter(|(p, q)| p != q).count()
}

#[test]
fn the_overlay_draws_the_measurement_and_the_reference_and_is_stored() {
    let rig = Rig::new();
    let (render, drawing) = (rig.face(&FACE_A, Style::RenderLike, false), rig.face(&FACE_A, Style::Drawing, false));
    let other = rig.face(&FACE_B, Style::RenderLike, false);
    let (alone, out) = overlay(&rig, json!({ "face": render.json() }), "overlay_A_render.png");
    assert_eq!((alone.width, alone.height), (render.drawn.image.width, render.drawn.image.height));
    assert_eq!(out["private"], false);
    let marks = changed(&alone, &render.drawn.image);
    assert!(marks > 300, "the samples and landmarks are drawn: {marks} pixels");
    let (occluded, _) = overlay(&rig, json!({ "face": drawing.json() }), "overlay_A_drawing.png");
    // The lock's rays are drawn as red lines: many more pixels than the green dots alone.
    assert!(changed(&occluded, &drawing.drawn.image) > marks, "the occluded rays are drawn");
    let (against, _) =
        overlay(&rig, json!({ "face": other.json(), "reference": render.json() }), "overlay_B_against_A.png");
    let (b_alone, _) = overlay(&rig, json!({ "face": other.json() }), "overlay_B_render.png");
    assert!(
        changed(&against, &other.drawn.image) > changed(&b_alone, &other.drawn.image) + 500,
        "the reference's outline and the worst sector are drawn over B"
    );
    // A dry run stores nothing.
    let before = rig.store.len();
    let dry = rig.plan(&mira(), "measure.face_overlay", json!({ "face": render.json() }), true).unwrap();
    assert_eq!(dry.output.unwrap()["dry_run"], true);
    assert_eq!(rig.store.len(), before);
}

#[test]
fn a_private_image_makes_private_answers_overlays_and_targets() {
    let rig = Rig::new();
    let mut doc = mira();
    let secret = rig.face(&FACE_A, Style::Drawing, true);
    let render = rig.face(&FACE_A, Style::RenderLike, false);
    assert_eq!(rig.answer(&doc, "measure.face_outline", secret.json())["private"], true);
    let l = rig.answer(&doc, "measure.likeness", json!({ "reference": secret.json(), "candidate": render.json() }));
    assert_eq!(l["private"], true);
    let (_, out) = overlay(&rig, json!({ "face": render.json(), "reference": secret.json() }), "overlay_private.png");
    let asset: ContentHash = out["asset"].as_str().unwrap().parse().unwrap();
    assert!(rig.store.provenance(&asset).unwrap().private_test, "the overlay of a private reference is private");
    let (_, set) = rig.edit(
        &mut doc,
        "measure.target.set",
        json!({ "character": "mira", "name": "front", "face": secret.json() }),
    );
    assert_eq!(set["private"], true);
    let target: ContentHash = set["asset"].as_str().unwrap().parse().unwrap();
    assert!(rig.store.provenance(&target).unwrap().private_test, "the target of a private reference is private");
}

fn link(doc: &MemDoc, name: &str) -> Option<Value> {
    doc.get(&DocPath::root().join("characters").join("mira").join("likeness").join(name)).cloned()
}

#[test]
fn a_target_is_stored_linked_by_name_used_by_name_or_hash_and_undone() {
    let rig = Rig::new();
    let mut doc = mira();
    let (drawing, render) = (rig.face(&FACE_A, Style::Drawing, false), rig.face(&FACE_A, Style::RenderLike, false));
    let args = json!({ "character": "mira", "name": "front", "face": drawing.json() });
    // A dry run plans the same link but stores nothing.
    let before = rig.store.len();
    let dry = rig.plan(&doc, "measure.target.set", args.clone(), true).unwrap();
    assert_eq!(rig.store.len(), before);
    assert_eq!(dry.output.as_ref().unwrap()["dry_run"], true);
    let (undo, out) = rig.edit(&mut doc, "measure.target.set", args);
    assert_eq!(out["asset"], dry.output.unwrap()["asset"], "a dry run names the asset the real run stores");
    let asset: ContentHash = out["asset"].as_str().unwrap().parse().unwrap();
    assert_eq!(link(&doc, "front").unwrap(), json!({ "asset": asset.to_string(), "view": "front" }));
    let target = Target::from_bytes(&rig.store.get(&asset).unwrap()).unwrap();
    assert_eq!(target.image.asset, drawing.asset);
    assert!(target.cache_is_current() && target.refine);
    // Used by name and by hash, the target measures as the face itself.
    let by_name = json!({ "target": { "character": "mira", "name": "front" } });
    let by_hash = json!({ "target": { "asset": asset.to_string() } });
    let direct =
        rig.answer(&doc, "measure.likeness", json!({ "reference": drawing.json(), "candidate": render.json() }));
    for reference in [by_name, by_hash] {
        let l = rig.answer(&doc, "measure.likeness", json!({ "reference": reference, "candidate": render.json() }));
        assert_eq!(l["score"], direct["score"], "{reference}");
        assert_eq!(l["from_cache"]["reference"], false);
    }
    // A second target sits beside the first; undoing it leaves the first.
    let (undo_side, _) = rig.edit(
        &mut doc,
        "measure.target.set",
        json!({ "character": "mira", "name": "render", "face": render.json(), "compare": "same_look" }),
    );
    assert!(link(&doc, "render").is_some() && link(&doc, "front").is_some());
    apply_patch(&mut doc, &undo_side).unwrap();
    assert!(link(&doc, "render").is_none() && link(&doc, "front").is_some());
    // Undoing the first takes the table with it.
    apply_patch(&mut doc, &undo).unwrap();
    assert_eq!(doc.root()["characters"]["mira"], json!({ "name": "Mira" }));
}

#[test]
fn target_set_refuses_files_unknown_characters_and_bad_names() {
    let rig = Rig::new();
    let doc = mira();
    let face = rig.face(&FACE_A, Style::RenderLike, false);
    let mut file = face.json();
    file["image"] = json!({ "path": "refs/front.png" });
    let e = rig.plan(&doc, "measure.target.set", json!({ "character": "mira", "name": "front", "face": file }), false);
    assert_eq!(e.unwrap_err().field.as_deref(), Some("face.image"));
    let e = rig.plan(
        &doc,
        "measure.target.set",
        json!({ "character": "nobody", "name": "front", "face": face.json() }),
        false,
    );
    assert_eq!(e.unwrap_err().code, ErrorCode::NotFound);
    let e = rig.plan(
        &doc,
        "measure.target.set",
        json!({ "character": "mira", "name": "Front View", "face": face.json() }),
        false,
    );
    assert_eq!(e.unwrap_err().field.as_deref(), Some("name"));
    let e = rig.plan(&doc, "measure.target.remove", json!({ "character": "mira", "name": "front" }), false);
    assert_eq!(e.unwrap_err().code, ErrorCode::NotFound);
}

#[test]
fn a_target_without_its_image_uses_a_current_cache_and_refuses_a_stale_one() {
    let rig = Rig::new();
    let mut doc = mira();
    let (drawing, render) = (rig.face(&FACE_A, Style::Drawing, false), rig.face(&FACE_A, Style::RenderLike, false));
    let (_, out) = rig.edit(
        &mut doc,
        "measure.target.set",
        json!({ "character": "mira", "name": "front", "face": drawing.json() }),
    );
    let asset: ContentHash = out["asset"].as_str().unwrap().parse().unwrap();
    let bytes = rig.store.get(&asset).unwrap();
    let measured = rig.answer(&doc, "measure.feature_spacing", json!({ "target": { "asset": asset.to_string() } }));
    // Another project's store holds the target and the render, but not the reference image.
    let other = Rig::new();
    assert_eq!(other.store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap(), asset);
    let render_here = other.store.put(&png(&render.drawn.image), sample_provenance(Commercial::Ok)).unwrap();
    assert_eq!(render_here, render.asset);
    let cached = other.answer(&doc, "measure.feature_spacing", json!({ "target": { "asset": asset.to_string() } }));
    assert_eq!(cached["from_cache"], true);
    assert_eq!(cached["spacing"]["eye_to_chin"], measured["spacing"]["eye_to_chin"]);
    assert_eq!(cached["spacing"]["width_max"], measured["spacing"]["width_max"]);
    let l = other.answer(
        &doc,
        "measure.likeness",
        json!({ "reference": { "target": { "character": "mira", "name": "front" } }, "candidate": render.json() }),
    );
    assert_eq!(l["from_cache"]["reference"], true);
    assert_eq!(l["within"], true, "{l}");
    // The overlay has nothing to draw on.
    let e = other.plan(
        &doc,
        "measure.face_overlay",
        json!({ "face": { "target": { "asset": asset.to_string() } } }),
        false,
    );
    assert_eq!(e.unwrap_err().field.as_deref(), Some("face"));
    // A cache from another version of the measures is stale: refused, never used.
    let stale = String::from_utf8(bytes).unwrap().replace("measure_version = 1", "measure_version = 0");
    let stale_hash = other.store.put(stale.as_bytes(), sample_provenance(Commercial::Ok)).unwrap();
    let e = other
        .plan(&doc, "measure.feature_spacing", json!({ "target": { "asset": stale_hash.to_string() } }), false)
        .unwrap_err();
    assert_eq!(e.field.as_deref(), Some("target"), "{e:?}");
    assert!(e.message.contains(&stale_hash.to_string()), "{e:?}");
    // With the image back, a stale target is simply measured again.
    rig.store.put(stale.as_bytes(), sample_provenance(Commercial::Ok)).unwrap();
    let again = rig.answer(&doc, "measure.feature_spacing", json!({ "target": { "asset": stale_hash.to_string() } }));
    assert!(again.get("from_cache").is_none());
}

#[test]
fn removing_a_target_unlinks_it_and_undo_brings_it_back() {
    let rig = Rig::new();
    let mut doc = mira();
    let face = rig.face(&FACE_A, Style::RenderLike, false);
    for name in ["front", "smile"] {
        rig.edit(&mut doc, "measure.target.set", json!({ "character": "mira", "name": name, "face": face.json() }));
    }
    let (undo, out) = rig.edit(&mut doc, "measure.target.remove", json!({ "character": "mira", "name": "smile" }));
    assert_eq!(out["removed"], true);
    assert!(link(&doc, "smile").is_none() && link(&doc, "front").is_some());
    apply_patch(&mut doc, &undo).unwrap();
    assert!(link(&doc, "smile").is_some());
    rig.edit(&mut doc, "measure.target.remove", json!({ "character": "mira", "name": "smile" }));
    rig.edit(&mut doc, "measure.target.remove", json!({ "character": "mira", "name": "front" }));
    assert_eq!(doc.root()["characters"]["mira"], json!({ "name": "Mira" }), "the last target takes its table with it");
}
