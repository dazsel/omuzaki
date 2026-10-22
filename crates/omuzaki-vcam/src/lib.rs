//! Virtual cameras (spec v3 BR-02): the program picture as a webcam that Zoom, Discord, Meet, OBS and browsers open.
//!
//! [`Vcam`] is an [`Output`] registered as the `vcam` output kind, so `output.start { kind: "vcam" }` starts it. The
//! stream service hands it the picture already flattened over the backdrop (a camera has no alpha, so `AlphaNeed`
//! is `Opaque`). `submit` copies the frame into a one-frame mailbox and returns; a writer thread converts the newest
//! frame to YUYV ([`yuv`]) and writes it to the device, so a slow consumer never holds up the stream. A frame that
//! arrives before the writer took the previous one replaces it and counts as dropped: a camera shows the newest
//! picture, never a queue.
//!
//! | OS | Device |
//! |---|---|
//! | Linux | a v4l2loopback device (`v4l2`), which the person installs |
//! | Windows, macOS | not yet: the kind reports itself unavailable with a hint, and the output window is the route |

pub mod yuv;

#[cfg(target_os = "linux")]
pub mod v4l2;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Instant;

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream_api::encoder::Frame;
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::output::{
    AlphaNeed, Lanes, Output, OutputConfig, OutputInfo, OutputKind, OutputState, OutputStatus, SubmitReport,
};

/// The registered output kind.
pub const KIND: &str = "vcam";

/// Where the writer thread puts converted frames: the device, or a test's memory.
pub trait FrameSink: Send {
    /// Writes one YUYV frame.
    ///
    /// # Errors
    /// [`StreamError::Io`] when the device is gone.
    fn write(&mut self, yuyv: &[u8]) -> Result<(), StreamError>;
}

/// Opens the sink for a config; the real one opens the OS device.
pub type Opener = Box<dyn Fn(&OutputConfig) -> Result<Box<dyn FrameSink>, StreamError> + Send>;

/// The newest frame waiting for the writer.
struct Mailbox {
    image: Image,
    fresh: bool,
}

struct Shared {
    mailbox: Mutex<Mailbox>,
    wake: Condvar,
    stop: AtomicBool,
    written: AtomicU64,
    replaced: AtomicU64,
    error: Mutex<Option<StreamError>>,
}

/// The virtual camera output.
pub struct Vcam {
    opener: Opener,
    state: OutputState,
    shared: Option<Arc<Shared>>,
    writer: Option<JoinHandle<()>>,
    accepted: u64,
}

impl Default for Vcam {
    /// The camera on this OS's device.
    fn default() -> Self {
        Self::with_opener(Box::new(open_device))
    }
}

impl Vcam {
    /// A camera writing to whatever `opener` opens: the OS device by default, a memory sink in tests.
    pub fn with_opener(opener: Opener) -> Self {
        Self { opener, state: OutputState::Idle, shared: None, writer: None, accepted: 0 }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn empty_image() -> Image {
    Image { size: UVec2::ZERO, alpha: AlphaEncoding::Straight, rgba8: Vec::new() }
}

fn writer(shared: &Shared, mut sink: Box<dyn FrameSink>, size: UVec2) {
    let mut back = empty_image();
    let mut yuyv = vec![0u8; yuv::yuyv_len(size.x, size.y)];
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
        if back.size != size || !yuv::rgba_to_yuyv(&back.rgba8, size.x, size.y, &mut yuyv) {
            lock(&shared.error).get_or_insert(StreamError::Config(format!(
                "the camera runs at {}x{} but got a {}x{} frame",
                size.x, size.y, back.size.x, back.size.y
            )));
            continue;
        }
        match sink.write(&yuyv) {
            Ok(()) => {
                shared.written.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                lock(&shared.error).get_or_insert(e);
                return;
            }
        }
    }
}

impl Output for Vcam {
    fn info(&self) -> OutputInfo {
        OutputInfo::new(KIND, AlphaNeed::Opaque, Lanes { gpu: None, cpu: true })
    }

    fn start(&mut self, cfg: &OutputConfig) -> Result<(), StreamError> {
        if self.state == OutputState::Running {
            return Err(StreamError::Busy);
        }
        if !cfg.size.x.is_multiple_of(2) {
            return Err(StreamError::Config(format!("a camera picture needs an even width; got {}", cfg.size.x)));
        }
        let sink = (self.opener)(cfg)?;
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox { image: empty_image(), fresh: false }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            written: AtomicU64::new(0),
            replaced: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let thread_shared = Arc::clone(&shared);
        let size = cfg.size;
        let handle = std::thread::Builder::new()
            .name("omuzaki-vcam".into())
            .spawn(move || writer(&thread_shared, sink, size))
            .map_err(|e| StreamError::Io(format!("the camera thread could not start: {e}")))?;
        self.shared = Some(shared);
        self.writer = Some(handle);
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
        let image = frame.cpu.ok_or_else(|| StreamError::Config("the camera reads the CPU image".into()))?;
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
        self.accepted += 1;
        Ok(SubmitReport::new(started.elapsed().as_secs_f32() * 1000.0, 0.0, dropped))
    }

    fn status(&self) -> OutputStatus {
        let mut status = OutputStatus::default();
        status.state = self.state;
        if let Some(shared) = &self.shared {
            status.frames = shared.written.load(Ordering::Relaxed);
            status.dropped = shared.replaced.load(Ordering::Relaxed);
            status.last_error.clone_from(&lock(&shared.error));
            status.queue_fill = if lock(&shared.mailbox).fresh { 1.0 } else { 0.0 };
        }
        status
    }

    fn stop(&mut self) {
        if let Some(shared) = &self.shared {
            shared.stop.store(true, Ordering::Release);
            shared.wake.notify_all();
        }
        if let Some(handle) = self.writer.take() {
            // A writer that panicked has stopped writing already; the status keeps its last error.
            let _ = handle.join();
        }
        if self.state == OutputState::Running {
            self.state = OutputState::Stopped;
        }
    }
}

impl Drop for Vcam {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(target_os = "linux")]
fn open_device(cfg: &OutputConfig) -> Result<Box<dyn FrameSink>, StreamError> {
    struct Device(v4l2::Device);
    impl FrameSink for Device {
        fn write(&mut self, yuyv: &[u8]) -> Result<(), StreamError> {
            self.0.write(yuyv)
        }
    }
    let found = v4l2::find()?;
    Ok(Box::new(Device(v4l2::Device::open(&found.path, cfg.size.x, cfg.size.y)?)))
}

#[cfg(not(target_os = "linux"))]
fn open_device(_cfg: &OutputConfig) -> Result<Box<dyn FrameSink>, StreamError> {
    Err(unbuilt())
}

/// Why this system has no virtual camera yet.
#[cfg(not(target_os = "linux"))]
fn unbuilt() -> StreamError {
    StreamError::Unavailable {
        what: "a virtual camera on this system".into(),
        hint: "not built for this system yet; open the output window (output.window.open) and capture it".into(),
    }
}

/// Whether this machine has a virtual camera device to write to.
///
/// # Errors
/// [`StreamError::Unavailable`] with what a person can do about it.
pub fn available() -> Result<(), StreamError> {
    #[cfg(target_os = "linux")]
    {
        v4l2::find().map(|_| ())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(unbuilt())
    }
}

omuzaki_stream_api::register_output!(OutputKind {
    name: KIND,
    summary: "a virtual webcam showing the program picture over the backdrop colour, for Zoom, Discord, Meet, OBS and browsers",
    available,
    make: || Box::new(Vcam::default()),
});

omuzaki_core::define_link!();
