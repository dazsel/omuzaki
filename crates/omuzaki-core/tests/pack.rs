//! Tests for `omuzaki_core::pack`: the acceptance numbers, property checks, and a timing report.

use omuzaki_core::pack::{PackItem, PackOptions, Packing, Placement, pack};
use omuzaki_core::rng::Rng;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use std::cmp::Reverse;

fn item(id: u32, w: u32, h: u32) -> PackItem {
    PackItem { id, w, h }
}

fn options(max_side: u32, padding: u32, pow2: bool) -> PackOptions {
    PackOptions { max_side, padding, pow2 }
}

/// Four 64 by 64 squares, ids 0 to 3.
fn four_squares() -> Vec<PackItem> {
    (0..4).map(|id| item(id, 64, 64)).collect()
}

/// `count` rectangles with sides from 8 to 64, drawn from a xorshift stream that starts at `seed`.
fn random_rects(count: u32, seed: u64) -> Vec<PackItem> {
    let mut state = seed;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..count)
        .map(|id| {
            let w = 8 + u32::try_from(next() % 57).unwrap_or_default();
            let h = 8 + u32::try_from(next() % 57).unwrap_or_default();
            item(id, w, h)
        })
        .collect()
}

fn size_of(items: &[PackItem], id: u32) -> (u32, u32) {
    let found = items.iter().find(|item| item.id == id).unwrap_or_else(|| panic!("no item {id}"));
    (found.w, found.h)
}

#[test]
fn one_item_packs_to_its_own_size() {
    let packing = pack(&[item(1, 16, 16)], &options(1024, 0, false));
    assert_eq!((packing.width, packing.height), (16, 16));
    assert_eq!(packing.placements, vec![Placement { id: 1, x: 0, y: 0 }]);
    assert_eq!(packing.unplaced, Vec::<u32>::new());
}

#[test]
fn four_squares_round_up_to_a_power_of_two() {
    let packing = pack(&four_squares(), &options(1024, 0, true));
    assert_eq!((packing.width, packing.height), (128, 128));
}

#[test]
fn padding_counts_toward_the_bin_but_not_the_placement() {
    let unrounded = pack(&four_squares(), &options(1024, 2, false));
    assert_eq!((unrounded.width, unrounded.height), (136, 136));
    for placement in &unrounded.placements {
        assert!(placement.x >= 2 && placement.x + 64 + 2 <= 136);
        assert!(placement.y >= 2 && placement.y + 64 + 2 <= 136);
    }

    let rounded = pack(&four_squares(), &options(1024, 2, true));
    assert_eq!((rounded.width, rounded.height), (256, 256));
}

#[test]
fn pow2_rounding_is_capped_at_max_side() {
    let packing = pack(&[item(1, 100, 100)], &options(120, 0, true));
    assert_eq!((packing.width, packing.height), (120, 120));
}

#[test]
fn zero_items_give_an_empty_bin() {
    let packing = pack(&[], &options(1024, 2, true));
    assert_eq!((packing.width, packing.height), (0, 0));
    assert_eq!(packing.placements, Vec::<Placement>::new());
    assert_eq!(packing.unplaced, Vec::<u32>::new());
}

#[test]
fn item_too_big_once_padded_is_unplaced_and_does_not_stop_the_rest() {
    // 64 + 2 * 1 = 66 is over max_side, so item 1 is left out; item 2 still fits.
    let packing = pack(&[item(1, 64, 64), item(2, 10, 10)], &options(64, 1, false));
    assert_eq!(packing.unplaced, vec![1]);
    assert_eq!(packing.placements, vec![Placement { id: 2, x: 1, y: 1 }]);
    assert_eq!((packing.width, packing.height), (12, 12));
}

#[test]
fn smaller_items_after_a_miss_at_max_side_are_still_placed() {
    // At 64 the 48 by 48 square takes a corner and the second one fits nowhere; the small squares still fit beside them.
    let items = [item(1, 48, 48), item(2, 48, 48), item(3, 8, 8), item(4, 8, 8), item(5, 8, 8)];
    let packing = pack(&items, &options(64, 0, false));
    assert_eq!(packing.unplaced, [2], "only the square that fits nowhere is left out");
    let mut placed: Vec<u32> = packing.placements.iter().map(|p| p.id).collect();
    placed.sort_unstable();
    assert_eq!(placed, [1, 3, 4, 5]);
}

