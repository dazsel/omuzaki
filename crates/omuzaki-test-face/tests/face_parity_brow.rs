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
//!
//! # An intended difference: raised and lowered brows
//!
//! The painter no longer moves a brow the way the prototype does (a rigid 0.012 m per unit of height, with a constant
//! arch). The inner end leads and the outer end hardly moves, and the arch grows when the brow rises and flattens when it
//! comes down (the Face lead's ruling on #1863, from the CC0 reference rig the Director measured). So the check keeps
//! the neutral brow at the face rule, case by case: a golden case whose two brows both sit at height 0 must match the
//! prototype; a case with a raised or lowered brow is expected to differ, and is listed (not failed) with its
//! brow heights, for the local lane to look at. A height of 0 is the prototype's brow, whatever the tilt: the shift
//! and the arch scale are both zero-based there.

use omuzaki_face::Section;
use omuzaki_test::compare::Tolerance;
use omuzaki_test::jsref::JsRef;
use omuzaki_test::regioncmp::{T_FACE, T_FACE_MIN_PSNR_DB};
use omuzaki_test_face::face_parity::{SectionCheck, SectionTolerance, check_section};

/// The two brows' heights in a golden case (`browY` plus the per-brow offset), or `None` when the dump lacks the case.
fn brow_heights(js: &JsRef, variant: &str, expression: &str) -> Option<[f64; 2]> {
    let states = js.face_states(variant).ok()?;
    let case = states.cases.iter().find(|c| c.expression == expression)?;
    let get = |key: &str| case.p.get(key).copied();
    Some([get("browY")? + get("browYL")?, get("browY")? + get("browYR")?])
}

#[test]
fn brows_match_the_prototype() {
    let tolerance =
        SectionTolerance { pixels: Tolerance { max_bad_fraction: 0.02, ..T_FACE }, min_psnr_db: T_FACE_MIN_PSNR_DB };
    let check = check_section(Section::Brows, tolerance).expect("the references are usable");
    let SectionCheck::Ran { results, .. } = &check else { return };
    let js = JsRef::open().expect("the references were just read");
    let mut moved = Vec::new();
    let mut wrong = Vec::new();
    for result in results.iter().filter(|r| !r.passed()) {
        match brow_heights(&js, &result.variant, &result.expression) {
            Some(heights) if heights.iter().any(|h| h.abs() > 1e-9) => {
                moved.push(format!("{}/{} (brow heights {heights:?})", result.variant, result.expression));
            }
            _ => wrong.push(result.clone()),
        }
    }
    if !moved.is_empty() {
        eprintln!("raised or lowered brows differ from the prototype, as intended:\n{}", moved.join("\n"));
    }
    assert!(wrong.is_empty(), "{} neutral-brow case(s) differ from the prototype:\n{wrong:#?}", wrong.len());
}
