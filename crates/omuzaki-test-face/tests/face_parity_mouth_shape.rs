//! Parity of the mouth shape and the kiss with the JS prototype: the painter's closed line, kiss and open shape
//! (`face.js` 290-318 and 324-336, the prototype's `mouth` section) against the prototype's isolated `mouth` reference
//! of every golden expression in both variants, under the face rule (rule `T_FACE`, premultiplied over mid-grey).
//!
//! The comparison is of that one tag. The painter's mouth section grows pieces as they land (interior, teeth, rim, tongue,
//! lower lip) and each has its own test, so this one paints the shape piece alone
//! ([`RealPainter::paint_mouth_piece`]) and stacks only the `mouth` section of the references.
//! (`omuzaki_test_face::face_parity::check_section` compares a whole section against all its tags, which would fail here the
//! moment the interior is drawn; a tag-subset check in the harness is filed as a follow-up.)
//!
//! The references and the face profile are the sample character's, so they stay in a local assets folder: the test skips, saying why,
//! when they are absent, and the pictures it writes (`OMUZAKI_FACE_PARITY_OUT`, an `--ignored` run) go there too. The
//! parameter extremes of the evidence sheet are painted from made-up numbers and a default face.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use image::RgbaImage;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{MouthPiece, RealPainter};
use omuzaki_face_api::{
    FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FacePainter, FaceProfile, FaceProjection, FaceRasterizer,
    FaceView, Raster,
};
use omuzaki_test::jsref::{JsRef, Xy};
use omuzaki_test::regioncmp::{Rect, RegionReport, compare_region, write_side_by_side};
use omuzaki_test_face::face_parity::js_params::{apply_style, case_inputs, paint_params};
use omuzaki_test_face::face_parity::{SectionTolerance, load_setup};
use serde::Deserialize;

/// The tolerance on the pixels as they are. The wave-3 face rule (per channel 12, 1 %, 34 dB) cannot be met by a mouth:
/// the section is a few thin strokes, so a tight box round it is mostly edge pixels, and the references' edges are
/// quantised by Chrome's canvas (coverage in quarters: the golden `neutral` line has a 0.75 pixel at one edge and
/// none at the other where the analytic rasteriser has 0.89 and 0.24), a difference of up to half a coverage step on
/// every edge pixel. Measured over the 26 golden cases (`measure_the_noise_floor`): per channel 12 gives up to 7.8 %
/// bad pixels, 48 gives 0.85 %, 64 gives 0.26 %; the lowest PSNR is 31.9 dB (the kiss, three translucent strokes). The 54
/// parameter extremes of the evidence sheet (not enforced here) stay inside the same limits: at most 0.99 % bad pixels
/// (a narrow closed line, 33.3 dB) and 31.3 dB at the lowest (the kiss).
/// The limits below are set just outside what was measured, and the centroid and ink checks hold the geometry tighter.
const TOLERANCE: SectionTolerance = SectionTolerance {
    pixels: omuzaki_test::compare::Tolerance { per_channel: 48, max_bad_fraction: 0.01 },
    min_psnr_db: 31.0,
};

/// How far the painted shape's centre of ink may sit from the prototype's, in pixels, and how much its total ink may
/// differ (a fraction). These do not depend on how an edge pixel is quantised, so they hold the geometry (position,
/// width, thickness) tighter than the per-pixel rule can: the golden cases measure at most 0.134 px and 1.17 %.
const MAX_CENTROID_SHIFT_PX: f64 = 0.15;
/// See [`MAX_CENTROID_SHIFT_PX`].
const MAX_INK_DIFFERENCE: f64 = 0.025;

/// Pixels around the mouth's box that still count as compared (the harness's own margin).
const PAD: u32 = 8;

/// The prototype tag this test covers.
const TAG: &str = "mouth";

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

/// One compared case.
struct Case {
    name: String,
    /// `None` when neither side drew a mouth shape.
    compared: Option<Compared>,
    /// Painted pixels outside the compared rectangle, or the whole shape missing (or extra).
    stray_pixels: u64,
}

struct Compared {
    report: RegionReport,
    /// Where the painted ink sits against the prototype's, and how much there is of it.
    ink: InkCheck,
    actual: RgbaImage,
    reference: RgbaImage,
}

impl Case {
    fn passed(&self) -> bool {
        let Some(c) = &self.compared else { return self.stray_pixels == 0 };
        let area = f64::from(c.report.rect.width) * f64::from(c.report.rect.height);
        #[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^52
        let stray_ok = self.stray_pixels as f64 <= TOLERANCE.pixels.max_bad_fraction * area;
        c.report.passes(TOLERANCE.min_psnr_db) && c.ink.passes() && stray_ok
    }