#[test]
fn placements_come_back_in_input_order() {
    let items = [item(9, 8, 8), item(3, 64, 64), item(5, 16, 16)];
    let ids: Vec<u32> = pack(&items, &options(1024, 0, false)).placements.iter().map(|p| p.id).collect();
    assert_eq!(ids, vec![9, 3, 5]);
}

#[test]
fn two_hundred_random_rectangles_fill_at_least_eighty_percent_of_the_bin() {
    let items = random_rects(200, 0x2545_F491_4F6C_DD1D);
    let packing = pack(&items, &options(1024, 0, false));
    assert_eq!(packing.unplaced, Vec::<u32>::new());
    let placed_area: u64 = items.iter().map(|item| u64::from(item.w) * u64::from(item.h)).sum();
    let bin_area = u64::from(packing.width) * u64::from(packing.height);
    let permille = placed_area * 1000 / bin_area;
    eprintln!(
        "fill {}.{}% ({placed_area} of {bin_area}, bin {}x{})",
        permille / 10,
        permille % 10,
        packing.width,
        packing.height
    );
    assert!(placed_area * 5 >= bin_area * 4, "fill {placed_area} of {bin_area}");
}

/// Timing report, not a gate. Run with `--ignored --nocapture` to see the number.
#[test]
#[ignore = "timing report: run with --ignored --nocapture"]
fn thousand_items_pack_within_budget() {
    let items = random_rects(1000, 0x9E37_79B9_7F4A_7C15);
    let started = std::time::Instant::now();
    let packing = pack(&items, &options(4096, 2, true));
    let elapsed = started.elapsed();
    eprintln!(
        "pack 1000 items: {elapsed:?} (budget 50 ms), bin {}x{}, unplaced {}",
        packing.width,
        packing.height,
        packing.unplaced.len()
    );
}

fn items_strategy() -> impl Strategy<Value = Vec<PackItem>> {
    proptest::collection::vec((1_u32..=64, 1_u32..=64), 0..40)
        .prop_map(|sizes| sizes.into_iter().zip(0_u32..).map(|((w, h), index)| item(index * 3 + 7, w, h)).collect())
}

/// Whether the item cannot fit in `max_side` even with no bin content beside it.
fn oversize(item: &PackItem, padding: u32, max_side: u32) -> bool {
    let grown = |side: u32| u64::from(side) + 2 * u64::from(padding);
    grown(item.w) > u64::from(max_side) || grown(item.h) > u64::from(max_side)
}

/// The packing order, restated here so the test does not trust the implementation's sort.
fn order_key(item: &PackItem) -> (Reverse<u32>, Reverse<u32>, Reverse<u32>, u32) {
    (Reverse(item.w.max(item.h)), Reverse(item.h), Reverse(item.w), item.id)
}

