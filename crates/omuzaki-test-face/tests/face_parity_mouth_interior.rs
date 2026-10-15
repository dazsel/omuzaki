//! Parity of the mouth interior pieces with the JS prototype (`face.js` 278-289 and 337-379): the lower-lip gloss, the
//! inside of the open mouth (tongue, teeth, fangs), the lip rim and the tongue tab, each painted alone
//! ([`RealPainter::paint_mouth_piece`]) against the stack of the prototype's isolated references it covers, over every
//! golden expression in both variants, under the face rule (`T_FACE`, premultiplied over mid-grey).
//!
//! The rule is the issue's starting point (per channel 12, 1 % bad pixels, 34 dB), not yet a measured one: the first run
//! with the references says what the port achieves per piece, and the final numbers are proposed from that.
//!
//! The references and the face profile are the sample character's, so they stay in a local assets folder: the test
//! skips, saying why, when they are absent (always, in the cloud), and the side-by-side pictures it writes
//! (`OMUZAKI_FACE_PARITY_OUT`, an `--ignored` run) go there too.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use image::RgbaImage;
use image::imageops::overlay;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{MouthPiece, RealPainter};
use omuzaki_face_api::{FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FacePainter, FaceRasterizer, Raster};
use omuzaki_test::jsref::JsRef;
use omuzaki_test::regioncmp::{Rect, compare_region, write_side_by_side};
use omuzaki_test_face::face_parity::SectionTolerance;
use omuzaki_test_face::face_parity::js_params::case_inputs;
use omuzaki_test_face::face_parity::load_setup;
use serde::Deserialize;

/// The rule every piece is held to: the issue's starting numbers.
const TOLERANCE: SectionTolerance = SectionTolerance::T_FACE;
/// Pixels around the pieces' ink that still count as compared.
const PAD: u32 = 8;

/// Each piece with the prototype tags it covers, in painting order.
const PIECES: [(MouthPiece, &[&str]); 4] = [
    (MouthPiece::Gloss, &["lower-lip"]),
    (MouthPiece::Interior, &["mouth-interior", "teeth"]),
    (MouthPiece::Rim, &["lip-rim"]),
    (MouthPiece::Tongue, &["tongue"]),
];

#[derive(Deserialize)]
struct Index {
    cases: Vec<IndexCase>,
}

#[derive(Deserialize)]
struct IndexCase {
    id: String,
    variant: String,
    expression: String,
    sections: Vec<IndexSection>,
}

#[derive(Deserialize)]
struct IndexSection {
    tag: String,
    file: String,
}

/// One compared piece of one case.
struct Row {
    name: String,
    rect: Rect,
    stats_line: String,
    passed: bool,
    pictures: (RgbaImage, RgbaImage, omuzaki_test::regioncmp::RegionReport),
}

fn unpremultiply(px: [u8; 4]) -> [u8; 4] {
    let a = u32::from(px[3]);
    if a == 0 {
        return [0; 4];
    }
    let un = |c: u8| u8::try_from(((u32::from(c) * 255 + a / 2) / a).min(255)).unwrap_or(u8::MAX);
    [un(px[0]), un(px[1]), un(px[2]), px[3]]
}

/// The bounding box `[x0, y0, x1, y1)` of the pixels with any ink.
fn ink_box(img: &RgbaImage) -> Option<[u32; 4]> {
    img.enumerate_pixels().filter(|(_, _, p)| p.0[3] > 0).fold(None, |b, (x, y, _)| {
        let [x0, y0, x1, y1] = b.unwrap_or([x, y, x + 1, y + 1]);
        Some([x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)])
    })
}

