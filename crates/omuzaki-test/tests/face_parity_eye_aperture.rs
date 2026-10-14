//! Parity of the eye aperture and the eye white with the JS prototype: the painter's clipped white
//! (`face.js` lines 101-140) against the prototype's isolated `eye-L-sclera` and `eye-R-sclera` sections, for every
//! golden expression in both variants.
//!
//! The comparison is of those two parts alone. The painter's eyes section grows pieces as they land (iris,
//! lash, ...), and each of those has its own test, so this one keeps only what the aperture draws: the fill of the
//! aperture, which comes just before the clip to it. (`omuzaki_test::face_parity::check_section` compares a whole section against all its tags,
//! which would fail here the moment the iris is drawn; a tag-subset check in the harness would replace [`eye_whites`]
//! and the reference reading below, and is filed as a follow-up.)
//!
//! The references and the face profile are the sample character's, so they stay in a local assets folder: the test skips, saying why,
//! when they are absent, and the pictures it writes (`OMUZAKI_FACE_PARITY_OUT`, an `--ignored` run) go there too.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::{Path, PathBuf};

use image::RgbaImage;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    Cmd, DrawList, FaceDrawing, FaceInput, FaceLayerId, FacePainter, FaceRasterizer, PathEl, PathRef, Raster,
};
use omuzaki_test::face_parity::js_params::case_inputs;
use omuzaki_test::face_parity::{SectionTolerance, load_setup};
use omuzaki_test::jsref::JsRef;
use omuzaki_test::regioncmp::{Rect, compare_region, write_side_by_side};
use serde::Deserialize;

/// The tolerance this section is held to: the face rule. Measured first (see the PR): the worst case is far inside it.
const TOLERANCE: SectionTolerance = SectionTolerance::T_FACE;

/// Pixels around the eyes' boxes that still count as compared (the harness's own margin).
const PAD: u32 = 8;

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
    bbox: [u32; 4],
}

/// One compared case.
struct Case {
    variant: String,
    expression: String,
    /// `None` when neither side drew an eye white (both eyes shut): nothing to compare.
    compared: Option<Compared>,
    /// Painted pixels where the prototype painted none.
    stray_pixels: u64,
}

struct Compared {
    report: omuzaki_test::regioncmp::RegionReport,
    actual: RgbaImage,
    reference: RgbaImage,
}

impl Case {
    fn passed(&self) -> bool {
        let Some(c) = &self.compared else { return self.stray_pixels == 0 };
        let area = f64::from(c.report.rect.width) * f64::from(c.report.rect.height);
        #[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^52
        let stray_ok = self.stray_pixels as f64 <= TOLERANCE.pixels.max_bad_fraction * area;
        c.report.passes(TOLERANCE.min_psnr_db) && stray_ok
    }

