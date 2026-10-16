//! COM is entered once per capture thread, not once per frame (#1878). Windows only: elsewhere this file is empty.
#![cfg(windows)]

use omuzaki_camera_mf::reader::bench_support::{Harness, com_inits};
use omuzaki_track_api::camera::PixelFormat;

/// A thread with no apartment of its own runs `CoInitializeEx` exactly once over 1,000 frames of the path
/// `next_frame` takes, so the apartment is not set up and torn down around every frame.
#[test]
fn a_thousand_frames_initialise_com_once() {
    std::thread::spawn(|| {
        assert_eq!(com_inits(), 0, "a fresh thread starts outside COM");
        let mut harness = Harness::new(PixelFormat::Nv12, 64, 48, 64).expect("the harness builds");
        let frame_len = PixelFormat::Nv12.frame_len(48, 64).expect("NV12 has a length");
        for _ in 0..1000 {
            assert_eq!(harness.next_frame().expect("a frame"), frame_len);
        }
        assert_eq!(com_inits(), 1, "CoInitializeEx ran more than once");
    })
    .join()
    .expect("the thread finished");
}

/// Each thread enters COM for itself: a second thread's count is its own.
#[test]
fn every_thread_enters_com_once() {
    let counts: Vec<u32> = (0..2)
        .map(|_| {
            std::thread::spawn(|| {
                let mut harness = Harness::new(PixelFormat::Yuy2, 32, 16, 64).expect("the harness builds");
                for _ in 0..10 {
                    harness.next_frame().expect("a frame");
                }
                com_inits()
            })
            .join()
            .expect("the thread finished")
        })
        .collect();
    assert_eq!(counts, [1, 1]);
}
