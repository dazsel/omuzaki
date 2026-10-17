//! Vello's bump-allocated buffers: how big they are, and the check that tells, a frame or two later, whether a repaint
//! needed more than they had.
//!
//! Vello's stages allocate lines, bins, tiles, segment counts, segments, blend spill and per-tile command lists by
//! bumping counters in one small buffer. Most stages check their counter against the size the config names and set
//! `failed` when it passes, which stops the stages after them; coarse bumps the segments counter with no check at all
//! (Vello 0.11), so a scene that outgrows the segments buffer draws wrong with nothing failed. The engine therefore
//! copies the counters out after every repaint into a buffer of the layer's own, maps it without waiting (`map_async`,
//! read once the GPU has run that repaint) and compares every counter with the capacity that repaint was made with; a
//! repaint that ran out of anything grows what ran out and has its layer repainted whole. Nothing on the frame path waits
//! on the GPU.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use vello_encoding::{BufferSize, BufferSizes, BumpAllocators, Layout, RenderConfig, WorkgroupCounts};

/// Vello's `failed` flags (`shader/shared/bump.wgsl`): which stage ran out.
const STAGE_BINNING: u32 = 0x1;
const STAGE_TILE_ALLOC: u32 = 0x2;
const STAGE_FLATTEN: u32 = 0x4;
const STAGE_PATH_COUNT: u32 = 0x8;
const STAGE_COARSE: u32 = 0x10;

/// The command words coarse gives every tile up front (`shader/shared/ptcl.wgsl`): its dynamic allocations, which the
/// `ptcl` counter counts, start after `tiles * PTCL_INITIAL_ALLOC` words.
const PTCL_INITIAL_ALLOC: u32 = 64;

/// The number of bump-allocated buffers.
const BUFFERS: usize = 7;

/// Per buffer, in [`BumpCapacity::entries`] order, the stage whose `failed` flag says it ran out. Segments have none:
/// no stage checks their counter.
const FLAGS: [u32; BUFFERS] =
    [STAGE_FLATTEN, STAGE_BINNING, STAGE_TILE_ALLOC, STAGE_PATH_COUNT, 0, STAGE_COARSE, STAGE_COARSE];

/// How many entries each of Vello's bump-allocated buffers holds. The GPU rasteriser starts at Vello's own sizes
/// ([`vello`](Self::vello)) and grows a buffer when a repaint needs more; tests of that path start it smaller
/// ([`VelloRasterizer::with_bump_capacity`](super::super::VelloRasterizer::with_bump_capacity)).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BumpCapacity {
    /// Flattened lines (`LineSoup`), which the flatten stage writes.
    pub lines: u32,
    /// Bin entries, which binning writes after the draw objects' info (the info is not counted here).
    pub binning: u32,
    /// Tiles of every path's bounding box, which tile allocation reserves.
    pub tiles: u32,
    /// Segment counts (one per line crossing a tile), which path counting writes.
    pub seg_counts: u32,
    /// Segments, which coarse reserves and path tiling writes. Vello checks no limit here.
    pub segments: u32,
    /// Blend-stack spill words, which coarse reserves for deep clip stacks.
    pub blend: u32,
    /// Per-tile command-list words, which coarse writes: every tile's first block, then the blocks it allocates.
    pub ptcl: u32,
}

/// What a check of a layer's earlier repaints found. Ordered so the graver one wins when several come back at once.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Recovery {
    /// The layer's picture may be wrong: a repaint ran out of room (the buffers now hold what it needed, grown by this
    /// check or by an earlier one for another layer), or one went unchecked. Repaint the layer whole.
    Repaint,
    /// A buffer a repaint ran out of is already as large as the device allows: the GPU can't draw this scene.
    Exhausted,
}

/// What a repaint was made with: the capacities its config named, and where coarse's dynamic command blocks start
/// (after every tile's first block, so it depends on the repaint's size).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct Made {
    pub(super) capacity: BumpCapacity,
    pub(super) ptcl_start: u32,
}

impl Made {
    /// What a repaint `width_in_tiles` x `height_in_tiles` tiles large made with `capacity` has.
    pub(super) fn new(capacity: BumpCapacity, width_in_tiles: u32, height_in_tiles: u32) -> Self {
        let ptcl_start = width_in_tiles.saturating_mul(height_in_tiles).saturating_mul(PTCL_INITIAL_ALLOC);
        Self { capacity, ptcl_start }
    }
}