    fn row(&self) -> String {
        match &self.compared {
            None => format!(
                "| {} | {} | both eyes shut | - | - | - | {} | {} |",
                self.variant,
                self.expression,
                "-",
                self.verdict()
            ),
            Some(c) => {
                let s = &c.report.stats;
                let psnr = s.psnr_db.map_or_else(|| "inf".to_owned(), |db| format!("{db:.2} dB"));
                format!(
                    "| {} | {} | {} | {psnr} | {} | {:.4}% | {} | {} |",
                    self.variant,
                    self.expression,
                    c.report.rect,
                    s.max_delta,
                    s.bad_fraction * 100.0,
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

const TABLE_HEADER: &str = "| variant | expression | region | PSNR | max delta | bad pixels | stray px | result |\n|---|---|---|---:|---:|---:|---:|---|";

// ---- reading the references

fn read_index(js: &JsRef) -> Option<Index> {
    let bytes = std::fs::read(js.root().join("face-sections").join("index.json")).ok()?;
    Some(serde_json::from_slice(&bytes).expect("index.json parses"))
}

/// `src` (straight alpha) over `dst` (premultiplied), in integer maths, as the harness stacks section PNGs.
fn over(dst: &mut [u8; 4], src: [u8; 4]) {
    let alpha = u32::from(src[3]);
    let keep = 255 - alpha;
    let mix = |s: u32, d: u8| u8::try_from((s + (u32::from(d) * keep + 127) / 255).min(255)).unwrap_or(u8::MAX);
    *dst = [
        mix((u32::from(src[0]) * alpha + 127) / 255, dst[0]),
        mix((u32::from(src[1]) * alpha + 127) / 255, dst[1]),
        mix((u32::from(src[2]) * alpha + 127) / 255, dst[2]),
        mix(alpha, dst[3]),
    ];
}

/// A premultiplied pixel back to straight alpha, rounded to nearest.
fn unpremultiply(px: [u8; 4]) -> [u8; 4] {
    let a = u32::from(px[3]);
    if a == 0 {
        return [0; 4];
    }
    let un = |c: u8| u8::try_from(((u32::from(c) * 255 + a / 2) / a).min(255)).unwrap_or(u8::MAX);
    [un(px[0]), un(px[1]), un(px[2]), px[3]]
}

fn straight(stack: Vec<[u8; 4]>, size: (u32, u32)) -> RgbaImage {
    RgbaImage::from_raw(size.0, size.1, stack.into_iter().flat_map(unpremultiply).collect()).unwrap()
}

/// The prototype's eye whites for a case: its sclera sections stacked, and the box round them.
fn reference(js: &JsRef, case: &IndexCase, size: (u32, u32)) -> (RgbaImage, Option<Rect>) {
    let entries: Vec<&IndexSection> =
        case.sections.iter().filter(|s| s.tag == "eye-L-sclera" || s.tag == "eye-R-sclera").collect();
    let mut stack = vec![[0_u8; 4]; size.0 as usize * size.1 as usize];
    for entry in &entries {
        let png = image::open(js.root().join(&entry.file)).unwrap().to_rgba8();
        assert_eq!(png.dimensions(), size, "{}", entry.file);
        for (dst, src) in stack.iter_mut().zip(png.pixels()) {
            over(dst, src.0);
        }
    }
    let rect = entries
        .iter()
        .map(|e| e.bbox)
        .reduce(|u, b| [u[0].min(b[0]), u[1].min(b[1]), u[2].max(b[2]), u[3].max(b[3])])
        .map(|u| {
            let (x0, y0) = (u[0].saturating_sub(PAD), u[1].saturating_sub(PAD));
            let (x1, y1) = (u[2].saturating_add(PAD).min(size.0), u[3].saturating_add(PAD).min(size.1));
            Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 }
        });
    (straight(stack, size), rect)
}

// ---- the painter's side

/// Copies `path` of `src` into `out`.
fn copy_path(src: &DrawList, path: PathRef, out: &mut DrawList) -> PathRef {
    let mut builder = out.path();
    for el in src.elements(path) {
        builder = match *el {
            PathEl::Move(p) => builder.move_to(p),
            PathEl::Line(p) => builder.line_to(p),
            PathEl::Quad(c, p) => builder.quad_to(c, p),
            PathEl::Cubic(a, b, p) => builder.cubic_to(a, b, p),
            PathEl::Close => builder.close(),
        };
    }
    builder.finish()
}

/// The eye whites of a painted base layer: each fill that is followed by a clip of the same path (the white, then the
/// clip the iris and highlights draw inside). Everything else the eyes section draws (iris, lash, ...) belongs to
/// other tests.
fn eye_whites(src: &DrawList) -> DrawList {
    let mut out = DrawList::new();
    let mut depth = 0_usize;
    for (i, cmd) in src.cmds().iter().enumerate() {
        match *cmd {
            Cmd::PushClip(_) => depth += 1,
            Cmd::PopClip => depth -= 1,
            // The iris is filled inside the clip too, with a clip of its own right after; only a fill outside every
            // clip is a white.
            Cmd::Fill { path, paint, opacity } if depth == 0 => {
                if let Some(Cmd::PushClip(clip)) = src.cmds().get(i + 1).copied()
                    && src.elements(path) == src.elements(clip)
                {
                    let path = copy_path(src, path, &mut out);
                    out.push(Cmd::Fill { path, paint, opacity });
                }
            }
            _ => {}
        }
    }
    out
}

/// The painter's eye whites rasterised on the CPU, straight alpha; `None` when it drew none.
fn paint_whites(
    input: &FaceInput<'_>,
    projection: omuzaki_face_api::FaceProjection,
    size: (u32, u32),
) -> Option<RgbaImage> {
    #[allow(clippy::cast_precision_loss)] // canvas sizes are a few thousand pixels
    let px_per_m = size.0 as f32 / projection.rect.width();
    let mut painter = RealPainter::new(projection, px_per_m);
    let mut drawing = FaceDrawing::new();
    painter.paint_section(Section::Eyes, input, &mut drawing).unwrap();
    let whites = eye_whites(&drawing[FaceLayerId::Base]);
    if whites.is_empty() {
        return None;
    }
    let spec = painter.layers().iter().find(|s| s.id == FaceLayerId::Base).unwrap();
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(spec, &whites, spec.px_per_m, &mut raster).unwrap();
    assert_eq!((raster.width, raster.height), size, "the base layer is the texture's size");
    let pixels = raster.pixels.as_chunks::<4>().0.to_vec();
    Some(straight(pixels, size))
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

/// Runs every golden case. `None` when the references are absent (printing why).
fn run() -> Option<Vec<Case>> {
    let Some(js) = JsRef::open() else {
        eprintln!("skipped: no JS reference dump");
        return None;
    };
    let Some(index) = read_index(&js) else {
        eprintln!("skipped: no face-section references at {}/face-sections", js.root().display());
        return None;
    };
    let mut cases = Vec::new();
    let mut states = std::collections::BTreeMap::new();
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
        let (reference, rect) = reference(&js, case, size);
        let actual = paint_whites(&input, setup.projection, size);
        let compared = match (&actual, rect) {
            (Some(actual), Some(rect)) => {
                let report = compare_region(actual, &reference, rect, TOLERANCE.pixels).unwrap();
                Some(Compared { report, actual: actual.clone(), reference })
            }
            _ => None,
        };
        let stray_pixels = actual.as_ref().map_or(0, |a| stray(a, rect, TOLERANCE.pixels.per_channel));
        // The painter drew whites where the prototype drew none (or the reverse): a failure whatever the numbers.
        let missing = u64::from(actual.is_none() != rect.is_none()) * u64::from(u32::MAX);
        cases.push(Case {
            variant: case.variant.clone(),
            expression: case.expression.clone(),
            compared,
            stray_pixels: stray_pixels + missing,
        });
    }
    Some(cases)
}

fn table(cases: &[Case]) -> String {
    let rows: Vec<String> = cases.iter().map(Case::row).collect();
    format!("{TABLE_HEADER}\n{}\n", rows.join("\n"))
}

#[test]
fn the_eye_whites_match_the_prototype() {
    let Some(cases) = run() else { return };
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

/// Writes the reference / painter / diff picture of every compared case and the table, to `OMUZAKI_FACE_PARITY_OUT`
/// (the evidence for the PR). `cargo test -p omuzaki-test --test face_parity_eye_aperture -- --ignored`.
#[test]
#[ignore = "writes evidence to a local assets folder"]
fn write_the_evidence() {
    let Some(out) = std::env::var_os("OMUZAKI_FACE_PARITY_OUT").map(PathBuf::from) else {
        eprintln!("set OMUZAKI_FACE_PARITY_OUT to the output folder");
        return;
    };
    let Some(cases) = run() else { return };
    for case in &cases {
        if let Some(c) = &case.compared {
            let path: PathBuf =
                Path::new(&out).join("golden").join(&case.variant).join(format!("{}.png", case.expression));
            write_side_by_side(&path, &c.actual, &c.reference, &c.report, 1).unwrap();
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("golden-table.md"), table(&cases)).unwrap();
}

/// One parameter set of the extremes sweep (`extremes_js.json`, written by evaluating the prototype's own `aperture()`
/// and path loop in node), and the prototype's path for it.
#[derive(Deserialize)]
struct Extreme {
    name: String,
    open: f32,
    tilt: f32,
    #[serde(rename = "lowLid")]
    low_lid: f32,
}

/// The left eye's aperture outline for one extreme, at the prototype's eye (centre at the origin, 0.051 half-width),
/// as `[kind, x, y, ...]` rows like the prototype's path log.
fn rust_outline(open: f32, tilt: f32, low_lid: f32) -> Vec<serde_json::Value> {
    let mut profile = omuzaki_face_api::FaceProfile::default();
    profile.layout.eye_spacing = 0.0;
    profile.layout.eye_y = 0.0;
    profile.layout.eye_width = 0.051;
    profile.eyes.aperture = omuzaki_face_api::Aperture {
        roundness: 0.56,
        tilt: 0.0,
        inner_sharpness: 0.8,
        outer_bluntness: 0.6,
        upper_height: 0.447_777_777_777_777_8,
        lower_depth: 0.56,
    };
    let eye = omuzaki_face_api::EyeParams { open, close: 0.0, arc: 0.0, lower_lid: low_lid };
    let params = omuzaki_face_api::PaintParams {
        left_eye: eye,
        right_eye: eye,
        eye_tilt: tilt,
        ..omuzaki_face_api::FaceMapping::default().rest_params()
    };
    let input = FaceInput {
        profile: &profile,
        params: &params,
        time: omuzaki_core::time::Seconds(0.0),
        view: omuzaki_face_api::FaceView::default(),
    };
    let mut painter = RealPainter::new(
        omuzaki_face_api::FaceProjection {
            rect: omuzaki_face_api::FaceRect { x0: -0.2, x1: 0.2, y0: -0.2, y1: 0.2 },
            front_fade: [0.0, 0.05],
        },
        omuzaki_face_api::DEFAULT_PX_PER_M,
    );
    let mut drawing = FaceDrawing::new();
    painter.paint_section(Section::Eyes, &input, &mut drawing).unwrap();
    let list = &drawing[FaceLayerId::Base];
    let Some(Cmd::Fill { path, .. }) = list.cmds().first().copied() else { return Vec::new() };
    list.elements(path)
        .iter()
        .map(|el| match *el {
            PathEl::Move(p) => serde_json::json!(["M", p.x, p.y]),
            PathEl::Line(p) => serde_json::json!(["L", p.x, p.y]),
            PathEl::Quad(c, p) => serde_json::json!(["Q", c.x, c.y, p.x, p.y]),
            PathEl::Cubic(..) => serde_json::json!(["C"]),
            PathEl::Close => serde_json::json!(["Z"]),
        })
        .collect()
}

/// Paints the sweep of `extremes_js.json` (in `OMUZAKI_FACE_PARITY_OUT`) and writes `extremes_rust.json` beside it.
#[test]
#[ignore = "writes evidence to a local assets folder"]
fn write_the_extremes() {
    let Some(out) = std::env::var_os("OMUZAKI_FACE_PARITY_OUT").map(PathBuf::from) else { return };
    let Ok(bytes) = std::fs::read(out.join("extremes_js.json")) else { return };
    let sets: Vec<Extreme> = serde_json::from_slice(&bytes).unwrap();
    let rows: Vec<_> = sets
        .iter()
        .map(|e| serde_json::json!({ "name": e.name, "open": e.open, "tilt": e.tilt, "lowLid": e.low_lid, "ops": rust_outline(e.open, e.tilt, e.low_lid) }))
        .collect();
    std::fs::write(out.join("extremes_rust.json"), serde_json::to_vec(&rows).unwrap()).unwrap();
}
