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
//! when they are absent, and the contact sheets it writes (`OMUZAKI_FACE_PARITY_OUT`, an `--ignored` run) go there too.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::PathBuf;

use image::RgbaImage;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    CatchlightShape, Cmd, DrawList, FaceDrawing, FaceError, FaceInput, FaceLayerId, FacePainter, FaceProfile,
    FaceProjection, FaceRasterizer, PaintParams, PathEl, PathRef, Raster,
};
use omuzaki_test::jsref::JsRef;
use omuzaki_test_face::face_parity::{SectionCheck, SectionTolerance, check_tags_painted, load_setup};

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

/// How far the painted ink's centre may sit from the prototype's, in pixels, and how much its total may differ (a
/// fraction). Neither depends on how Chrome's canvas quantises an edge pixel's coverage, so they hold position and
/// size tighter than the per-pixel rule can on thin shapes.
const MAX_CENTROID_SHIFT_PX: f64 = 0.35;
/// See [`MAX_CENTROID_SHIFT_PX`].
const MAX_INK_DIFFERENCE: f64 = 0.03;

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
fn paint_stages(input: &FaceInput<'_>, projection: FaceProjection, px_per_m: f32) -> Result<[DrawList; 3], FaceError> {
    let paint = |stage| {
        let (profile, params) = staged(input.profile, input.params, stage);
        let input = FaceInput { profile: &profile, params: &params, ..*input };
        let mut painter = RealPainter::new(projection, px_per_m);
        let mut drawing = FaceDrawing::new();
        painter.paint_section(Section::Eyes, &input, &mut drawing)?;
        Ok(drawing[FaceLayerId::Base].clone())
    };
    Ok([paint(Stage::Iris)?, paint(Stage::Highlights)?, paint(Stage::All)?])
}

/// The painter's pieces for a case, as [`check_tags_painted`] wants them.
fn paint_piece(
    piece: Piece,
    input: &FaceInput<'_>,
    projection: FaceProjection,
    px_per_m: f32,
) -> Result<DrawList, FaceError> {
    Ok(piece_list(&paint_stages(input, projection, px_per_m)?, piece))
}

#[test]
fn the_iris_highlights_and_shimmer_match_the_prototype() {
    for piece in Piece::ALL {
        let paint = |input: &FaceInput<'_>, projection, px_per_m| paint_piece(piece, input, projection, px_per_m);
        let check = check_tags_painted(Section::Eyes, &piece.tags(), &paint, piece.tolerance()).unwrap();
        if let SectionCheck::Ran { results, .. } = &check {
            assert!(!results.is_empty(), "{}: no case could be compared", piece.name());
        }
        assert!(check.problems().is_none(), "{}: {}", piece.name(), check.problems().unwrap_or_default());
        if piece.checks_ink() {
            let ink = check.ink_problems(MAX_CENTROID_SHIFT_PX, MAX_INK_DIFFERENCE);
            assert!(ink.is_none(), "{}: {}", piece.name(), ink.unwrap_or_default());
        }
    }
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
    let pixels = raster.pixels.as_chunks::<4>().0;
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
    // The raster is premultiplied, so over mid-grey a channel is its value plus the grey the ink lets through.
    let mut flat = image::RgbImage::new(w, h);
    for (x, y, out) in flat.enumerate_pixels_mut() {
        let [red, green, blue, alpha] = pixels[((y0 + y) * size.0 + x0 + x) as usize];
        let mix = |c: u8| u8::try_from(u32::from(c) + 128 * (255 - u32::from(alpha)) / 255).unwrap_or(u8::MAX);
        *out = image::Rgb([mix(red), mix(green), mix(blue)]);
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
