//! Vello's bump-allocated buffers: how big they are, and the check that tells, a frame or two later, whether a repaint
//! needed more than they had.
//!
//! Vello's stages allocate lines, bins, tiles, segments, blend spill and per-tile command lists by bumping counters in
//! one small buffer, against the capacities the config names. A scene that needs more sets `failed` and draws nothing,
//! so the engine copies the counters out after every repaint into a buffer of the layer's own, maps it without waiting
//! (`map_async`, read on a later paint once the GPU has run it), and on a failure grows what ran out and repaints the
//! layer whole. Nothing on the frame path waits on the GPU.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use vello_encoding::{BufferSize, BufferSizes, BumpAllocators, Layout, RenderConfig, WorkgroupCounts};

/// Vello's `failed` flags (`shader/shared/bump.wgsl`): which stage ran out.
const STAGE_BINNING: u32 = 0x1;
const STAGE_TILE_ALLOC: u32 = 0x2;
const STAGE_FLATTEN: u32 = 0x4;
const STAGE_PATH_COUNT: u32 = 0x8;
const STAGE_COARSE: u32 = 0x10;

/// The entries of each bump-allocated buffer. The bin data buffer also holds the draw objects' info in front of the bins;
/// `binning` counts the bins alone.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct Capacity {
    lines: u32,
    binning: u32,
    tiles: u32,
    seg_counts: u32,
    segments: u32,
    blend: u32,
    ptcl: u32,
}

/// What a grow found.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Grow {
    /// Larger buffers are set; the next repaint fits (or fails again and grows again).
    Grew,
    /// A buffer is already as large as the device allows: the GPU can't draw this scene.
    Exhausted,
}

