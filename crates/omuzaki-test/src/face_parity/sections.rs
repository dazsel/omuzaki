//! One comparison per golden case and [`Section`]: the Rust painter's section against the prototype's isolated PNGs.
//!
//! The prototype's references are finer than the painter's sections: the reference tool tags 31 parts of `face.js` (`eye-L-iris`,
//! `teeth`, ...), the painter has seven sections ([`Section`]). A painter section draws all its parts in one list, so
//! the reference for a section is the references of its tags stacked in painting order ([`tags`]); the stack differs
//! from a true single pass only where clipped parts meet (the references' README, check 3), which the rule's tolerance covers.
//!
//! The comparison happens inside the union of the tags' bounding boxes (plus [`PAD`] pixels), so a thousand right
//! pixels of empty canvas cannot hide a wrong brow; ink the painter puts outside that rectangle is counted too
//! ([`Outcome::Compared::stray`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use image::RgbaImage;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{FaceDrawing, FaceInput, FaceLayerId, FacePainter, FaceRasterizer, Raster};
use serde::Deserialize;

use super::js_params::case_inputs;
use super::{LOOKS, ParityError, SectionTolerance, Setup, load_setup};
use crate::jsref::{FaceStates, JsRef};
use crate::regioncmp::{Rect, compare_region, write_side_by_side};

/// Pixels added around the tags' bounding boxes, so a shape that lands a little outside still counts as compared.
pub const PAD: u32 = 8;

/// The layers a section's pixels are taken from, bottom to top. The brows layer is the prototype's second, half-size
/// brow canvas (its references are logged but not replayed), so it is not compared.
const COMPARED_LAYERS: [FaceLayerId; 2] = [FaceLayerId::Base, FaceLayerId::Overlay];

/// The eye parts the references tag, per eye (`eye-L-<part>`, `eye-R-<part>`).
const EYE_PARTS: [&str; 10] =
    ["sclera", "iris", "highlights", "sparkle", "lower-lid", "crease", "lash", "inner-tick", "tear", "closed"];

/// The prototype tags that `section` covers. Marks and the overlay have no prototype counterpart: their
/// reference is an empty canvas.
pub fn tags(section: Section) -> Vec<String> {
    let plain = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect();
    match section {
        Section::Cheeks => plain(&["blush", "fringe-shade"]),
        Section::Brows => plain(&["brow-L", "brow-R"]),
        Section::Eyes => {
            ["L", "R"].iter().flat_map(|side| EYE_PARTS.iter().map(move |part| format!("eye-{side}-{part}"))).collect()
        }
        Section::Nose => plain(&["nose"]),
        Section::Mouth => plain(&["lower-lip", "mouth", "mouth-interior", "teeth", "lip-rim", "tongue"]),
        Section::Marks | Section::Overlay => Vec::new(),
    }
}

/// How far the painter's ink is from the reference's inside the compared rectangle, weighting each pixel by its alpha.
///
/// A few-pixel mark can sit within the per-pixel rule while every edge pixel is off by a quantum of coverage; these two
/// numbers say whether the mark is in the right place with the right weight, which edge noise cannot fake.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkDrift {
    /// Distance between the two alpha-weighted centroids, in pixels (0 when either picture has no ink).
    pub centroid_shift_px: f64,
    /// The painter's total alpha over the reference's (1 when both are empty, infinite when only the reference is).
    pub mass_ratio: f64,
}

/// Total alpha and the alpha-weighted centroid of `img` inside `rect`.
fn ink_moments(img: &RgbaImage, rect: Rect) -> (f64, f64, f64) {
    let (mut mass, mut sx, mut sy) = (0.0, 0.0, 0.0);
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            let alpha = f64::from(img.get_pixel(x, y).0[3]);
            mass += alpha;
            sx += alpha * (f64::from(x) + 0.5);
            sy += alpha * (f64::from(y) + 0.5);
        }
    }
    if mass > 0.0 { (mass, sx / mass, sy / mass) } else { (0.0, 0.0, 0.0) }
}

impl InkDrift {
    fn measure(actual: &RgbaImage, reference: &RgbaImage, rect: Rect) -> Self {
        let (ma, xa, ya) = ink_moments(actual, rect);
        let (mr, xr, yr) = ink_moments(reference, rect);
        let centroid_shift_px = if ma > 0.0 && mr > 0.0 { (xa - xr).hypot(ya - yr) } else { 0.0 };
        let mass_ratio = if mr > 0.0 {
            ma / mr
        } else if ma > 0.0 {
            f64::INFINITY
        } else {
            1.0
        };
        Self { centroid_shift_px, mass_ratio }
    }
}

/// What came of comparing one section in one case.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The prototype painted something and the painter's lists are empty (a stub, or a section that does not run).
    NotDrawn {
        /// Pixels with ink in the reference.
        reference_pixels: u64,
    },
    /// Neither painted anything.
    BothEmpty,
    /// Both painted; the numbers are for the reference's rectangle.
    Compared {
        /// The rectangle compared.
        rect: Rect,
        /// [`compare`](crate::compare::compare)'s numbers for it.
        stats: crate::compare::Stats,
        /// Painted pixels (alpha above the rule's per-channel limit) outside the rectangle.
        stray: u64,
        /// Where the painter's ink sits and how much there is, against the reference's, inside the rectangle.
        ink: InkDrift,
        /// Whether the numbers and the stray ink are within the tolerance.
        passed: bool,
    },
}

