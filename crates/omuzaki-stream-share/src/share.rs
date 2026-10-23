//! The output both texture senders share: a one-frame mailbox in front of a thread that owns the platform publisher.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::Instant;

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream_api::encoder::Frame;
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::output::{
    AlphaNeed, Lanes, Output, OutputConfig, OutputInfo, OutputState, OutputStatus, SubmitReport,
};

/// Where the output's thread puts each frame: a Spout sender, a Syphon server, or a test's memory.
pub trait Publisher {
    /// Publishes one frame: `size.x * size.y * 4` bytes of BGRA8, top row first, rows tightly packed.
    ///
    /// # Errors
    /// [`StreamError::Io`] when the sender is gone (a lost GPU device, a closed server); the output stops.
    fn publish(&mut self, bgra: &[u8]) -> Result<(), StreamError>;
}

/// Opens the publisher for a config. It runs on the output's own thread, so what it makes never crosses threads.
pub type Opener = Arc<dyn Fn(&OutputConfig) -> Result<Box<dyn Publisher>, StreamError> + Send + Sync>;

/// Reorders RGBA8 pixels to BGRA8, the layout both Spout's and Syphon's receivers read. A length mismatch converts
/// the shorter prefix only; the caller sizes both from one image.
pub fn rgba_to_bgra(rgba: &[u8], bgra: &mut [u8]) {
    for (s, d) in rgba.as_chunks::<4>().0.iter().zip(bgra.as_chunks_mut::<4>().0.iter_mut()) {
        *d = [s[2], s[1], s[0], s[3]];
    }
}

/// The newest frame waiting for the thread.
struct Mailbox {
    image: Image,
    fresh: bool,
}

struct Shared {
    mailbox: Mutex<Mailbox>,
    wake: Condvar,
    stop: AtomicBool,
    published: AtomicU64,
    replaced: AtomicU64,
    error: Mutex<Option<StreamError>>,
}

/// A texture-sharing output: the mailbox, the thread, and the kind it was registered as.
pub struct Share {
    kind: &'static str,
    alpha: AlphaNeed,
    opener: Opener,
    state: OutputState,
    shared: Option<Arc<Shared>>,
    worker: Option<JoinHandle<()>>,
}

