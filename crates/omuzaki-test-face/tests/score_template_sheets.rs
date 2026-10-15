//! Contact sheets of the nine score templates, for taste sign-off.
//!
//! Each template file in `omuzaki-anim/library/scores/` is read here (the score table only; the header is ignored) and
//! compiled with [`Score::compile`]. Five frames, at the start, a quarter, half, three quarters and the end of the
//! score, are painted with the real face painter on the default face profile (a synthetic face), rasterised on the CPU
//! as `face_parity` does, and written as one row per template. The caption under the row names the channels the
//! score drives that the sheet does not show: head and body channels are "not painted", and the eye effects whose
//! painters are still stubs ([`UNDRAWN`]) are "not drawn yet", so the reviewer knows what is missing.
//!
//! `sheet` is ignored and writes `$OMUZAKI_SHEET_DIR/<id>.png` (the evidence case `score-templates` runs it).

// The library's files are embedded and checked here: a template that does not parse is a failing test, not a panic in
// the product.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use image::{Rgba, RgbaImage, imageops};
use omuzaki_anim_api::channel::{Channel, ChannelBuf, Region};
use omuzaki_anim_api::{BlendMode, CompiledScore, Layer, Score};
use omuzaki_core::time::Seconds;
use omuzaki_face::RealPainter;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face_api::{
    FaceDrawing, FaceInput, FacePainter, FaceProfile, FaceProjection, FaceRasterizer, FaceRect, FaceView, Palette,
    Raster,
};
use serde::Deserialize;

/// The nine templates, as the library embeds them.
const TEMPLATES: [(&str, &str); 9] = [
    ("intro-wave", include_str!("../../omuzaki-anim/library/scores/intro-wave.toml")),
    ("outro", include_str!("../../omuzaki-anim/library/scores/outro.toml")),
    ("kiss-mwah", include_str!("../../omuzaki-anim/library/scores/kiss-mwah.toml")),
    ("laugh", include_str!("../../omuzaki-anim/library/scores/laugh.toml")),
    ("gasp", include_str!("../../omuzaki-anim/library/scores/gasp.toml")),
    ("sad", include_str!("../../omuzaki-anim/library/scores/sad.toml")),
    ("smug", include_str!("../../omuzaki-anim/library/scores/smug.toml")),
    ("shy", include_str!("../../omuzaki-anim/library/scores/shy.toml")),
    ("angry", include_str!("../../omuzaki-anim/library/scores/angry.toml")),
];

/// The fractions of the duration that are painted, in order.
const FRACTIONS: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

/// One face frame's side, in pixels.
const FRAME: u32 = 256;
const GAP: u32 = 8;
const MARGIN: u32 = 8;
/// Caption text: each glyph pixel is drawn this many pixels square.
const TEXT_SCALE: u32 = 2;
const PAPER: [u8; 4] = [250, 250, 248, 255];
const INK: [u8; 4] = [20, 20, 20, 255];

/// Only the score table of a template file; the header fields are not this test's business.
#[derive(Deserialize)]
struct Template {
    score: Score,
}

fn parse(text: &str) -> Score {
    toml::from_str::<Template>(text).expect("a template file has a score table").score
}

/// The default face profile with its palette slots resolved, the way the painter needs it.
fn default_profile() -> FaceProfile {
    FaceProfile::default().resolve_colours(&Palette::default()).expect("the default face profile resolves")
}

/// The side of the window the frame covers, in metres, and the height of its centre on the face plane: the default
/// face's eyes sit below the origin, so the window is lowered to take in the whole head.
const WINDOW_M: f32 = 0.5;
const CENTRE_Y_M: f32 = -0.06;

/// The window the frame covers, as `face_parity` sets its canvas.
fn projection() -> FaceProjection {
    let half = WINDOW_M / 2.0;
    let (y0, y1) = (CENTRE_Y_M - half, CENTRE_Y_M + half);
    FaceProjection { rect: FaceRect { x0: -half, x1: half, y0, y1 }, front_fade: [0.0, 0.05] }
}

/// Pixels per metre across the window: `FRAME` pixels over `WINDOW_M` metres.
fn px_per_m() -> f32 {
    // The frame is a few hundred pixels, exact in f32.
    #[allow(clippy::cast_precision_loss)]
    let frame = FRAME as f32;
    frame / WINDOW_M
}