/// One section of one golden case.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseResult {
    /// The golden variant (`game` or `close-up`).
    pub variant: String,
    /// The expression of the case.
    pub expression: String,
    /// The section.
    pub section: Section,
    /// The prototype tags that painted in this case and make up the reference.
    pub tags: Vec<String>,
    /// The verdict.
    pub outcome: Outcome,
}

impl CaseResult {
    /// True when the section matches: both empty, or compared within the tolerance. A section that is not drawn fails.
    pub fn passed(&self) -> bool {
        match &self.outcome {
            Outcome::BothEmpty => true,
            Outcome::NotDrawn { .. } => false,
            Outcome::Compared { passed, .. } => *passed,
        }
    }
}

/// The result of [`check_section`].
#[derive(Debug, Clone, PartialEq)]
pub enum SectionCheck {
    /// The references are not available; the reason is already printed. A test returns without asserting.
    Skipped(String),
    /// Every case of both variants.
    Ran {
        /// The tolerance applied.
        tolerance: SectionTolerance,
        /// One result per case.
        results: Vec<CaseResult>,
    },
}

impl SectionCheck {
    /// The results that failed (none when skipped).
    pub fn failures(&self) -> Vec<&CaseResult> {
        match self {
            Self::Skipped(_) => Vec::new(),
            Self::Ran { results, .. } => results.iter().filter(|r| !r.passed()).collect(),
        }
    }

    /// `Some(a list of the cases)` whose ink centroid is more than `max_shift_px` from the reference's or whose ink
    /// mass differs from it by more than the fraction `max_mass_error` (a case with no ink on either side is fine).
    pub fn ink_problems(&self, max_shift_px: f64, max_mass_error: f64) -> Option<String> {
        let Self::Ran { results, .. } = self else { return None };
        let bad: Vec<String> = results
            .iter()
            .filter_map(|r| match &r.outcome {
                Outcome::Compared { ink, .. }
                    if ink.centroid_shift_px > max_shift_px || (ink.mass_ratio - 1.0).abs() > max_mass_error =>
                {
                    Some(format!(
                        "{}/{}: centroid {:.3} px, mass {:.3}x",
                        r.variant, r.expression, ink.centroid_shift_px, ink.mass_ratio
                    ))
                }
                _ => None,
            })
            .collect();
        (!bad.is_empty()).then(|| format!("ink outside {max_shift_px} px / {max_mass_error}:\n{}", bad.join("\n")))
    }

    /// The worst ink drift over the compared cases: the largest centroid shift and the mass ratio farthest from 1.
    pub fn worst_ink(&self) -> Option<InkDrift> {
        let Self::Ran { results, .. } = self else { return None };
        results
            .iter()
            .filter_map(|r| if let Outcome::Compared { ink, .. } = &r.outcome { Some(*ink) } else { None })
            .fold(None, |worst, ink| {
                Some(worst.map_or(ink, |w: InkDrift| InkDrift {
                    centroid_shift_px: w.centroid_shift_px.max(ink.centroid_shift_px),
                    mass_ratio: if (ink.mass_ratio - 1.0).abs() > (w.mass_ratio - 1.0).abs() {
                        ink.mass_ratio
                    } else {
                        w.mass_ratio
                    },
                }))
            })
    }

    /// `Some(a table of the failing cases)` when any case fails, for `assert!`/`panic!` in a test.
    pub fn problems(&self) -> Option<String> {
        let failures = self.failures();
        let Self::Ran { tolerance, .. } = self else { return None };
        (!failures.is_empty()).then(|| {
            let owned: Vec<CaseResult> = failures.into_iter().cloned().collect();
            format!("{} case(s) outside {tolerance:?}:\n{}", owned.len(), table(&owned))
        })
    }
}

/// A section's tolerance for [`report`]: every section is judged by the face rule there.
const REPORT_TOLERANCE: SectionTolerance = SectionTolerance::T_FACE;

// ------------------------------------------------------------------------------------------------ the references

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
    /// `[x0, y0, x1, y1)` in pixels.
    bbox: [u32; 4],
}

