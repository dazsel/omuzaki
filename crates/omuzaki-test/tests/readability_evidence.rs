//! The readability measures on the project's own default face (VF-06, #578): the built-in expression library painted by
//! the real face painter, read back through `measure.readability` at Twitch's emote sizes over its dark chat ground,
//! next to the face's own range test (FE-04) on the same face. Set `OMUZAKI_EVIDENCE_DIR` to keep the report and a
//! sheet of every expression at 112, 56 and 28 px.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]
#![allow(clippy::disallowed_methods, reason = "the evidence is written to the folder the caller names")]

use std::sync::Arc;

use image::{Rgba, RgbaImage, imageops};
use omuzaki_char_api::HeadParams;
use omuzaki_core::colour::Colour;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::preview::range::{RangeOptions, compare, eyes_region, mouth_region, paint_all};
use omuzaki_face::preview::sheet::{Cell, compose, png};
use omuzaki_face::preview::{FaceSubject, Preview};
use omuzaki_face_api::{ExpressionKind, ExpressionSet, FaceProfile, FaceRect, FaceView, Palette};
use omuzaki_measure::image::decode;
use omuzaki_measure::readability::emote::{self, STREAM_DARK};
use omuzaki_ops_api::{Actor, OpCtx, ServicesBuilder, find};
use omuzaki_store_api::{Commercial, Store};
use omuzaki_store_mock::MemStore;
use omuzaki_store_mock::conformance::sample_provenance;
use serde_json::json;

/// The width the face window is painted at, pixels: well above the largest emote size.
const PAINT_PX: u32 = 512;
const SKIN: Colour = Colour::rgb(0xf9, 0xd3, 0xc5);

fn keep(name: &str, bytes: &[u8]) {
    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join(name), bytes).unwrap();
    }
}

fn subject() -> FaceSubject {
    let profile = FaceProfile { expressions: ExpressionSet::library().unwrap(), ..FaceProfile::default() };
    FaceSubject {
        profile: profile.resolve_colours(&Palette::default()).unwrap(),
        projection: HeadParams::default().face_projection(),
        skin: SKIN,
    }
}

/// `rect` (face-plane metres, y up) as a box in the picture's pixels (origin top-left).
fn pixels(rect: FaceRect, window: FaceRect, width: u32) -> [f64; 4] {
    let k = f64::from(width) / f64::from(window.x1 - window.x0);
    [
        f64::from(rect.x0 - window.x0) * k,
        f64::from(window.y1 - rect.y1) * k,
        f64::from(rect.x1 - window.x0) * k,
        f64::from(window.y1 - rect.y0) * k,
    ]
}

#[test]
fn the_default_face_reads_at_emote_sizes() {
    omuzaki_measure::link();
    let face = subject();
    let window = face.projection.rect;
    let rest = face.profile.rest_buf();
    let mut preview = Preview::new(face.clone(), PAINT_PX);
    let mut names = vec!["rest".to_owned()];
    let mut pictures = vec![{
        preview.paint(&rest).unwrap();
        preview.picture()
    }];
    for (id, expression) in &face.profile.expressions.0 {
        if expression.kind == ExpressionKind::Viseme {
            continue;
        }
        let bus = face.profile.expressions.blend(&rest, &[(id, 1.0)]).unwrap();
        preview.paint(&bus).unwrap();
        names.push(id.to_string());
        pictures.push(preview.picture());
    }

    // Every picture goes into the store as a PNG, the way a render reaches a measure.
    let store = Arc::new(MemStore::new());
    let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
    let mut expressions = Vec::new();
    for (name, picture) in names.iter().zip(&pictures) {
        let asset = store.put(&png(picture).unwrap(), sample_provenance(Commercial::Ok)).unwrap();
        expressions.push(json!({ "name": name, "image": { "asset": asset.to_string() } }));
    }
    let (eyes, mouth) = (pixels(eyes_region(&face), window, PAINT_PX), pixels(mouth_region(&face), window, PAINT_PX));
    let args = json!({ "expressions": expressions, "eyes": eyes, "mouth": mouth, "skin": SKIN.to_string() });
    let op = (find("measure.readability").expect("registered").decode)(args).unwrap();
    let (actor, clock, doc) =
        (Actor::Agent { id: "agent".into(), model: None }, FakeClock::default(), MemDoc::from_value(json!({})));
    let out = op.plan(&OpCtx::new(&doc, &actor, &clock).with_services(&services)).unwrap().output.unwrap();

    // The face's own range test on the same face, for comparison.
    let options = RangeOptions { sizes: vec![112, 56, 28], ..RangeOptions::default() };
    let painted = paint_all(&face, &options, &|| false).unwrap().unwrap();
    let range = compare(&painted, &options, &face);

    println!("{} expressions (the rest face and the moods and accents of the library)", names.len());
    let sizes = out["sizes"].as_array().unwrap();
    for (s, r) in sizes.iter().zip(&range.sizes) {
        println!(
            "{:>3} px: {:>2} read apart (FE-04 on the features crop: {:>2} besides rest); closest {} / {} at {}; \
             eyes {:.1} and mouth {:.1} ΔL* against the skin",
            s["size"],
            s["distinct"],
            r.distinct,
            s["closest"]["a"],
            s["closest"]["b"],
            s["closest"]["similarity"],
            s["eye_contrast"].as_f64().unwrap(),
            s["mouth_contrast"].as_f64().unwrap()
        );
        for p in s["too_similar"].as_array().unwrap() {
            println!("        reads the same: {} / {} ({})", p["a"], p["b"], p["similarity"]);
        }
    }
    keep("readability-default-face.json", serde_json::to_string_pretty(&out).unwrap().as_bytes());

    // The sheet: each expression at 112, 56 and 28 px over the chat ground, flagged when it reads like another at 28.
    let blurred: Vec<&str> = sizes[2]["too_similar"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| [p["a"].as_str().unwrap(), p["b"].as_str().unwrap()])
        .collect();
    let cells: Vec<Cell> = names
        .iter()
        .zip(&pictures)
        .map(|(name, picture)| {
            let image = decode(&png(picture).unwrap(), omuzaki_store_api::ContentHash::of(b""), "image").unwrap();
            let mut cell = RgbaImage::from_pixel(
                112 + 56 + 28 + 16,
                112,
                Rgba([STREAM_DARK[0], STREAM_DARK[1], STREAM_DARK[2], 255]),
            );
            let mut x = 0;
            for size in [112, 56, 28] {
                let seen = emote::see(&image, size, STREAM_DARK, None, None);
                imageops::replace(&mut cell, &seen.picture, x, i64::from(112 - size));
                x += i64::from(size) + 8;
            }
            Cell { picture: cell, label: name.clone(), flagged: blurred.contains(&name.as_str()) }
        })
        .collect();
    keep("readability-default-face.png", &png(&compose(&cells, 4)).unwrap());

    // What the library's own range test promises (`omuzaki-face/tests/library_range.rs`) holds through the measure: no
    // two moods read the same at 112 or 56 px, and the eyes hold their contrast down to 28.
    for s in &sizes[..2] {
        assert!(s["too_similar"].as_array().unwrap().is_empty(), "{s}");
    }
    for s in sizes {
        assert!(s["eye_contrast"].as_f64().unwrap() >= 15.0, "{s}");
    }
    // The default mouth line is thin and close to the skin in value: it holds at 112 px and fades at emote size, which
    // is what the measure is for (the spec's fix: bigger mouth shapes and an emote line weight).
    let mouth: Vec<f64> = sizes.iter().map(|s| s["mouth_contrast"].as_f64().unwrap()).collect();
    assert!(mouth[0] >= 15.0 && mouth[2] < mouth[1] && mouth[1] < mouth[0], "{mouth:?}");
    assert_eq!(out["within"], false, "the mouth line fades at 56 and 28 px");
}