/// The face at `at` seconds into the score: the score's channels over the neutral face, painted and rasterised over
/// the paper colour. Every value is a pure function of the score and the time, so equal inputs give equal pixels.
fn paint_frame(score: &CompiledScore, at: Seconds, profile: &FaceProfile) -> RgbaImage {
    let projection = projection();
    let mut painter = RealPainter::new(projection, px_per_m());
    // Channels the score does not drive stay at rest (`ChannelBuf::default` is neutral, not zero).
    let mut layer = Layer::absent(BlendMode::Override);
    layer.values = ChannelBuf::default();
    score.sample(at, &mut layer);
    let params = profile.mapping.apply(&layer.values);
    let input = FaceInput { profile, params: &params, time: at, view: FaceView::default() };
    let mut drawing = FaceDrawing::new();
    painter.paint(&input, &mut drawing).expect("the default face paints");

    let mut pixels: Vec<u8> = PAPER.repeat(FRAME as usize * FRAME as usize);
    let mut rasterizer = SkiaRasterizer::new();
    let mut raster = Raster::new(1, 1);
    for spec in painter.layers() {
        let list = &drawing[spec.id];
        if list.is_empty() {
            continue;
        }
        rasterizer.rasterize(spec, list, spec.px_per_m, &mut raster).expect("the default face rasterises");
        // A layer is rasterised at its own density (the brows are half-density); it is scaled up to the frame before it
        // is composited, as the stage does.
        let mut image = RgbaImage::from_raw(raster.width, raster.height, raster.pixels.clone())
            .expect("the raster has width x height pixels");
        if (raster.width, raster.height) != (FRAME, FRAME) {
            image = imageops::resize(&image, FRAME, FRAME, imageops::FilterType::Triangle);
        }
        for (dst, src) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(image.as_raw().as_chunks::<4>().0) {
            over(dst, *src);
        }
    }
    RgbaImage::from_raw(FRAME, FRAME, pixels).expect("the frame buffer has FRAME x FRAME pixels")
}

/// Premultiplied source-over: `dst = src + dst * (1 - src.alpha)`, per channel, rounded.
fn over(dst: &mut [u8; 4], src: [u8; 4]) {
    let inverse = 255 - u32::from(src[3]);
    for (d, s) in dst.iter_mut().zip(src) {
        let mixed = u32::from(s) + (u32::from(*d) * inverse + 127) / 255;
        *d = u8::try_from(mixed.min(255)).unwrap_or(u8::MAX);
    }
}

/// Channels the face region paints but whose painter is still a stub in `omuzaki-face`. The caption names them as "not
/// drawn yet", so a sheet never hides an effect the score asks for. Each entry says which stub file holds it, and the
/// list shrinks as those files are implemented.
const UNDRAWN: [Channel; 4] = [
    // eyeArc*: `painter/eyes/closed.rs`
    Channel::EYE_ARC_LEFT,
    Channel::EYE_ARC_RIGHT,
    // eyesHeart: `painter/eyes/effects.rs`
    Channel::EYES_HEART,
    // tears: `painter/eyes/effects.rs`
    Channel::TEARS,
];

/// A channel's name as words: `headShiftX` becomes `head shift x`.
fn words(channel: Channel) -> String {
    let mut words = String::new();
    for ch in channel.name().chars() {
        if ch.is_ascii_uppercase() {
            words.push(' ');
        }
        words.push(ch.to_ascii_lowercase());
    }
    words
}

/// The channels the score drives that the face does not paint, as words: head and body channels are "not painted", and
/// the stubs in [`UNDRAWN`] are "not drawn yet".
fn not_painted(score: &Score) -> String {
    let mut not_painted = Vec::new();
    let mut not_drawn_yet = Vec::new();
    for channel in score.tracks.iter().map(|t| t.channel) {
        if UNDRAWN.contains(&channel) {
            not_drawn_yet.push(words(channel));
        } else if matches!(channel.region(), Region::Head | Region::Body) {
            not_painted.push(words(channel));
        }
    }
    let mut parts = Vec::new();
    if !not_painted.is_empty() {
        parts.push(format!("not painted: {}", not_painted.join(", ")));
    }
    if !not_drawn_yet.is_empty() {
        parts.push(format!("not drawn yet: {}", not_drawn_yet.join(", ")));
    }
    if parts.is_empty() {
        "nothing the score drives is left out".into()
    } else {
        parts.join("; ")
    }
}

/// The row for one template: the five frames side by side, then the caption.
fn sheet_row(id: &str, score: &Score, profile: &FaceProfile) -> RgbaImage {
    let compiled = score.compile().expect("the template compiles");
    let length = score.duration.0;
    let width = 2 * MARGIN + 5 * FRAME + 4 * GAP;
    let line = 7 * TEXT_SCALE;
    let height = MARGIN + FRAME + MARGIN + line + TEXT_SCALE * 3 + line + MARGIN;
    let mut row = RgbaImage::from_pixel(width, height, Rgba(PAPER));
    for (i, fraction) in FRACTIONS.iter().enumerate() {
        let frame = paint_frame(&compiled, Seconds(length * fraction), profile);
        #[allow(clippy::cast_possible_truncation)] // i < 5, so the position fits in u32
        let x = MARGIN + i as u32 * (FRAME + GAP);
        imageops::replace(&mut row, &frame, i64::from(x), i64::from(MARGIN));
    }
    let caption_y = MARGIN + FRAME + MARGIN;
    draw_text(&mut row, MARGIN, caption_y, &format!("{id}, frames from start to end"));
    draw_text(&mut row, MARGIN, caption_y + line + TEXT_SCALE * 3, &not_painted(score));
    row
}

