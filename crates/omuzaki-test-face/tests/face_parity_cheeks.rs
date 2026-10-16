//! The cheeks and the nose against the prototype's `blush`, `fringe-shade` and `nose` sections: both variants,
//! every golden expression. Skips without the local assets. See `omuzaki_test_face::face_parity` for what stays local.
//!
//! Blush and the fringe shade meet the face rule (T-FACE) as it stands: their gradients are smooth, and the worst
//! premultiplied channel error over a whole texture is 2.
//!
//! The nose is one stroke 4 px wide and a 6 px dot, in a 35 x 43 px rectangle, so about every fortieth pixel of the
//! rectangle is an edge pixel and 4 % of them is 50 pixels. The references' edge coverages come in sixteenths, and
//! their dot has 3 % more ink than the circle it draws (33.5 px of coverage against pi r^2 = 32.5), so whatever drew
//! them supersampled or multisampled. Headless Chromium 141 in software fills analytically instead
//! (`omuzaki-test-face/tests/raster_supersample.rs`), and the CPU rasteriser's coverage has 2 % less ink than the
//! references' dot. Fitted to the references, the tick's end points agree to 0.09 px and its ink to 0.2 %, and the dot's
//! centre to 0.03 px, so the geometry matches. #2023 settles how the references were rendered; until then the nose is
//! allowed 4 % bad pixels, and then it returns to `T_FACE`. That allowance is conditional (a maintainer decision): the
//! same test asserts the nose's alpha-weighted ink centroid is within 0.30 px of the reference's and its ink mass (sum
//! of alpha) within 5 %, which edge quantisation cannot fake. Measured: centroid 0.269 px (the same on all 13 cases, so
//! a constant bias, not drift; the fitted geometry agrees to 0.09 px for the tick and 0.03 px for the dot) and mass
//! 0.993x.

use omuzaki_face::Section;
use omuzaki_test::compare::Tolerance;
use omuzaki_test::regioncmp::{T_FACE, T_FACE_MIN_PSNR_DB};
use omuzaki_test_face::face_parity::{ParityError, SectionCheck, SectionTolerance, check_section};

fn assert_matches(section: Section, tolerance: SectionTolerance) -> Result<SectionCheck, ParityError> {
    let check = check_section(section, tolerance)?;
    assert!(check.problems().is_none(), "{}", check.problems().unwrap_or_default());
    Ok(check)
}

#[test]
fn blush_and_the_fringe_shade_match_the_prototype() -> Result<(), ParityError> {
    assert_matches(Section::Cheeks, SectionTolerance::T_FACE).map(drop)
}

#[test]
fn the_nose_matches_the_prototype() -> Result<(), ParityError> {
    let pixels = Tolerance { max_bad_fraction: 0.04, ..T_FACE };
    let check = assert_matches(Section::Nose, SectionTolerance { pixels, min_psnr_db: T_FACE_MIN_PSNR_DB })?;
    assert!(check.ink_problems(0.30, 0.05).is_none(), "{}", check.ink_problems(0.30, 0.05).unwrap_or_default());
    println!("worst nose ink drift: {:?}", check.worst_ink());
    Ok(())
}