fn read_index(js: &JsRef) -> Result<Option<Index>, ParityError> {
    let path = js.root().join("face-sections").join("index.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ParityError::File { path, message: e.to_string() }),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|e| ParityError::File { path, message: e.to_string() })
}

fn read_png(path: &Path) -> Result<RgbaImage, ParityError> {
    image::open(path)
        .map(|i| i.to_rgba8())
        .map_err(|e| ParityError::File { path: path.to_path_buf(), message: e.to_string() })
}

/// `src` over `dst` (premultiplied RGBA8), in integer maths. `src` is premultiplied when `src_premultiplied`, else
/// straight alpha.
fn over(dst: &mut [u8; 4], src: [u8; 4], src_premultiplied: bool) {
    let alpha = u32::from(src[3]);
    let keep = 255 - alpha;
    let premultiply = |c: u8| if src_premultiplied { u32::from(c) } else { (u32::from(c) * alpha + 127) / 255 };
    let mix = |s: u32, d: u8| u8::try_from((s + (u32::from(d) * keep + 127) / 255).min(255)).unwrap_or(u8::MAX);
    *dst = [
        mix(premultiply(src[0]), dst[0]),
        mix(premultiply(src[1]), dst[1]),
        mix(premultiply(src[2]), dst[2]),
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

/// The reference for one section: its tags' PNGs stacked in painting order on a transparent canvas of `size`.
fn stack_reference(js: &JsRef, entries: &[&IndexSection], size: (u32, u32)) -> Result<RgbaImage, ParityError> {
    let mut stack = vec![[0_u8; 4]; size.0 as usize * size.1 as usize];
    for entry in entries {
        let png = read_png(&js.root().join(&entry.file))?;
        if png.dimensions() != size {
            return Err(ParityError::Inconsistent(format!(
                "{} is {}x{}, the texture is {}x{}",
                entry.file,
                png.width(),
                png.height(),
                size.0,
                size.1
            )));
        }
        for (dst, src) in stack.iter_mut().zip(png.pixels()) {
            over(dst, src.0, false);
        }
    }
    let straight = stack.into_iter().flat_map(unpremultiply).collect();
    RgbaImage::from_raw(size.0, size.1, straight)
        .ok_or_else(|| ParityError::Inconsistent("reference buffer size".to_owned()))
}

/// The union of the entries' boxes grown by [`PAD`] and clipped to the canvas; the whole canvas for no entries.
fn region(entries: &[&IndexSection], size: (u32, u32)) -> Rect {
    let Some(first) = entries.first() else { return Rect::full(size.0, size.1) };
    let union = entries
        .iter()
        .fold(first.bbox, |u, e| [u[0].min(e.bbox[0]), u[1].min(e.bbox[1]), u[2].max(e.bbox[2]), u[3].max(e.bbox[3])]);
    let (x0, y0) = (union[0].saturating_sub(PAD), union[1].saturating_sub(PAD));
    let (x1, y1) = (union[2].saturating_add(PAD).min(size.0), union[3].saturating_add(PAD).min(size.1));
    Rect { x: x0, y: y0, width: x1.saturating_sub(x0), height: y1.saturating_sub(y0) }
}

// ------------------------------------------------------------------------------------------------ the Rust side

/// One section of one case through the painter and the CPU rasteriser, as straight-alpha pixels; `None` when the
/// painter's lists for the compared layers are empty.
fn paint_section(
    section: Section,
    setup: &Setup,
    input: &FaceInput<'_>,
    size: (u32, u32),
    case_id: &str,
) -> Result<Option<RgbaImage>, ParityError> {
    let face = |source| ParityError::Face { case: case_id.to_owned(), source };
    // Canvas sizes are a few thousand pixels, exact in f32.
    #[allow(clippy::cast_precision_loss)]
    let px_per_m = size.0 as f32 / setup.projection.rect.width();
    let mut painter = RealPainter::new(setup.projection, px_per_m);
    let mut drawing = FaceDrawing::new();
    painter.paint_section(section, input, &mut drawing).map_err(face)?;

    let mut stack = vec![[0_u8; 4]; size.0 as usize * size.1 as usize];
    let mut drew = false;
    let mut rasterizer = SkiaRasterizer::new();
    let mut raster = Raster::new(1, 1);
    for spec in painter.layers().iter().filter(|s| COMPARED_LAYERS.contains(&s.id)) {
        let list = &drawing[spec.id];
        if list.is_empty() {
            continue;
        }
        drew = true;
        rasterizer.rasterize(spec, list, spec.px_per_m, &mut raster).map_err(face)?;
        if (raster.width, raster.height) != size {
            return Err(ParityError::Inconsistent(format!(
                "the {:?} layer is {}x{} px, the references are {}x{}",
                spec.id, raster.width, raster.height, size.0, size.1
            )));
        }
        for (dst, src) in stack.iter_mut().zip(raster.pixels.as_chunks::<4>().0) {
            over(dst, *src, true);
        }
    }
    if !drew {
        return Ok(None);
    }
    let straight = stack.into_iter().flat_map(unpremultiply).collect();
    RgbaImage::from_raw(size.0, size.1, straight)
        .map(Some)
        .ok_or_else(|| ParityError::Inconsistent("painted buffer size".to_owned()))
}

fn ink(img: &RgbaImage) -> u64 {
    img.pixels().filter(|p| p.0[3] > 0).count() as u64
}

/// Pixels of `img` outside `rect` whose alpha is above `limit`.
fn stray_ink(img: &RgbaImage, rect: Rect, limit: u8) -> u64 {
    img.enumerate_pixels()
        .filter(|(x, y, p)| {
            let inside = *x >= rect.x && *x < rect.x + rect.width && *y >= rect.y && *y < rect.y + rect.height;
            !inside && p.0[3] > limit
        })
        .count() as u64
}

// ------------------------------------------------------------------------------------------------ running

/// Everything one variant's cases share.
struct Variant {
    states: FaceStates,
    setup: Setup,
}

/// The expressions whose face state the dump lacks: the 12 synthetic tear and tongue cases, by
/// exact name. Only these may be left out of a run; any other missing case is an error, so a broken dump cannot pass.
const NO_STATE_EXPRESSIONS: [&str; 12] = [
    "tear-30-blush",
    "tear-30-no-blush",
    "tear-60-blush",
    "tear-60-no-blush",
    "tear-100-blush",
    "tear-100-no-blush",
    "tongue-open-30",
    "tongue-open-70",
    "tongue-open-100",
    "tongue-closed-30",
    "tongue-closed-70",
    "tongue-closed-100",
];

/// What [`run`] found.
struct Run {
    results: Vec<CaseResult>,
    /// Per variant: the setup parts that fell back to defaults.
    defaulted: BTreeMap<String, Vec<&'static str>>,
    /// Cases of the index the face-state dump has no record of; they are not compared.
    missing_state: Vec<String>,
}

/// Compares `sections` over every case of the index; `evidence` (when given) receives a picture per compared case.
fn run(
    js: &JsRef,
    index: &Index,
    sections: &[Section],
    tolerance: SectionTolerance,
    evidence: Option<&Path>,
) -> Result<Run, ParityError> {
    let mut missing_state = Vec::new();
    let mut variants: BTreeMap<String, Variant> = BTreeMap::new();
    let mut results = Vec::new();
    for case in &index.cases {
        let texture = js.root().join(&case.variant).join("face-textures").join(format!("{}.png", case.expression));
        let size = image::image_dimensions(&texture)
            .map_err(|e| ParityError::File { path: texture.clone(), message: e.to_string() })?;
        if !variants.contains_key(&case.variant) {
            let setup = load_setup(js, &case.variant, size)?;
            variants.insert(case.variant.clone(), Variant { states: js.face_states(&case.variant)?, setup });
        }
        let Some(variant) = variants.get(&case.variant) else { continue };
        // The face-state dump lacks the synthetic tear and tongue cases (the reference tool added them); they cannot be painted.
        let Some(state) = variant.states.case(&case.id) else {
            if !NO_STATE_EXPRESSIONS.contains(&case.expression.as_str()) {
                return Err(ParityError::Inconsistent(format!(
                    "the face state of {} has no case {}, and that expression is not a known exclusion",
                    case.variant, case.id
                )));
            }
            missing_state.push(case.id.clone());
            continue;
        };
        let (inputs, profile) = case_inputs(state, &variant.setup.profile, variant.setup.follow_weight)
            .map_err(|source| ParityError::Params { case: case.id.clone(), source })?;
        let input = FaceInput { profile: &profile, params: &inputs.params, time: inputs.time, view: inputs.view };

        for &section in sections {
            let wanted = tags(section);
            let entries: Vec<&IndexSection> = case.sections.iter().filter(|e| wanted.contains(&e.tag)).collect();
            let reference = stack_reference(js, &entries, size)?;
            let rect = region(&entries, size);
            let actual = paint_section(section, &variant.setup, &input, size, &case.id)?;
            let outcome = judge(actual.as_ref(), &reference, rect, tolerance, &case.id)?;
            if let (Some(dir), Outcome::Compared { .. }, Some(actual)) = (evidence, &outcome, &actual) {
                write_evidence(dir, case, section, actual, &reference, rect, tolerance)?;
            }
            results.push(CaseResult {
                variant: case.variant.clone(),
                expression: case.expression.clone(),
                section,
                tags: entries.iter().map(|e| e.tag.clone()).collect(),
                outcome,
            });
        }
    }
    if results.is_empty() {
        return Err(ParityError::Inconsistent(
            "no case could be compared (an empty index or no face state)".to_owned(),
        ));
    }
    let defaulted = variants.into_iter().map(|(name, v)| (name, v.setup.defaulted)).collect();
    Ok(Run { results, defaulted, missing_state })
}

/// The verdict for one section from the painter's pixels (`None`: nothing drawn) and the reference.
fn judge(
    actual: Option<&RgbaImage>,
    reference: &RgbaImage,
    rect: Rect,
    tolerance: SectionTolerance,
    case_id: &str,
) -> Result<Outcome, ParityError> {
    let reference_pixels = ink(reference);
    let Some(actual) = actual else {
        return Ok(if reference_pixels == 0 { Outcome::BothEmpty } else { Outcome::NotDrawn { reference_pixels } });
    };
    let report = compare_region(actual, reference, rect, tolerance.pixels)
        .map_err(|source| ParityError::Region { case: case_id.to_owned(), source })?;
    let stray = stray_ink(actual, rect, tolerance.pixels.per_channel);
    let area = f64::from(rect.width) * f64::from(rect.height);
    #[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^52
    let stray_ok = stray as f64 <= tolerance.pixels.max_bad_fraction * area;
    let ink = InkDrift::measure(actual, reference, rect);
    Ok(Outcome::Compared {
        rect,
        stats: report.stats,
        stray,
        ink,
        passed: report.passes(tolerance.min_psnr_db) && stray_ok,
    })
}

/// Reference / painter / diff for one case, in a local assets folder.
fn write_evidence(
    dir: &Path,
    case: &IndexCase,
    section: Section,
    actual: &RgbaImage,
    reference: &RgbaImage,
    rect: Rect,
    tolerance: SectionTolerance,
) -> Result<(), ParityError> {
    let report = compare_region(actual, reference, rect, tolerance.pixels)
        .map_err(|source| ParityError::Region { case: case.id.clone(), source })?;
    let path: PathBuf =
        dir.join(&case.variant).join(&case.expression).join(format!("{}.png", format!("{section:?}").to_lowercase()));
    write_side_by_side(&path, actual, reference, &report, 1)
        .map_err(|source| ParityError::Region { case: case.id.clone(), source })
}

/// Compares `section` in every golden case under `tolerance`. Skips (printing why) when the references are absent.
///
/// A section task calls this from its own `tests/face_parity_<section>.rs`:
///
/// ```ignore
/// #[test]
/// fn brows_match_the_prototype() {
///     let check = face_parity::check_section(Section::Brows, SectionTolerance::T_FACE).unwrap();
///     assert!(check.problems().is_none(), "{}", check.problems().unwrap_or_default());
/// }
/// ```
///
/// # Errors
/// [`ParityError`] when the references are present but unusable; a section that is merely wrong is a failing
/// [`CaseResult`], not an error.
pub fn check_section(section: Section, tolerance: SectionTolerance) -> Result<SectionCheck, ParityError> {
    match JsRef::open() {
        Some(js) => check_section_in(&js, section, tolerance),
        None => Ok(SectionCheck::Skipped("no JS reference dump".to_owned())),
    }
}

/// [`check_section`] against the dump `js` (a fixture, in tests).
///
/// # Errors
/// As [`check_section`].
pub fn check_section_in(
    js: &JsRef,
    section: Section,
    tolerance: SectionTolerance,
) -> Result<SectionCheck, ParityError> {
    let Some(index) = read_index(js)? else {
        let reason = format!("no face-section references at {}/face-sections", js.root().display());
        eprintln!("skipped: {reason}");
        return Ok(SectionCheck::Skipped(reason));
    };
    let Run { results, missing_state, .. } = run(js, &index, &[section], tolerance, None)?;
    if !missing_state.is_empty() {
        eprintln!("not compared, no face state in the dump: {}", missing_state.join(", "));
    }
    Ok(SectionCheck::Ran { tolerance, results })
}

/// The whole picture: every section, whether or not it has an enforcement test.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionReport {
    /// Why nothing ran, when the references are absent.
    pub skipped: Option<String>,
    /// The markdown report (also written to `report.md` in the output folder).
    pub markdown: String,
    /// Every result.
    pub results: Vec<CaseResult>,
}

/// Compares every section in every case under the face rule and writes `report.md` plus a reference / painter / diff
/// picture per compared case into `out_dir` (in a local assets folder: the pictures are the sample character's).
///
/// # Errors
/// As [`check_section`], and [`ParityError::File`] when the report cannot be written.
pub fn report(out_dir: &Path) -> Result<SectionReport, ParityError> {
    let Some(js) = JsRef::open() else {
        return Ok(SectionReport {
            skipped: Some("no JS reference dump".to_owned()),
            markdown: String::new(),
            results: Vec::new(),
        });
    };
    report_in(&js, out_dir)
}

/// [`report`] against the dump `js`.
///
/// # Errors
/// As [`report`].
pub fn report_in(js: &JsRef, out_dir: &Path) -> Result<SectionReport, ParityError> {
    let Some(index) = read_index(js)? else {
        let reason = format!("no face-section references at {}/face-sections", js.root().display());
        return Ok(SectionReport { skipped: Some(reason), markdown: String::new(), results: Vec::new() });
    };
    let Run { results, defaulted, missing_state } = run(js, &index, &Section::ALL, REPORT_TOLERANCE, Some(out_dir))?;
    let mut markdown = markdown(&results, &defaulted, REPORT_TOLERANCE);
    if !missing_state.is_empty() {
        let _ = write!(markdown, "\n## Not compared: no face state in the dump\n\n");
        for id in &missing_state {
            let _ = writeln!(markdown, "- {id}");
        }
    }
    std::fs::create_dir_all(out_dir)
        .and_then(|()| std::fs::write(out_dir.join("report.md"), &markdown))
        .map_err(|e| ParityError::File { path: out_dir.join("report.md"), message: e.to_string() })?;
    Ok(SectionReport { skipped: None, markdown, results })
}

// ------------------------------------------------------------------------------------------------ the tables

fn cells(r: &CaseResult) -> (String, String, String, String, String) {
    let tags = if r.tags.is_empty() { "none".to_owned() } else { r.tags.join(" ") };
    match &r.outcome {
        Outcome::BothEmpty => (tags, "both empty".into(), "-".into(), "-".into(), "-".into()),
        Outcome::NotDrawn { reference_pixels } => {
            (tags, format!("**not drawn** ({reference_pixels} px expected)"), "-".into(), "-".into(), "-".into())
        }
        Outcome::Compared { rect, stats, stray, passed, .. } => {
            let psnr = stats.psnr_db.map_or_else(|| "inf".to_owned(), |db| format!("{db:.2} dB"));
            let result = if *passed { "pass" } else { "**FAIL**" };
            let result = if *stray > 0 { format!("{result} ({stray} stray px)") } else { result.to_owned() };
            (tags, result, psnr, stats.max_delta.to_string(), format!("{:.3}% of {rect}", stats.bad_fraction * 100.0))
        }
    }
}

/// A markdown table, one row per result: variant, expression, section, tags, result, PSNR, max delta, bad pixels.
pub fn table(results: &[CaseResult]) -> String {
    let mut out = String::from(
        "| variant | expression | section | reference tags | result | PSNR | max delta | bad pixels |\n|---|---|---|---|---|---:|---:|---:|\n",
    );
    for r in results {
        let (tags, result, psnr, delta, bad) = cells(r);
        let _ = writeln!(
            out,
            "| {} | {} | {:?} | {tags} | {result} | {psnr} | {delta} | {bad} |",
            r.variant, r.expression, r.section
        );
    }
    out
}

/// The full report: setup, a section x variant summary, and one table per section.
pub fn markdown(
    results: &[CaseResult],
    defaulted: &BTreeMap<String, Vec<&'static str>>,
    tolerance: SectionTolerance,
) -> String {
    let mut out = String::from("# Face parity: Rust painter against the JS prototype's face sections\n\n");
    let _ = writeln!(
        out,
        "Rule: per channel {} (premultiplied), at most {:.1}% bad pixels inside the section's rectangle, PSNR at least {:.0} dB.\n",
        tolerance.pixels.per_channel,
        tolerance.pixels.max_bad_fraction * 100.0,
        tolerance.min_psnr_db
    );
    for (variant, look) in LOOKS {
        let source = match defaulted.get(variant) {
            Some(parts) if parts.is_empty() => "setup file from a local assets folder".to_owned(),
            Some(parts) => format!("DEFAULTS for {} (not in the setup file)", parts.join(", ")),
            None => "not run".to_owned(),
        };
        let _ = writeln!(out, "- {variant} ({look}): {source}");
    }
    out.push_str(
        "\n## Summary\n\n| section | variant | cases | pass | not drawn | fail |\n|---|---|---:|---:|---:|---:|\n",
    );
    for section in Section::ALL {
        for (variant, _) in LOOKS {
            let rows: Vec<&CaseResult> =
                results.iter().filter(|r| r.section == section && r.variant == variant).collect();
            let count = |f: &dyn Fn(&CaseResult) -> bool| rows.iter().filter(|r| f(r)).count();
            let not_drawn = count(&|r| matches!(r.outcome, Outcome::NotDrawn { .. }));
            let pass = count(&CaseResult::passed);
            let _ = writeln!(
                out,
                "| {section:?} | {variant} | {} | {pass} | {not_drawn} | {} |",
                rows.len(),
                rows.len() - pass - not_drawn
            );
        }
    }
    for section in Section::ALL {
        let rows: Vec<CaseResult> = results.iter().filter(|r| r.section == section).cloned().collect();
        let _ = write!(out, "\n## {section:?}\n\n{}", table(&rows));
    }
    out
}

#[cfg(test)]
mod tests {
    use image::Rgba;
    use serde_json::{Value, json};

    use super::*;

    const TOL: SectionTolerance = SectionTolerance::T_FACE;
    const SIZE: (u32, u32) = (64, 48);

    #[test]
    fn every_prototype_tag_belongs_to_exactly_one_section() {
        // The 31 tags of the references' README, in its order.
        let mut expected: Vec<String> = [
            "blush",
            "fringe-shade",
            "brow-L",
            "brow-R",
            "nose",
            "lower-lip",
            "mouth",
            "mouth-interior",
            "teeth",
            "lip-rim",
            "tongue",
        ]
        .map(str::to_owned)
        .to_vec();
        expected.extend(["L", "R"].iter().flat_map(|s| EYE_PARTS.iter().map(move |p| format!("eye-{s}-{p}"))));
        let mut claimed: Vec<String> = Section::ALL.into_iter().flat_map(tags).collect();
        claimed.sort();
        expected.sort();
        assert_eq!(claimed, expected);
        assert_eq!(claimed.len(), 31);
    }

    #[test]
    fn over_composites_premultiplied_pixels() {
        let mut px = [0, 0, 0, 0];
        over(&mut px, [255, 0, 0, 128], false);
        assert_eq!(px, [128, 0, 0, 128]);
        over(&mut px, [0, 0, 0, 255], false);
        assert_eq!(px, [0, 0, 0, 255]);
        // Premultiplied input is taken as is.
        let mut px = [10, 20, 30, 255];
        over(&mut px, [100, 0, 0, 128], true);
        assert_eq!(px, [105, 10, 15, 255]);
    }

    #[test]
    fn unpremultiply_round_trips_a_translucent_pixel() {
        let mut px = [0; 4];
        over(&mut px, [200, 100, 50, 128], false);
        let back = unpremultiply(px);
        assert!(back[0].abs_diff(200) <= 1 && back[1].abs_diff(100) <= 1 && back[2].abs_diff(50) <= 2, "{back:?}");
        assert_eq!(unpremultiply([0, 0, 0, 0]), [0; 4]);
    }

    fn entry(tag: &str, bbox: [u32; 4]) -> IndexSection {
        IndexSection { tag: tag.to_owned(), file: format!("{tag}.png"), bbox }
    }

    #[test]
    fn the_region_is_the_padded_union_of_the_boxes_clipped_to_the_canvas() {
        let (a, b) = (entry("brow-L", [20, 10, 30, 14]), entry("brow-R", [40, 12, 60, 16]));
        assert_eq!(region(&[&a, &b], (1000, 1000)), Rect { x: 12, y: 2, width: 56, height: 22 });
        // Near the corner the padding is cut off.
        assert_eq!(region(&[&entry("x", [2, 3, 60, 46])], SIZE), Rect { x: 0, y: 0, width: 64, height: 48 });
        assert_eq!(region(&[], SIZE), Rect::full(64, 48));
    }

    fn blob(x: u32, y: u32) -> RgbaImage {
        let mut img = RgbaImage::new(SIZE.0, SIZE.1);
        for dy in 0..6 {
            for dx in 0..10 {
                img.put_pixel(x + dx, y + dy, Rgba([200, 90, 90, 255]));
            }
        }
        img
    }

    #[test]
    fn ink_drift_measures_the_shift_and_the_mass() {
        let rect = Rect::full(SIZE.0, SIZE.1);
        let same = InkDrift::measure(&blob(10, 10), &blob(10, 10), rect);
        assert_eq!((same.centroid_shift_px, same.mass_ratio), (0.0, 1.0));
        let moved = InkDrift::measure(&blob(13, 14), &blob(10, 10), rect);
        assert!((moved.centroid_shift_px - 5.0).abs() < 1e-9 && (moved.mass_ratio - 1.0).abs() < 1e-9);
        let none = RgbaImage::new(SIZE.0, SIZE.1);
        assert_eq!(InkDrift::measure(&none, &none, rect).mass_ratio, 1.0);
        assert_eq!(InkDrift::measure(&none, &blob(1, 1), rect).mass_ratio, 0.0);
        assert!(InkDrift::measure(&blob(1, 1), &none, rect).mass_ratio.is_infinite());
    }

    #[test]
    fn a_painter_that_drew_nothing_is_not_drawn_unless_nothing_was_expected() {
        let rect = Rect::full(SIZE.0, SIZE.1);
        let outcome = judge(None, &blob(10, 10), rect, TOL, "c").unwrap();
        assert_eq!(outcome, Outcome::NotDrawn { reference_pixels: 60 });
        assert_eq!(judge(None, &RgbaImage::new(SIZE.0, SIZE.1), rect, TOL, "c").unwrap(), Outcome::BothEmpty);
    }

    fn passed(o: &Outcome) -> bool {
        matches!(o, Outcome::Compared { passed: true, .. })
    }

    #[test]
    fn an_identical_picture_passes_and_a_shifted_or_stray_one_fails() {
        let rect = Rect { x: 4, y: 4, width: 40, height: 30 };
        let reference = blob(10, 10);
        assert!(passed(&judge(Some(&reference), &reference, rect, TOL, "c").unwrap()));
        assert!(!passed(&judge(Some(&blob(10, 16)), &reference, rect, TOL, "c").unwrap()));
        // Same shape plus a speck far outside the rectangle: the numbers inside are perfect, the stray ink is not.
        let mut speckled = blob(10, 10);
        for (x, y) in (50..60).flat_map(|x| [(x, 40), (x, 41)]) {
            speckled.put_pixel(x, y, Rgba([0, 0, 0, 255]));
        }
        let Outcome::Compared { stray, passed, .. } = judge(Some(&speckled), &reference, rect, TOL, "c").unwrap()
        else {
            panic!("not compared");
        };
        assert_eq!(stray, 20);
        assert!(!passed);
    }

    // ---- a made-up dump (a nose in one game case) laid out like the real one; nothing here is the sample character's ----

    fn write(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn png(img: &RgbaImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        bytes
    }

    fn face_record() -> Value {
        let keys = [
            "open",
            "tilt",
            "smileEye",
            "lowLid",
            "browY",
            "browTilt",
            "browArch",
            "mOpen",
            "mWide",
            "mSmile",
            "mRound",
            "teeth",
            "blush",
            "tear",
            "pupil",
            "spark",
            "winkL",
            "winkR",
            "fang",
            "teethLow",
            "mScaleClosed",
            "mScale",
            "pucker",
            "lips",
            "nose",
            "eyeScale",
            "eyeDrop",
            "refl",
            "heart",
            "openL",
            "openR",
            "lowLidL",
            "lowLidR",
            "winkArc",
            "browYL",
            "browYR",
            "browTiltL",
            "browTiltR",
            "mSmileL",
            "mSmileR",
            "mX",
            "mPress",
            "tongue",
            "smileLids",
        ];
        let mut record: serde_json::Map<String, Value> = keys.iter().map(|k| ((*k).to_owned(), json!(0.5))).collect();
        // No nose, so the nose section paints nothing whatever the painter: the reference's mark is "not drawn".
        record.insert("nose".to_owned(), json!(0.0));
        Value::Object(record)
    }

    fn fake_dump(root: &Path) {
        write(root, "manifest.json", b"{}");
        let id = "game/smile__test__face__64__nobloom";
        let xy = json!({ "x": 0, "y": 0 });
        let cam = json!({ "fov": 30, "near": 0.05, "far": 80, "position": [0, 0, 1], "target": [0, 0, 0] });
        let identity = json!([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
        let case = json!({ "id": id, "expression": "smile", "camera": "face",
            "input": { "channels": {}, "lookAdd": xy, "t": 0.5, "cameraSpec": cam }, "p": face_record(),
            "view": { "cameraInHead": [0, 0, 1], "eyeRelative": [0, 0, 1], "distance": 1, "azimuth": 0, "elevation": 0,
                      "facing": true, "lookT": xy, "reflV": xy },
            "look": xy, "lookD": xy, "reflV": xy, "twT": 0.5, "dart": xy, "headMatrixWorld": identity,
            "cameraMatrixWorld": identity });
        let constants = json!({ "eyeLift": 0.22, "frontGate": 0.25, "lookYawScale": 0.7, "lookPitchScale": 0.6,
            "reflGain": 0.35, "reflMaxX": 0.25, "reflMaxY": 0.2 });
        let state =
            json!({ "schema": 1, "kind": "face-state", "variant": "game", "faceView": constants, "cases": [case] });
        write(root, "state/game/face-state.json", state.to_string().as_bytes());
        write(root, "game/face-textures/smile.png", &png(&RgbaImage::new(SIZE.0, SIZE.1)));
        write(root, "face-sections/game/smile/nose.png", &png(&blob(20, 20)));
        let index = json!({ "schema": 1, "cases": [{ "id": id, "variant": "game", "expression": "smile",
            "sections": [{ "tag": "nose", "file": "face-sections/game/smile/nose.png", "bbox": [20, 20, 30, 26] }] }] });
        write(root, "face-sections/index.json", index.to_string().as_bytes());
    }

    #[test]
    fn a_section_with_a_reference_that_paints_nothing_is_not_drawn_and_one_without_a_reference_passes() {
        let dir = tempfile::tempdir().unwrap();
        fake_dump(dir.path());
        let js = JsRef::open_at(dir.path()).unwrap();

        let nose = check_section_in(&js, Section::Nose, TOL).unwrap();
        let [failure] = nose.failures()[..] else { panic!("expected one failure: {nose:?}") };
        assert_eq!(failure.outcome, Outcome::NotDrawn { reference_pixels: 60 });
        assert_eq!(failure.tags, ["nose"]);
        assert!(nose.problems().unwrap().contains("not drawn"));

        // The sections with no prototype counterpart: they have no reference, so they pass whatever they draw. The
        // others are real painters now and are judged by their own parity tests.
        for section in [Section::Marks, Section::Overlay] {
            let check = check_section_in(&js, section, TOL).unwrap();
            assert!(check.problems().is_none(), "{section:?}: {check:?}");
            assert!(matches!(&check, SectionCheck::Ran { results, .. } if results.len() == 1));
        }
    }

    #[test]
    fn the_report_covers_every_section_and_writes_the_table() {
        let dir = tempfile::tempdir().unwrap();
        fake_dump(dir.path());
        let out = tempfile::tempdir().unwrap();
        let js = JsRef::open_at(dir.path()).unwrap();
        let report = report_in(&js, out.path()).unwrap();
        assert_eq!(report.results.len(), Section::ALL.len());
        assert!(report.markdown.contains("**not drawn** (60 px expected)"), "{}", report.markdown);
        assert!(
            report.markdown.contains("game (sheet): DEFAULTS for projection, profile, follow_weight"),
            "{}",
            report.markdown
        );
        assert_eq!(std::fs::read_to_string(out.path().join("report.md")).unwrap(), report.markdown);
    }

    /// Rewrites the made-up dump's face state with `cases` emptied.
    fn drop_state(root: &Path) {
        let path = root.join("state/game/face-state.json");
        let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        state["cases"] = json!([]);
        std::fs::write(path, state.to_string()).unwrap();
    }

    #[test]
    fn a_missing_face_state_is_an_error_not_a_green_check() {
        let dir = tempfile::tempdir().unwrap();
        fake_dump(dir.path());
        drop_state(dir.path());
        let js = JsRef::open_at(dir.path()).unwrap();
        let err = check_section_in(&js, Section::Nose, TOL).unwrap_err();
        assert!(err.to_string().contains("not a known exclusion"), "{err}");
        assert!(report_in(&js, dir.path()).is_err());
    }

    #[test]
    fn only_the_named_tear_and_tongue_cases_may_lack_state_and_an_empty_run_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fake_dump(dir.path());
        drop_state(dir.path());
        let index_path = dir.path().join("face-sections/index.json");
        let mut index: Value = serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
        index["cases"][0]["expression"] = json!("tear-30-blush");
        std::fs::write(&index_path, index.to_string()).unwrap();
        write(dir.path(), "game/face-textures/tear-30-blush.png", &png(&RgbaImage::new(SIZE.0, SIZE.1)));
        let js = JsRef::open_at(dir.path()).unwrap();
        // The one case is a known exclusion, so nothing was compared: still not a pass.
        let err = check_section_in(&js, Section::Nose, TOL).unwrap_err();
        assert!(err.to_string().contains("no case could be compared"), "{err}");
    }

    #[test]
    fn an_unlisted_tear_case_is_an_error_even_when_other_cases_compare() {
        let dir = tempfile::tempdir().unwrap();
        fake_dump(dir.path());
        let index_path = dir.path().join("face-sections/index.json");
        let mut index: Value = serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
        let mut extra = index["cases"][0].clone();
        extra["id"] = json!("game/tear-999-blush__test__face__64__nobloom");
        extra["expression"] = json!("tear-999-blush");
        index["cases"].as_array_mut().unwrap().push(extra);
        std::fs::write(&index_path, index.to_string()).unwrap();
        write(dir.path(), "game/face-textures/tear-999-blush.png", &png(&RgbaImage::new(SIZE.0, SIZE.1)));
        let js = JsRef::open_at(dir.path()).unwrap();
        let err = check_section_in(&js, Section::Nose, TOL).unwrap_err();
        assert!(err.to_string().contains("tear-999-blush"), "{err}");
        assert!(err.to_string().contains("not a known exclusion"), "{err}");
    }

    #[test]
    fn without_references_everything_skips() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "manifest.json", b"{}");
        let js = JsRef::open_at(dir.path()).unwrap();
        assert!(matches!(check_section_in(&js, Section::Eyes, TOL).unwrap(), SectionCheck::Skipped(_)));
        assert!(report_in(&js, dir.path()).unwrap().skipped.is_some());
    }

    /// Writes the real report; `tools/face-parity-report.sh` runs it with `OMUZAKI_FACE_PARITY_OUT` set.
    #[test]
    #[ignore = "writes to a local assets folder; run by tools/face-parity-report.sh"]
    fn write_the_report() {
        let out = std::env::var_os("OMUZAKI_FACE_PARITY_OUT").map(PathBuf::from).expect("OMUZAKI_FACE_PARITY_OUT");
        let report = report(&out).unwrap();
        match &report.skipped {
            Some(reason) => println!("skipped: {reason}"),
            None => println!("{}", report.markdown),
        }
    }
}