impl Share {
    /// An idle output of `kind` that wants frames in the `alpha` form and publishes through what `opener` opens.
    pub fn new(kind: &'static str, alpha: AlphaNeed, opener: Opener) -> Self {
        Self { kind, alpha, opener, state: OutputState::Idle, shared: None, worker: None }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn empty_image() -> Image {
    Image { size: UVec2::ZERO, alpha: AlphaEncoding::Straight, rgba8: Vec::new() }
}

fn run(shared: &Shared, mut publisher: Box<dyn Publisher>, size: UVec2) {
    let mut back = empty_image();
    let mut bgra = vec![0u8; size.x as usize * size.y as usize * 4];
    loop {
        {
            let mut mailbox = lock(&shared.mailbox);
            while !mailbox.fresh && !shared.stop.load(Ordering::Acquire) {
                mailbox = shared.wake.wait(mailbox).unwrap_or_else(PoisonError::into_inner);
            }
            if shared.stop.load(Ordering::Acquire) {
                return;
            }
            // Swap buffers so the lock is held for a pointer swap, not a conversion.
            std::mem::swap(&mut mailbox.image, &mut back);
            mailbox.fresh = false;
        }
        if back.size != size || back.rgba8.len() != bgra.len() {
            lock(&shared.error).get_or_insert(StreamError::Config(format!(
                "the sender runs at {}x{} but got a {}x{} frame",
                size.x, size.y, back.size.x, back.size.y
            )));
            continue;
        }
        rgba_to_bgra(&back.rgba8, &mut bgra);
        match publisher.publish(&bgra) {
            Ok(()) => {
                shared.published.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                lock(&shared.error).get_or_insert(e);
                return;
            }
        }
    }
}

impl Output for Share {
    fn info(&self) -> OutputInfo {
        OutputInfo::new(self.kind, self.alpha, Lanes { gpu: None, cpu: true })
    }

    fn start(&mut self, cfg: &OutputConfig) -> Result<(), StreamError> {
        if self.state == OutputState::Running {
            return Err(StreamError::Busy);
        }
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox { image: empty_image(), fresh: false }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            published: AtomicU64::new(0),
            replaced: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let (thread_shared, opener, thread_cfg) = (Arc::clone(&shared), Arc::clone(&self.opener), cfg.clone());
        // The thread opens the publisher and says whether that worked before it takes frames, so `start` reports a
        // missing framework or a taken name itself.
        let (tell, told) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name(format!("omuzaki-{}", self.kind))
            .spawn(move || match opener(&thread_cfg) {
                Ok(publisher) => {
                    let _ = tell.send(Ok(()));
                    run(&thread_shared, publisher, thread_cfg.size);
                }
                Err(e) => {
                    let _ = tell.send(Err(e));
                }
            })
            .map_err(|e| StreamError::Io(format!("the {} thread could not start: {e}", self.kind)))?;
        let outcome = told.recv().unwrap_or_else(|_| Err(StreamError::Io(format!("the {} thread ended", self.kind))));
        if let Err(e) = outcome {
            // The thread has returned or is about to; it holds nothing.
            let _ = worker.join();
            return Err(e);
        }
        self.shared = Some(shared);
        self.worker = Some(worker);
        self.state = OutputState::Running;
        Ok(())
    }

    fn submit(&mut self, frame: &Frame<'_>) -> Result<SubmitReport, StreamError> {
        let shared = self.shared.as_ref().filter(|_| self.state == OutputState::Running).ok_or(StreamError::Stopped)?;
        if let Some(e) = lock(&shared.error).clone() {
            self.state = OutputState::Failed;
            return Err(e);
        }
        let started = Instant::now();
        let image =
            frame.cpu.ok_or_else(|| StreamError::Config(format!("the {} sender reads the CPU image", self.kind)))?;
        let mut dropped = 0;
        {
            let mut mailbox = lock(&shared.mailbox);
            if mailbox.fresh {
                shared.replaced.fetch_add(1, Ordering::Relaxed);
                dropped = 1;
            }
            mailbox.image.size = image.size;
            mailbox.image.alpha = image.alpha;
            mailbox.image.rgba8.clear();
            mailbox.image.rgba8.extend_from_slice(&image.rgba8);
            mailbox.fresh = true;
        }
        shared.wake.notify_one();
        Ok(SubmitReport::new(started.elapsed().as_secs_f32() * 1000.0, 0.0, dropped))
    }

    fn status(&self) -> OutputStatus {
        let mut status = OutputStatus::default();
        status.state = self.state;
        if let Some(shared) = &self.shared {
            status.frames = shared.published.load(Ordering::Relaxed);
            status.dropped = shared.replaced.load(Ordering::Relaxed);
            status.last_error.clone_from(&lock(&shared.error));
            status.queue_fill = if lock(&shared.mailbox).fresh { 1.0 } else { 0.0 };
            if status.last_error.is_some() && self.state == OutputState::Running {
                status.state = OutputState::Failed;
            }
        }
        status
    }

    fn stop(&mut self) {
        if let Some(shared) = &self.shared {
            shared.stop.store(true, Ordering::Release);
            shared.wake.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            // The publisher is dropped on its thread as it returns, which withdraws the sender; a thread that
            // panicked has stopped publishing already, and the status keeps its last error.
            let _ = worker.join();
        }
        if self.state == OutputState::Running {
            self.state = OutputState::Stopped;
        }
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_stream_api::time::Ticks;

    /// Records what it was handed, and can be told to fail.
    struct Memory {
        frames: Arc<Mutex<Vec<Vec<u8>>>>,
        gate: Arc<(Mutex<bool>, Condvar)>,
        fail_after: Option<usize>,
        closed: Arc<AtomicBool>,
    }

    impl Publisher for Memory {
        fn publish(&mut self, bgra: &[u8]) -> Result<(), StreamError> {
            // Wait until the test opens the gate, so it can stack frames in the mailbox first.
            let (open, turn) = &*self.gate;
            let mut open = lock(open);
            while !*open {
                open = turn.wait(open).unwrap_or_else(PoisonError::into_inner);
            }
            drop(open);
            let mut frames = lock(&self.frames);
            if self.fail_after.is_some_and(|n| frames.len() >= n) {
                return Err(StreamError::Io("the device went away".into()));
            }
            frames.push(bgra.to_vec());
            Ok(())
        }
    }

    impl Drop for Memory {
        fn drop(&mut self) {
            self.closed.store(true, Ordering::Release);
        }
    }

    struct Rig {
        out: Share,
        frames: Arc<Mutex<Vec<Vec<u8>>>>,
        gate: Arc<(Mutex<bool>, Condvar)>,
        closed: Arc<AtomicBool>,
    }

    fn rig(fail_after: Option<usize>, gate_open: bool) -> Rig {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new((Mutex::new(gate_open), Condvar::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let (f, g, c) = (Arc::clone(&frames), Arc::clone(&gate), Arc::clone(&closed));
        let opener: Opener = Arc::new(move |_cfg| {
            Ok(Box::new(Memory { frames: Arc::clone(&f), gate: Arc::clone(&g), fail_after, closed: Arc::clone(&c) })
                as Box<dyn Publisher>)
        });
        Rig { out: Share::new("test", AlphaNeed::Premultiplied, opener), frames, gate, closed }
    }

    fn open(gate: &(Mutex<bool>, Condvar)) {
        *lock(&gate.0) = true;
        gate.1.notify_all();
    }

    fn image(value: [u8; 4]) -> Image {
        Image { size: UVec2::new(2, 1), alpha: AlphaEncoding::Premultiplied, rgba8: [value, value].concat() }
    }

    fn config() -> OutputConfig {
        OutputConfig::new("Omuzaki", UVec2::new(2, 1), (60, 1)).expect("a valid config")
    }

    fn submit(out: &mut Share, image: &Image) -> SubmitReport {
        out.submit(&Frame::new(image.size, 0, Ticks(0)).with_cpu(image)).expect("the frame is taken")
    }

    fn wait_for(frames: &Mutex<Vec<Vec<u8>>>, count: usize) {
        let started = Instant::now();
        while lock(frames).len() < count {
            assert!(started.elapsed().as_secs() < 10, "the sender published {} of {count}", lock(frames).len());
            std::thread::yield_now();
        }
    }

    #[test]
    fn rgba_becomes_bgra_with_alpha_kept() {
        let mut out = [0u8; 8];
        rgba_to_bgra(&[1, 2, 3, 4, 5, 6, 7, 8], &mut out);
        assert_eq!(out, [3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn a_frame_reaches_the_publisher_as_bgra() {
        let mut r = rig(None, true);
        r.out.start(&config()).expect("it starts");
        assert_eq!(r.out.info().alpha, AlphaNeed::Premultiplied);
        let report = submit(&mut r.out, &image([10, 20, 30, 128]));
        assert_eq!(report.dropped, 0);
        wait_for(&r.frames, 1);
        assert_eq!(lock(&r.frames)[0], [30, 20, 10, 128, 30, 20, 10, 128]);
        assert_eq!(r.out.status().frames, 1);
        r.out.stop();
        assert!(r.closed.load(Ordering::Acquire), "stopping drops the publisher, which withdraws the sender");
        assert_eq!(r.out.status().state, OutputState::Stopped);
    }

    #[test]
    fn a_frame_that_finds_the_mailbox_full_replaces_it_and_counts_as_dropped() {
        let mut r = rig(None, false);
        r.out.start(&config()).expect("it starts");
        // The first frame is taken by the thread, which then waits at the gate; the next two meet in the mailbox.
        submit(&mut r.out, &image([1, 1, 1, 255]));
        let started = Instant::now();
        while r.out.status().queue_fill > 0.0 {
            assert!(started.elapsed().as_secs() < 10, "the thread never took the first frame");
            std::thread::yield_now();
        }
        assert_eq!(submit(&mut r.out, &image([2, 2, 2, 255])).dropped, 0);
        assert_eq!(submit(&mut r.out, &image([3, 3, 3, 255])).dropped, 1);
        open(&r.gate);
        wait_for(&r.frames, 2);
        let frames = lock(&r.frames).clone();
        assert_eq!(frames[0][0], 1);
        assert_eq!(frames[1][0], 3, "the newest frame wins");
        assert_eq!(r.out.status().dropped, 1);
    }

    #[test]
    fn a_publisher_error_fails_the_output_and_the_next_submit_reports_it() {
        let mut r = rig(Some(1), true);
        r.out.start(&config()).expect("it starts");
        submit(&mut r.out, &image([1, 1, 1, 255]));
        wait_for(&r.frames, 1);
        submit(&mut r.out, &image([2, 2, 2, 255]));
        let started = Instant::now();
        while r.out.status().last_error.is_none() {
            assert!(started.elapsed().as_secs() < 10, "the error never surfaced");
            std::thread::yield_now();
        }
        assert_eq!(r.out.status().state, OutputState::Failed);
        let frame = image([3, 3, 3, 255]);
        let err = r.out.submit(&Frame::new(frame.size, 0, Ticks(0)).with_cpu(&frame)).expect_err("it failed");
        assert!(matches!(err, StreamError::Io(_)), "{err:?}");
    }

    #[test]
    fn an_opener_error_is_the_start_error_and_nothing_runs() {
        let opener: Opener =
            Arc::new(|_cfg| Err(StreamError::Unavailable { what: "a sender".into(), hint: "install it".into() }));
        let mut out = Share::new("test", AlphaNeed::Straight, opener);
        let err = out.start(&config()).expect_err("it cannot open");
        assert!(matches!(err, StreamError::Unavailable { .. }), "{err:?}");
        assert_eq!(out.status().state, OutputState::Idle);
        let frame = image([1, 1, 1, 255]);
        let refused = out.submit(&Frame::new(frame.size, 0, Ticks(0)).with_cpu(&frame)).expect_err("not running");
        assert!(matches!(refused, StreamError::Stopped), "{refused:?}");
    }

    #[test]
    fn a_frame_of_the_wrong_size_is_reported_not_published() {
        let mut r = rig(None, true);
        r.out.start(&config()).expect("it starts");
        let wrong = Image { size: UVec2::new(1, 1), alpha: AlphaEncoding::Premultiplied, rgba8: vec![1, 2, 3, 4] };
        submit(&mut r.out, &wrong);
        let started = Instant::now();
        while r.out.status().last_error.is_none() {
            assert!(started.elapsed().as_secs() < 10, "the size error never surfaced");
            std::thread::yield_now();
        }
        assert!(lock(&r.frames).is_empty());
        assert!(matches!(r.out.status().last_error, Some(StreamError::Config(_))));
    }
}