    fn row(&self) -> String {
        match &self.compared {
            None => format!(
                "| {} | nothing drawn by either | - | - | - | {} | {} |",
                self.name,
                self.stray_pixels,
                self.verdict()
            ),
            Some(c) => {
                let s = &c.report.stats;
                let psnr = s.psnr_db.map_or_else(|| "inf".to_owned(), |db| format!("{db:.2} dB"));
                format!(
                    "| {} | {} | {psnr} | {} | {:.4}% | {:.3} px | {:+.2}% | {} | {} |",
                    self.name,
                    c.report.rect,
                    s.max_delta,
                    s.bad_fraction * 100.0,
                    c.ink.shift_px,
                    c.ink.mass_ratio_percent(),
                    self.stray_pixels,
                    self.verdict()
                )
            }
        }
    }

    fn verdict(&self) -> &'static str {
        if self.passed() { "pass" } else { "**FAIL**" }
    }
}

const TABLE_HEADER: &str = "| case | region | PSNR | max delta | bad pixels | ink centre shift | ink mass | stray px | result |\n|---|---|---:|---:|---:|---:|---:|---:|---|";

fn table(cases: &[Case]) -> String {
    let rows: Vec<String> = cases.iter().map(Case::row).collect();
    format!("{TABLE_HEADER}\n{}\n", rows.join("\n"))
}

// ---- pixels

/// `src` (straight alpha) over `dst` (premultiplied), in integer maths, as the harness stacks section PNGs.
fn unpremultiply(px: [u8; 4]) -> [u8; 4] {
    let a = u32::from(px[3]);
    if a == 0 {
        return [0; 4];
    }
    let un = |c: u8| u8::try_from(((u32::from(c) * 255 + a / 2) / a).min(255)).unwrap_or(u8::MAX);
    [un(px[0]), un(px[1]), un(px[2]), px[3]]
}

/// The bounding box `[x0, y0, x1, y1)` of the pixels with any ink, or `None` for an empty picture.
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

fn stray(actual: &RgbaImage, rect: Option<Rect>, limit: u8) -> u64 {
    actual
        .enumerate_pixels()
        .filter(|(x, y, p)| {
            let inside = rect.is_some_and(|r| *x >= r.x && *x < r.x + r.width && *y >= r.y && *y < r.y + r.height);
            !inside && p.0[3] > limit
        })
        .count() as u64
}

/// Where the ink of a painted picture sits against the reference's, and how much of it there is.
struct InkCheck {
    /// Distance between the two alpha-weighted centres, in pixels.
    shift_px: f64,
    /// Painted ink over reference ink.
    mass_ratio: f64,
}

impl InkCheck {
    fn passes(&self) -> bool {
        self.shift_px <= MAX_CENTROID_SHIFT_PX && (self.mass_ratio - 1.0).abs() <= MAX_INK_DIFFERENCE
    }

    fn mass_ratio_percent(&self) -> f64 {
        (self.mass_ratio - 1.0) * 100.0
    }
}

/// The alpha-weighted centre and total of `img` inside `rect`.
fn alpha_centre(img: &RgbaImage, rect: Rect) -> (f64, f64, f64) {
    let (mut total, mut sx, mut sy) = (0.0, 0.0, 0.0);
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            let a = f64::from(img.get_pixel(x, y).0[3]);
            (total, sx, sy) = (total + a, sx + a * f64::from(x), sy + a * f64::from(y));
        }
    }
    if total == 0.0 { (0.0, 0.0, 0.0) } else { (total, sx / total, sy / total) }
}

fn ink_check(actual: &RgbaImage, reference: &RgbaImage, rect: Rect) -> InkCheck {
    let ((ta, ax, ay), (tr, rx, ry)) = (alpha_centre(actual, rect), alpha_centre(reference, rect));
    InkCheck { shift_px: (ax - rx).hypot(ay - ry), mass_ratio: if tr == 0.0 { 1.0 } else { ta / tr } }
}

// ---- the painter's side

/// The shape piece through the CPU rasteriser, straight alpha; `None` when it drew nothing.
fn paint_shape(input: &FaceInput<'_>, projection: FaceProjection, size: (u32, u32)) -> Option<RgbaImage> {
    #[allow(clippy::cast_precision_loss)] // canvas sizes are a few thousand pixels
    let px_per_m = size.0 as f32 / projection.rect.width();
    let mut painter = RealPainter::new(projection, px_per_m);
    let mut drawing = FaceDrawing::new();
    painter.paint_mouth_piece(MouthPiece::Shape, input, &mut drawing).unwrap();
    let list = &drawing[FaceLayerId::Base];
    if list.is_empty() {
        return None;
    }
    let spec: &FaceLayerSpec = painter.layers().iter().find(|s| s.id == FaceLayerId::Base).unwrap();
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(spec, list, spec.px_per_m, &mut raster).unwrap();
    assert_eq!((raster.width, raster.height), size, "the base layer is the texture's size");
    let pixels: Vec<u8> = raster.pixels.as_chunks::<4>().0.iter().copied().flat_map(unpremultiply).collect();
    RgbaImage::from_raw(size.0, size.1, pixels)
}

