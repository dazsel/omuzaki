//! Rectangle packing for texture atlases and sprite sheets (spec v3 §27, EX-03 and EX-07).
//!
//! The packer places axis-aligned rectangles into a square bin with `MaxRects` and the best-short-side-fit rule. It
//! never rotates an item, because rotated UVs cost more in the shaders than the space they save. The result depends
//! only on the set of items, never on the order they arrive in: items are placed largest first, ties broken by `id`,
//! so a caller can shuffle its inputs without the atlas changing.
//!
//! The bin is the bounding box of the first trial square, grown from the padded area, that holds everything. Once the
//! square reaches `max_side`, an item that fits nowhere is left out and the later, smaller items are still tried, so a
//! full atlas leaves out only what truly does not fit, and a multi-page caller needs no more pages than it must.

/// One rectangle to place. `id` is the caller's key and the tie-break, so the ids in one call should be unique.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackItem {
    /// The caller's key for this item. Equal sizes pack in ascending `id` order.
    pub id: u32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

/// How the bin is sized and how items are spaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackOptions {
    /// The bin is never larger than this on either side.
    pub max_side: u32,
    /// Empty pixels kept around every item (a gutter against mip bleeding). They count toward the bin size, not toward
    /// the placement.
    pub padding: u32,
    /// Round the bin's sides up to powers of two. The result is still capped at `max_side`, so a `max_side` that is
    /// not a power of two can yield a bin that is not one either.
    pub pow2: bool,
}

/// Where one item landed. `x` and `y` are the item's own top-left corner, padding excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The id of the item this places.
    pub id: u32,
    /// Left edge of the item inside the bin.
    pub x: u32,
    /// Top edge of the item inside the bin.
    pub y: u32,
}

/// The result of [`pack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packing {
    /// Bin width in pixels; `0` when nothing was placed.
    pub width: u32,
    /// Bin height in pixels; `0` when nothing was placed.
    pub height: u32,
    /// The placed items, in input order.
    pub placements: Vec<Placement>,
    /// The ids left out, in the order they were considered: items too large for `max_side`, and the items that fit
    /// nowhere once the bin reached `max_side`.
    pub unplaced: Vec<u32>,
}

/// Each retry grows the trial square by `1 / GROWTH_DIVISOR` of its side: a few percent of slack per step, so a real
/// input needs only a handful of retries.
const GROWTH_DIVISOR: u32 = 64;

/// Packs `items` into the first trial square, no larger than `options.max_side` on either side, that holds them all.
///
/// The bin side is searched rather than fixed. `MaxRects` inside a `max_side` square would lay items out in strips, so the
/// packer starts from a square just big enough for the padded area and grows it in steps until everything fits. That is
/// a heuristic, not a guarantee of the smallest bin. The reported bin is the bounding box of the placed items, which can
/// be rectangular, rounded up for `pow2` and capped at `max_side`.
///
/// Items are considered largest first (longest side, then height, then width, all descending; then `id` ascending).
/// An item that cannot fit within `max_side` once padded is left out and does not count against the bin. At `max_side`,
/// an item that fits nowhere is left out and every later item is still tried in the same order, so only the items that
/// fit nowhere are left out.
///
/// The function is pure: no randomness, no global state, and no panics. Zero items give a `0` by `0` packing.
#[must_use]
pub fn pack(items: &[PackItem], options: &PackOptions) -> Packing {
    let max_side = options.max_side;
    let padded: Vec<Option<(u32, u32)>> =
        items.iter().map(|&item| padded_size(item, options.padding, max_side)).collect();
    let order = packing_order(items);
    let mut side = first_trial_side(&padded, max_side);
    loop {
        let attempt = try_side(items, &order, &padded, options.padding, side, side >= max_side);
        if attempt.complete || side >= max_side {
            return attempt.into_packing(items, options);
        }
        // Saturating: a side near `u32::MAX` must cap at `max_side`, not wrap around.
        side = side.saturating_add((side / GROWTH_DIVISOR).max(1)).min(max_side);
    }
}

