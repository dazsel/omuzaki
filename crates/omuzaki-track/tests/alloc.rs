//! Proves the hot path allocates nothing once the sessions' output buffers have grown: `track` over 300 frames in
//! image mode (detector on every frame) and in video mode (detector, tracking, smoothing and re-detection as the
//! replayed landmarks wander), and `to_track_frame`.
//!
//! The scripted sessions of `omuzaki-ai-mock` allocate nothing after their first run, so the count is the tracker's
//! own. The allocator counts only allocations on the measuring thread (the test harness allocates on its own).

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{
    FACE_ROI, Pose, SIDE, blendshapes_script, canonical_face, detector_script, frame, geometry, graphs, grey_pixels,
    landmarks_script, project,
};
use omuzaki_core::time::Seconds;
use omuzaki_track::{FaceTracker, FaceTrackerConfig, Mode, to_track_frame};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn measure(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.load(Ordering::Relaxed) - before
}

fn allocations_over_300_frames(mode: Mode) -> (usize, usize) {
    let face = project(&canonical_face(), &Pose::facing(0.2, -0.1, 0.05), SIDE, SIDE);
    let graphs = graphs(detector_script(true), landmarks_script(&face, FACE_ROI, 10.0), blendshapes_script());
    let mut tracker =
        FaceTracker::new(graphs, &geometry(), FaceTrackerConfig { mode, ..FaceTrackerConfig::default() }).unwrap();
    let pixels = grey_pixels();
    let mut faces = 0;
    // Warm-up: the output buffers grow once, in whichever order the paths are first taken.
    for i in 0..10_u32 {
        tracker.track(&frame(&pixels), Seconds(f64::from(i) / 30.0)).unwrap();
    }
    let allocations = measure(|| {
        for i in 10..310_u32 {
            let (sample, _timings) = tracker.track(&frame(&pixels), Seconds(f64::from(i) / 30.0)).unwrap();
            if let Some(sample) = &sample {
                faces += 1;
                std::hint::black_box(to_track_frame(Some(sample), u64::from(i), Seconds(f64::from(i) / 30.0)));
            }
        }
    });
    (allocations, faces)
}

#[test]
fn track_allocates_nothing_after_warm_up_in_image_mode() {
    let (allocations, faces) = allocations_over_300_frames(Mode::Image);
    assert_eq!(faces, 300, "every frame found the scripted face");
    assert_eq!(allocations, 0, "FaceTracker::track allocated");
}

#[test]
fn track_allocates_nothing_after_warm_up_in_video_mode() {
    let (allocations, faces) = allocations_over_300_frames(Mode::Video);
    assert!(faces > 0, "the scripted face was tracked at least some of the time");
    assert_eq!(allocations, 0, "FaceTracker::track allocated");
}
