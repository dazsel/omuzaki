//! Proves a life-layer frame allocates nothing (spec §8 "no allocation in the hot loop"): blinks, saccades,
//! glances, laughs, nods, eyes leading a head turn and a hand-off to a person all run in the measured frames.
//!
//! It lives in its own test file, and the allocator counts only allocations made on the measuring thread: the test
//! harness's own threads allocate at unpredictable times, which makes a process-wide count flaky under load.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::life::Life;
use omuzaki_anim_api::{Bus, Channel, ChannelMask, Cues, LifeConfig, LifeLayer, Personality, Source, Tick};

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

/// One frame of a busy minute: a smile that grows into a laugh, talking, a head turn, and a tracker that takes the
/// eyes for a while.
fn frame(tick: Tick) -> (Bus, Cues) {
    let t = tick.time.0;
    let mut bus = Bus::default();
    let laugh = (30.0..34.0).contains(&t);
    let smile = if laugh { 1.0 } else { 0.2 };
    bus.values[Channel::MOUTH_SMILE_LEFT] = smile;
    bus.values[Channel::MOUTH_SMILE_RIGHT] = smile;
    bus.values[Channel::JAW_OPEN] = if laugh { 0.6 } else { 0.1 };
    bus.values[Channel::HEAD_YAW] = if (20.0..40.0).contains(&t) { 0.4 } else { 0.0 };
    if (45.0..55.0).contains(&t) {
        bus.owners.claim(Source::Tracker, ChannelMask::EYES);
    }
    let cues = Cues { loudness: if t >= 10.0 { 0.7 } else { 0.0 }, onset: tick.index.is_multiple_of(14), laugh: 0.0 };
    (bus, cues)
}

#[test]
fn a_frame_of_the_life_layer_allocates_nothing() {
    for personality in [Personality::Natural, Personality::Energetic] {
        let mut life = Life::new(LifeConfig::new(11).with_personality(personality));
        let mut run = |from: u64, to: u64| {
            for i in from..to {
                let tick = Tick::at_rate(i, 60.0);
                let (mut bus, cues) = frame(tick);
                life.step(tick, &cues, &mut bus);
            }
        };
        run(0, 1200); // warm-up: the first blink, glance and saccade, and the first spring steps

        let before = ALLOCATIONS.load(Ordering::Relaxed);
        MEASURING.with(|m| m.set(true));
        run(1200, 3600); // the head turn, the laugh, talking, and the hand-off of the eyes
        MEASURING.with(|m| m.set(false));
        let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
        assert_eq!(allocations, 0, "{personality:?}: 2400 frames of the life layer made {allocations} allocations");
    }
}