/// The side of the first square to try: the side of the smallest square with the padded area, or the largest padded
/// side if that is bigger, capped at `max_side`.
fn first_trial_side(padded: &[Option<(u32, u32)>], max_side: u32) -> u32 {
    // Saturating: the sum of many `u32::MAX` squares can exceed `u64`, and any area that large is simply too big.
    let area = padded.iter().flatten().fold(0_u64, |total, &(w, h)| total.saturating_add(u64::from(w) * u64::from(h)));
    let largest = padded.iter().flatten().map(|&(w, h)| w.max(h)).max().unwrap_or(0);
    let root = u64::isqrt(area);
    let root = if root * root < area { root + 1 } else { root };
    let root = u32::try_from(root).unwrap_or(u32::MAX);
    root.max(largest).min(max_side)
}

/// One packing attempt inside a square bin of a fixed side.
struct Attempt {
    /// Where each item's padded rectangle went, as the item's own corner, padding excluded.
    corners: Vec<Option<(u32, u32)>>,
    /// Right and bottom edges of the placed padded rectangles: the bounding box before pow2 rounding.
    extent: (u32, u32),
    /// Ids left out, in the order they were considered.
    unplaced: Vec<u32>,
    /// Whether every item that fits `max_side` was placed. Only a complete attempt is accepted before `max_side`.
    complete: bool,
}

/// Runs `MaxRects` over a `side` by `side` square, in packing order. At the last side (`last`) an item that fits nowhere
/// is skipped and the rest are still tried; before it, the first miss ends the attempt, since a bigger square follows.
fn try_side(
    items: &[PackItem],
    order: &[usize],
    padded: &[Option<(u32, u32)>],
    padding: u32,
    side: u32,
    last: bool,
) -> Attempt {
    let mut packer = MaxRects::new(side);
    let mut attempt =
        Attempt { corners: vec![None; items.len()], extent: (0, 0), unplaced: Vec::new(), complete: true };
    for &index in order {
        let item = items[index];
        let Some((pw, ph)) = padded[index] else {
            attempt.unplaced.push(item.id);
            continue;
        };
        let slot = if attempt.complete || last { packer.insert(pw, ph) } else { None };
        if let Some(rect) = slot {
            attempt.corners[index] = Some((rect.x + padding, rect.y + padding));
            attempt.extent = (attempt.extent.0.max(rect.x + rect.w), attempt.extent.1.max(rect.y + rect.h));
        } else {
            attempt.complete = false;
            attempt.unplaced.push(item.id);
        }
    }
    attempt
}

impl Attempt {
    /// Turns the accepted attempt into the public result, sizing the bin from its bounding box.
    fn into_packing(self, items: &[PackItem], options: &PackOptions) -> Packing {
        let placements = items
            .iter()
            .zip(self.corners)
            .filter_map(|(item, corner)| corner.map(|(x, y)| Placement { id: item.id, x, y }))
            .collect();
        Packing {
            width: bin_side(self.extent.0, options.pow2, options.max_side),
            height: bin_side(self.extent.1, options.pow2, options.max_side),
            placements,
            unplaced: self.unplaced,
        }
    }
}

/// Indices of `items` in placement order: longest side, then height, then width, all descending, then `id` ascending.
fn packing_order(items: &[PackItem]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    // The key ends in `id`, so two items with the same key are the same item for the packing; the sort need not be
    // stable.
    order.sort_unstable_by_key(|&index| {
        let item = items[index];
        (std::cmp::Reverse(item.w.max(item.h)), std::cmp::Reverse(item.h), std::cmp::Reverse(item.w), item.id)
    });
    order
}

/// The item's size with padding on every side, or `None` when that is wider or taller than `max_side`.
///
/// The sum is done in `u64` so a huge `w` or `padding` cannot overflow: it is simply too big for any bin.
fn padded_size(item: PackItem, padding: u32, max_side: u32) -> Option<(u32, u32)> {
    let grow = |side: u32| {
        let grown = u64::from(side) + 2 * u64::from(padding);
        u32::try_from(grown).ok().filter(|&grown| grown <= max_side)
    };
    Some((grow(item.w)?, grow(item.h)?))
}

