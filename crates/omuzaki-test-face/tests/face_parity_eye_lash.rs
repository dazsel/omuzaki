//! Parity of the lids, the lash, the inner tick and the closed eyes with the JS prototype (`face.js` lines 188-232 and
//! `closedEye` 243-256), against the prototype's isolated `eye-L-<part>` / `eye-R-<part>` sections, for every golden
//! expression in both variants: `lower-lid` (the fine lower-lid line and the smiling lids with their rim), `crease`,
//! `lash` (the polygon with the flick and the small lower lash), `inner-tick` and `closed`.
//!
//! Each piece is the painter's eyes list cut down to its own commands, by colour and width (`pieces`): the eyes section
//! draws them outside the aperture clip, after the iris, in the prototype's order, so they never mix with the white, the
//! iris or the highlights. The tears (a later piece, tag `eye-*-tear`) are switched off in the painting, so a tear never
//! counts as one of these.
//!
//! The references and the face profile are the sample character's, so they stay in a local assets folder: the tests
//! skip, saying why, when they are absent. Known differences for the local lane to look at (`PR #2130`): the profile's
//! `lash.thickness` is the thick end of the lash line (the prototype's `0.0036 + 0.0040`, so `0.0076`) and `lash.flick`
//! 1 is the prototype's flick, so the sample character's setup must say so; the lash's inner tick and lower lash are
//! drawn only when the profile says so.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]

use std::path::PathBuf;

use omuzaki_core::colour::Colour;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    Cmd, ColourRef, DrawList, FaceDrawing, FaceError, FaceInput, FaceLayerId, FaceProfile, FaceProjection, Paint,
    PathEl, PathRef,
};
use omuzaki_test::regioncmp::{compare_region, write_side_by_side};
use omuzaki_test_face::face_parity::sections::table;
use omuzaki_test_face::face_parity::{SectionCheck, SectionTolerance, check_tags_painted, tag_images_painted};

/// The tolerance every piece is held to: the face rule. To be measured on the local lane first (the PR lists it).
const TOLERANCE: SectionTolerance = SectionTolerance::T_FACE;

/// One of the isolated parts of an eye.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Piece {
    /// The lower-lid line and the smiling lids with their rim.
    LowerLid,
    /// The crease above the lid.
    Crease,
    /// The lash polygon with the flick, and the small lower lash.
    Lash,
    /// The tick at the inner corner.
    InnerTick,
    /// The closed eye: the happy arc or the relaxed curve with its lashes.
    Closed,
}

impl Piece {
    const ALL: [Self; 5] = [Self::LowerLid, Self::Crease, Self::Lash, Self::InnerTick, Self::Closed];

    fn name(self) -> &'static str {
        match self {
            Self::LowerLid => "lower-lid",
            Self::Crease => "crease",
            Self::Lash => "lash",
            Self::InnerTick => "inner-tick",
            Self::Closed => "closed",
        }
    }

    /// The prototype's tags, left eye then right.
    fn tags(self) -> [String; 2] {
        ["L", "R"].map(|side| format!("eye-{side}-{}", self.name()))
    }
}

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

/// Whether the command at depth 0 belongs to `piece` (see the module docs): by colour, and by width where one colour
/// serves two pieces (the lash colour draws the polygon, the lower lash and the closed curves).
fn belongs(profile: &FaceProfile, piece: Piece, cmd: &Cmd) -> bool {
    let eyes = &profile.eyes;
    let (lash, lid, crease) = (fixed(&eyes.lash.colour), fixed(&eyes.lower_lid.colour), fixed(&eyes.crease.colour));
    match (*cmd, piece) {
        (Cmd::Fill { paint: Paint::Solid(c), .. }, Piece::Lash) => same_rgb(c, lash),
        (Cmd::Stroke { paint: Paint::Solid(c), width, .. }, _) => match piece {
            Piece::LowerLid => same_rgb(c, lid) || same_rgb(c, Colour::rgb(255, 236, 232)),
            Piece::Crease => same_rgb(c, crease),
            Piece::Lash => same_rgb(c, lash) && (width - 0.0020).abs() < 1e-7,
            Piece::InnerTick => same_rgb(c, Colour::rgb(120, 60, 60)),
            Piece::Closed => same_rgb(c, lash) && (width > 0.0040 || (width - 0.0018).abs() < 1e-7),
        },
        _ => false,
    }
}

