//! [`CpalBackend`]: the operating system's audio through cpal (ALSA on Linux, WASAPI on Windows, `CoreAudio` on macOS).
//!
//! Each open stream lives on a thread of its own that builds it, starts it and holds it until the [`DeviceStream`] is
//! dropped, because a cpal stream is not `Send` on every host and the engine moves its streams between threads. The
//! data callback converts whatever sample format the device delivers to `f32` into a buffer sized when the stream opens,
//! so the callback allocates only if the host hands it a larger buffer than it announced.
//!
//! Device ids are cpal's persistent ids (`host:id`), so a binding survives reordering and restarts.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use omuzaki_audio_api::backend::{
    AudioBackend, BackendError, CallbackTime, DeviceStream, ErrorCallback, InputCallback, OutputCallback, StreamConfig,
};
use omuzaki_audio_api::device::{DeviceId, DeviceInfo, Direction};
use omuzaki_core::time::Seconds;

/// The rates a device is asked about, the graph's own first.
const PROBED_RATES: [u32; 6] = [48_000, 44_100, 96_000, 32_000, 24_000, 16_000];
/// Samples per channel the conversion buffer holds before a callback must grow it.
const CONVERT_FRAMES: usize = 8192;

/// The system's audio through cpal.
pub struct CpalBackend {
    host: cpal::Host,
    epoch: Instant,
}

impl std::fmt::Debug for CpalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpalBackend").field("host", &self.host.id().name()).finish_non_exhaustive()
    }
}

impl CpalBackend {
    /// The system's default host. `epoch` is the instant the engine timeline counts from (its clock's zero), so
    /// callback times land on the same timeline as everything else.
    #[must_use]
    pub fn new(epoch: Instant) -> Self {
        Self { host: cpal::default_host(), epoch }
    }

    fn find(&self, id: &DeviceId) -> Result<cpal::Device, BackendError> {
        let wanted: cpal::DeviceId = format!("{}:{}", id.host, id.id)
            .parse()
            .map_err(|_| BackendError::UnknownDevice(id.clone()))?;
        self.host.device_by_id(&wanted).ok_or_else(|| BackendError::UnknownDevice(id.clone()))
    }
}

fn other(e: impl std::fmt::Display) -> BackendError {
    BackendError::Other(e.to_string())
}

fn map_error(e: &cpal::Error) -> BackendError {
    match e.kind() {
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated => BackendError::DeviceNotAvailable,
        cpal::ErrorKind::DeviceChanged => BackendError::DeviceChanged,
        cpal::ErrorKind::UnsupportedConfig | cpal::ErrorKind::InvalidInput => BackendError::Unsupported(e.to_string()),
        _ => BackendError::Other(e.to_string()),
    }
}

fn device_id(device: &cpal::Device) -> Option<DeviceId> {
    let id = device.id().ok()?;
    Some(DeviceId { host: id.host().to_string(), id: id.id().to_owned() })
}

fn info(device: &cpal::Device, direction: Direction, default: Option<&DeviceId>) -> Option<DeviceInfo> {
    let id = device_id(device)?;
    let label = device.description().map_or_else(|_| id.id.clone(), |d| d.name().to_owned());
    let ranges: Vec<_> = match direction {
        Direction::Input => device.supported_input_configs().ok()?.collect(),
        Direction::Output => device.supported_output_configs().ok()?.collect(),
    };
    let channels = ranges.iter().map(cpal::SupportedStreamConfigRange::channels).max().unwrap_or(0);
    let rates = PROBED_RATES
        .into_iter()
        .filter(|r| ranges.iter().any(|c| (c.min_sample_rate()..=c.max_sample_rate()).contains(r)))
        .collect();
    Some(DeviceInfo {
        is_default: default == Some(&id),
        id,
        label,
        direction,
        rates,
        channels,
        latency_hint: None,
    })
}

impl AudioBackend for CpalBackend {
    fn devices(&self) -> Result<Vec<DeviceInfo>, BackendError> {
        let default_in = self.default_device(Direction::Input)?;
        let default_out = self.default_device(Direction::Output)?;
        let mut out = Vec::new();
        for device in self.host.devices().map_err(other)? {
            if device.supports_input()
                && let Some(i) = info(&device, Direction::Input, default_in.as_ref())
            {
                out.push(i);
            }
            if device.supports_output()
                && let Some(i) = info(&device, Direction::Output, default_out.as_ref())
            {
                out.push(i);
            }
        }
        Ok(out)
    }