/// Compares the painter's picture with the prototype's (`None`: the prototype drew nothing) and reports.
fn judge(name: String, actual: Option<&RgbaImage>, reference: Option<&RgbaImage>, size: (u32, u32)) -> Case {
    let blank = RgbaImage::new(size.0, size.1);
    let boxes: Vec<[u32; 4]> = [actual, reference].into_iter().flatten().filter_map(ink_box).collect();
    let Some(rect) = region(&boxes, size) else {
        return Case { name, compared: None, stray_pixels: 0 };
    };
    let (a, r) = (actual.unwrap_or(&blank), reference.unwrap_or(&blank));
    let report = compare_region(a, r, rect, TOLERANCE.pixels).unwrap();
    Case {
        name,
        stray_pixels: 0,
        compared: Some(Compared { report, ink: ink_check(a, r, rect), actual: a.clone(), reference: r.clone() }),
    }
}

// ---- the goldens

fn read_index(js: &JsRef) -> Option<Index> {
    let bytes = std::fs::read(js.root().join("face-sections").join("index.json")).ok()?;
    Some(serde_json::from_slice(&bytes).expect("index.json parses"))
}

/// The prototype's `mouth` section for a case (`None`: it painted none), and the size of the texture.
fn reference(js: &JsRef, case: &IndexCase, size: (u32, u32)) -> Option<RgbaImage> {
    let entry = case.sections.iter().find(|s| s.tag == TAG)?;
    let png = image::open(js.root().join(&entry.file)).unwrap().to_rgba8();
    assert_eq!(png.dimensions(), size, "{}", entry.file);
    // The PNG is the section alone on a transparent canvas, in straight alpha already.
    Some(png)
}

/// Runs every golden case. `None` when the references are absent (printing why).
fn run_goldens() -> Option<Vec<Case>> {
    let Some(js) = JsRef::open() else {
        eprintln!("skipped: no JS reference dump");
        return None;
    };
    let Some(index) = read_index(&js) else {
        eprintln!("skipped: no face-section references at {}/face-sections", js.root().display());
        return None;
    };
    let mut cases = Vec::new();
    let mut states = BTreeMap::new();
    for case in &index.cases {
        let texture = js.root().join(&case.variant).join("face-textures").join(format!("{}.png", case.expression));
        let size = image::image_dimensions(&texture).unwrap();
        let (setup, face) = states
            .entry(case.variant.clone())
            .or_insert_with(|| (load_setup(&js, &case.variant, size).unwrap(), js.face_states(&case.variant).unwrap()));
        // The synthetic tear and tongue cases have no face state; they cannot be painted.
        let Some(state) = face.case(&case.id) else { continue };
        assert!(
            !setup.defaulted.contains(&"projection") && !setup.defaulted.contains(&"profile"),
            "the {} setup file (face-sections/parity-setup) must give the projection and the profile: parity against a default face means nothing",
            case.variant
        );
        let (inputs, profile) = case_inputs(state, &setup.profile, setup.follow_weight).unwrap();
        let input = FaceInput { profile: &profile, params: &inputs.params, time: inputs.time, view: inputs.view };
        let actual = paint_shape(&input, setup.projection, size);
        let reference = reference(&js, case, size);
        let mut judged =
            judge(format!("{} / {}", case.variant, case.expression), actual.as_ref(), reference.as_ref(), size);
        // The painter drew a shape where the prototype drew none (or the reverse): a failure whatever the numbers.
        if actual.is_none() != reference.is_none() {
            judged.stray_pixels += u64::from(u32::MAX);
        }
        let rect = judged.compared.as_ref().map(|c| c.report.rect);
        judged.stray_pixels += actual.as_ref().map_or(0, |a| stray(a, rect, TOLERANCE.pixels.per_channel));
        cases.push(judged);
    }
    Some(cases)
}

#[test]
fn the_mouth_shape_matches_the_prototype() {
    let Some(cases) = run_goldens() else { return };
    println!("{}", table(&cases));
    assert!(!cases.is_empty(), "no case could be compared");
    let failed: Vec<&Case> = cases.iter().filter(|c| !c.passed()).collect();
    assert!(
        failed.is_empty(),
        "{} case(s) outside {TOLERANCE:?}:\n{}",
        failed.len(),
        failed.iter().map(|c| c.row()).collect::<Vec<_>>().join("\n")
    );
}

