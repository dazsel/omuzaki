//! Everything the audio thread calls in this crate must not allocate. A counting global allocator, as in the speech
//! host's `tests/no_alloc.rs`, counts the allocations of the thread that asks.

// A global allocator is `unsafe` by definition; this wrapper only counts and forwards to the system allocator.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use omuzaki_audio_api::clock::{ClockSnapshot, EngineClock};
use omuzaki_audio_api::node::{BlockMut, NodeReport};
use omuzaki_audio_api::report::{MeterCell, Meters};
use omuzaki_audio_api::ring::{TapFlags, ring};
use omuzaki_audio_api::semantics::{DuckEnvelope, FaderRamp, LevelFollower, db_to_gain, duck_factor};
use omuzaki_audio_api::tap::DropPolicy;

thread_local! {
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: forwards every call unchanged to `System`, which upholds the `GlobalAlloc` contract; the counter is a
// const-initialised thread-local `Cell`, which neither allocates nor panics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The allocations `f` makes on this thread.
fn allocations(f: impl FnOnce()) -> usize {
    let before = COUNT.with(Cell::get);
    f();
    COUNT.with(Cell::get) - before
}

#[test]
fn the_counter_sees_allocations() {
    assert!(allocations(|| drop(std::hint::black_box(vec![1u8; 64]))) >= 1);
}

#[test]
fn ring_push_and_read_do_not_allocate_in_either_policy() {
    for policy in [DropPolicy::DropNewest, DropPolicy::Overwrite] {
        let (mut tx, mut rx) = ring(1024, policy, 0);
        let block = [0.5f32; 256];
        let mut out = [0.0f32; 256];
        // Fill past capacity so refusal, overwrite, gaps and flag changes are all exercised.
        let n = allocations(|| {
            for round in 0..20 {
                tx.set_flags(TapFlags { muted: round % 3 == 0, private: round % 5 == 0 });
                tx.push(&block);
                tx.push_gap(7);
                if round % 2 == 0 {
                    rx.read(&mut out);
                }
            }
            let _ = (tx.free(), tx.written(), tx.dropped(), rx.available(), rx.pop_into(&mut out));
        });
        assert_eq!(n, 0, "{policy:?}");
    }
}

#[test]
fn the_fader_ramp_and_gain_conversion_do_not_allocate() {
    let mut buf = [1.0f32; 512];
    let n = allocations(|| {
        let mut ramp = FaderRamp::new(1.0);
        ramp.set_target(db_to_gain(-6.0));
        ramp.apply(&mut buf);
        ramp.set_target(0.0);
        ramp.apply(&mut buf);
    });
    assert_eq!(n, 0);
}

#[test]
fn ducking_does_not_allocate() {
    let n = allocations(|| {
        let mut follower = LevelFollower::new();
        let mut envelope = DuckEnvelope::new(12.0, 10.0, 100.0, 300.0);
        let mut sum = 0.0;
        for i in 0..4096u32 {
            let level = follower.step(if i < 2000 { 0.5 } else { 0.0 });
            let g = envelope.step(level > 0.1);
            sum += duck_factor([g, -3.0]);
        }
        envelope.set_params(6.0, 5.0, 0.0, 100.0);
        envelope.retire();
        std::hint::black_box(sum);
    });
    assert_eq!(n, 0);
}

#[test]
fn clock_meters_and_blocks_do_not_allocate() {
    let clock = EngineClock::new(ClockSnapshot::nominal(48_000));
    let meters = MeterCell::new();
    let (mut l, mut r, side) = ([0.0f32; 64], [0.0f32; 64], [0.0f32; 64]);
    let n = allocations(|| {
        clock.publish(ClockSnapshot {
            sample: 64,
            time: omuzaki_core::time::Seconds(0.001),
            seconds_per_sample: 1.0 / 48_000.0,
        });
        let _ = (clock.read(), clock.time_of(100), clock.sample_at(omuzaki_core::time::Seconds(1.0)));
        meters.store(Meters { peak_db: -6.0, ..Meters::SILENT });
        let _ = meters.load();
        let mut block = BlockMut::stereo(&mut l, &mut r, 0).with_sidechain(&side);
        block.for_each_channel(|c| c.fill(0.25));
        let _ = (block.frames(), block.channel_count(), NodeReport::default());
    });
    assert_eq!(n, 0);
}

#[test]
fn a_feed_pull_with_its_playhead_and_close_fade_does_not_allocate() {
    use omuzaki_audio_api::feed::feed_channel;
    for rate in [48_000, 30_238, 24_000] {
        let (producer, consumer) = ring(96_000, DropPolicy::DropNewest, 0);
        let (mut writer, mut source) = feed_channel(producer, consumer, rate, false);
        let closer = writer.closer();
        writer.push(&vec![0.5; 20_000]);
        let (mut left, mut right) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
        let n = allocations(|| {
            for block in 0..40u64 {
                if block == 30 {
                    closer.close();
                }
                source.pull(&mut left, &mut right, 256, block * 256);
            }
        });
        assert_eq!(n, 0, "{rate} Hz");
        assert!(source.is_cut());
    }
}
