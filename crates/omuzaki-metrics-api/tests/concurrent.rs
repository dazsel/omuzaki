//! A reader racing a real writer thread: never torn, never backwards, and a writer at 1 kHz never starves a read.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use omuzaki_core::time::Seconds;
use omuzaki_metrics_api::{FrameMetrics, metrics};

/// A section whose fields all derive from `k`, so a mix of two publishes is visible.
fn frame(k: u32) -> FrameMetrics {
    let mut m = FrameMetrics::new(Seconds(f64::from(k)));
    m.late_frames = u64::from(k);
    m.triangles = u64::from(k) * 2;
    m.quality_changes = u64::from(k) * 3;
    m.fps = k as f32;
    m
}

fn assert_consistent(m: &FrameMetrics) -> u64 {
    let k = m.late_frames;
    assert_eq!(m.triangles, k * 2, "torn read");
    assert_eq!(m.quality_changes, k * 3, "torn read");
    assert_eq!(m.at, Seconds(k as f64), "torn read");
    k
}

#[test]
fn a_flat_out_writer_never_shows_a_torn_or_backwards_read() {
    let (mut w, reader) = metrics();
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let stop = &stop;
        scope.spawn(move || {
            let mut k = 1;
            while !stop.load(Ordering::Relaxed) {
                w.frame.publish(&frame(k));
                k += 1;
            }
        });
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let reader = reader.clone();
                scope.spawn(move || {
                    let (mut last, mut seen) = (0, 0u32);
                    let deadline = Instant::now() + Duration::from_millis(300);
                    while Instant::now() < deadline {
                        // A read that lost every retry is `None`; any `Some` must be whole and not older.
                        if let Some(m) = reader.read().frame {
                            let k = assert_consistent(&m);
                            assert!(k >= last, "went backwards: {last} then {k}");
                            (last, seen) = (k, seen + 1);
                        }
                    }
                    seen
                })
            })
            .collect();
        for r in readers {
            assert!(r.join().expect("reader thread") > 0, "a reader never got a value");
        }
        stop.store(true, Ordering::Relaxed);
    });
}

#[test]
fn a_writer_at_1_khz_never_exhausts_a_reads_retries() {
    let (mut w, reader) = metrics();
    w.frame.publish(&frame(1));
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let stop = &stop;
        scope.spawn(move || {
            let mut k = 2;
            while !stop.load(Ordering::Relaxed) {
                w.frame.publish(&frame(k));
                k += 1;
                thread::sleep(Duration::from_millis(1));
            }
        });
        let mut last = 0;
        for i in 0..10_000 {
            let m = reader.read().frame.unwrap_or_else(|| panic!("read {i} lost every retry"));
            let k = assert_consistent(&m);
            assert!(k >= last, "went backwards: {last} then {k}");
            last = k;
            thread::sleep(Duration::from_micros(20));
        }
        stop.store(true, Ordering::Relaxed);
    });
}
