//! The devices of a [`FakeBackend`](super::FakeBackend).

use std::sync::Arc;

use omuzaki_audio_api::device::{DeviceId, DeviceInfo, Direction};
use omuzaki_audio_api::node::SAMPLE_RATE;
use omuzaki_core::time::Seconds;

/// The host name of every fake device id.
pub const FAKE_HOST: &str = "fake";

/// The id of a fake device called `name`.
#[must_use]
pub fn fake_id(name: &str) -> DeviceId {
    DeviceId { host: FAKE_HOST.into(), id: name.into() }
}

/// What a capture device hears. It is a mono signal on the device's own timeline: frame `n` of a stream is sample `n`
/// of the source, and every channel carries the same value.
#[derive(Clone)]
pub enum InputSource {
    /// Silence.
    Silence,
    /// These samples, then silence.
    Samples(Arc<[f32]>),
    /// A function of the sample index.
    Function(Arc<dyn Fn(u64) -> f32 + Send + Sync>),
}

impl InputSource {
    /// The source for `samples`, which are played once from their first.
    #[must_use]
    pub fn samples(samples: Vec<f32>) -> Self {
        Self::Samples(samples.into())
    }

    pub(crate) fn sample(&self, index: u64) -> f32 {
        match self {
            Self::Silence => 0.0,
            Self::Samples(samples) => usize::try_from(index).ok().and_then(|i| samples.get(i)).copied().unwrap_or(0.0),
            Self::Function(f) => f(index),
        }
    }
}

impl std::fmt::Debug for InputSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Silence => f.write_str("Silence"),
            Self::Samples(s) => write!(f, "Samples({} samples)", s.len()),
            Self::Function(_) => f.write_str("Function"),
        }
    }
}

/// One device of the fake host, built up with the `with_` methods.
#[derive(Clone, Debug)]
pub struct FakeDevice {
    pub(crate) info: DeviceInfo,
    pub(crate) drift_ppm: f64,
    pub(crate) latency: Seconds,
    pub(crate) input: InputSource,
    pub(crate) callback_frames: Option<u32>,
    pub(crate) record: bool,
}

impl FakeDevice {
    fn new(name: &str, label: &str, direction: Direction, channels: u16) -> Self {
        Self {
            info: DeviceInfo {
                id: fake_id(name),
                label: label.into(),
                direction,
                is_default: false,
                rates: vec![44_100, SAMPLE_RATE],
                channels,
                latency_hint: Some(Seconds(0.005)),
            },
            drift_ppm: 0.0,
            latency: Seconds(0.005),
            input: InputSource::Silence,
            callback_frames: None,
            record: false,
        }
    }

    /// A capture device with `channels` channels at 44.1 and 48 kHz and 5 ms of latency.
    #[must_use]
    pub fn input(name: &str, label: &str, channels: u16) -> Self {
        Self::new(name, label, Direction::Input, channels)
    }

    /// A playback device with `channels` channels at 44.1 and 48 kHz and 5 ms of latency.
    #[must_use]
    pub fn output(name: &str, label: &str, channels: u16) -> Self {
        Self::new(name, label, Direction::Output, channels)
    }

    /// Makes it the OS default for its direction.
    #[must_use]
    pub fn default_device(mut self) -> Self {
        self.info.is_default = true;
        self
    }

    /// The rates it accepts, in Hz.
    #[must_use]
    pub fn with_rates(mut self, rates: &[u32]) -> Self {
        self.info.rates = rates.to_vec();
        self
    }

    /// Its clock runs fast by `ppm` parts per million (negative: slow). At -1 000 000 ppm or below the clock does not run
    /// at all: a stream on such a device is refused, or fails if a script event brings the drift there.
    #[must_use]
    pub fn with_drift_ppm(mut self, ppm: f64) -> Self {
        self.drift_ppm = ppm;
        self
    }

    /// The capture-to-callback (input) or callback-to-playback (output) latency, which is also its `latency_hint`.
    #[must_use]
    pub fn with_latency(mut self, latency: Seconds) -> Self {
        self.latency = latency;
        self.info.latency_hint = Some(latency);
        self
    }

    /// What a capture device hears.
    #[must_use]
    pub fn with_input(mut self, input: InputSource) -> Self {
        self.input = input;
        self
    }

    /// The frames the device really delivers per callback, whatever was requested. A device with 0 refuses every
    /// stream with `BackendError::Unsupported`.
    #[must_use]
    pub fn with_callback_frames(mut self, frames: u32) -> Self {
        self.callback_frames = Some(frames);
        self
    }

    /// Keeps what a playback device is given, for [`FakeBackend::played`](super::FakeBackend::played). Off by default,
    /// because the recording grows and so allocates, which an allocation-counting test does not want.
    #[must_use]
    pub fn recording(mut self) -> Self {
        self.record = true;
        self
    }

    /// The device's id.
    #[must_use]
    pub fn id(&self) -> &DeviceId {
        &self.info.id
    }
}
