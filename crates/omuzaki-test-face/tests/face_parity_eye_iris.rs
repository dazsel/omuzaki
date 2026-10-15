//! Parity of the iris, highlights and shimmer with the JS prototype: the painter's iris, crescent, pupil, ring and
//! lid shadow (`face.js` 141-161), its catchlights (162-168, 235-240) and its shimmer, stars and spark (169-186)
//! against the prototype's isolated `eye-{L,R}-iris`, `eye-{L,R}-highlights` and `eye-{L,R}-sparkle` sections, for every
//! golden case in both variants.
//!
//! The painter draws the three pieces one after another inside the aperture clip. To compare each alone, the eyes
//! section is painted three times, with the later pieces switched off (no catchlight and second spark; also no
//! shimmer and spark); the pieces are the commands each stage adds to every eye's clip bracket, and a piece is
//! rasterised inside a clip of its own eye's aperture, as the prototype's isolated sections are.
//!
//! The references and the face profile are the sample character's, so they stay in a local assets folder: the test skips, saying why,
//! when they are absent, and the pictures it writes (`OMUZAKI_FACE_PARITY_OUT`, an `--ignored` run) go there too.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::{Path, PathBuf};

use image::RgbaImage;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    CatchlightShape, Cmd, DrawList, FaceDrawing, FaceInput, FaceLayerId, FacePainter, FaceProfile, FaceRasterizer,
    PaintParams, PathEl, PathRef, Raster,
};
use omuzaki_test::jsref::JsRef;
use omuzaki_test::regioncmp::{Rect, compare_region, write_side_by_side};
use omuzaki_test_face::face_parity::js_params::case_inputs;
use omuzaki_test_face::face_parity::{SectionTolerance, load_setup};
use serde::Deserialize;

/// Pixels around the eyes' boxes that still count as compared (the harness's own margin).
const PAD: u32 = 8;

/// One of the three isolated sections of the eye.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Piece {
    /// Iris, crescent, pupil, ring, lid shadow (`face.js` 141-161).
    Iris,
    /// The main catchlight and the second spark (162-168, 235-240).
    Highlights,
    /// The shimmer band, the two stars and the surprised dot (169-186).
    Sparkle,
}

impl Piece {
    const ALL: [Self; 3] = [Self::Iris, Self::Highlights, Self::Sparkle];

    fn name(self) -> &'static str {
        match self {
            Self::Iris => "iris",
            Self::Highlights => "highlights",
            Self::Sparkle => "sparkle",
        }
    }

    /// The prototype's tags, left eye then right.
    fn tags(self) -> [&'static str; 2] {
        match self {
            Self::Iris => ["eye-L-iris", "eye-R-iris"],
            Self::Highlights => ["eye-L-highlights", "eye-R-highlights"],
            Self::Sparkle => ["eye-L-sparkle", "eye-R-sparkle"],
        }
    }

    /// Whether the ink centre and mass are enforced. Not for the sparkle: its soft band is a few levels of alpha over a
    /// wide area, so a handful of rounding steps in its tail move the centre by pixels (measured up to 1 px at 46 dB);
    /// the pixel rule and the stars' own parity hold it.
    fn checks_ink(self) -> bool {
        self != Self::Sparkle
    }

    /// The tolerance this piece is held to (measured first; see the PR).
    #[allow(clippy::unused_self)] // every piece shares the plain face rule today; the per-piece hook stays for a measured exception
    fn tolerance(self) -> SectionTolerance {
        SectionTolerance::T_FACE
    }
}

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
    piece: Piece,
    variant: String,
    expression: String,
    /// `None` when neither side drew the piece (for instance both eyes shut): nothing to compare.
    compared: Option<Compared>,
    /// Painted pixels where the prototype painted none.
    stray_pixels: u64,
}

/// How far the painted ink's centre may sit from the prototype's, in pixels, and how much its total may differ (a
/// fraction). Neither depends on how Chrome's canvas quantises an edge pixel's coverage, so they hold position and
/// size tighter than the per-pixel rule can on thin shapes.
const MAX_CENTROID_SHIFT_PX: f64 = 0.35;
/// See [`MAX_CENTROID_SHIFT_PX`].
const MAX_INK_DIFFERENCE: f64 = 0.03;

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

