#![allow(unsafe_code)] // a counting global allocator is unsafe by definition; it only forwards to `System`

//! The per-frame seams allocate nothing once the stream is open: a counting allocator over the hand-off, the
//! top-down copy (across a renegotiation to a larger mode), the capture clock and the sequence.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use omuzaki_camera_mf::modes::{
    Capacity, CaptureClock, ModeInfo, RowFlip, SeqTracker, Signal, Take, TokenSlot, take_after_retype,
};
use omuzaki_track_api::camera::PixelFormat;

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // The allocator is global, so it would also count the harness and runtime threads. Only the test's own thread
    // counts, and the flag is `const`-initialised so reading it inside `alloc` cannot itself allocate.
    static COUNTING_THIS_THREAD: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: forwards every call to the system allocator, only counting allocations on the way.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING_THIS_THREAD.with(Cell::get) {
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        // SAFETY: the caller's layout, passed through unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// A signal that never blocks, so the test needs no synchronisation primitive that could allocate.
struct Quiet;

impl Signal for Quiet {
    fn notify(&self) {}
    fn wait(&self, _timeout: Duration) {}
}

fn bgra(width: u32, height: u32) -> ModeInfo {
    ModeInfo { format: PixelFormat::Bgra8, width, height, fps_num: 30, fps_den: 1 }
}

#[test]
fn nothing_allocates_after_open_even_across_a_renegotiation() {
    // Open: size everything for the largest offered mode.
    let small = bgra(64, 48);
    let large = bgra(128, 96);
    let capacity = Capacity::for_modes(&[small, large]);
    let mut flip = RowFlip::new(capacity.flip);
    let slot = TokenSlot::new(Quiet);
    let mut clock = CaptureClock::new();
    let mut seq = SeqTracker::new(30, 1);
    let source = vec![7_u8; 4 * 128 * 96];
    let pointee = 0_u8;
    let token = NonNull::from(&pointee).cast::<c_void>();

    COUNTING_THIS_THREAD.with(|counting| counting.set(true));
    let before = ALLOCATIONS.load(Ordering::SeqCst);
    for round in 0..1000_i64 {
        // The camera renegotiates between the two modes: only the length in use changes.
        let mode = if round % 100 < 50 { small } else { large };
        capacity.admits(&mode).unwrap();
        flip.set_len(Capacity::flip_len(&mode)).unwrap();
        let (row, rows) = (4 * mode.width as usize, mode.height as usize);
        let out = flip.fill(&source[..row * rows], -isize::try_from(row).unwrap(), row, rows).unwrap();
        assert_eq!(out.len(), row * rows);

        slot.put(token, |_| {});
        if round % 100 == 0 {
            slot.set_retype();
        }
        let got = take_after_retype(&slot, Duration::ZERO, |_| -> Result<(), ()> { Ok(()) });
        assert!(matches!(got, Ok(Take::Item(_) | Take::Timeout)));
        clock.stamp(Some(u64::try_from(round).unwrap() * 333_333), round * 333_333);
        seq.next(round * 333_333, false);
    }
    COUNTING_THIS_THREAD.with(|counting| counting.set(false));
    assert_eq!(ALLOCATIONS.load(Ordering::SeqCst) - before, 0, "the per-frame path allocated");
}
