//! Tests for the chat-emote cell allocator (`effects::atlas`, D13, and the atlas part of the D14 restore): LRU order,
//! pins, the placeholder, the request budget, determinism and zero allocation.

// A `GlobalAlloc` impl is `unsafe` by definition. This one only counts and forwards to `System`, so the allow is local
// to this test binary.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell as StdCell;

use omuzaki_stage::effects::atlas::{Cell, EmoteAtlas, MAX_PICK, Request};

/// Counts the allocations made on the current thread while [`TRACKING`] is set. Thread-local, so other tests running
/// in parallel in this binary do not disturb the count.
struct CountingAlloc;

thread_local! {
    static TRACKING: StdCell<bool> = const { StdCell::new(false) };
    static ALLOCATIONS: StdCell<usize> = const { StdCell::new(0) };
}

fn count_allocation() {
    // `try_with`: during thread teardown the slot can be gone, and an allocator must not panic.
    let _ = TRACKING.try_with(|tracking| {
        if tracking.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_allocation();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// A fixed-seed xorshift, so the random call sequences are the same on every run. No `rand` dependency.
struct XorShift(u64);

impl XorShift {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// A key's ready cell, or the placeholder when it is not ready (checked through `cells_for`).
fn cell_of(atlas: &mut EmoteAtlas, key: u64, frame: u64) -> u16 {
    let mut out = [0; MAX_PICK];
    atlas.cells_for(&[key], frame, &mut out);
    out[0]
}

/// Requests, fetches and inserts `key`, returning its cell.
fn load(atlas: &mut EmoteAtlas, key: u64, frame: u64) -> Option<u16> {
    assert_eq!(atlas.request(key), Request::Started);
    atlas.insert(key, frame)
}

#[test]
fn sixty_three_keys_fill_the_atlas_and_the_64th_evicts_the_least_recently_used() {
    let mut atlas = EmoteAtlas::chat();
    let mut cells = Vec::new();
    for key in 1..=63 {
        cells.push(load(&mut atlas, key, key).expect("a free cell"));
    }

    // Cell 0 is the placeholder, so the 63 keys take cells 1 to 63, one each.
    let mut sorted = cells.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (1..=63).collect::<Vec<u16>>());

    // Key 1 is used at frame 100, so key 2 (stamp 2) is now the least recently used.
    assert_eq!(atlas.lookup(1, 100), Cell::Ready(cells[0]));
    assert_eq!(load(&mut atlas, 64, 101), Some(cells[1]));

    assert_eq!(atlas.lookup(2, 102), Cell::Missing);
    assert_eq!(atlas.lookup(1, 102), Cell::Ready(cells[0]));
    assert_eq!(atlas.lookup(64, 102), Cell::Ready(cells[1]));
}

#[test]
fn pinned_cells_survive_any_number_of_inserts() {
    let mut atlas = EmoteAtlas::chat();
    let kept = load(&mut atlas, 1, 1).expect("a free cell");
    atlas.pin(kept);

    for key in 2..=1_000 {
        let cell = load(&mut atlas, key, key).expect("an unpinned cell is always free to evict");
        assert_ne!(cell, kept, "a pinned cell was handed out again");
    }

    assert_eq!(atlas.lookup(1, 2_000), Cell::Ready(kept));
}

#[test]
fn a_pinned_free_cell_is_not_handed_out() {
    let mut atlas = EmoteAtlas::chat();
    atlas.pin(5);

    // With cell 5 pinned, the 62 other cells take keys 1 to 62 without touching it.
    for key in 1..=62 {
        let cell = load(&mut atlas, key, key).expect("62 usable cells");
        assert_ne!(cell, 5);
    }
    // The 63rd insert finds no free cell, so it evicts the least recently used unpinned one (key 1, cell 1), and
    // still never touches the pinned cell 5.
    assert_eq!(load(&mut atlas, 63, 63), Some(1));
    assert_eq!(atlas.lookup(1, 64), Cell::Missing);
    assert_eq!(atlas.lookup(63, 64), Cell::Ready(1));
}

#[test]
fn a_cell_pinned_twice_stays_pinned_until_unpinned_twice() {
    let mut atlas = EmoteAtlas::chat();
    let kept = load(&mut atlas, 1, 1).expect("a free cell");
    atlas.pin(kept);
    atlas.pin(kept);
    atlas.unpin(kept);

    for key in 2..=200 {
        load(&mut atlas, key, key).expect("cells are free to evict");
    }
    assert_eq!(atlas.lookup(1, 300), Cell::Ready(kept));

    atlas.unpin(kept);
    for key in 201..=400 {
        load(&mut atlas, key, key).expect("cells are free to evict");
    }
    assert_eq!(atlas.lookup(1, 500), Cell::Missing);
}

#[test]
fn everything_pinned_gives_none_and_the_key_goes_back_to_missing() {
    let mut atlas = EmoteAtlas::chat();
    for key in 1..=63 {
        let cell = load(&mut atlas, key, key).expect("a free cell");
        atlas.pin(cell);
    }

    assert_eq!(load(&mut atlas, 100, 1_000), None);
    assert_eq!(atlas.lookup(100, 1_001), Cell::Missing);
    // The failed insert freed its request slot, so the next request starts rather than reporting Busy.
    assert_eq!(atlas.request(101), Request::Started);
}

#[test]
fn unknown_keys_get_the_placeholder() {
    let mut atlas = EmoteAtlas::chat();
    assert_eq!(atlas.lookup(42, 0), Cell::Missing);

    let mut out = [9; MAX_PICK];
    assert_eq!(atlas.cells_for(&[42, 43], 0, &mut out), 2);
    assert_eq!(&out[..2], &[0, 0]);

    // A key that is still loading also draws the placeholder.
    assert_eq!(atlas.request(42), Request::Started);
    assert_eq!(atlas.lookup(42, 1), Cell::Loading);
    assert_eq!(cell_of(&mut atlas, 42, 1), 0);
}

#[test]
fn a_ninth_concurrent_request_is_busy() {
    let mut atlas = EmoteAtlas::chat();
    for key in 1..=8 {
        assert_eq!(atlas.request(key), Request::Started);
    }

    assert_eq!(atlas.request(9), Request::Busy);
    // A key already loading is known, so it is not Busy even while the budget is spent.
    assert_eq!(atlas.request(3), Request::AlreadyKnown);

    // Finishing one load frees its request slot.
    assert_eq!(atlas.insert(1, 0), Some(1));
    assert_eq!(atlas.request(9), Request::Started);
    // A ready key is known too.
    assert_eq!(atlas.request(1), Request::AlreadyKnown);
}

#[test]
fn cells_for_writes_at_most_eight_and_touches_ready_keys() {
    let mut atlas = EmoteAtlas::chat();
    for key in 1..=63 {
        load(&mut atlas, key, key).expect("a free cell");
    }

    // Ten keys give eight cells. Only the first eight are written, and the count reports that.
    let keys: Vec<u64> = (1..=10).collect();
    let mut out = [u16::MAX; MAX_PICK];
    assert_eq!(atlas.cells_for(&keys, 100, &mut out), MAX_PICK);
    assert!(out.iter().all(|&cell| (1..=63).contains(&cell)));

    // Keys 1 to 8 were touched at frame 100, so key 9 (stamp 9) is the least recently used, not key 1. Keys 1 to 63
    // fill cells 1 to 63 in order, so key 9 sits in cell 9.
    assert_eq!(load(&mut atlas, 64, 101), Some(9));
    assert_eq!(atlas.lookup(9, 102), Cell::Missing);
    assert_eq!(atlas.lookup(1, 102), Cell::Ready(out[0]));
}

#[test]
fn checkpoint_restore_re_resolves_evicted_keys_to_the_placeholder() {
    // D14: a batch's keys are inserted and pinned, then unpinned. Enough new keys evict them. Restore then re-resolves
    // the old keys, and each must give the placeholder, never a cell another key now holds.
    let mut atlas = EmoteAtlas::chat();
    let batch = [1_001_u64, 1_002, 1_003];
    let mut picked = [0; MAX_PICK];
    for &key in &batch {
        load(&mut atlas, key, 1).expect("a free cell");
    }
    assert_eq!(atlas.cells_for(&batch, 1, &mut picked), batch.len());
    let batch_cells = [picked[0], picked[1], picked[2]];
    for &cell in &batch_cells {
        assert_ne!(cell, 0, "the batch's keys are all ready");
        atlas.pin(cell);
    }
    for &cell in &batch_cells {
        atlas.unpin(cell);
    }

    // 70 new keys: the batch's stamp-1 cells are the least recently used, so they go first.
    let mut newest = 0;
    for key in 2_000..2_070 {
        newest = key;
        load(&mut atlas, key, key).expect("unpinned cells are free to evict");
    }

    let mut restored = [9; MAX_PICK];
    assert_eq!(atlas.cells_for(&batch, 200, &mut restored), batch.len());
    assert_eq!(&restored[..3], &[0, 0, 0]);
    for &key in &batch {
        assert_eq!(atlas.lookup(key, 201), Cell::Missing);
    }

    // A key that survived the flood still resolves to the cell it was given.
    let mut survivor = [0; MAX_PICK];
    assert_eq!(atlas.cells_for(&[newest], 202, &mut survivor), 1);
    assert_ne!(survivor[0], 0);
    assert_eq!(atlas.lookup(newest, 203), Cell::Ready(survivor[0]));
}

#[test]
fn the_same_calls_give_the_same_cells() {
    fn run(seed: u64) -> Vec<Option<u16>> {
        let mut rng = XorShift(seed);
        let mut atlas = EmoteAtlas::chat();
        let mut results = Vec::new();
        for frame in 0..2_000 {
            let key = rng.next_u64() % 150;
            match rng.next_u64() % 4 {
                0 => results.push(match atlas.lookup(key, frame) {
                    Cell::Ready(cell) => Some(cell),
                    _ => None,
                }),
                1 => {
                    atlas.request(key);
                }
                2 => results.push(atlas.insert(key, frame)),
                _ => {
                    let cell = u16::try_from(1 + rng.next_u64() % 63).unwrap_or(1);
                    atlas.pin(cell);
                    atlas.unpin(cell);
                }
            }
        }
        results
    }

    assert_eq!(run(0x9E37_79B9_7F4A_7C15), run(0x9E37_79B9_7F4A_7C15));
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "not pinned")]
fn unpinning_below_zero_is_a_debug_assertion() {
    let mut atlas = EmoteAtlas::chat();
    atlas.unpin(1);
}

#[test]
fn ten_thousand_random_calls_do_not_allocate() {
    let mut atlas = EmoteAtlas::chat();
    let mut rng = XorShift(0x2545_F491_4F6C_DD1D);
    // Pins are counted here so the random unpins only ever match a pin, which keeps the debug assertion quiet.
    let mut pinned = [0_u32; 64];
    let mut keys = [0_u64; MAX_PICK];
    let mut out = [0_u16; MAX_PICK];
    let mut checksum = 0_u64;

    ALLOCATIONS.with(|count| count.set(0));
    TRACKING.with(|tracking| tracking.set(true));
    for frame in 0..10_000_u64 {
        let key = rng.next_u64() % 200;
        let cell = u16::try_from(1 + rng.next_u64() % 63).unwrap_or(1);
        match rng.next_u64() % 6 {
            0 => {
                if let Cell::Ready(found) = atlas.lookup(key, frame) {
                    checksum = checksum.wrapping_add(u64::from(found));
                }
            }
            1 => {
                let _ = atlas.request(key);
            }
            2 => {
                if let Some(placed) = atlas.insert(key, frame) {
                    checksum = checksum.wrapping_add(u64::from(placed));
                }
            }
            3 => {
                atlas.pin(cell);
                pinned[usize::from(cell)] += 1;
            }
            4 => {
                if pinned[usize::from(cell)] > 0 {
                    atlas.unpin(cell);
                    pinned[usize::from(cell)] -= 1;
                }
            }
            _ => {
                for slot in &mut keys {
                    *slot = rng.next_u64() % 200;
                }
                let written = atlas.cells_for(&keys, frame, &mut out);
                checksum = checksum.wrapping_add(u64::try_from(written).unwrap_or(0));
            }
        }
    }
    TRACKING.with(|tracking| tracking.set(false));
    let allocations = ALLOCATIONS.with(StdCell::get);

    // The checksum keeps the calls observable; the allocation count is the claim.
    assert_ne!(checksum, 0);
    assert_eq!(allocations, 0, "the atlas allocated during its calls");
}