impl BumpCapacity {
    /// Vello's own sizes (`BufferSizes::new`'s fixed ones), the rasteriser's start.
    pub fn vello() -> Self {
        let sizes = BufferSizes::new(&Layout::default(), &WorkgroupCounts::default());
        Self {
            lines: sizes.lines.len(),
            binning: sizes.bin_data.len(),
            tiles: sizes.tiles.len(),
            seg_counts: sizes.seg_counts.len(),
            segments: sizes.segments.len(),
            blend: sizes.blend_spill.len(),
            ptcl: sizes.ptcl.len(),
        }
    }

    /// Every buffer at `entries` entries (at least one).
    pub fn uniform(entries: u32) -> Self {
        let n = entries.max(1);
        Self { lines: n, binning: n, tiles: n, seg_counts: n, segments: n, blend: n, ptcl: n }
    }

    /// The entries of every buffer, in a fixed order ([`FLAGS`] and `grow` follow it).
    fn entries(&self) -> [u32; BUFFERS] {
        [self.lines, self.binning, self.tiles, self.seg_counts, self.segments, self.blend, self.ptcl]
    }

    /// The entries of every buffer, in [`entries`](Self::entries) order, to change.
    fn entries_mut(&mut self) -> [&mut u32; BUFFERS] {
        [
            &mut self.lines,
            &mut self.binning,
            &mut self.tiles,
            &mut self.seg_counts,
            &mut self.segments,
            &mut self.blend,
            &mut self.ptcl,
        ]
    }

    /// What a repaint's counters `bump` say each buffer needed, in [`entries`](Self::entries) order. Every check Vello
    /// makes is "counter past size" except coarse's command lists, whose counter counts only the blocks allocated after
    /// `ptcl_start`.
    fn used(bump: &BumpAllocators, ptcl_start: u32) -> [u32; BUFFERS] {
        let ptcl = ptcl_start.saturating_add(bump.ptcl);
        [bump.lines, bump.binning, bump.tile, bump.seg_counts, bump.segments, bump.blend, ptcl]
    }

    /// Writes these capacities into `config`: the buffer sizes the scratch is made with, and the limits the stages
    /// check their counters against.
    pub(super) fn apply(&self, config: &mut RenderConfig, layout: &Layout) {
        let sizes = &mut config.buffer_sizes;
        sizes.lines = BufferSize::new(self.lines);
        sizes.bin_data = BufferSize::new(layout.bin_data_start.saturating_add(self.binning));
        sizes.tiles = BufferSize::new(self.tiles);
        sizes.seg_counts = BufferSize::new(self.seg_counts);
        sizes.segments = BufferSize::new(self.segments);
        sizes.blend_spill = BufferSize::new(self.blend);
        sizes.ptcl = BufferSize::new(self.ptcl);
        let gpu = &mut config.gpu;
        gpu.lines_size = sizes.lines.len();
        gpu.binning_size = sizes.bin_data.len() - layout.bin_data_start;
        gpu.tiles_size = sizes.tiles.len();
        gpu.seg_counts_size = sizes.seg_counts.len();
        gpu.segments_size = sizes.segments.len();
        gpu.blend_size = sizes.blend_spill.len();
        gpu.ptcl_size = sizes.ptcl.len();
    }

    /// Checks the counters `bump` of a repaint made with `made` and grows these capacities (the ones the next repaint
    /// gets) to what it needed. `None` if it had room for everything and nothing failed.
    ///
    /// Every counter is compared with the capacity that repaint had, failed or not: coarse never flags the segments it
    /// runs out of. A buffer it ran out of grows to at least twice what it had and past its counter (a power of two), at
    /// most to what one binding may hold (`max_binding` bytes). A stage that fails stops the ones after it, so their
    /// counters are short: every buffer is also raised to at least Vello's own size (which only a capacity set below
    /// it, a test's, is under), and a later stage that still runs out grows on its own report. A failed flag with no
    /// counter past its size (which Vello's checks don't produce) grows its stage's buffers all the same.
    pub(super) fn grow(&mut self, bump: &BumpAllocators, made: &Made, max_binding: u64) -> Option<Recovery> {
        let had = made.capacity.entries();
        let used = Self::used(bump, made.ptcl_start);
        let past: [bool; BUFFERS] = std::array::from_fn(|at| used[at] > had[at]);
        let counted = past.contains(&true);
        if !counted && bump.failed == 0 {
            return None;
        }
        // Bytes per entry, and Vello's own sizes.
        let sizes = BufferSizes::new(&Layout::default(), &WorkgroupCounts::default());
        let bytes = [
            entry_bytes(sizes.lines),
            entry_bytes(sizes.bin_data),
            entry_bytes(sizes.tiles),
            entry_bytes(sizes.seg_counts),
            entry_bytes(sizes.segments),
            entry_bytes(sizes.blend_spill),
            entry_bytes(sizes.ptcl),
        ];
        let vello = Self::vello().entries();
        let mut exhausted = false;
        for (at, entries) in self.entries_mut().into_iter().enumerate() {
            let ran_out = past[at] || (!counted && bump.failed & FLAGS[at] != 0);
            let limit = u32::try_from(max_binding / bytes[at].max(1)).unwrap_or(u32::MAX);
            let mut wanted = vello[at];
            if ran_out {
                wanted = wanted.max(had[at].saturating_mul(2)).max(used[at].saturating_add(1).next_power_of_two());
            }
            *entries = (*entries).max(wanted.min(limit));
            exhausted |= ran_out && *entries <= had[at];
        }
        Some(if exhausted { Recovery::Exhausted } else { Recovery::Repaint })
    }
}

