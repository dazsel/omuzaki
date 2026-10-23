//! The tracker section of the live metrics (#1255): a 10 Hz tick copies `Tracker::latency` into the section, the p95
//! and the drops show there, and the tick allocates nothing after warm-up.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use omuzaki_anim_api::{DeviceProfile, TrackFrame};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_metrics_api::metrics;
use omuzaki_track::metrics::{TICK, TrackerTick, TrackerTicker};
use omuzaki_track_api::tracker::{LatencyStats, Tracker, TrackerConfig};
use omuzaki_track_mock::{FakeCamera, FakeTracker};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
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

fn tracked(seq: u64) -> TrackFrame {
    TrackFrame::empty(seq, Seconds(0.0), DeviceProfile::Webcam)
}

fn slow_tracker() -> FakeTracker {
    let script = (0..30).map(|i| (Duration::from_millis(33 * i), tracked(i))).collect();
    let ms = Duration::from_micros;
    FakeTracker::scripted(script).with_latency(LatencyStats::new(0, 7, ms(3_100), ms(6_400), ms(9_000)))
}

#[test]
fn the_p95_and_the_drops_appear_in_the_tracker_section() {
    let (writers, reader) = metrics();
    let mut tracker = slow_tracker();
    let mut source = tracker.start(&FakeCamera::id(), &TrackerConfig::default()).unwrap();
    let mut tick = TrackerTick::new(writers.tracker);
    let mut published = 0;
    // A control loop at 60 Hz for one second: the tick publishes ten times.
    for k in 0..60 {
        let now = Seconds(f64::from(k) / 60.0);
        let _ = source.poll(now);
        published += usize::from(tick.tick(now, &tracker));
    }
    assert_eq!(published, 10, "10 Hz");
    let section = reader.read().tracker.unwrap();
    assert!((section.track_ms_p50 - 3.1).abs() < 1e-4, "{section:?}");
    assert!((section.track_ms_p95 - 6.4).abs() < 1e-4, "{section:?}");
    assert!((section.track_ms_max - 9.0).abs() < 1e-4, "{section:?}");
    assert_eq!(section.tracker_dropped, 7);
    assert!(section.tracker_frames >= 27, "{section:?}");
    assert!((section.at.0 - 0.9).abs() < 1e-9, "the last publish was at 0.9 s: {:?}", section.at);
    // The p95 over the 5 ms `track.face` budget shows in the reading's budget rows.
    let budget = reader.read().budget;
    assert!(budget.iter().any(|row| format!("{row:?}").contains("TrackFace")), "{budget:?}");
}

#[test]
fn the_tick_allocates_nothing_after_warm_up() {
    let (writers, _reader) = metrics();
    let tracker = slow_tracker();
    let mut tick = TrackerTick::new(writers.tracker);
    tick.tick(Seconds(0.0), &tracker);
    MEASURING.with(|m| m.set(true));
    let mut published = 0;
    for k in 1..=1_000 {
        published += usize::from(tick.tick(Seconds(f64::from(k) * 0.01), &tracker));
    }
    MEASURING.with(|m| m.set(false));
    assert_eq!(published, 100);
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}

#[test]
fn the_ticker_thread_publishes_what_its_owner_reads() {
    let (writers, reader) = metrics();
    let clock = Arc::new(FakeClock::new(Seconds(4.0)));
    let (tx, rx) = std::sync::mpsc::channel();
    let ms = Duration::from_millis;
    let ticker = TrackerTicker::start(writers.tracker, clock, move || {
        let _ = tx.send(());
        Some(LatencyStats::new(120, 3, ms(2), ms(4), ms(5)))
    })
    .unwrap();
    // Two reads: the first publish happened before the second read began.
    rx.recv_timeout(TICK * 50).unwrap();
    rx.recv_timeout(TICK * 50).unwrap();
    let section = reader.read().tracker.unwrap();
    assert_eq!((section.tracker_frames, section.tracker_dropped), (120, 3));
    assert!((section.track_ms_p95 - 4.0).abs() < 1e-4);
    assert_eq!(section.at, Seconds(4.0));
    assert!(ticker.stop().is_some());
}
