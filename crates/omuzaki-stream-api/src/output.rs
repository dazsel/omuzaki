//! The output trait: one live or recorded destination for rendered frames.

use omuzaki_core::glam::UVec2;

use crate::encoder::Frame;
use crate::error::StreamError;

/// Which picture an output shows (D9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Feed {
    /// What is on the show, overlays included.
    #[default]
    Program,
    /// The picture without overlays.
    Clean,
}

/// The alpha form an output wants; the stream layer converts, outputs do not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AlphaNeed {
    /// Colour multiplied by alpha (Spout, recorders).
    Premultiplied,
    /// Colour independent of alpha (NDI).
    Straight,
    /// No alpha: flatten over a background (virtual camera).
    Opaque,
}

/// A GPU API an output can share a texture with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GpuBackend {
    /// Direct3D 12.
    Dx12,
    /// Metal.
    Metal,
    /// Vulkan.
    Vulkan,
}

/// Which frame lanes an output reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Lanes {
    /// The GPU texture on this backend, if the output can take one.
    pub gpu: Option<GpuBackend>,
    /// Whether it can take the CPU image.
    pub cpu: bool,
}

/// What an output needs.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct OutputInfo {
    /// The output's registered name (`spout`, `ndi`, `recorder`), not a central enum.
    pub kind: &'static str,
    /// The alpha form it wants.
    pub alpha: AlphaNeed,
    /// The frame lanes it reads.
    pub lanes: Lanes,
}

impl OutputInfo {
    /// Describes one output.
    pub fn new(kind: &'static str, alpha: AlphaNeed, lanes: Lanes) -> Self {
        Self { kind, alpha, lanes }
    }
}

/// How to start an output.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct OutputConfig {
    /// The name receivers see (a Spout sender name, an NDI source name).
    pub name: String,
    /// Frame size in pixels; fixed until `stop`.
    pub size: UVec2,
    /// Frames per second as `(numerator, denominator)`.
    pub fps: (u32, u32),
    /// Which picture it shows.
    pub feed: Feed,
}

impl OutputConfig {
    /// A config for the program feed.
    ///
    /// # Errors
    /// [`StreamError::Config`] if the name is empty or the size or frame rate has a zero part.
    pub fn new(name: impl Into<String>, size: UVec2, fps: (u32, u32)) -> Result<Self, StreamError> {
        let name = name.into();
        if name.is_empty() {
            return Err(StreamError::Config("output name is empty".into()));
        }
        if size.x == 0 || size.y == 0 {
            return Err(StreamError::Config(format!("output size {}x{} has a zero side", size.x, size.y)));
        }
        if fps.0 == 0 || fps.1 == 0 {
            return Err(StreamError::Config(format!("frame rate {}/{} has a zero part", fps.0, fps.1)));
        }
        Ok(Self { name, size, fps, feed: Feed::Program })
    }

    /// Returns the config showing this feed.
    #[must_use]
    pub fn with_feed(mut self, feed: Feed) -> Self {
        self.feed = feed;
        self
    }
}

/// What one `submit` cost; reported to the frame-budget scheduler.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct SubmitReport {
    /// CPU time on the calling thread.
    pub cpu_ms: f32,
    /// Time spent waiting on the GPU.
    pub gpu_wait_ms: f32,
    /// Frames dropped by this call.
    pub dropped: u32,
}

impl SubmitReport {
    /// A report of these costs.
    pub fn new(cpu_ms: f32, gpu_wait_ms: f32, dropped: u32) -> Self {
        Self { cpu_ms, gpu_wait_ms, dropped }
    }
}

/// Where an output is in its life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OutputState {
    /// Created, not started.
    #[default]
    Idle,
    /// Accepting frames.
    Running,
    /// Stopped on request.
    Stopped,
    /// Stopped by an error; see [`OutputStatus::last_error`].
    Failed,
}

/// A snapshot of an output for the UI and the supervisor.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct OutputStatus {
    /// Where it is in its life.
    pub state: OutputState,
    /// Connected receivers, if the output can tell.
    pub receivers: Option<u32>,
    /// Frames accepted.
    pub frames: u64,
    /// Frames dropped.
    pub dropped: u64,
    /// How full the frame queue is, `0.0` to `1.0`.
    pub queue_fill: f32,
    /// Encode time per frame divided by the frame period.
    pub load: f32,
    /// Bytes held beyond the cap's count: frame slots, encoder buffers (D4).
    pub working_bytes: u64,
    /// The last error, if any.
    pub last_error: Option<StreamError>,
}

/// One live or recorded output: a Spout sender, an NDI source, a virtual camera, a recorder, the replay buffer.
pub trait Output: Send {
    /// What the output needs.
    fn info(&self) -> OutputInfo;

    /// Starts the output.
    ///
    /// # Errors
    /// [`StreamError::Unavailable`] with a hint if the device or library is missing, [`StreamError::Config`] for a bad
    /// config.
    fn start(&mut self, cfg: &OutputConfig) -> Result<(), StreamError>;

    /// Hands over a frame. Copies what it needs and returns at once: it never waits for the GPU, a lock or a full queue.
    ///
    /// # Errors
    /// [`StreamError::Stopped`] if not running; a full queue is a dropped frame in the report, not an error.
    fn submit(&mut self, frame: &Frame<'_>) -> Result<SubmitReport, StreamError>;

    /// A snapshot of the output's state and counters.
    fn status(&self) -> OutputStatus;

    /// Stops the output and releases its resources; idempotent.
    fn stop(&mut self);
}