/// The union of the boxes grown by [`PAD`] and clipped to the picture.
fn region(boxes: &[[u32; 4]], size: (u32, u32)) -> Option<Rect> {
    let u = boxes.iter().copied().reduce(|u, b| [u[0].min(b[0]), u[1].min(b[1]), u[2].max(b[2]), u[3].max(b[3])])?;
    let (x0, y0) = (u[0].saturating_sub(PAD), u[1].saturating_sub(PAD));
    let (x1, y1) = (u[2].saturating_add(PAD).min(size.0), u[3].saturating_add(PAD).min(size.1));
    Some(Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
}

/// One piece through the CPU rasteriser, straight alpha.
fn paint_piece(
    piece: MouthPiece,
    input: &FaceInput<'_>,
    setup: &omuzaki_test_face::face_parity::Setup,
    size: (u32, u32),
) -> RgbaImage {
    #[allow(clippy::cast_precision_loss)] // canvas sizes are a few thousand pixels
    let px_per_m = size.0 as f32 / setup.projection.rect.width();
    let mut painter = RealPainter::new(setup.projection, px_per_m);
    let mut drawing = FaceDrawing::new();
    painter.paint_mouth_piece(piece, input, &mut drawing).unwrap();
    let spec: &FaceLayerSpec = painter.layers().iter().find(|s| s.id == FaceLayerId::Base).unwrap();
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(spec, &drawing[FaceLayerId::Base], spec.px_per_m, &mut raster).unwrap();
    assert_eq!((raster.width, raster.height), size, "the base layer is the texture's size");
    let pixels: Vec<u8> = raster.pixels.as_chunks::<4>().0.iter().copied().flat_map(unpremultiply).collect();
    RgbaImage::from_raw(size.0, size.1, pixels).unwrap()
}

/// The prototype's references of `tags` that painted in this case, stacked in order on a transparent canvas.
fn reference(js: &JsRef, case: &IndexCase, tags: &[&str], size: (u32, u32)) -> RgbaImage {
    let mut stack = RgbaImage::new(size.0, size.1);
    for tag in tags {
        let Some(entry) = case.sections.iter().find(|s| s.tag == *tag) else { continue };
        let png = image::open(js.root().join(&entry.file)).unwrap().to_rgba8();
        assert_eq!(png.dimensions(), size, "{}", entry.file);
        overlay(&mut stack, &png, 0, 0);
    }
    stack
}

/// Runs every golden case against every piece. `None` when the references are absent (printing why).
fn run_goldens() -> Option<Vec<Row>> {
    let Some(js) = JsRef::open() else {
        eprintln!("skipped: no JS reference dump (the sample character's renders are private and absent here)");
        return None;
    };
    let Ok(bytes) = std::fs::read(js.root().join("face-sections").join("index.json")) else {
        eprintln!("skipped: no face-section references at {}/face-sections", js.root().display());
        return None;
    };
    let index: Index = serde_json::from_slice(&bytes).expect("index.json parses");
    let (mut rows, mut states) = (Vec::new(), BTreeMap::new());
    for case in &index.cases {
        let texture = js.root().join(&case.variant).join("face-textures").join(format!("{}.png", case.expression));
        let size = image::image_dimensions(&texture).unwrap();
        let (setup, face) = states
            .entry(case.variant.clone())
            .or_insert_with(|| (load_setup(&js, &case.variant, size).unwrap(), js.face_states(&case.variant).unwrap()));
        let Some(state) = face.case(&case.id) else { continue };
        assert!(
            !setup.defaulted.contains(&"projection") && !setup.defaulted.contains(&"profile"),
            "the {} setup file must give the projection and the profile: parity against a default face means nothing",
            case.variant
        );
        let (inputs, profile) = case_inputs(state, &setup.profile, setup.follow_weight).unwrap();
        let input = FaceInput { profile: &profile, params: &inputs.params, time: inputs.time, view: inputs.view };
        for (piece, tags) in PIECES {
            let actual = paint_piece(piece, &input, setup, size);
            let expected = reference(&js, case, tags, size);
            let boxes: Vec<[u32; 4]> = [&actual, &expected].into_iter().filter_map(ink_box).collect();
            let Some(rect) = region(&boxes, size) else { continue };
            let report = compare_region(&actual, &expected, rect, TOLERANCE.pixels).unwrap();
            let psnr = report.stats.psnr_db.map_or_else(|| "inf".to_owned(), |db| format!("{db:.2} dB"));
            rows.push(Row {
                name: format!("{} / {} / {piece:?}", case.variant, case.expression),
                rect,
                stats_line: format!(
                    "{psnr}, max delta {}, {:.4}% bad",
                    report.stats.max_delta,
                    report.stats.bad_fraction * 100.0
                ),
                passed: report.passes(TOLERANCE.min_psnr_db),
                pictures: (actual, expected, report),
            });
        }
    }
    Some(rows)
}

#[test]
fn the_mouth_interior_matches_the_prototype() {
    let Some(rows) = run_goldens() else { return };
    for r in &rows {
        println!("| {} | {} | {} | {} |", r.name, r.rect, r.stats_line, if r.passed { "pass" } else { "**FAIL**" });
    }
    assert!(!rows.is_empty(), "no case could be compared");
    let failed: Vec<String> =
        rows.iter().filter(|r| !r.passed).map(|r| format!("{}: {}", r.name, r.stats_line)).collect();
    assert!(failed.is_empty(), "{} piece(s) outside {TOLERANCE:?}:\n{}", failed.len(), failed.join("\n"));
}

/// Writes the painter / prototype / difference picture of every compared piece to `OMUZAKI_FACE_PARITY_OUT/interior`.
/// `cargo test -p omuzaki-test-face --test face_parity_mouth_interior -- --ignored`.
#[test]
#[ignore = "writes evidence to a local assets folder"]
fn write_the_golden_evidence() {
    let Some(out) = std::env::var_os("OMUZAKI_FACE_PARITY_OUT").map(PathBuf::from) else {
        eprintln!("set OMUZAKI_FACE_PARITY_OUT to the output folder");
        return;
    };
    let Some(rows) = run_goldens() else { return };
    let dir = out.join("interior");
    std::fs::create_dir_all(&dir).unwrap();
    for r in &rows {
        let file = format!("{}.png", r.name.replace(" / ", "__").replace(' ', "_"));
        write_side_by_side(&dir.join(file), &r.pictures.0, &r.pictures.1, &r.pictures.2, 1).unwrap();
    }
}
