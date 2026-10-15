//! The publish and read paths. Targets (research note §4): `frame_publish` at most 100 ns; `read_uncontended` and
//! `read_while_writing` (a writer at 1 kHz) just have to run, and report their time per read.

use std::hint::black_box;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::time::Seconds;
use omuzaki_metrics_api::{FrameMetrics, metrics};

fn frame() -> FrameMetrics {
    let mut m = FrameMetrics::new(Seconds(12.5));
    m.fps = 60.0;
    m.frame_ms = 16.6;
    m.gpu_ms = Some(1.2);
    m.track_to_submit_ms = Some(40.0);
    m
}

fn bench(c: &mut Criterion) {
    let (mut writers, reader) = metrics();
    let m = frame();
    c.bench_function("frame_publish", |b| b.iter(|| writers.frame.publish(black_box(&m))));
    c.bench_function("read_uncontended", |b| b.iter(|| black_box(reader.read())));

    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let mut frame_writer = writers.frame;
        let stop = &stop;
        scope.spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                frame_writer.publish(&frame());
                thread::sleep(Duration::from_millis(1));
            }
        });
        c.bench_function("read_while_writing", |b| b.iter(|| black_box(reader.read())));
        stop.store(true, Ordering::Relaxed);
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
