//! The virtual camera against a memory sink: what reaches the device, and that it shows the newest frame.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream_api::encoder::Frame;
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::output::{AlphaNeed, Output, OutputConfig, OutputState, find_output_kind};
use omuzaki_stream_api::time::Ticks;
use omuzaki_vcam::{FrameSink, Vcam, yuv};

/// Sends every frame written to the test, after waiting for the test's go-ahead, so a test controls the writer's
/// pace with a handshake rather than a sleep.
struct Channel {
    frames: Sender<Vec<u8>>,
    go: Receiver<()>,
}

impl FrameSink for Channel {
    fn write(&mut self, yuyv: &[u8]) -> Result<(), StreamError> {
        self.go.recv().map_err(|_| StreamError::Stopped)?;
        self.frames.send(yuyv.to_vec()).map_err(|_| StreamError::Stopped)
    }
}

fn camera() -> (Vcam, Receiver<Vec<u8>>, Sender<()>) {
    let (frames_tx, frames_rx) = channel();
    let (go_tx, go_rx) = channel();
    let slot = std::sync::Mutex::new(Some(Channel { frames: frames_tx, go: go_rx }));
    let vcam = Vcam::with_opener(Box::new(move |_cfg: &OutputConfig| {
        let sink = slot.lock().unwrap().take().ok_or(StreamError::Busy)?;
        Ok(Box::new(sink) as Box<dyn FrameSink>)
    }));
    (vcam, frames_rx, go_tx)
}

fn flat(size: UVec2, rgb: [u8; 3]) -> Image {
    Image {
        size,
        alpha: AlphaEncoding::Straight,
        rgba8: [rgb[0], rgb[1], rgb[2], 255].repeat((size.x * size.y) as usize),
    }
}

const WAIT: Duration = Duration::from_secs(10);

#[test]
fn frames_reach_the_device_as_yuyv_and_the_newest_wins() {
    let size = UVec2::new(4, 2);
    let (mut vcam, frames, go) = camera();
    assert_eq!(vcam.info().alpha, AlphaNeed::Opaque, "a camera has no alpha: the stream flattens for it");
    vcam.start(&OutputConfig::new("Omuzaki", size, (30, 1)).unwrap()).unwrap();

    let red = flat(size, [255, 0, 0]);
    vcam.submit(&Frame::new(size, 0, Ticks(0)).with_cpu(&red)).unwrap();
    go.send(()).unwrap();
    let first = frames.recv_timeout(WAIT).unwrap();
    let mut want = vec![0; yuv::yuyv_len(4, 2)];
    assert!(yuv::rgba_to_yuyv(&red.rgba8, 4, 2, &mut want));
    assert_eq!(first, want);

    // Two frames while the writer is held: the second replaces the first, which counts as dropped.
    let green = flat(size, [0, 255, 0]);
    let blue = flat(size, [0, 0, 255]);
    vcam.submit(&Frame::new(size, 1, Ticks(1)).with_cpu(&green)).unwrap();
    // The writer may already have taken green into its buffer (waiting on `go`), or not: either way blue arrives.
    let report = vcam.submit(&Frame::new(size, 2, Ticks(2)).with_cpu(&blue)).unwrap();
    go.send(()).unwrap();
    let mut last = frames.recv_timeout(WAIT).unwrap();
    if report.dropped == 0 {
        go.send(()).unwrap();
        last = frames.recv_timeout(WAIT).unwrap();
    }
    assert!(yuv::rgba_to_yuyv(&blue.rgba8, 4, 2, &mut want));
    assert_eq!(last, want, "the camera ends on the newest frame");

    vcam.stop();
    assert_eq!(vcam.status().state, OutputState::Stopped);
    assert!(vcam.submit(&Frame::new(size, 3, Ticks(3)).with_cpu(&blue)).is_err());
}

#[test]
fn an_odd_width_is_refused_at_start() {
    let (mut vcam, _frames, _go) = camera();
    let err = vcam.start(&OutputConfig::new("Omuzaki", UVec2::new(5, 2), (30, 1)).unwrap()).unwrap_err();
    assert!(matches!(err, StreamError::Config(_)), "{err:?}");
}

#[test]
fn the_kind_is_registered_and_says_whether_it_works_here() {
    omuzaki_vcam::link();
    let kind = find_output_kind(omuzaki_vcam::KIND).expect("vcam registers itself");
    match (kind.available)() {
        Ok(()) => {}
        Err(StreamError::Unavailable { hint, .. }) => assert!(!hint.is_empty(), "a missing camera says what to do"),
        Err(other) => panic!("unexpected {other:?}"),
    }
}

/// On a machine with v4l2loopback loaded, a real frame reaches the device. Skipped elsewhere (CI and most VMs).
#[cfg(target_os = "linux")]
#[test]
fn a_loopback_device_takes_frames_when_present() {
    if omuzaki_vcam::available().is_err() {
        eprintln!("skipped: no v4l2loopback device");
        return;
    }
    let size = UVec2::new(640, 360);
    let mut vcam = Vcam::default();
    vcam.start(&OutputConfig::new("Omuzaki", size, (30, 1)).unwrap()).unwrap();
    let picture = flat(size, [0, 177, 64]);
    vcam.submit(&Frame::new(size, 0, Ticks(0)).with_cpu(&picture)).unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    while vcam.status().frames == 0 {
        assert!(std::time::Instant::now() < deadline, "{:?}", vcam.status());
        std::thread::yield_now();
    }
    vcam.stop();
}