    fn default_device(&self, direction: Direction) -> Result<Option<DeviceId>, BackendError> {
        let device = match direction {
            Direction::Input => self.host.default_input_device(),
            Direction::Output => self.host.default_output_device(),
        };
        Ok(device.as_ref().and_then(device_id))
    }

    fn open_input(
        &self,
        id: &DeviceId,
        config: StreamConfig,
        on_data: InputCallback,
        on_error: ErrorCallback,
    ) -> Result<Box<dyn DeviceStream>, BackendError> {
        let device = self.find(id)?;
        let epoch = self.epoch;
        StreamThread::spawn(format!("omuzaki-audio-in {}", id.id), move || {
            open_input(&device, config, on_data, on_error, epoch)
        })
    }

    fn open_output(
        &self,
        id: &DeviceId,
        config: StreamConfig,
        on_data: OutputCallback,
        on_error: ErrorCallback,
    ) -> Result<Box<dyn DeviceStream>, BackendError> {
        let device = self.find(id)?;
        let epoch = self.epoch;
        StreamThread::spawn(format!("omuzaki-audio-out {}", id.id), move || {
            open_output(&device, config, on_data, on_error, epoch)
        })
    }
}

/// Picks the device's configuration nearest to `wanted`: the wanted rate if any range has it (else the device's
/// default), the wanted channel count if offered (else the most), and `f32` when offered.
fn choose(
    ranges: Vec<cpal::SupportedStreamConfigRange>,
    default: Option<cpal::SupportedStreamConfig>,
    wanted: StreamConfig,
) -> Result<(cpal::StreamConfig, SampleFormat), BackendError> {
    let has_rate = |c: &cpal::SupportedStreamConfigRange| (c.min_sample_rate()..=c.max_sample_rate()).contains(&wanted.rate);
    let score = |c: &cpal::SupportedStreamConfigRange| {
        (has_rate(c), c.channels() == wanted.channels, c.sample_format() == SampleFormat::F32, c.channels())
    };
    let best = ranges.into_iter().filter(|c| convertible(c.sample_format())).max_by_key(score);
    let (channels, rate, format) = match (best, default) {
        (Some(c), _) if has_rate(&c) => (c.channels(), wanted.rate, c.sample_format()),
        (_, Some(d)) if convertible(d.sample_format()) => (d.channels(), d.sample_rate(), d.sample_format()),
        (Some(c), _) => (c.channels(), c.max_sample_rate().min(wanted.rate.max(c.min_sample_rate())), c.sample_format()),
        _ => return Err(BackendError::Unsupported("the device offers no sample format the engine reads".into())),
    };
    let buffer_size = if wanted.buffer_frames > 0 {
        cpal::BufferSize::Fixed(wanted.buffer_frames)
    } else {
        cpal::BufferSize::Default
    };
    Ok((cpal::StreamConfig { channels, sample_rate: rate, buffer_size }, format))
}

fn convertible(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::F32 | SampleFormat::F64 | SampleFormat::I16 | SampleFormat::I32 | SampleFormat::U16 | SampleFormat::I8 | SampleFormat::U8
    )
}

fn seconds(epoch: Instant, now: Instant) -> Seconds {
    Seconds(now.saturating_duration_since(epoch).as_secs_f64())
}

/// Converts `data` to `f32` into `out` (resized to fit, which allocates only when the host's buffer outgrows it).
fn to_f32<T: SizedSample>(data: &cpal::Data, out: &mut Vec<f32>) -> bool
where
    f32: FromSample<T>,
{
    let Some(samples) = data.as_slice::<T>() else { return false };
    out.resize(samples.len(), 0.0);
    for (o, s) in out.iter_mut().zip(samples) {
        *o = s.to_sample::<f32>();
    }
    true
}

fn convert(format: SampleFormat, data: &cpal::Data, out: &mut Vec<f32>) -> bool {
    match format {
        SampleFormat::F32 => to_f32::<f32>(data, out),
        SampleFormat::F64 => to_f32::<f64>(data, out),
        SampleFormat::I16 => to_f32::<i16>(data, out),
        SampleFormat::I32 => to_f32::<i32>(data, out),
        SampleFormat::U16 => to_f32::<u16>(data, out),
        SampleFormat::I8 => to_f32::<i8>(data, out),
        SampleFormat::U8 => to_f32::<u8>(data, out),
        _ => false,
    }
}

