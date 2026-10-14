//! The stage's variant pick (SH-10): the variant for the head's size on screen, held steady near its edges.
//!
//! This is the glue the camera work and the stage call; the rule itself is `select_variant` in `omuzaki-char-api`, and
//! the selector only remembers the last pick, so each frame's answer depends on the frame before it.

use omuzaki_char_api::{HeadCoverage, VariantId, VariantSet, select_variant};

/// Holds the variants of a character and the one currently picked. Feed it the head coverage each frame with
/// [`update`](Self::update).
///
/// It never allocates: it borrows the variant set, and a pick is a borrow of one of its keys.
#[derive(Clone, Debug)]
pub struct VariantSelector<'a> {
    variants: &'a VariantSet,
    current: Option<&'a VariantId>,
}

impl<'a> VariantSelector<'a> {
    /// A selector over `variants` with nothing picked yet.
    pub fn new(variants: &'a VariantSet) -> Self {
        Self { variants, current: None }
    }

    /// The variant picked by the last [`update`](Self::update), if any.
    pub fn current(&self) -> Option<&'a VariantId> {
        self.current
    }

    /// Picks the variant for `coverage`: the one with the narrowest shot range that contains it, or the one already
    /// picked while `coverage` stays within its range widened by the hysteresis band. `None` when no variant covers
    /// the coverage.
    pub fn update(&mut self, coverage: HeadCoverage) -> Option<&'a VariantId> {
        self.current = select_variant(self.variants, coverage, self.current);
        self.current
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_char_api::{ShotRange, VariantDef};
    use omuzaki_core::rng::Rng;

    use super::*;

    fn variant(min: f64, max: f64) -> VariantDef {
        VariantDef { shot: Some(ShotRange { min, max }), ..VariantDef::default() }
    }

    fn id(s: &str) -> VariantId {
        VariantId::new(s).unwrap()
    }

    /// Feeds `coverages` through a fresh selector, returning the picks in order.
    fn picks(variants: &VariantSet, coverages: impl IntoIterator<Item = f64>) -> Vec<Option<String>> {
        let mut selector = VariantSelector::new(variants);
        coverages.into_iter().map(|c| selector.update(c).map(ToString::to_string)).collect()
    }

    /// The number of times the pick changes after its first value, a flip counted only between two picks.
    fn flips(picks: &[Option<String>]) -> usize {
        picks.windows(2).filter(|w| w[0] != w[1]).count()
    }

    #[test]
    fn overlapping_ranges_resolve_to_the_narrowest() {
        let variants: VariantSet = [(id("wide"), variant(0.0, 0.6)), (id("close"), variant(0.3, 5.0))].into();
        let mut selector = VariantSelector::new(&variants);
        assert_eq!(selector.update(0.45).map(VariantId::as_str), Some("wide"), "0.6 wide beats 4.7 wide");
        assert_eq!(selector.current().map(VariantId::as_str), Some("wide"));
        assert_eq!(selector.update(2.0).map(VariantId::as_str), Some("close"));
        assert_eq!(selector.update(6.0), None, "nothing covers it");
    }

    #[test]
    fn a_zoom_sweep_with_noise_flips_once_each_way() {
        let variants = omuzaki_char_mock::mock_variants().unwrap();
        let mut rng = Rng::new(465);
        // Coverage moves steadily from 0.10 to 1.00 and back, each reading jittered by at most 0.01.
        let mut noisy = |step: u32| 0.10 + 0.0010 * f64::from(step) + f64::from(rng.range_f32(-0.01, 0.01));
        let up: Vec<f64> = (0..=900).map(&mut noisy).collect();
        let down: Vec<f64> = (0..=900).rev().map(&mut noisy).collect();
        assert_eq!(flips(&picks(&variants, up)), 1, "game to close-up once, at the band's edge");
        assert_eq!(flips(&picks(&variants, down)), 1, "close-up to game once, on the way back");
    }

    #[test]
    fn the_band_holds_a_pick_just_outside_its_range() {
        let variants: VariantSet = [(id("game"), variant(0.0, 0.35)), (id("close"), variant(0.35, 5.0))].into();
        // The band is 0.04 either side of each range: 0.31 to 0.39 keeps the pick, outside it the pick moves on.
        assert_eq!(picks(&variants, [0.30, 0.37]), vec![Some("game".into()), Some("game".into())]);
        assert_eq!(picks(&variants, [0.30, 0.40]), vec![Some("game".into()), Some("close".into())]);
        assert_eq!(picks(&variants, [0.37, 0.32]), vec![Some("close".into()), Some("close".into())]);
        assert_eq!(picks(&variants, [0.37, 0.28]), vec![Some("close".into()), Some("game".into())]);
    }
}
