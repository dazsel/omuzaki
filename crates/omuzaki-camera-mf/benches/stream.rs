//! The per-frame cost of `next_frame`'s path on Windows, with no camera: a synthetic 1280x720 sample goes through the
//! callback hand-off (a token through the lock-free slot, waking the kernel event), the buffer choice, the lock, the
//! bounds check against the allocation, and, for a bottom-up BGRA image, the top-down copy.
//!
//! The device itself, `ReadSample` and the worker thread are not in the numbers: they need a camera. Everything else
//! `next_frame` does per frame is. Elsewhere this bench is empty.

#[cfg(windows)]
mod windows_bench {
    use std::hint::black_box;

    use criterion::{Criterion, criterion_group};
    use omuzaki_camera_mf::reader::bench_support::Harness;
    use omuzaki_track_api::camera::PixelFormat;

    const WIDTH: u32 = 1280;
    const HEIGHT: u32 = 720;

    fn stream(c: &mut Criterion) {
        let mut group = c.benchmark_group("camera_mf_frame_path");
        let tight = |format: PixelFormat| isize::try_from(format.tight_stride(WIDTH).unwrap_or(0)).unwrap_or(0);

        // The hand-off alone, which does not depend on the layout.
        if let Ok(harness) = Harness::new(PixelFormat::Nv12, WIDTH, HEIGHT, tight(PixelFormat::Nv12)) {
            group.bench_function("hand_off", |b| b.iter(|| black_box(harness.hand_off())));
        }
        // Lock and bounds check, borrowing the buffer.
        for (name, format) in [("lock_nv12", PixelFormat::Nv12), ("lock_yuy2", PixelFormat::Yuy2)] {
            if let Ok(mut harness) = Harness::new(format, WIDTH, HEIGHT, tight(format)) {
                group.bench_function(name, |b| b.iter(|| black_box(harness.lock_and_read())));
            }
        }
        // A bottom-up BGRA image, which costs a full top-down copy.
        let bottom_up = -tight(PixelFormat::Bgra8);
        if let Ok(mut harness) = Harness::new(PixelFormat::Bgra8, WIDTH, HEIGHT, bottom_up) {
            group.bench_function("lock_and_flip_bgra", |b| b.iter(|| black_box(harness.lock_and_read())));
        }
        group.finish();
    }

    criterion_group!(benches, stream);
}

#[cfg(windows)]
criterion::criterion_main!(windows_bench::benches);

#[cfg(not(windows))]
fn main() {}
