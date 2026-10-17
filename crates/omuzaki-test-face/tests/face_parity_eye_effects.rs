//! Parity of the tear drop with the JS prototype (`face.js` 257-263), against the prototype's isolated `eye-L-tear` and
//! `eye-R-tear` sections, for every golden expression in both variants. The heart catchlight and the sparkle are
//! `face_parity_eye_iris.rs`'s (pieces `highlights` and `sparkle`); the effects the prototype lacks (star, gloom, shock,
//! squeeze, spiral, sweat) are new designs with no parity, signed on a contact sheet.
//!
//! The piece is the painter's eyes list cut down to the fill in the style's tear colour outside every clip: the eyes
//! section draws the tear last, after the lash and the tick. Known difference for the local lane (`PR #2130`): the
//! prototype's tear is at full size and only its alpha animates, as here; its colour is the profile's `eyes.tear`.
//!
//! The references and the face profile are the sample character's, so they stay in a local assets folder: the test
//! skips, saying why, when they are absent.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::PathBuf;

use omuzaki_core::colour::Colour;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    Cmd, ColourRef, DrawList, FaceDrawing, FaceError, FaceInput, FaceLayerId, FaceProjection, Paint, PathEl, PathRef,
};
use omuzaki_test::regioncmp::{compare_region, write_side_by_side};
use omuzaki_test_face::face_parity::sections::table;
use omuzaki_test_face::face_parity::{SectionCheck, SectionTolerance, check_tags_painted, tag_images_painted};

/// The tolerance the tear is held to: the face rule. To be measured on the local lane first (the PR lists it).
const TOLERANCE: SectionTolerance = SectionTolerance::T_FACE;

// ---- the painter's side

/// The colour of a resolved profile colour (a slot never reaches the painter).
fn fixed(colour: &ColourRef) -> Colour {
    match colour {
        ColourRef::Fixed(c) => *c,
        ColourRef::Slot(_) => Colour::rgb(128, 128, 128),
    }
}

fn same_rgb(a: Colour, b: Colour) -> bool {
    (a.r, a.g, a.b) == (b.r, b.g, b.b)
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

/// The prototype's tags of the tear, left eye then right.
const TAGS: [&str; 2] = ["eye-L-tear", "eye-R-tear"];

/// The painter's tear: the eyes section's fills outside every clip in the profile's tear colour.
fn tear_list(input: &FaceInput<'_>, projection: FaceProjection, px_per_m: f32) -> Result<DrawList, FaceError> {
    let tear = fixed(&input.profile.eyes.tear);
    let mut painter = RealPainter::new(projection, px_per_m);
    let mut drawing = FaceDrawing::new();
    painter.paint_section(Section::Eyes, input, &mut drawing)?;
    let src = &drawing[FaceLayerId::Base];
    let mut out = DrawList::new();
    let mut depth = 0_usize;
    for cmd in src.cmds() {
        match *cmd {
            Cmd::PushClip(_) => depth += 1,
            Cmd::PopClip => depth -= 1,
            Cmd::Fill { path, paint: Paint::Solid(c), opacity } if depth == 0 && same_rgb(c, tear) => {
                let path = copy_path(src, path, &mut out);
                out.push(Cmd::Fill { path, paint: Paint::Solid(c), opacity });
            }
            _ => {}
        }
    }
    Ok(out)
}

#[test]
fn the_tear_matches_the_prototype() {
    let check = check_tags_painted(Section::Eyes, &TAGS, &tear_list, TOLERANCE).unwrap();
    if let SectionCheck::Ran { results, .. } = &check {
        assert!(!results.is_empty(), "no case could be compared");
    }
    let setup = check.setup_problems(&["projection", "profile"]);
    assert!(setup.is_none(), "{}", setup.unwrap_or_default());
    assert!(check.problems().is_none(), "{}", check.problems().unwrap_or_default());
}

/// Writes the reference / painter / diff picture of every compared case and the table, to `OMUZAKI_FACE_PARITY_OUT`
/// (the evidence for the PR). `cargo test -p omuzaki-test-face --test face_parity_eye_effects -- --ignored`.
#[test]
#[ignore = "writes evidence to a local assets folder"]
fn write_the_evidence() {
    let Some(out) = std::env::var_os("OMUZAKI_FACE_PARITY_OUT").map(PathBuf::from) else {
        eprintln!("set OMUZAKI_FACE_PARITY_OUT to the output folder");
        return;
    };
    let Some(images) = tag_images_painted(Section::Eyes, &TAGS, &tear_list, TOLERANCE).unwrap() else { return };
    for case in &images {
        if let Some(actual) = &case.actual {
            let report = compare_region(actual, &case.reference, case.rect, TOLERANCE.pixels).unwrap();
            let path = out.join("tear").join(&case.variant).join(format!("{}.png", case.expression));
            write_side_by_side(&path, actual, &case.reference, &report, 1).unwrap();
        }
    }
    if let SectionCheck::Ran { results, .. } = check_tags_painted(Section::Eyes, &TAGS, &tear_list, TOLERANCE).unwrap()
    {
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("tear-table.md"), table(&results)).unwrap();
    }
}