/// The bytes of one entry of a buffer like `size`.
fn entry_bytes<T>(_: BufferSize<T>) -> u64 {
    std::mem::size_of::<T>() as u64
}

/// Where a counters read-back is, and what the repaint it reads was made with.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum State {
    /// Free for the next repaint.
    Idle,
    /// A repaint's commands copy the counters in; mapping waits until they are submitted, at the next check.
    Recorded(Made),
    /// Mapping asked for; `ready` says when it is done.
    Mapping(Made),
}

/// What the map callback reports in `ready`.
const PENDING: u8 = 0;
const MAPPED: u8 = 1;
const UNMAPPABLE: u8 = 2;

/// One buffer the counters are copied into and read back from.
struct Slot {
    buffer: wgpu::Buffer,
    state: State,
    /// Set by the map callback, which runs while the device is polled (the renderer polls once a frame).
    ready: Arc<AtomicU8>,
}

impl Slot {
    fn new(device: &wgpu::Device) -> Self {
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("face-vello-bump-read"),
                size: BYTES,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            state: State::Idle,
            ready: Arc::new(AtomicU8::new(PENDING)),
        }
    }
}

/// The read-backs a layer starts with: the stage's GPU runs a frame or two behind, so a read-back is free again by the
/// time the next repaint but two is recorded.
const FIRST_SLOTS: usize = 3;
/// The most read-backs a layer makes. A GPU further behind than its first ones (a benchmark that submits a batch of
/// repaints before it polls) gets more, made once and kept, so every repaint is still checked; past this many a repaint
/// goes unchecked, and the layer is repainted whole once a read-back is free again.
const MAX_SLOTS: usize = 128;

/// A layer's read-backs of Vello's counters.
pub(super) struct BumpReads {
    /// Made with room for [`MAX_SLOTS`], so adding one never reallocates the list.
    slots: Vec<Slot>,
    /// A repaint found no free read-back, so nothing checks it.
    unchecked: bool,
}

/// The bytes of the counters.
const BYTES: u64 = std::mem::size_of::<BumpAllocators>() as u64;

