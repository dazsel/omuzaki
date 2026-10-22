//! `render.contact_sheet` (#1715): stored renders, or a character rendered here in each of several expressions or
//! variants, laid out in a labelled grid as one PNG, the same bytes every run. The mock renderer draws the cells; the
//! kit's standard character has the starter expressions and the mock character two variants.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "render/support.rs"]
mod support;

use omuzaki_char::ops::character::create::CharacterCreate;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocWrite;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError};
use omuzaki_stage::ops::render::contact_sheet::{ContactSheet, ContactSheetResult};
use omuzaki_stage::ops::render::preview::{Preview, PreviewResult};
use omuzaki_store_api::Store;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use support::{Harness, TestDoc, mock_doc};

/// A rendered cell in these tests.
const CELL: [u32; 2] = [48, 64];

/// The mock character (with its `game` and `close-up` variants) and the kit's standard character at `characters/kit`.
fn harness() -> Harness {
    let TestDoc(mut doc) = mock_doc();
    let clock = FakeClock::new(Seconds(0.0));
    let actor = Actor::Person { name: "test".to_owned() };
    let op: CharacterCreate =
        serde_json::from_value(json!({ "id": "kit", "name": "Kit", "template": "standard", "seed": 7 })).unwrap();
    doc.apply(&op.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap().patch).unwrap();
    Harness::new(TestDoc(doc))
}

fn run<O: Op + DeserializeOwned, R: DeserializeOwned>(harness: &Harness, args: Value) -> Result<R, OpError> {
    let op: O = serde_json::from_value(args).expect("the arguments read");
    let (plan, value) = harness.run(&op)?;
    assert!(plan.patch.is_empty(), "a render op changes nothing in the document");
    Ok(serde_json::from_value(value).expect("the result reads"))
}

fn sheet(harness: &Harness, args: Value) -> Result<ContactSheetResult, OpError> {
    run::<ContactSheet, _>(harness, args)
}

fn picture(harness: &Harness, hash: &str) -> image::RgbaImage {
    let png = harness.store.get(&hash.parse().expect("a hash")).expect("stored");
    image::load_from_memory(&png).expect("a PNG").to_rgba8()
}

/// Cell `i` of a sheet with an 8-pixel gutter and `band` pixels of label under each cell, as raw RGBA.
fn cell(image: &image::RgbaImage, result: &ContactSheetResult, band: u32, index: u32) -> Vec<u8> {
    let [width, height] = result.cell;
    let left = 8 + (index % result.columns) * (width + 8);
    let top = 8 + (index / result.columns) * (height + band + 8);
    image::imageops::crop_imm(image, left, top, width, height).to_image().into_raw()
}

/// The label band's height on a labelled sheet: what is left of a row's pitch after the cell and the gutter.
fn band(result: &ContactSheetResult) -> u32 {
    (result.size[1] - 8) / result.rows - 8 - result.cell[1]
}

fn previews(harness: &Harness) -> Vec<String> {
    [0.0, 40.0, 80.0]
        .iter()
        .map(|yaw| {
            let args = json!({ "character": "mock", "yaw": yaw, "size": CELL, "quality": "offline" });
            run::<Preview, PreviewResult>(harness, args).expect("previews").uri
        })
        .collect()
}

#[test]
fn stored_images_make_a_sheet() {
    let harness = harness();
    let uris = previews(&harness);
    let args = json!({ "images": uris, "columns": 2 });
    let result = sheet(&harness, args.clone()).unwrap();
    assert_eq!((result.columns, result.rows, result.cell), (2, 2, CELL));
    let band = band(&result);
    assert!(band >= 16, "a line of type under each row: {band}");
    assert_eq!(result.size, [2 * (CELL[0] + 8) + 8, 2 * (CELL[1] + band + 8) + 8]);
    let image = picture(&harness, &result.hash);
    assert_eq!([image.width(), image.height()], result.size);
    // Each label is the start of its picture's hash.
    for (label, uri) in result.cells.iter().zip(&uris) {
        assert_eq!(label.len(), 12);
        assert!(uri.starts_with(&format!("omuzaki://render/{label}")), "{label} names {uri}");
    }
    assert_eq!(sheet(&harness, args).unwrap(), result, "the same arguments give the same sheet");
}