fn open_input(
    device: &cpal::Device,
    wanted: StreamConfig,
    mut on_data: InputCallback,
    mut on_error: ErrorCallback,
    epoch: Instant,
) -> Result<(cpal::Stream, StreamConfig), BackendError> {
    let ranges = device.supported_input_configs().map_err(|e| map_error(&e))?.collect();
    let (config, format) = choose(ranges, device.default_input_config().ok(), wanted)?;
    let mut buffer = Vec::with_capacity(CONVERT_FRAMES * usize::from(config.channels));
    // The stream's own instants have an arbitrary origin; the first callback ties them to the engine's epoch.
    let mut origin: Option<(cpal::StreamInstant, Instant)> = None;
    let stream = device
        .build_input_stream_raw(
            config,
            format,
            move |data, info| {
                let now = Instant::now();
                let stamp = info.timestamp();
                let (zero, at) = *origin.get_or_insert((stamp.callback, now));
                let callback = at + stamp.callback.duration_since(zero);
                let lag = stamp.callback.duration_since(stamp.capture);
                let device = callback.checked_sub(lag).unwrap_or(callback);
                if convert(format, data, &mut buffer) {
                    on_data(&buffer, CallbackTime { callback: seconds(epoch, callback), device: seconds(epoch, device) });
                }
            },
            move |e| on_error(map_error(&e)),
            None,
        )
        .map_err(|e| map_error(&e))?;
    stream.play().map_err(|e| map_error(&e))?;
    Ok((stream, StreamConfig { rate: config.sample_rate, channels: config.channels, buffer_frames: wanted.buffer_frames }))
}

fn open_output(
    device: &cpal::Device,
    wanted: StreamConfig,
    mut on_data: OutputCallback,
    mut on_error: ErrorCallback,
    epoch: Instant,
) -> Result<(cpal::Stream, StreamConfig), BackendError> {
    let ranges: Vec<_> = device
        .supported_output_configs()
        .map_err(|e| map_error(&e))?
        .filter(|c| c.sample_format() == SampleFormat::F32)
        .collect();
    let (config, _) = choose(ranges, None, wanted)?;
    let mut origin: Option<(cpal::StreamInstant, Instant)> = None;
    let stream = device
        .build_output_stream::<f32, _, _>(
            config,
            move |data, info| {
                data.fill(0.0);
                let now = Instant::now();
                let stamp = info.timestamp();
                let (zero, at) = *origin.get_or_insert((stamp.callback, now));
                let callback = at + stamp.callback.duration_since(zero);
                let device = callback + stamp.playback.duration_since(stamp.callback);
                on_data(data, CallbackTime { callback: seconds(epoch, callback), device: seconds(epoch, device) });
            },
            move |e| on_error(map_error(&e)),
            None,
        )
        .map_err(|e| map_error(&e))?;
    stream.play().map_err(|e| map_error(&e))?;
    Ok((stream, StreamConfig { rate: config.sample_rate, channels: config.channels, buffer_frames: wanted.buffer_frames }))
}

/// A stream held on its own thread until dropped.
struct StreamThread {
    config: StreamConfig,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl StreamThread {
    fn spawn(
        name: String,
        open: impl FnOnce() -> Result<(cpal::Stream, StreamConfig), BackendError> + Send + 'static,
    ) -> Result<Box<dyn DeviceStream>, BackendError> {
        let (ready_tx, ready_rx) = channel::<Result<StreamConfig, BackendError>>();
        let (stop_tx, stop_rx): (Sender<()>, Receiver<()>) = channel();
        let thread = std::thread::Builder::new()
            .name(name)
            .spawn(move || match open() {
                Ok((stream, config)) => {
                    let _ = ready_tx.send(Ok(config));
                    // Held until the handle drops (the sender goes away) or asks to stop.
                    let _ = stop_rx.recv();
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(other)?;
        match ready_rx.recv() {
            Ok(Ok(config)) => Ok(Box::new(Self { config, stop: Some(stop_tx), thread: Some(thread) })),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(BackendError::Other("the stream thread ended before the stream opened".into()))
            }
        }
    }
}

impl DeviceStream for StreamThread {
    fn config(&self) -> StreamConfig {
        self.config
    }
}

impl Drop for StreamThread {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            // The stream is dropped on its thread before it ends, so no callback runs after this returns.
            let _ = thread.join();
        }
    }
}
