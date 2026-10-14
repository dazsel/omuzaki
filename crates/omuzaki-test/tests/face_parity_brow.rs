//! The brows against the prototype's `brow-L`/`brow-R` sections: both variants, every golden expression. Skips
//! without the assets drive. See `omuzaki_test::face_parity` for what is private.
//!
//! The face rule (T-FACE) except for the share of bad pixels, 2 % instead of 1 %. A brow is a 5-10 px sliver, so
//! nearly every pixel of its rectangle that has ink is an edge pixel. The reference's edges take only the coverages
//! 1/4, 1/2 and 3/4: Chrome's Skia fills a path with 46 points over a 236 px width with its 4-sub-scanline
//! supersampler (`SkScan_AntiPath`; it uses analytic AA only when the points number under width / 8), and the CPU
//! rasteriser has analytic AA only. The errors are zero-mean (+-0.05 coverage, no bias on either edge, ink area within
//! 0.2 %), so the geometry matches tracks the supersampling fallback, after which this returns to `T_FACE`.

use omuzaki_face::Section;
use omuzaki_test::compare::Tolerance;
use omuzaki_test::face_parity::{SectionTolerance, check_section};
use omuzaki_test::regioncmp::{T_FACE, T_FACE_MIN_PSNR_DB};

#[test]
fn brows_match_the_prototype() {
    let tolerance =
        SectionTolerance { pixels: Tolerance { max_bad_fraction: 0.02, ..T_FACE }, min_psnr_db: T_FACE_MIN_PSNR_DB };
    let check = check_section(Section::Brows, tolerance).expect("the references are usable");
    assert!(check.problems().is_none(), "{}", check.problems().unwrap_or_default());
}