#[test]
fn expressions_make_one_cell_each() {
    let harness = harness();
    let args = json!({ "character": "kit", "expressions": ["happy", "sad"], "cell": CELL, "framing": "face" });
    let result = sheet(&harness, args.clone()).unwrap();
    assert_eq!((result.columns, result.rows, result.cell), (2, 1, CELL));
    assert_eq!(result.cells, ["happy", "sad"]);
    let image = picture(&harness, &result.hash);
    let band = band(&result);
    assert_ne!(cell(&image, &result, band, 0), cell(&image, &result, band, 1), "the two faces differ");
    assert_eq!(sheet(&harness, args).unwrap(), result, "the same bytes every run");
}

#[test]
fn variants_make_one_cell_each() {
    let harness = harness();
    let args = json!({ "character": "mock", "variants": ["game", "close-up"], "cell": CELL, "labels": false });
    let result = sheet(&harness, args).unwrap();
    assert_eq!((result.columns, result.rows, result.cell), (2, 1, CELL));
    assert_eq!(result.cells, ["game", "close-up"]);
    assert_eq!(result.size, [2 * (CELL[0] + 8) + 8, CELL[1] + 16], "no label band without labels");
    let image = picture(&harness, &result.hash);
    assert_ne!(cell(&image, &result, 0, 0), cell(&image, &result, 0, 1), "the two variants differ");
}

#[test]
fn labels_change_the_sheet() {
    let harness = harness();
    let uris = previews(&harness);
    let plain = sheet(&harness, json!({ "images": uris, "labels": false })).unwrap();
    let labelled = sheet(&harness, json!({ "images": uris })).unwrap();
    assert_ne!(plain.hash, labelled.hash);
    assert!(labelled.size[1] > plain.size[1], "the labels take a band under each row");
    // The band under the first cell has ink in it: the label is drawn, dark on the paper.
    let image = picture(&harness, &labelled.hash);
    let band = band(&labelled);
    let top = 8 + labelled.cell[1];
    let ink = (8..8 + labelled.cell[0])
        .flat_map(|x| (top..top + band).map(move |y| (x, y)))
        .filter(|&(x, y)| image.get_pixel(x, y).0[..3].iter().all(|&c| c < 96))
        .count();
    assert!(ink > 10, "{ink} dark pixels in the label band");
    // Without labels the sheet is transparent around the pictures; with them it is opaque paper.
    assert_eq!(picture(&harness, &plain.hash).get_pixel(0, 0).0[3], 0);
    assert_eq!(image.get_pixel(0, 0).0[3], 255);
}

#[test]
fn limits_are_refused() {
    let harness = harness();
    let uris = previews(&harness);
    let refused = |args: Value| sheet(&harness, args).expect_err("refused");
    let unknown_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let many: Vec<&str> = std::iter::repeat_n(uris[0].as_str(), 65).collect();
    let kit = |extra: Value| {
        let mut args = json!({ "character": "kit", "expressions": ["happy"], "cell": CELL });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        args
    };
    for (args, code, field) in [
        (json!({ "images": [] }), ErrorCode::Invalid, "images"),
        (json!({ "images": many }), ErrorCode::Invalid, "images"),
        (json!({ "images": uris, "columns": 17 }), ErrorCode::Invalid, "columns"),
        (json!({ "images": uris, "columns": 0 }), ErrorCode::Invalid, "columns"),
        (json!({ "images": [unknown_hash] }), ErrorCode::NotFound, ""),
        (kit(json!({ "cell": [2048, 2048] })), ErrorCode::Invalid, "cell"),
        (kit(json!({ "expressions": ["happy", "no-such-face"] })), ErrorCode::Invalid, "expressions/1"),
        (kit(json!({ "images": uris })), ErrorCode::Invalid, "images"),
        (kit(json!({ "variants": ["game"] })), ErrorCode::Invalid, "variants"),
        (kit(json!({ "expression": "happy" })), ErrorCode::Invalid, "expression"),
        (kit(json!({ "framing": "hands" })), ErrorCode::Invalid, "framing"),
        (kit(json!({ "light_rig": "nope" })), ErrorCode::Invalid, "light_rig"),
        (kit(json!({ "character": "nobody" })), ErrorCode::NotFound, ""),
        (json!({ "expressions": ["happy"] }), ErrorCode::Invalid, "character"),
        (json!({ "character": "kit" }), ErrorCode::Invalid, "expressions"),
        (json!({ "character": "mock", "variants": ["game", "nope"], "cell": CELL }), ErrorCode::NotFound, "variants/1"),
    ] {
        let error = refused(args.clone());
        assert_eq!(error.code, code, "{args}: {error:?}");
        if !field.is_empty() {
            assert_eq!(error.field.as_deref(), Some(field), "{args}: {error:?}");
        }
    }
}