/// Deterministic unless `PROPTEST_RNG_SEED` is set, and without regression files: a failure found that way becomes an
/// ordinary test.
fn config() -> Config {
    let mut config = Config { failure_persistence: None, ..Config::default() };
    if matches!(config.rng_seed, RngSeed::Random) {
        config.rng_seed = RngSeed::Fixed(0x5EED);
    }
    config
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn no_two_placements_overlap_once_grown_by_padding(
        items in items_strategy(),
        max_side in 16_u32..=256,
        padding in 0_u32..=3,
        pow2 in any::<bool>(),
    ) {
        let packing = pack(&items, &options(max_side, padding, pow2));
        let pad = i64::from(padding);
        let boxes: Vec<[i64; 4]> = packing
            .placements
            .iter()
            .map(|p| {
                let (w, h) = size_of(&items, p.id);
                let (x, y) = (i64::from(p.x), i64::from(p.y));
                [x - pad, y - pad, x + i64::from(w) + pad, y + i64::from(h) + pad]
            })
            .collect();
        for (i, a) in boxes.iter().enumerate() {
            for b in &boxes[i + 1..] {
                let overlap = a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
                prop_assert!(!overlap, "grown boxes {a:?} and {b:?} overlap");
            }
        }
    }

    #[test]
    fn every_placement_lies_inside_the_bin(
        items in items_strategy(),
        max_side in 16_u32..=256,
        padding in 0_u32..=3,
        pow2 in any::<bool>(),
    ) {
        let packing = pack(&items, &options(max_side, padding, pow2));
        prop_assert!(packing.width <= max_side && packing.height <= max_side);
        for p in &packing.placements {
            let (w, h) = size_of(&items, p.id);
            prop_assert!(p.x >= padding && p.y >= padding);
            prop_assert!(p.x + w + padding <= packing.width);
            prop_assert!(p.y + h + padding <= packing.height);
        }
    }

    #[test]
    fn every_item_is_placed_or_unplaced_once_and_oversize_items_are_unplaced(
        items in items_strategy(),
        max_side in 16_u32..=256,
        padding in 0_u32..=3,
    ) {
        let packing = pack(&items, &options(max_side, padding, false));
        for item in items.iter().filter(|item| oversize(item, padding, max_side)) {
            prop_assert!(packing.unplaced.contains(&item.id));
        }
        // Each item is either placed or left out, never both and never neither.
        let mut seen: Vec<u32> = packing.placements.iter().map(|p| p.id).chain(packing.unplaced.iter().copied()).collect();
        seen.sort_unstable();
        let mut ids: Vec<u32> = items.iter().map(|item| item.id).collect();
        ids.sort_unstable();
        prop_assert_eq!(seen, ids);
        // Left out in packing order.
        let mut ordered = packing.unplaced.clone();
        ordered.sort_by_key(|id| order_key(items.iter().find(|item| item.id == *id).unwrap_or(&items[0])));
        prop_assert_eq!(&ordered, &packing.unplaced);
    }

    #[test]
    fn shuffling_the_input_keeps_every_placement(
        items in items_strategy(),
        max_side in 16_u32..=256,
        padding in 0_u32..=3,
        pow2 in any::<bool>(),
        seed in any::<u64>(),
    ) {
        let opts = options(max_side, padding, pow2);
        let mut shuffled = items.clone();
        shuffled.sort_by_key(|item| Rng::new(seed ^ u64::from(item.id)).next_u64());

        let original = pack(&items, &opts);
        let reordered = pack(&shuffled, &opts);
        prop_assert_eq!((original.width, original.height), (reordered.width, reordered.height));
        prop_assert_eq!(&original.unplaced, &reordered.unplaced);
        prop_assert_eq!(sorted_by_id(&original), sorted_by_id(&reordered));
    }
}

fn sorted_by_id(packing: &Packing) -> Vec<Placement> {
    let mut placements = packing.placements.clone();
    placements.sort_by_key(|p| p.id);
    placements
}

#[test]
fn a_side_near_u32_max_grows_to_max_side_without_overflow() {
    // The first trial side is `u32::MAX - 1`; growing it must cap at `u32::MAX` rather than wrap around.
    let max = u32::MAX;
    let items = [item(1, max - 1, max - 2), item(2, 2, 2)];
    let packing = pack(&items, &options(max, 0, false));
    assert_eq!(packing.unplaced, Vec::<u32>::new());
    assert_eq!(packing.placements.len(), 2);
    assert_eq!((packing.width, packing.height), (max - 1, max));
}

#[test]
fn many_huge_items_do_not_overflow_the_area_sum() {
    // Two `u32::MAX` squares have an area sum beyond `u64`. One fits the bin; the other is left out.
    let max = u32::MAX;
    let items = [item(1, max, max), item(2, max, max)];
    let packing = pack(&items, &options(max, 0, false));
    assert_eq!(packing.placements, vec![Placement { id: 1, x: 0, y: 0 }]);
    assert_eq!(packing.unplaced, vec![2]);
    assert_eq!((packing.width, packing.height), (max, max));
}