/// #464: in a wide shot the mouth grows (`mouth.wide_shot_scale`), so a talking mouth still reads when the face is a few
/// dozen pixels wide. The default face says "aa" and "oh", painted as a close-up and as a full-body shot would paint it,
/// each seen at 28 px through `measure.readability`: the wide-shot mouth holds more contrast against the skin.
#[test]
fn the_talking_mouth_reads_better_in_a_wide_shot() {
    omuzaki_measure::link();
    let face = subject();
    let window = face.projection.rect;
    let rest = face.profile.rest_buf();
    let mut preview = Preview::new(face.clone(), PAINT_PX);
    let store = Arc::new(MemStore::new());
    let services = ServicesBuilder::new().insert::<dyn Store>(store.clone()).build();
    let mut cells = Vec::new();
    let mut contrast = Vec::new();
    for (shot, coverage) in [("close-up", None), ("full body", Some(0.1))] {
        let view = FaceView { head_coverage: coverage, ..FaceView::default() };
        let mut expressions = Vec::new();
        for viseme in ["aa", "oh"] {
            let id = face.profile.expressions.0.keys().find(|k| k.as_str() == viseme).unwrap();
            let bus = face.profile.expressions.blend(&rest, &[(id, 1.0)]).unwrap();
            preview.paint_in(&bus, view).unwrap();
            let picture = preview.picture();
            let asset = store.put(&png(&picture).unwrap(), sample_provenance(Commercial::Ok)).unwrap();
            expressions.push(json!({ "name": viseme, "image": { "asset": asset.to_string() } }));
            let image = decode(&png(&picture).unwrap(), omuzaki_store_api::ContentHash::of(b""), "image").unwrap();
            let mut cell =
                RgbaImage::from_pixel(112 + 28 + 8, 112, Rgba([STREAM_DARK[0], STREAM_DARK[1], STREAM_DARK[2], 255]));
            imageops::replace(&mut cell, &emote::see(&image, 112, STREAM_DARK, None, None).picture, 0, 0);
            imageops::replace(&mut cell, &emote::see(&image, 28, STREAM_DARK, None, None).picture, 120, 84);
            cells.push(Cell { picture: cell, label: format!("{viseme}, {shot}"), flagged: false });
        }
        let mouth = pixels(mouth_region(&face), window, PAINT_PX);
        let args = json!({ "expressions": expressions, "mouth": mouth, "eyes": pixels(eyes_region(&face), window, PAINT_PX), "skin": SKIN.to_string(), "sizes": [28] });
        let op = (find("measure.readability").expect("registered").decode)(args).unwrap();
        let (actor, clock, doc) =
            (Actor::Agent { id: "agent".into(), model: None }, FakeClock::default(), MemDoc::from_value(json!({})));
        let out = op.plan(&OpCtx::new(&doc, &actor, &clock).with_services(&services)).unwrap().output.unwrap();
        let c = out["sizes"][0]["mouth_contrast"].as_f64().unwrap();
        println!("{shot}: the open mouth (aa) at 28 px holds {c:.1} ΔL* against the skin");
        contrast.push(c);
    }
    keep("lip-sync-wide-shot.png", &png(&compose(&cells, 2)).unwrap());
    assert!(contrast[1] > contrast[0] + 3.0, "the wide-shot mouth reads better at 28 px: {contrast:?}");
}