// ---- the evidence

fn out_dir() -> Option<PathBuf> {
    let out = std::env::var_os("OMUZAKI_FACE_PARITY_OUT").map(PathBuf::from);
    if out.is_none() {
        eprintln!("set OMUZAKI_FACE_PARITY_OUT to the output folder");
    }
    out
}

fn write_cases(dir: &Path, cases: &[Case], table_name: &str) {
    for case in cases {
        if let Some(c) = &case.compared {
            let file = format!("{}.png", case.name.replace(" / ", "__").replace(' ', "_"));
            write_side_by_side(&dir.join(file), &c.actual, &c.reference, &c.report, 1).unwrap();
        }
    }
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(table_name), table(cases)).unwrap();
}

/// Writes the reference / painter / diff picture of every golden case and the table, to `OMUZAKI_FACE_PARITY_OUT/golden`.
/// `cargo test -p omuzaki-test-face --test face_parity_mouth_shape -- --ignored`.
#[test]
#[ignore = "writes evidence to a local assets folder"]
fn write_the_golden_evidence() {
    let Some(out) = out_dir() else { return };
    let Some(cases) = run_goldens() else { return };
    write_cases(&out.join("golden"), &cases, "golden-table.md");
}

/// Prints, for a range of per-channel limits, the worst bad-pixel fraction and the lowest PSNR over every golden case:
/// the noise floor the tolerance was chosen from (see the PR). `-- --ignored --nocapture`.
#[test]
#[ignore = "prints a measurement"]
fn measure_the_noise_floor() {
    let Some(cases) = run_goldens() else { return };
    for limit in [12_u8, 16, 24, 32, 48, 64, 96, 128] {
        let tolerance = omuzaki_test::compare::Tolerance { per_channel: limit, max_bad_fraction: 1.0 };
        let (mut worst, mut worst_case) = (0.0_f64, String::new());
        for case in &cases {
            let Some(c) = &case.compared else { continue };
            let report = compare_region(&c.actual, &c.reference, c.report.rect, tolerance).unwrap();
            if report.stats.bad_fraction > worst {
                (worst, worst_case) = (report.stats.bad_fraction, case.name.clone());
            }
        }
        println!("per channel {limit:>3}: worst bad fraction {:.3}% ({worst_case})", worst * 100.0);
    }
    let psnr = cases.iter().filter_map(|c| c.compared.as_ref()?.report.stats.psnr_db).fold(f64::MAX, f64::min);
    println!("lowest PSNR {psnr:.2} dB");
}

/// One extreme: a name and the prototype's full face record (every `P` key), written by `proto_mouth.mjs` beside the
/// prototype's pictures (`OMUZAKI_FACE_PARITY_OUT/extremes/js/<name>.png`).
#[derive(Deserialize)]
struct Extreme {
    name: String,
    p: BTreeMap<String, f64>,
}

/// Paints every extreme of `extremes/cases.json` with the sheet look's window and a default face, compares it with
/// the prototype's picture and writes pictures and `extremes-table.md`.
#[test]
#[ignore = "writes evidence to a local assets folder"]
fn write_the_extremes_evidence() {
    let Some(out) = out_dir() else { return };
    let Some(js) = JsRef::open() else { return };
    let bytes = std::fs::read(out.join("extremes").join("cases.json")).expect("run proto_mouth.mjs first");
    let extremes: Vec<Extreme> = serde_json::from_slice(&bytes).unwrap();
    let size = (1536, 1107);
    let setup = load_setup(&js, "game", size).unwrap();
    let mut cases = Vec::new();
    for e in &extremes {
        let mut profile: FaceProfile = setup.profile.clone();
        apply_style(&mut profile, &e.p).unwrap();
        let zero = Xy { x: 0.0, y: 0.0 };
        let params = paint_params(&e.p, zero, zero, 0.0).unwrap();
        let input = FaceInput {
            profile: &profile,
            params: &params,
            time: omuzaki_core::time::Seconds(0.0),
            view: FaceView::default(),
        };
        let actual = paint_shape(&input, setup.projection, size);
        let reference = image::open(out.join("extremes").join("js").join(format!("{}.png", e.name)))
            .ok()
            .map(|i| i.to_rgba8())
            .filter(|i| ink_box(i).is_some());
        cases.push(judge(e.name.clone(), actual.as_ref(), reference.as_ref(), size));
    }
    write_cases(&out.join("extremes").join("diff"), &cases, "extremes-table.md");
}