impl Capacity {
    /// Vello's own sizes (`BufferSizes::new`'s fixed ones), the engine's start.
    pub(super) fn vello() -> Self {
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

    /// Every buffer at `entries` entries: for tests that check the overflow path.
    pub(super) fn uniform(entries: u32) -> Self {
        let n = entries.max(1);
        Self { lines: n, binning: n, tiles: n, seg_counts: n, segments: n, blend: n, ptcl: n }
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

    /// Grows what `bump`, a failed repaint's counters, says ran out: each buffer whose stage failed or whose counter
    /// passed its size doubles, at least to the counter, at most to what one binding may hold (`max_binding` bytes).
    /// A stage that fails stops the ones after it, so their counters are short: every buffer is also raised to at least
    /// Vello's own size (which only a capacity set below it, a test's, is under), and a later stage that still fails
    /// grows on its own failure.
    pub(super) fn grow(&mut self, bump: &BumpAllocators, max_binding: u64) -> Grow {
        // Bytes per entry, and Vello's own sizes.
        let sizes = BufferSizes::new(&Layout::default(), &WorkgroupCounts::default());
        let vello = Self::vello();
        let failed = |stage| bump.failed & stage != 0;
        let (mut grown, mut at_limit) = (false, false);
        let mut grow = |entries: &mut u32, used: u32, failed: bool, floor: u32, entry_bytes: u64| {
            let limit = u32::try_from(max_binding / entry_bytes.max(1)).unwrap_or(u32::MAX);
            let mut wanted = (*entries).max(floor);
            if failed || used > *entries {
                wanted = wanted.max(entries.saturating_mul(2)).max(used.saturating_add(1).next_power_of_two());
            }
            let wanted = wanted.min(limit);
            if wanted > *entries {
                *entries = wanted;
                grown = true;
            } else if failed || used > *entries {
                at_limit = true;
            }
        };
        grow(&mut self.lines, bump.lines, failed(STAGE_FLATTEN), vello.lines, entry_bytes(sizes.lines));
        grow(&mut self.binning, bump.binning, failed(STAGE_BINNING), vello.binning, entry_bytes(sizes.bin_data));
        grow(&mut self.tiles, bump.tile, failed(STAGE_TILE_ALLOC), vello.tiles, entry_bytes(sizes.tiles));
        let path_count = failed(STAGE_PATH_COUNT);
        grow(&mut self.seg_counts, bump.seg_counts, path_count, vello.seg_counts, entry_bytes(sizes.seg_counts));
        let coarse = failed(STAGE_COARSE);
        grow(&mut self.segments, bump.segments, coarse, vello.segments, entry_bytes(sizes.segments));
        grow(&mut self.blend, bump.blend, coarse, vello.blend, entry_bytes(sizes.blend_spill));
        grow(&mut self.ptcl, bump.ptcl, coarse, vello.ptcl, entry_bytes(sizes.ptcl));
        if grown || !at_limit { Grow::Grew } else { Grow::Exhausted }
    }
}

/// The bytes of one entry of a buffer like `size`.
fn entry_bytes<T>(_: BufferSize<T>) -> u64 {
    std::mem::size_of::<T>() as u64
}

/// Where a counters read-back is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum State {
    /// Free for the next repaint.
    Idle,
    /// A repaint's commands copy the counters in; mapping waits until they are submitted, at the next paint.
    Recorded,
    /// Mapping asked for; `ready` says when it is done.
    Mapping,
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

/// How many repaints of a layer can be in flight before one goes unchecked: the GPU runs a frame or two behind.
const SLOTS: usize = 3;

/// A layer's read-backs of Vello's counters.
pub(super) struct BumpReads {
    slots: [Slot; SLOTS],
}

/// The bytes of the counters.
const BYTES: u64 = std::mem::size_of::<BumpAllocators>() as u64;

impl BumpReads {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let slot = || Slot {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("face-vello-bump-read"),
                size: BYTES,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            state: State::Idle,
            ready: Arc::new(AtomicU8::new(PENDING)),
        };
        Self { slots: [slot(), slot(), slot()] }
    }

    /// Moves the read-backs along and returns the counters of a repaint that failed, if one came back. Asks for the
    /// mapping of the read-backs recorded by earlier paints (each was submitted since: the renderer submits a paint as
    /// soon as the painter returns) and reads those that are mapped. Never waits.
    pub(super) fn poll(&mut self) -> Option<BumpAllocators> {
        let mut failed = None;
        for slot in &mut self.slots {
            match slot.state {
                State::Idle => {}
                State::Recorded => {
                    slot.ready.store(PENDING, Ordering::Release);
                    let ready = Arc::clone(&slot.ready);
                    slot.buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
                        ready.store(if result.is_ok() { MAPPED } else { UNMAPPABLE }, Ordering::Release);
                    });
                    slot.state = State::Mapping;
                }
                State::Mapping => match slot.ready.load(Ordering::Acquire) {
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
                        if let Some(bump) = bump.filter(|bump| bump.failed != 0) {
                            failed = Some(bump);
                        }
                    }
                    _ => slot.state = State::Idle,
                },
            }
        }
        failed
    }

    /// Records the copy of `bump`, the counters this repaint's stages just bumped, into a free read-back. With none free
    /// (the GPU is more than [`SLOTS`] repaints behind) this repaint goes unchecked: a scene too big for the buffers
    /// fails on the next one too, which is checked.
    pub(super) fn record(&mut self, encoder: &mut wgpu::CommandEncoder, bump: &wgpu::Buffer) {
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.state == State::Idle) {
            encoder.copy_buffer_to_buffer(bump, 0, &slot.buffer, 0, BYTES);
            slot.state = State::Recorded;
        }
    }

    /// Forgets the copy the last paint recorded: the renderer dropped its commands, so it never ran.
    pub(super) fn forget_recorded(&mut self) {
        for slot in &mut self.slots {
            if slot.state == State::Recorded {
                slot.state = State::Idle;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bump(failed: u32) -> BumpAllocators {
        BumpAllocators { failed, ..BumpAllocators::default() }
    }

    #[test]
    fn a_failed_stage_doubles_its_buffers_and_a_counter_past_its_size_reaches_it() {
        let mut capacity = Capacity::vello();
        let report = BumpAllocators { lines: (1 << 22) + 5, ..bump(STAGE_FLATTEN) };
        assert_eq!(capacity.grow(&report, u64::MAX), Grow::Grew);
        assert_eq!(capacity.lines, 1 << 23, "at least the counter, rounded to a power of two");
        assert_eq!(capacity.tiles, Capacity::vello().tiles, "a stage that did not fail keeps its size");
        assert_eq!(capacity.grow(&bump(STAGE_COARSE), u64::MAX), Grow::Grew);
        let vello = Capacity::vello();
        assert_eq!((capacity.ptcl, capacity.segments), (vello.ptcl * 2, vello.segments * 2), "coarse's buffers double");
    }

    #[test]
    fn a_failure_raises_capacities_below_vellos_own_to_vellos_own() {
        let mut capacity = Capacity::uniform(64);
        assert_eq!(capacity.grow(&bump(STAGE_FLATTEN), u64::MAX), Grow::Grew);
        let vello = Capacity::vello();
        assert_eq!(capacity.tiles, vello.tiles, "a later stage that never ran gets Vello's size, not a guess");
        assert_eq!(capacity.lines, vello.lines, "and the failed one at least that");
    }

    #[test]
    fn growth_stops_at_what_one_binding_may_hold() {
        // 400 bytes: 100 command-list or blend entries (4 bytes each), fewer segments than there are now.
        let mut capacity = Capacity::vello();
        let ptcl = capacity.ptcl;
        assert_eq!(capacity.grow(&bump(STAGE_COARSE), u64::from(ptcl) * 4 * 3 / 2), Grow::Grew);
        assert_eq!(capacity.ptcl, ptcl * 3 / 2, "capped at the binding's size");
        assert_eq!(capacity.grow(&bump(STAGE_COARSE), 400), Grow::Exhausted, "nothing of coarse's can grow now");
    }

    #[test]
    fn the_capacities_reach_the_config_the_stages_check() {
        let layout = Layout { bin_data_start: 10, ..Layout::default() };
        let mut config = RenderConfig::new(&layout, 64, 64, &vello::peniko::Color::TRANSPARENT);
        Capacity::uniform(300).apply(&mut config, &layout);
        assert_eq!(config.gpu.lines_size, 300);
        assert_eq!(config.gpu.binning_size, 300, "the bins come after the draw objects' info");
        assert_eq!(config.buffer_sizes.bin_data.len(), 310);
        assert_eq!((config.gpu.ptcl_size, config.buffer_sizes.ptcl.len()), (300, 300));
    }

    #[test]
    fn vellos_own_sizes_are_the_start() {
        let capacity = Capacity::vello();
        assert_eq!((capacity.lines, capacity.ptcl, capacity.binning), (1 << 21, 1 << 23, 1 << 18));
    }
}