/// The painter's list for `piece`: the eyes section with the tears off, cut down to the piece's commands outside every
/// clip.
fn piece_list(
    piece: Piece,
    input: &FaceInput<'_>,
    projection: FaceProjection,
    px_per_m: f32,
) -> Result<DrawList, FaceError> {
    let mut params = *input.params;
    params.effects.tears = 0.0;
    let input = FaceInput { params: &params, ..*input };
    let mut painter = RealPainter::new(projection, px_per_m);
    let mut drawing = FaceDrawing::new();
    painter.paint_section(Section::Eyes, &input, &mut drawing)?;
    let src = &drawing[FaceLayerId::Base];
    let mut out = DrawList::new();
    let mut depth = 0_usize;
    for cmd in src.cmds() {
        match *cmd {
            Cmd::PushClip(_) => depth += 1,
            Cmd::PopClip => depth -= 1,
            Cmd::Fill { path, paint, opacity } if depth == 0 && belongs(input.profile, piece, cmd) => {
                let path = copy_path(src, path, &mut out);
                out.push(Cmd::Fill { path, paint, opacity });
            }
            Cmd::Stroke { path, paint, width, cap, join, opacity }
                if depth == 0 && belongs(input.profile, piece, cmd) =>
            {
                let path = copy_path(src, path, &mut out);
                out.push(Cmd::Stroke { path, paint, width, cap, join, opacity });
            }
            _ => {}
        }
    }
    Ok(out)
}

#[test]
fn the_lids_lash_tick_and_closed_eyes_match_the_prototype() {
    for piece in Piece::ALL {
        let tags = piece.tags();
        let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
        let paint = |input: &FaceInput<'_>, projection, px_per_m| piece_list(piece, input, projection, px_per_m);
        let check = check_tags_painted(Section::Eyes, &tags, &paint, TOLERANCE).unwrap();
        if let SectionCheck::Ran { results, .. } = &check {
            assert!(!results.is_empty(), "{}: no case could be compared", piece.name());
        }
        let setup = check.setup_problems(&["projection", "profile"]);
        assert!(setup.is_none(), "{}: {}", piece.name(), setup.unwrap_or_default());
        assert!(check.problems().is_none(), "{}: {}", piece.name(), check.problems().unwrap_or_default());
    }
}

/// Writes the reference / painter / diff picture of every compared case and the tables, to `OMUZAKI_FACE_PARITY_OUT`
/// (the evidence for the PR). `cargo test -p omuzaki-test-face --test face_parity_eye_lash -- --ignored`.
#[test]
#[ignore = "writes evidence to a local assets folder"]
fn write_the_evidence() {
    let Some(out) = std::env::var_os("OMUZAKI_FACE_PARITY_OUT").map(PathBuf::from) else {
        eprintln!("set OMUZAKI_FACE_PARITY_OUT to the output folder");
        return;
    };
    for piece in Piece::ALL {
        let tags = piece.tags();
        let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
        let paint = |input: &FaceInput<'_>, projection, px_per_m| piece_list(piece, input, projection, px_per_m);
        let Some(images) = tag_images_painted(Section::Eyes, &tags, &paint, TOLERANCE).unwrap() else { return };
        for case in &images {
            if let Some(actual) = &case.actual {
                let report = compare_region(actual, &case.reference, case.rect, TOLERANCE.pixels).unwrap();
                let path = out.join(piece.name()).join(&case.variant).join(format!("{}.png", case.expression));
                write_side_by_side(&path, actual, &case.reference, &report, 1).unwrap();
            }
        }
        if let SectionCheck::Ran { results, .. } = check_tags_painted(Section::Eyes, &tags, &paint, TOLERANCE).unwrap()
        {
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.join(format!("{}-table.md", piece.name())), table(&results)).unwrap();
        }
    }
}