struct Compared {
    report: omuzaki_test::regioncmp::RegionReport,
    ink: InkCheck,
    actual: RgbaImage,
    reference: RgbaImage,
}

impl Case {
    fn passed(&self) -> bool {
        let tolerance = self.piece.tolerance();
        let Some(c) = &self.compared else { return self.stray_pixels == 0 };
        let area = f64::from(c.report.rect.width) * f64::from(c.report.rect.height);
        #[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^52
        let stray_ok = self.stray_pixels as f64 <= tolerance.pixels.max_bad_fraction * area;
        c.report.passes(tolerance.min_psnr_db) && (!self.piece.checks_ink() || c.ink.passes()) && stray_ok
    }

    fn row(&self) -> String {
        let name = self.piece.name();
        match &self.compared {
            None => format!(
                "| {name} | {} | {} | nothing drawn | - | - | - | - | - | {} | {} |",
                self.variant,
                self.expression,
                self.stray_pixels,
                self.verdict()
            ),
            Some(c) => {
                let s = &c.report.stats;
                let psnr = s.psnr_db.map_or_else(|| "inf".to_owned(), |db| format!("{db:.2} dB"));
                format!(
                    "| {name} | {} | {} | {} | {psnr} | {} | {:.4}% | {:.3} px | {:+.2}% | {} | {} |",
                    self.variant,
                    self.expression,
                    c.report.rect,
                    s.max_delta,
                    s.bad_fraction * 100.0,
                    c.ink.shift_px,
                    (c.ink.mass_ratio - 1.0) * 100.0,
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

const TABLE_HEADER: &str = "| piece | variant | expression | region | PSNR | max delta | bad pixels | ink centre shift | ink mass | stray px | result |\n|---|---|---|---|---:|---:|---:|---:|---:|---:|---|";

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

/// The prototype's sections of `piece` for a case: stacked, and the box round them.
fn reference(js: &JsRef, case: &IndexCase, piece: Piece, size: (u32, u32)) -> (RgbaImage, Option<Rect>) {
    let entries: Vec<&IndexSection> = case.sections.iter().filter(|s| piece.tags().contains(&s.tag.as_str())).collect();
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

// ---- the painter's side: the three stages and the pieces between them

/// What to switch off to get the eyes up to and including a stage.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Stage {
    /// Iris only: no catchlight, second spark, shimmer or spark.
    Iris,
    /// And the catchlight and second spark.
    Highlights,
    /// Everything.
    All,
}

/// `profile` and `params` with the pieces after `stage` switched off.
fn staged(profile: &FaceProfile, params: &PaintParams, stage: Stage) -> (FaceProfile, PaintParams) {
    let (mut profile, mut params) = (profile.clone(), *params);
    if stage != Stage::All {
        profile.eyes.highlights.shimmer = 0.0;
        params.effects.sparkle = 0.0;
    }
    if stage == Stage::Iris {
        profile.eyes.highlights.shape = CatchlightShape::None;
        profile.eyes.highlights.second = 0.0;
        params.effects.heart = 0.0;
        params.effects.star = 0.0;
    }
    (profile, params)
}

/// The open eyes of a base layer: for each, the index of its clip push and of the matching pop. An eye is a fill
/// outside every clip that is followed by a clip of the same path (the iris is filled inside the clip).
fn brackets(list: &DrawList) -> Vec<(usize, usize)> {
    let (mut depth, mut open, mut found) = (0_usize, None, Vec::new());
    for (i, cmd) in list.cmds().iter().enumerate() {
        match *cmd {
            Cmd::PushClip(_) => {
                if depth == 0 {
                    open = Some(i);
                }
                depth += 1;
            }
            Cmd::PopClip => {
                depth -= 1;
                if depth == 0 {
                    found.push((open.take().unwrap(), i));
                }
            }
            _ => {}
        }
    }
    found
}

fn copy_cmd(src: &DrawList, cmd: Cmd, out: &mut DrawList) {
    let cmd = match cmd {
        Cmd::Fill { path, paint, opacity } => Cmd::Fill { path: copy_path(src, path, out), paint, opacity },
        Cmd::Stroke { path, paint, width, cap, join, opacity } => {
            Cmd::Stroke { path: copy_path(src, path, out), paint, width, cap, join, opacity }
        }
        Cmd::PushClip(path) => Cmd::PushClip(copy_path(src, path, out)),
        other => other,
    };
    out.push(cmd);
}

/// The commands `piece` adds, inside a clip of each eye's aperture, from the three staged paintings.
fn piece_list(stages: &[DrawList; 3], piece: Piece) -> DrawList {
    let [iris, highlights, all] = stages;
    let (bi, bh, ba) = (brackets(iris), brackets(highlights), brackets(all));
    assert!(bi.len() == bh.len() && bh.len() == ba.len(), "every stage paints the same eyes");
    let mut out = DrawList::new();
    for ((i, h), a) in bi.iter().zip(&bh).zip(&ba) {
        // Content lengths (between the push and the pop) of the eye in each stage.
        let (li, lh, la) = (i.1 - i.0 - 1, h.1 - h.0 - 1, a.1 - a.0 - 1);
        let range = match piece {
            Piece::Iris => 0..li,
            Piece::Highlights => li..lh,
            Piece::Sparkle => lh..la,
        };
        let Cmd::PushClip(aperture) = all.cmds()[a.0] else { unreachable!("a bracket starts with a clip") };
        let clip = copy_path(all, aperture, &mut out);
        out.push(Cmd::PushClip(clip));
        for cmd in &all.cmds()[a.0 + 1 + range.start..a.0 + 1 + range.end] {
            copy_cmd(all, *cmd, &mut out);
        }
        out.push(Cmd::PopClip);
    }
    out
}

/// The three staged base layers of a case.
fn paint_stages(input: &FaceInput<'_>, projection: omuzaki_face_api::FaceProjection, px_per_m: f32) -> [DrawList; 3] {
    [Stage::Iris, Stage::Highlights, Stage::All].map(|stage| {
        let (profile, params) = staged(input.profile, input.params, stage);
        let input = FaceInput { profile: &profile, params: &params, ..*input };
        let mut painter = RealPainter::new(projection, px_per_m);
        let mut drawing = FaceDrawing::new();
        painter.paint_section(Section::Eyes, &input, &mut drawing).unwrap();
        drawing[FaceLayerId::Base].clone()
    })
}

/// A list rasterised on the CPU, straight alpha; `None` when it has nothing to draw.
fn rasterise(
    list: &DrawList,
    projection: omuzaki_face_api::FaceProjection,
    px_per_m: f32,
    size: (u32, u32),
) -> Option<RgbaImage> {
    if list.is_empty() {
        return None;
    }
    let painter = RealPainter::new(projection, px_per_m);
    let spec = painter.layers().iter().find(|s| s.id == FaceLayerId::Base).unwrap();
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(spec, list, spec.px_per_m, &mut raster).unwrap();
    assert_eq!((raster.width, raster.height), size, "the base layer is the texture's size");
    Some(straight(raster.pixels.as_chunks::<4>().0.to_vec(), size))
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
            !setup.defaulted.contains(&"projection")
                && !setup.defaulted.contains(&"profile")
                && !setup.defaulted.contains(&"follow_weight"),
            "the {} setup file (face-sections/parity-setup) must give the projection, the profile and the follow weight: parity against a default face means nothing",
            case.variant
        );
        let (inputs, profile) = case_inputs(state, &setup.profile, setup.follow_weight).unwrap();
        let input = FaceInput { profile: &profile, params: &inputs.params, time: inputs.time, view: inputs.view };
        #[allow(clippy::cast_precision_loss)] // canvas sizes are a few thousand pixels
        let px_per_m = size.0 as f32 / setup.projection.rect.width();
        let painted = paint_stages(&input, setup.projection, px_per_m);
        for piece in Piece::ALL {
            let tolerance = piece.tolerance();
            let (reference, rect) = reference(&js, case, piece, size);
            let actual = rasterise(&piece_list(&painted, piece), setup.projection, px_per_m, size);
            // A piece the painter drew as nothing (no commands) is blank; one it drew with the shape off too.
            let actual = actual.filter(|a| a.pixels().any(|p| p.0[3] > 0));
            let compared = match (&actual, rect) {
                (Some(actual), Some(rect)) => {
                    let report = compare_region(actual, &reference, rect, tolerance.pixels).unwrap();
                    let ink = ink_check(actual, &reference, rect);
                    Some(Compared { report, ink, actual: actual.clone(), reference })
                }
                _ => None,
            };
            let stray_pixels = actual.as_ref().map_or(0, |a| stray(a, rect, tolerance.pixels.per_channel));
            // The painter drew where the prototype drew none (or the reverse): a failure whatever the numbers.
            let missing = u64::from(actual.is_none() != rect.is_none()) * u64::from(u32::MAX);
            cases.push(Case {
                piece,
                variant: case.variant.clone(),
                expression: case.expression.clone(),
                compared,
                stray_pixels: stray_pixels + missing,
            });
        }
    }
    Some(cases)
}

fn table(cases: &[Case]) -> String {
    let rows: Vec<String> = cases.iter().map(Case::row).collect();
    format!("{TABLE_HEADER}\n{}\n", rows.join("\n"))
}

#[test]
fn the_iris_highlights_and_shimmer_match_the_prototype() {
    let Some(cases) = run() else { return };
    println!("{}", table(&cases));
    assert!(!cases.is_empty(), "no case could be compared");
    let failed: Vec<&Case> = cases.iter().filter(|c| !c.passed()).collect();
    assert!(
        failed.is_empty(),
        "{} case(s) outside their tolerance:\n{}",
        failed.len(),
        failed.iter().map(|c| c.row()).collect::<Vec<_>>().join("\n")
    );
}

/// Writes the reference / painter / diff picture of every compared case and the table, to `OMUZAKI_FACE_PARITY_OUT`
/// (the evidence for the PR). `cargo test -p omuzaki-test-face --test face_parity_eye_iris -- --ignored`.
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
            let path: PathBuf = Path::new(&out)
                .join("golden")
                .join(case.piece.name())
                .join(&case.variant)
                .join(format!("{}.png", case.expression));
            write_side_by_side(&path, &c.actual, &c.reference, &c.report, 1).unwrap();
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("golden-table.md"), table(&cases)).unwrap();
}
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

/// The noise floor: for each piece, the worst bad fraction at a range of per-channel limits over the goldens, and the
/// lowest PSNR, largest ink shift and mass difference. The numbers the tolerances above are set from.
#[test]
#[ignore = "measures; prints a table"]
fn measure_the_noise_floor() {
    let Some(js) = JsRef::open() else { return };
    let Some(index) = read_index(&js) else { return };
    let Some(cases) = run() else { return };
    drop(index);
    for piece in Piece::ALL {
        let mine: Vec<&Compared> =
            cases.iter().filter(|c| c.piece == piece).filter_map(|c| c.compared.as_ref()).collect();
        let psnr = mine.iter().filter_map(|c| c.report.stats.psnr_db).fold(f64::INFINITY, f64::min);
        let shift = mine.iter().map(|c| c.ink.shift_px).fold(0.0, f64::max);
        let mass = mine.iter().map(|c| (c.ink.mass_ratio - 1.0).abs()).fold(0.0, f64::max);
        let bad: Vec<String> = [12_u8, 16, 24, 32, 48, 64]
            .into_iter()
            .map(|limit| {
                let tolerance = omuzaki_test::compare::Tolerance { per_channel: limit, max_bad_fraction: 1.0 };
                let worst = mine
                    .iter()
                    .map(|c| {
                        compare_region(&c.actual, &c.reference, c.report.rect, tolerance).unwrap().stats.bad_fraction
                    })
                    .fold(0.0, f64::max);
                format!("{limit}: {:.2}%", worst * 100.0)
            })
            .collect();
        println!(
            "{}: {} cases, lowest PSNR {psnr:.2} dB, ink shift <= {shift:.3} px, ink mass within {:.2}%, worst bad fraction by per-channel limit: {}",
            piece.name(),
            mine.len(),
            mass * 100.0,
            bad.join(", ")
        );
    }
}

// ---- the contact sheets (evidence only)

/// The left eye of a painting of the whole eyes section, cropped round the eye, composited over mid-grey and
/// enlarged, for a contact sheet.
fn eye_cell(
    setup: &omuzaki_test_face::face_parity::Setup,
    profile: &FaceProfile,
    params: &PaintParams,
    time: f64,
    shift: omuzaki_core::glam::Vec2,
) -> RgbaImage {
    const WIDTH: u32 = 1536;
    #[allow(clippy::cast_precision_loss)]
    let px_per_m = WIDTH as f32 / setup.projection.rect.width();
    let view = omuzaki_face_api::FaceView { highlight_shift: shift };
    let input = FaceInput { profile, params, time: omuzaki_core::time::Seconds(time), view };
    let mut painter = RealPainter::new(setup.projection, px_per_m);
    let mut drawing = FaceDrawing::new();
    painter.paint_section(Section::Eyes, &input, &mut drawing).unwrap();
    let list = &drawing[FaceLayerId::Base];
    let spec = painter.layers().iter().find(|s| s.id == FaceLayerId::Base).unwrap();
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(spec, list, spec.px_per_m, &mut raster).unwrap();
    let size = (raster.width, raster.height);
    let image = straight(raster.pixels.as_chunks::<4>().0.to_vec(), size);
    // The character's left eye, a little wider than the almond.
    let (cx, cy, half_w, half_h) = (profile.layout.eye_spacing, profile.layout.eye_y, 0.07, 0.045);
    let rect = setup.projection.rect;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (x0, y0, w, h) = (
        ((cx - half_w - f64::from(rect.x0)) * f64::from(px_per_m)) as u32,
        ((f64::from(rect.y1) - cy - half_h) * f64::from(px_per_m)) as u32,
        (2.0 * half_w * f64::from(px_per_m)) as u32,
        (2.0 * half_h * f64::from(px_per_m)) as u32,
    );
    let crop = image::imageops::crop_imm(&image, x0, y0, w, h).to_image();
    let mut flat = image::RgbImage::from_pixel(w, h, image::Rgb([128, 128, 128]));
    for (x, y, p) in crop.enumerate_pixels() {
        let a = u32::from(p.0[3]);
        let mix = |c: u8| u8::try_from((u32::from(c) * a + 128 * (255 - a)) / 255).unwrap_or(u8::MAX);
        flat.put_pixel(x, y, image::Rgb([mix(p.0[0]), mix(p.0[1]), mix(p.0[2])]));
    }
    image::imageops::resize(
        &image::DynamicImage::ImageRgb8(flat).to_rgba8(),
        w * 2,
        h * 2,
        image::imageops::FilterType::Lanczos3,
    )
}

/// Tiles `cells` into rows of `columns`, a dark gap between.
#[allow(clippy::cast_possible_truncation)] // a contact sheet has a few dozen cells
fn tile(cells: &[RgbaImage], columns: usize) -> RgbaImage {
    const GAP: u32 = 6;
    let (w, h) = cells[0].dimensions();
    let rows = cells.len().div_ceil(columns);
    let (cols, rows) = (columns as u32, rows as u32);
    let mut sheet =
        RgbaImage::from_pixel(cols * w + (cols + 1) * GAP, rows * h + (rows + 1) * GAP, image::Rgba([30, 30, 30, 255]));
    for (i, cell) in cells.iter().enumerate() {
        let (c, r) = (i as u32 % cols, i as u32 / cols);
        image::imageops::replace(&mut sheet, cell, i64::from(GAP + c * (w + GAP)), i64::from(GAP + r * (h + GAP)));
    }
    sheet
}

/// The sheets of the extremes: the gaze sweep, each pupil shape and size, each catchlight shape sliding with the view,
/// and the shimmer through time. Painted with the `game` look's face (the sample character's: sheets stay in a local assets
/// drive). `OMUZAKI_FACE_PARITY_OUT=... cargo test -p omuzaki-test-face --test face_parity_eye_iris -- --ignored write_the_sheets`.
#[test]
#[ignore = "writes evidence to a local assets folder"]
fn write_the_sheets() {
    use omuzaki_core::glam::Vec2;
    use omuzaki_face_api::{EyeParams, FaceMapping, PupilShape};
    let Some(out) = std::env::var_os("OMUZAKI_FACE_PARITY_OUT").map(PathBuf::from) else { return };
    let Some(js) = JsRef::open() else { return };
    let setup = load_setup(&js, "game", (1536, 1107)).unwrap();
    let open = EyeParams { open: 1.0, close: 0.0, arc: 0.0, lower_lid: 0.0 };
    let rest = PaintParams { left_eye: open, right_eye: open, ..FaceMapping::default().rest_params() };
    let cell = |profile: &FaceProfile, params: &PaintParams, time: f64, shift: Vec2| {
        eye_cell(&setup, profile, params, time, shift)
    };
    std::fs::create_dir_all(out.join("sheets")).unwrap();
    let save = |name: &str, cells: Vec<RgbaImage>, columns: usize| {
        tile(&cells, columns).save(out.join("sheets").join(name)).unwrap();
    };

    // The gaze sweep: x across, y down, -1 to 1. The iris stays inside the lids at every corner.
    let mut cells = Vec::new();
    for y in [1.0_f32, 0.5, 0.0, -0.5, -1.0] {
        for x in [-1.0_f32, -0.5, 0.0, 0.5, 1.0] {
            cells.push(cell(&setup.profile, &PaintParams { gaze: Vec2::new(x, y), ..rest }, 0.0, Vec2::ZERO));
        }
    }
    save("gaze-sweep.png", cells, 5);

    // Pupils: shape across (round, slit, heart, none), channel down (0.4, 1, 1.6).
    let mut cells = Vec::new();
    for channel in [0.4_f32, 1.0, 1.6] {
        for shape in [PupilShape::Round, PupilShape::Slit, PupilShape::Heart, PupilShape::None] {
            let mut profile = setup.profile.clone();
            profile.eyes.iris.pupil.shape = shape;
            cells.push(cell(&profile, &PaintParams { pupil: channel, ..rest }, 0.0, Vec2::ZERO));
        }
    }
    save("pupils.png", cells, 4);

    // Catchlights: blob, star, heart, none across; the view shift down (left, none, right-up).
    let mut cells = Vec::new();
    for shift in [Vec2::new(-1.0, 0.0), Vec2::ZERO, Vec2::new(1.0, 1.0)] {
        for shape in [CatchlightShape::Blob, CatchlightShape::Star, CatchlightShape::Heart, CatchlightShape::None] {
            let mut profile = setup.profile.clone();
            profile.eyes.highlights.shape = shape;
            profile.eyes.highlights.view_motion = 0.5;
            cells.push(cell(&profile, &rest, 0.0, shift));
        }
    }
    save("catchlights.png", cells, 4);

    // The shimmer through time: t across (0, 0.4, 0.8, 1.2, 2, 3.7), strength down (0.35, 1).
    let mut cells = Vec::new();
    for strength in [0.35, 1.0] {
        for t in [0.0, 0.4, 0.8, 1.2, 2.0, 3.7] {
            let mut profile = setup.profile.clone();
            profile.eyes.highlights.shimmer = strength;
            cells.push(cell(&profile, &rest, t, Vec2::ZERO));
        }
    }
    save("shimmer.png", cells, 6);

    // The effects: heart, star and sparkle, and a surprised wide eye.
    let mut cells = Vec::new();
    let mut with = |edit: &dyn Fn(&mut PaintParams)| {
        let mut params = rest;
        edit(&mut params);
        cells.push(cell(&setup.profile, &params, 0.0, Vec2::ZERO));
    };
    with(&|p| p.effects.heart = 1.0);
    with(&|p| p.effects.star = 1.0);
    with(&|p| p.effects.sparkle = 1.0);
    with(&|p| {
        p.left_eye.open = 1.6;
        p.pupil = 0.6;
    });
    save("effects.png", cells, 4);
}
