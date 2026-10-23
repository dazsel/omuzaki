//! Proves the whole mock pipeline allocates nothing per frame: replay poll, retarget, score sampling, blending, life,
//! sanitising and the animator (spec §8 "no allocation in the hot loop"). The real implementations are held to the same
//! rule and can reuse this test with their own types.
//!
//! It lives in its own test file, and the allocator counts only allocations made on the measuring thread: the test
//! harness's own threads allocate at unpredictable times, which makes a process-wide count flaky under load.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{ChannelBuf, Cues, DeviceProfile, FrameSource, LifeConfig, Tick};
use omuzaki_anim_mock::fixtures::demo_score;
use omuzaki_anim_mock::performer::MockPerformer;
use omuzaki_anim_mock::retarget::MockRetarget;
use omuzaki_anim_mock::track::{SyntheticSpec, TrackReplay, synthetic_track};
use omuzaki_core::time::Seconds;

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

#[test]
fn a_frame_of_the_mock_pipeline_allocates_nothing() {
    let spec = SyntheticSpec { seconds: 30.0, lost: Some((Seconds(10.0), Seconds(14.0))), ..SyntheticSpec::default() };
    let track = synthetic_track(&spec);
    let frames = track.frames.len() as u64;
    let mut replay = TrackReplay::new(track).looping();
    let mut performer =
        MockPerformer::new(MockRetarget::new(DeviceProfile::Webcam), LifeConfig::new(9), ChannelBuf::NEUTRAL);
    performer.play(demo_score().unwrap().compile().unwrap(), Seconds(0.0));

    let mut run = |from: u64, to: u64| {
        for i in from..to {
            let tick = Tick::at_rate(i, 30.0);
            let frame = replay.poll(tick.time);
            let _ = performer.step(
                tick,
                frame.as_ref(),
                &Cues { loudness: 0.5, onset: i % 20 == 0, laugh: 0.0, ..Cues::default() },
            );
        }
    };
    run(0, frames / 2); // warm-up: covers tracking, the lost span, the score fade and the loop wrap

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    run(frames / 2, 3 * frames);
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(
        allocations,
        0,
        "{} frames of the mock pipeline made {allocations} allocations",
        3 * frames - frames / 2
    );
}
