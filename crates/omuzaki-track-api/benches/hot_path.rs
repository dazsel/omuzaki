//! The per-frame cost of the contract's hot path (design §4, §9): `ClockMap::map` with its 64-pair fit full, and the
//! mailbox's `publish` and `poll`, alone and with the producer and the consumer on two threads at once, so the
//! cache-line traffic between the tracker thread and the stage thread is in the numbers.

#![allow(clippy::cast_precision_loss)]

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::{Channel, DeviceProfile, FrameSource, HeadPose, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_track_api::camera::DeviceTime;
use omuzaki_track_api::clock::{CLOCK_WINDOW, ClockMap};
use omuzaki_track_api::mailbox::mailbox;

/// A frame with every part present, so encoding and decoding do all their work.
fn full_frame() -> TrackFrame {
    let mut frame = TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Webcam);
    frame.head = Some(HeadPose::default());
    for (i, channel) in [Channel::JAW_OPEN, Channel::EYE_BLINK_LEFT, Channel::EYE_BLINK_RIGHT].into_iter().enumerate() {
        frame.set(channel, 0.25 * (i + 1) as f32);
    }
    frame
}

/// 60 fps on a 200 ppm fast device clock, with 0 to 4.9 ms of arrival jitter.
fn pair(i: u64) -> (DeviceTime, Seconds) {
    let t = i as f64 / 60.0;
    let jitter = ((i * 7919) % 50) as f64 * 1e-4;
    (DeviceTime(5_000_000_000 + i * 16_669_000), Seconds(t + 0.01 + jitter))
}

fn clock(c: &mut Criterion) {
    let mut map = ClockMap::new();
    let mut i = 0_u64;
    for _ in 0..2 * CLOCK_WINDOW {
        let (captured, arrived) = pair(i);
        let _ = map.map(captured, arrived);
        i += 1;
    }
    c.bench_function("track/clock_map_full_window", |b| {
        b.iter(|| {
            let (captured, arrived) = pair(i);
            i += 1;
            black_box(map.map(black_box(captured), black_box(arrived)))
        });
    });
}

fn mailbox_one_thread(c: &mut Criterion) {
    let frame = full_frame();
    let (mut tx, mut rx) = mailbox();
    c.bench_function("track/mailbox_publish", |b| b.iter(|| tx.publish(black_box(&frame))));
    c.bench_function("track/mailbox_publish_then_poll", |b| {
        b.iter(|| {
            tx.publish(black_box(&frame));
            black_box(rx.poll(Seconds(0.0)))
        });
    });
    c.bench_function("track/mailbox_poll_empty", |b| b.iter(|| black_box(rx.poll(Seconds(0.0)))));
}

/// Runs `other` in a loop on a second thread for as long as `measure` runs on this one.
fn overlapped<R>(other: impl FnMut() + Send + 'static, measure: impl FnOnce() -> R) {
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = Arc::clone(&stop);
        let mut other = other;
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                other();
            }
        })
    };
    let _ = measure();
    stop.store(true, Ordering::Relaxed);
    let _ = worker.join();
}

fn mailbox_two_threads(c: &mut Criterion) {
    let frame = full_frame();

    // The stage thread's poll while the tracker thread publishes as fast as it can.
    let (mut tx, mut rx) = mailbox();
    overlapped(
        move || tx.publish(&frame),
        || c.bench_function("track/mailbox_poll_vs_publisher", |b| b.iter(|| black_box(rx.poll(Seconds(0.0))))),
    );

    // The tracker thread's publish while the stage thread polls as fast as it can.
    let (mut tx, mut rx) = mailbox();
    overlapped(
        move || {
            black_box(rx.poll(Seconds(0.0)));
        },
        || c.bench_function("track/mailbox_publish_vs_poller", |b| b.iter(|| tx.publish(black_box(&frame)))),
    );
}

criterion_group!(benches, clock, mailbox_one_thread, mailbox_two_threads);
criterion_main!(benches);