/// The glyphs the captions use: lower-case letters, space, comma and hyphen, five columns by seven rows, a `1` bit
/// is ink.
const GLYPHS: [(char, [u8; 7]); 29] = [
    ('a', [0b00000, 0b00000, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111]),
    ('b', [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b11110]),
    ('c', [0b00000, 0b00000, 0b01110, 0b10000, 0b10000, 0b10001, 0b01110]),
    ('d', [0b00001, 0b00001, 0b01101, 0b10011, 0b10001, 0b10001, 0b01111]),
    ('e', [0b00000, 0b00000, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110]),
    ('f', [0b00110, 0b01001, 0b01000, 0b11100, 0b01000, 0b01000, 0b01000]),
    ('g', [0b00000, 0b01111, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110]),
    ('h', [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001]),
    ('i', [0b00100, 0b00000, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('j', [0b00010, 0b00000, 0b00110, 0b00010, 0b00010, 0b10010, 0b01100]),
    ('k', [0b10000, 0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010]),
    ('l', [0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('m', [0b00000, 0b00000, 0b11010, 0b10101, 0b10101, 0b10001, 0b10001]),
    ('n', [0b00000, 0b00000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001]),
    ('o', [0b00000, 0b00000, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('p', [0b00000, 0b00000, 0b11110, 0b10001, 0b11110, 0b10000, 0b10000]),
    ('q', [0b00000, 0b00000, 0b01101, 0b10011, 0b01111, 0b00001, 0b00001]),
    ('r', [0b00000, 0b00000, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000]),
    ('s', [0b00000, 0b00000, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110]),
    ('t', [0b01000, 0b01000, 0b11100, 0b01000, 0b01000, 0b01001, 0b00110]),
    ('u', [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101]),
    ('v', [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100]),
    ('w', [0b00000, 0b00000, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010]),
    ('x', [0b00000, 0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001]),
    ('y', [0b00000, 0b00000, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110]),
    ('z', [0b00000, 0b00000, 0b11111, 0b00010, 0b00100, 0b01000, 0b11111]),
    (' ', [0; 7]),
    (',', [0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00100, 0b01000]),
    ('-', [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000]),
];

/// Draws `text` at `(x, y)` in ink, clipped to the image. Characters without a glyph are left out.
fn draw_text(image: &mut RgbaImage, x: u32, y: u32, text: &str) {
    let mut pen = x;
    for c in text.chars() {
        let Some((_, rows)) = GLYPHS.iter().find(|(g, _)| *g == c) else { continue };
        for (dy, bits) in rows.iter().enumerate() {
            for dx in 0..5 {
                if bits & (0b10000 >> dx) == 0 {
                    continue;
                }
                for sy in 0..TEXT_SCALE {
                    for sx in 0..TEXT_SCALE {
                        #[allow(clippy::cast_possible_truncation)] // dx and dy are below 7
                        let (px, py) = (pen + dx * TEXT_SCALE + sx, y + dy as u32 * TEXT_SCALE + sy);
                        if px < image.width() && py < image.height() {
                            image.put_pixel(px, py, Rgba(INK));
                        }
                    }
                }
            }
        }
        pen += 6 * TEXT_SCALE;
    }
}

#[test]
fn the_nine_templates_parse_and_compile() {
    assert_eq!(TEMPLATES.len(), 9);
    for (id, text) in TEMPLATES {
        parse(text).compile().unwrap_or_else(|e| panic!("{id} does not compile: {e}"));
    }
}

#[test]
fn the_same_frame_twice_is_the_same_pixels() {
    let profile = default_profile();
    let compiled = parse(TEMPLATES[8].1).compile().expect("angry compiles");
    let at = Seconds(compiled.duration().0 / 2.0);
    let first = paint_frame(&compiled, at, &profile);
    let second = paint_frame(&compiled, at, &profile);
    assert_eq!(first.as_raw(), second.as_raw());
    assert!(first.pixels().any(|p| p.0 != PAPER), "the frame paints something");
}

/// Writes `$OMUZAKI_SHEET_DIR/<id>.png` for each template. Run by the evidence case `score-templates`.
#[test]
#[ignore = "writes the contact sheets; set OMUZAKI_SHEET_DIR (the evidence case does)"]
fn sheet() {
    let dir = std::env::var_os("OMUZAKI_SHEET_DIR").expect("set OMUZAKI_SHEET_DIR to the folder for the sheets");
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).expect("the sheet folder can be created");
    let profile = default_profile();
    for (id, text) in TEMPLATES {
        let row = sheet_row(id, &parse(text), &profile);
        row.save(dir.join(format!("{id}.png"))).unwrap_or_else(|e| panic!("writing {id}.png: {e}"));
    }
}
