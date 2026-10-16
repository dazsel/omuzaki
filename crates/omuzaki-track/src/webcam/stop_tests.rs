//! Stop tests that need to act on the exact moment `stop` is asked for, through the `StopSignal::on_request` seam
//! (compiled for unit tests only). Ordering here comes from that hook and from channels, never from a sleep.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_track_api::camera::{CameraFormat, CameraFrame, CameraInfo, PixelFormat};
use omuzaki_track_mock::FakeCamera;

use super::*;

/// How long a test waits for something that happens as soon as the machine lets it: a backstop against a hang only.
const BACKSTOP: Duration = Duration::from_secs(60);

/// A tracker that finds no face and counts the frames it is given.
struct Counting(Arc<AtomicUsize>);

impl FrameTracker for Counting {
    fn track(&mut self, _: &RgbFrame<'_>, _: Seconds) -> Result<(Option<FaceSample>, StageTimings), FaceTrackError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok((None, StageTimings::default()))
    }

    fn reset(&mut self) {}
}

/// A camera whose `next_frame` is held until the test sends a token, as a driver is that returns a capture late. It
/// tells the test each time the camera thread enters a call.
struct Held {
    inner: FakeCamera,
    entered: Sender<()>,
    release: Arc<Mutex<Receiver<()>>>,
}

struct HeldStream {
    inner: Box<dyn CameraStream>,
    entered: Sender<()>,
    release: Arc<Mutex<Receiver<()>>>,
}

impl Camera for Held {
    fn enumerate(&self) -> Vec<CameraInfo> {
        self.inner.enumerate()
    }

    fn open(&mut self, id: &CameraId, request: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
        Ok(Box::new(HeldStream {
            inner: self.inner.open(id, request)?,
            entered: self.entered.clone(),
            release: Arc::clone(&self.release),
        }))
    }
}

impl CameraStream for HeldStream {
    fn format(&self) -> CameraFormat {
        self.inner.format()
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
        // Best effort: the test only listens while it waits for an entry.
        let _ = self.entered.send(());
        let token = self.release.lock().unwrap_or_else(PoisonError::into_inner).recv_timeout(BACKSTOP);
        if token.is_err() {
            return Ok(None);
        }
        self.inner.next_frame(timeout)
    }
}

/// A capture that returns after `stop` was asked for is dropped, not tracked: the tracker never sees it and comes back
/// at once. The held capture is released from inside `StopSignal::request`, after the flag is set, so correct code
/// cannot lose a race to a descheduled test thread; the tracker's own call counter says whether the frame was submitted.
#[test]
fn a_capture_that_returns_after_stop_is_not_tracked() {
    let (entered, entries) = channel();
    let (release, gate) = channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let camera = Held {
        inner: FakeCamera::synthetic(CameraFormat::new(64, 48, 30, PixelFormat::Nv12), 10_000_000),
        entered,
        release: Arc::new(Mutex::new(gate)),
    };
    let mut tracker = WebcamTracker::new(
        Box::new(camera),
        Box::new(Counting(Arc::clone(&calls))),
        WebcamConfig::default().with_mode(64, 48, 30),
        Arc::new(FakeClock::new(Seconds(100.0))),
    );
    let config = tracker.tracker_config();
    let mut source = tracker.start(&FakeCamera::id(), &config).expect("the fake camera opens");
    // One frame goes through, so the session is live; then the camera thread is held inside its next call.
    release.send(()).unwrap();
    let deadline = Instant::now() + BACKSTOP;
    while calls.load(Ordering::SeqCst) < 1 {
        assert!(Instant::now() < deadline, "the first frame never reached the tracker");
        std::thread::sleep(Duration::from_millis(1));
    }
    entries.recv_timeout(BACKSTOP).expect("the camera thread entered its first call");
    entries.recv_timeout(BACKSTOP).expect("and is held inside the next one");
    let before = calls.load(Ordering::SeqCst);
    // The capture is let go the moment `stop` is asked for.
    let hook_release = release.clone();
    let session = tracker.session.as_ref().expect("a session runs");
    *session.stop.on_request.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(move || {
        hook_release.send(()).ok();
    }));
    tracker.stop();
    assert_eq!(calls.load(Ordering::SeqCst), before, "the late capture was handed to the tracker");
    assert_eq!(tracker.error(), None, "the tracker came back; nothing was let go");
    assert!(source.poll(Seconds(0.0)).is_none(), "the source is quiet after stop");
}
