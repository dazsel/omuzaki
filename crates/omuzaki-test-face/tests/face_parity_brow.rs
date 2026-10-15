//! The brows against the prototype's `brow-L`/`brow-R` sections: both variants, every golden expression. Skips
//! without the assets drive. See `omuzaki_test_face::face_parity` for what is private.
//!
//! The face rule (T-FACE) except for the share of bad pixels, 2 % instead of 1 %. A brow is a 5-10 px sliver, so
//! nearly every pixel of its rectangle that has ink is an edge pixel. The prototype's references take only the coverages
//! 1/4, 1/2 and 3/4 on those edges, so whatever drew them supersampled or multisampled. Headless Chromium 141 in
//! software does not: it fills a 46-point, 236 px path analytically (`omuzaki-test-face/tests/raster_supersample.rs`
//! pins its values), and the CPU rasteriser matches that. The errors are zero-mean (+-0.05 coverage, no bias on either
//! edge, ink area within 0.2 %), so the geometry matches. The allowance stays until #2023 settles how the references
//! were rendered; then this returns to `T_FACE`.

use omuzaki_face::Section;
use omuzaki_test::compare::Tolerance;
use omuzaki_test::regioncmp::{T_FACE, T_FACE_MIN_PSNR_DB};
use omuzaki_test_face::face_parity::{SectionTolerance, check_section};

#[test]
fn brows_match_the_prototype() {
    let tolerance =
        SectionTolerance { pixels: Tolerance { max_bad_fraction: 0.02, ..T_FACE }, min_psnr_db: T_FACE_MIN_PSNR_DB };
    let check = check_section(Section::Brows, tolerance).expect("the references are usable");
    assert!(check.problems().is_none(), "{}", check.problems().unwrap_or_default());
}