impl BumpReads {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let mut slots = Vec::with_capacity(MAX_SLOTS);
        slots.extend((0..FIRST_SLOTS).map(|_| Slot::new(device)));
        Self { slots, unchecked: false }
    }

    /// Moves the read-backs along and checks the counters of every repaint that came back against what it was made
    /// with ([`BumpCapacity::grow`], which grows `capacity`): `Exhausted` if one ran out of what can't grow, else
    /// `Repaint` if one ran out or a repaint went unchecked and a read-back is free again, else `None`. Asks for the
    /// mapping of the read-backs recorded since the last check (each
    /// was submitted since: the renderer submits a paint as soon as the painter returns) and reads those that are
    /// mapped. Never waits.
    pub(super) fn poll(&mut self, capacity: &mut BumpCapacity, max_binding: u64) -> Option<Recovery> {
        let mut found = None;
        for slot in &mut self.slots {
            match slot.state {
                State::Idle => {}
                State::Recorded(made) => {
                    slot.ready.store(PENDING, Ordering::Release);
                    let ready = Arc::clone(&slot.ready);
                    slot.buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
                        ready.store(if result.is_ok() { MAPPED } else { UNMAPPABLE }, Ordering::Release);
                    });
                    slot.state = State::Mapping(made);
                }
                State::Mapping(made) => match slot.ready.load(Ordering::Acquire) {
                    PENDING => {}
                    MAPPED => {
                        let bump = slot
                            .buffer
                            .slice(..)
                            .get_mapped_range()
                            .ok()
                            .and_then(|bytes| bytemuck::try_pod_read_unaligned::<BumpAllocators>(&bytes).ok());
                        slot.buffer.unmap();
                        slot.state = State::Idle;
                        found = found.max(bump.and_then(|bump| capacity.grow(&bump, &made, max_binding)));
                    }
                    _ => slot.state = State::Idle,
                },
            }
        }
        if self.unchecked && self.slots.iter().any(|slot| slot.state == State::Idle) {
            // The repaint that found no free read-back may have run out of room unseen: the next one is checked.
            self.unchecked = false;
            found = found.max(Some(Recovery::Repaint));
        }
        found
    }

    /// Records the copy of `bump`, the counters this repaint's stages just bumped, into a free read-back, with what the
    /// repaint was `made` with. With none free (the GPU is that many repaints behind) it makes one on `device`, up to
    /// [`MAX_SLOTS`]; past that this repaint goes unchecked, and the first [`poll`](Self::poll) that finds a read-back
    /// free again asks for a whole repaint, which is checked: a layer that then stops changing is never left showing a
    /// repaint that ran out of room.
    pub(super) fn record(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        bump: &wgpu::Buffer,
        made: Made,
    ) {
        let free = match self.slots.iter().position(|slot| slot.state == State::Idle) {
            Some(at) => Some(at),
            None if self.slots.len() < MAX_SLOTS => {
                self.slots.push(Slot::new(device));
                Some(self.slots.len() - 1)
            }
            None => None,
        };
        match free.and_then(|at| self.slots.get_mut(at)) {
            Some(slot) => {
                encoder.copy_buffer_to_buffer(bump, 0, &slot.buffer, 0, BYTES);
                slot.state = State::Recorded(made);
            }
            None => self.unchecked = true,
        }
    }

    /// Forgets the copy the last paint recorded: the renderer dropped its commands, so it never ran.
    pub(super) fn forget_recorded(&mut self) {
        for slot in &mut self.slots {
            if matches!(slot.state, State::Recorded(_)) {
                slot.state = State::Idle;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a repaint with no tiles made with `capacity` has (its command lists start at 0).
    fn made(capacity: BumpCapacity) -> Made {
        Made::new(capacity, 0, 0)
    }

    fn failed(flags: u32) -> BumpAllocators {
        BumpAllocators { failed: flags, ..BumpAllocators::default() }
    }

    #[test]
    fn a_repaint_with_room_for_everything_needs_nothing() {
        let vello = BumpCapacity::vello();
        let mut capacity = vello;
        let report = BumpAllocators { lines: vello.lines, segments: vello.segments, ..failed(0) };
        assert_eq!(capacity.grow(&report, &made(vello), u64::MAX), None, "a counter at its size fits");
        assert_eq!(capacity, vello);
    }

    #[test]
    fn segments_past_their_capacity_grow_although_nothing_failed() {
        // Coarse bumps the segments counter with no check, so this is all a segments overflow leaves.
        let vello = BumpCapacity::vello();
        let mut capacity = vello;
        let report = BumpAllocators { segments: vello.segments + 5, ..failed(0) };
        assert_eq!(capacity.grow(&report, &made(vello), u64::MAX), Some(Recovery::Repaint));
        assert_eq!(capacity.segments, vello.segments * 2, "twice what the repaint had, past the counter");
        assert_eq!(BumpCapacity { segments: vello.segments, ..capacity }, vello, "nothing else grows");
    }

    #[test]
    fn segments_that_run_out_once_the_segment_counts_grew_are_caught() {
        // The review's case on #2222: path counting runs out, so coarse never runs and the segments counter is short;
        // with the counts grown, the next repaint runs coarse, which runs out of segments and flags nothing.
        let vello = BumpCapacity::vello();
        let mut capacity = vello;
        let counts = BumpAllocators { seg_counts: vello.seg_counts + 1, ..failed(STAGE_PATH_COUNT) };
        assert_eq!(capacity.grow(&counts, &made(vello), u64::MAX), Some(Recovery::Repaint));
        assert_eq!((capacity.seg_counts, capacity.segments), (vello.seg_counts * 2, vello.segments));
        let grown = capacity;
        let segments = BumpAllocators { seg_counts: vello.seg_counts + 1, segments: vello.segments + 1, ..failed(0) };
        assert_eq!(capacity.grow(&segments, &made(grown), u64::MAX), Some(Recovery::Repaint));
        assert_eq!(capacity.segments, vello.segments * 2);
    }

    #[test]
    fn command_lists_count_from_after_every_tiles_first_block() {
        let vello = BumpCapacity::vello();
        let had = Made::new(vello, 16, 16);
        assert_eq!(had.ptcl_start, 16 * 16 * PTCL_INITIAL_ALLOC);
        let mut capacity = vello;
        let within = BumpAllocators { ptcl: vello.ptcl - had.ptcl_start, ..failed(0) };
        assert_eq!(
            capacity.grow(&within, &had, u64::MAX),
            None,
            "the first blocks and the counted ones fill it exactly"
        );
        let past = BumpAllocators { ptcl: vello.ptcl - had.ptcl_start + 1, ..failed(0) };
        assert_eq!(capacity.grow(&past, &had, u64::MAX), Some(Recovery::Repaint), "a counter under the size alone");
        assert_eq!(capacity.ptcl, vello.ptcl * 2);
    }

    #[test]
    fn a_repaint_made_before_another_layer_grew_the_buffers_is_repainted_without_growing_again() {
        let vello = BumpCapacity::vello();
        let mut capacity = vello;
        let report = BumpAllocators { lines: vello.lines + 1, ..failed(STAGE_FLATTEN) };
        assert_eq!(capacity.grow(&report, &made(vello), u64::MAX), Some(Recovery::Repaint));
        let grown = capacity;
        assert_eq!(capacity.grow(&report, &made(vello), u64::MAX), Some(Recovery::Repaint), "its picture is wrong too");
        assert_eq!(capacity, grown);
    }

    #[test]
    fn a_failed_flag_with_no_counter_past_its_size_grows_its_stages_buffers() {
        let vello = BumpCapacity::vello();
        let mut capacity = vello;
        assert_eq!(capacity.grow(&failed(STAGE_COARSE), &made(vello), u64::MAX), Some(Recovery::Repaint));
        assert_eq!((capacity.blend, capacity.ptcl), (vello.blend * 2, vello.ptcl * 2));
        assert_eq!((capacity.segments, capacity.lines), (vello.segments, vello.lines));
    }

    #[test]
    fn a_failure_raises_capacities_below_vellos_own_to_vellos_own() {
        let tiny = BumpCapacity::uniform(64);
        let mut capacity = tiny;
        let report = BumpAllocators { lines: 65, ..failed(STAGE_FLATTEN) };
        assert_eq!(capacity.grow(&report, &made(tiny), u64::MAX), Some(Recovery::Repaint));
        let vello = BumpCapacity::vello();
        assert_eq!(capacity.tiles, vello.tiles, "a later stage that never ran gets Vello's size, not a guess");
        assert_eq!(capacity.lines, vello.lines, "and the one that ran out at least that");
    }

    #[test]
    fn growth_stops_at_what_one_binding_may_hold() {
        // Command-list words are 4 bytes: a binding of 1.5 times the buffer holds 1.5 times its words.
        let vello = BumpCapacity::vello();
        let mut capacity = vello;
        let report = BumpAllocators { ptcl: vello.ptcl + 1, ..failed(STAGE_COARSE) };
        let binding = u64::from(vello.ptcl) * 4 * 3 / 2;
        assert_eq!(capacity.grow(&report, &made(vello), binding), Some(Recovery::Repaint));
        assert_eq!(capacity.ptcl, vello.ptcl * 3 / 2, "capped at the binding's size");
        let capped = capacity;
        let again = BumpAllocators { ptcl: capped.ptcl + 1, ..failed(STAGE_COARSE) };
        assert_eq!(capacity.grow(&again, &made(capped), binding), Some(Recovery::Exhausted), "it can't grow now");
    }

    #[test]
    fn the_graver_recovery_wins() {
        assert_eq!(Some(Recovery::Repaint).max(Some(Recovery::Exhausted)), Some(Recovery::Exhausted));
        assert_eq!(None.max(Some(Recovery::Repaint)), Some(Recovery::Repaint));
    }

    #[test]
    fn the_capacities_reach_the_config_the_stages_check() {
        let layout = Layout { bin_data_start: 10, ..Layout::default() };
        let mut config = RenderConfig::new(&layout, 64, 64, &vello::peniko::Color::TRANSPARENT);
        BumpCapacity::uniform(300).apply(&mut config, &layout);
        assert_eq!(config.gpu.lines_size, 300);
        assert_eq!(config.gpu.binning_size, 300, "the bins come after the draw objects' info");
        assert_eq!(config.buffer_sizes.bin_data.len(), 310);
        assert_eq!((config.gpu.ptcl_size, config.buffer_sizes.ptcl.len()), (300, 300));
        assert_eq!(config.gpu.segments_size, 300);
    }

    #[test]
    fn vellos_own_sizes_are_the_start() {
        let capacity = BumpCapacity::vello();
        assert_eq!((capacity.lines, capacity.ptcl, capacity.binning), (1 << 21, 1 << 23, 1 << 18));
    }
}
