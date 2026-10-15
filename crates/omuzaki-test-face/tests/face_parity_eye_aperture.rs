//! Parity of the eye aperture and the eye white with the JS prototype: the painter's clipped white
//! (`face.js` lines 101-140) against the prototype's isolated `eye-L-sclera` and `eye-R-sclera` sections, for every
//! golden expression in both variants.
//!
//! The comparison is of those two parts alone. The painter's eyes section grows pieces as they land (iris,
//! lash, ...), and each of those has its own test, so this one keeps only what the aperture draws: the fill of the
//! aperture, which comes just before the clip to it. (`check_tags` compares the two sclera tags only, and
//! [`eye_whites`] cuts the painter's list down to match.)
//!
//! The references and the face profile are the sample character's, so they stay in a local assets folder: the test skips, saying why,
//! when they are absent.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::PathBuf;

use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{Cmd, DrawList, FaceDrawing, FaceInput, FaceLayerId, PathEl, PathRef};
use omuzaki_test_face::face_parity::{SectionCheck, SectionTolerance, check_tags};
use serde::Deserialize;

/// The tolerance this section is held to: the face rule. Measured first (see the PR): the worst case is far inside it.
const TOLERANCE: SectionTolerance = SectionTolerance::T_FACE;

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

#[test]
fn the_eye_whites_match_the_prototype() {
    let check = check_tags(Section::Eyes, &["eye-L-sclera", "eye-R-sclera"], &eye_whites, TOLERANCE).unwrap();
    if let SectionCheck::Ran { results, .. } = &check {
        assert!(!results.is_empty(), "no case could be compared");
    }
    assert!(check.problems().is_none(), "{}", check.problems().unwrap_or_default());
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