/// The bin side for content that reaches `used` pixels: `used` itself, or its next power of two, capped at `max_side`.
fn bin_side(used: u32, pow2: bool, max_side: u32) -> u32 {
    if used == 0 {
        // `0_u32.next_power_of_two()` is 1, which would give an empty packing a one-pixel bin.
        return 0;
    }
    if pow2 { used.checked_next_power_of_two().unwrap_or(max_side).min(max_side) } else { used }
}

/// An axis-aligned rectangle in bin pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Rect {
    /// Whether the two rectangles share any area. Rectangles that only touch along an edge do not overlap.
    fn overlaps(self, other: Self) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// Whether `other` lies entirely inside `self`.
    fn contains(self, other: Self) -> bool {
        self.x <= other.x
            && other.x + other.w <= self.x + self.w
            && self.y <= other.y
            && other.y + other.h <= self.y + self.h
    }
}

/// `MaxRects` state: the free space of the bin, kept as a list of maximal rectangles.
///
/// A free rectangle is maximal when no other free rectangle contains it. Keeping only maximal ones lets every
/// placement consider the whole free space, so the best-fit choice is never blind to a gap.
struct MaxRects {
    free: Vec<Rect>,
    /// Reused between placements, so packing allocates no more than the free list itself grows to.
    fragments: Vec<Rect>,
}

impl MaxRects {
    fn new(side: u32) -> Self {
        Self { free: vec![Rect { x: 0, y: 0, w: side, h: side }], fragments: Vec::new() }
    }

    /// Places a `w` by `h` rectangle in the free space that leaves the least slack, and returns where it went.
    ///
    /// The slack is judged by the shorter leftover side first, then the longer one. Ties go to the higher, then the
    /// further left, so the choice is the same on every run.
    fn insert(&mut self, w: u32, h: u32) -> Option<Rect> {
        let slot = self.free.iter().filter(|free| w <= free.w && h <= free.h).min_by_key(|free| {
            let (leftover_w, leftover_h) = (free.w - w, free.h - h);
            (leftover_w.min(leftover_h), leftover_w.max(leftover_h), free.y, free.x)
        })?;
        let placed = Rect { x: slot.x, y: slot.y, w, h };
        self.occupy(placed);
        Some(placed)
    }

    /// Removes `used` from the free space: every free rectangle it touches is cut into the parts that stay free.
    fn occupy(&mut self, used: Rect) {
        self.fragments.clear();
        let mut index = 0;
        while index < self.free.len() {
            let free = self.free[index];
            if free.overlaps(used) {
                self.free.swap_remove(index);
                push_remainders(free, used, &mut self.fragments);
            } else {
                index += 1;
            }
        }

        let first_new = self.free.len();
        self.free.extend_from_slice(&self.fragments);
        // Drop the new pieces that lie inside another free rectangle. Walking backwards keeps the indices still to
        // visit valid after each `swap_remove`.
        for index in (first_new..self.free.len()).rev() {
            if is_redundant(&self.free, index) {
                self.free.swap_remove(index);
            }
        }
    }
}

/// Appends the parts of `free` that lie outside `used`. The parts may overlap each other; pruning removes the surplus.
fn push_remainders(free: Rect, used: Rect, out: &mut Vec<Rect>) {
    let free_right = free.x + free.w;
    let free_bottom = free.y + free.h;
    let used_right = used.x + used.w;
    let used_bottom = used.y + used.h;
    if used.x > free.x {
        out.push(Rect { x: free.x, y: free.y, w: used.x - free.x, h: free.h });
    }
    if used_right < free_right {
        out.push(Rect { x: used_right, y: free.y, w: free_right - used_right, h: free.h });
    }
    if used.y > free.y {
        out.push(Rect { x: free.x, y: free.y, w: free.w, h: used.y - free.y });
    }
    if used_bottom < free_bottom {
        out.push(Rect { x: free.x, y: used_bottom, w: free.w, h: free_bottom - used_bottom });
    }
}

/// Whether `free[index]` lies inside another free rectangle. Between two equal rectangles, only the later one is
/// redundant, so exactly one copy survives.
fn is_redundant(free: &[Rect], index: usize) -> bool {
    let rect = free[index];
    free.iter()
        .enumerate()
        .any(|(other, container)| other != index && container.contains(rect) && (*container != rect || other < index))
}
