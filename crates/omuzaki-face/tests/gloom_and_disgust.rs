//! The gloom's shade and lines fade out instead of stopping at a hard edge, and `disgusted` reads apart from the gloom
//! (#2428): its own marks (the nose's sneer creases, the raised upper lip) instead of the gloom's.
//!
//! With `OMUZAKI_EVIDENCE` set to a folder, writes the before-and-after sheet of the two expressions at the readability
//! sizes there.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_char_api::HeadParams;
use omuzaki_core::colour::Colour;
use omuzaki_core::time::Seconds;
use omuzaki_face::RealPainter;
use omuzaki_face::preview::range::{DEFAULT_SIMILAR, RangeOptions, paint_all};
use omuzaki_face::preview::sheet::{Cell, compose, png};
use omuzaki_face::preview::{FaceSubject, Preview};
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, ExpressionKind, ExpressionSet, FaceDrawing, FaceInput, FaceLayerId, FacePainter, FaceProfile,
    FaceRasterizer, FaceView, Palette, Raster,
};

fn subject(set: ExpressionSet) -> FaceSubject {
    let profile = FaceProfile { expressions: set, ..FaceProfile::default() };
    FaceSubject {
        profile: profile.resolve_colours(&Palette::default()).unwrap(),
        projection: HeadParams::default().face_projection(),
        skin: Colour::rgb(0xf9, 0xd3, 0xc5),
    }
}

/// The Overlay layer of the default face with the gloom at `gloom`, rasterised at the default density.
fn overlay(gloom: f32) -> Raster {
    let face = subject(ExpressionSet::library().unwrap());
    let mut painter = RealPainter::new(face.projection, DEFAULT_PX_PER_M);
    let mut buf = ChannelBuf::NEUTRAL;
    buf.set(Channel::EYES_GLOOM, gloom);
    let params = face.profile.mapping.apply(&buf);
    let mut drawing = FaceDrawing::new();
    let input = FaceInput { profile: &face.profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    painter.paint(&input, &mut drawing).unwrap();
    let spec = *painter.layers().iter().find(|s| s.id == FaceLayerId::Overlay).unwrap();
    let mut raster = Raster::new(0, 0);
    SkiaRasterizer::new().rasterize(&spec, &drawing[FaceLayerId::Overlay], spec.px_per_m, &mut raster).unwrap();
    raster
}

fn alpha(r: &Raster, x: u32, y: u32) -> u8 {
    r.pixels[(y as usize * r.width as usize + x as usize) * 4 + 3]
}

#[test]
fn the_gloom_fades_out_at_its_top_and_its_sides() {
    let r = overlay(1.0);
    let drawn: Vec<(u32, u32)> =
        (0..r.height).flat_map(|y| (0..r.width).map(move |x| (x, y))).filter(|&(x, y)| alpha(&r, x, y) > 0).collect();
    assert!(!drawn.is_empty(), "the gloom draws on the Overlay layer");
    let (top, left, right) = (
        drawn.iter().map(|p| p.1).min().unwrap(),
        drawn.iter().map(|p| p.0).min().unwrap(),
        drawn.iter().map(|p| p.0).max().unwrap(),
    );
    let peak = drawn.iter().map(|&(x, y)| alpha(&r, x, y)).max().unwrap();
    assert!(peak >= 40, "the gloom shows: its darkest pixel is {peak}");
    // The first rows it reaches, and its outermost columns, are nearly clear: no edge where it starts.
    let edge = 16;
    let top_rows = drawn.iter().filter(|p| p.1 < top + 3).map(|&(x, y)| alpha(&r, x, y)).max().unwrap();
    assert!(top_rows <= edge, "its top rows reach alpha {top_rows} (peak {peak})");
    let sides = drawn.iter().filter(|p| p.0 < left + 3 || p.0 > right - 3).map(|&(x, y)| alpha(&r, x, y)).max().unwrap();
    assert!(sides <= edge, "its outer columns reach alpha {sides} (peak {peak})");
    // Nowhere does a column jump from clear to dark in one row going down from the top.
    for x in left..=right {
        let first = (top..r.height).find(|&y| alpha(&r, x, y) > 0);
        if let Some(y) = first {
            assert!(alpha(&r, x, y) <= edge * 2, "column {x} starts at alpha {}", alpha(&r, x, y));
        }
    }
}

#[test]
fn disgusted_reads_apart_from_the_gloom() {
    let face = subject(ExpressionSet::library().unwrap());
    let options = RangeOptions { sizes: vec![28, 56, 112], ..RangeOptions::default() };
    let painted = paint_all(&face, &options, &|| false).unwrap().unwrap();
    let find = |name: &str| painted.iter().find(|p| p.name == name).unwrap();
    let (gloom, disgusted) = (find("gloom"), find("disgusted"));
    for (s, size) in options.sizes.iter().enumerate() {
        let (score, _) = disgusted.likeness(gloom, s).unwrap();
        assert!(score < DEFAULT_SIMILAR, "disgusted and gloom read the same at {size} px ({score:.3})");
        // Nor is gloom the expression disgusted reads closest to.
        let (closest, best) = painted
            .iter()
            .filter(|p| p.name != "disgusted" && p.kind != ExpressionKind::Viseme)
            .map(|p| (p.name.as_str(), disgusted.likeness(p, s).unwrap().0))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        assert_ne!(closest, "gloom", "at {size} px disgusted reads closest to gloom ({best:.3})");
    }
}

#[test]
fn a_sneer_wrinkles_the_nose() {
    let face = subject(ExpressionSet::library().unwrap());
    let mut preview = Preview::new(face, 387);
    let picture = |preview: &mut Preview, sneer: f32| {
        let mut buf = ChannelBuf::NEUTRAL;
        buf.set(Channel::NOSE_SNEER_LEFT, sneer);
        buf.set(Channel::NOSE_SNEER_RIGHT, sneer);
        preview.paint(&buf).unwrap();
        preview.picture()
    };
    let (rest, sneer) = (picture(&mut preview, 0.0), picture(&mut preview, 1.0));
    let changed =
        rest.pixels().zip(sneer.pixels()).filter(|(a, b)| a.0.iter().zip(b.0).any(|(x, y)| x.abs_diff(y) > 24)).count();
    // The creases are a few short lines: a visible handful of pixels, not a wash over the face.
    let area = (rest.width() * rest.height()) as usize;
    assert!(changed >= area / 2000, "a full sneer changed {changed} of {area} pixels");
}

/// Writes the sheet the issue asks for: gloom and disgusted at the readability sizes.
#[test]
fn sheet() {
    let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE") else { return };
    let face = subject(ExpressionSet::library().unwrap());
    let options = RangeOptions { sizes: vec![112, 56, 28], ..RangeOptions::default() };
    let painted = paint_all(&face, &options, &|| false).unwrap().unwrap();
    let mut cells = Vec::new();
    for (s, _) in options.sizes.iter().enumerate() {
        for name in ["rest", "gloom", "disgusted", "angry", "sad"] {
            let p = painted.iter().find(|p| p.name == name).unwrap();
            cells.push(Cell { picture: p.window[s].clone(), label: name.to_owned(), flagged: false });
        }
    }
    let path = std::path::Path::new(&dir).join("gloom-disgusted.png");
    std::fs::write(path, png(&compose(&cells, 5)).unwrap()).unwrap();
}
