//! [`FakeBackend`]: an [`AudioBackend`] whose devices a script controls (design section 9.3, "Devices").
//!
//! There are no threads and no real clock. The test steps it: [`FakeBackend::step`] advances the engine timeline by a
//! number of frames at 48 kHz, applies the [`Script`]'s events at their sample indices and runs every stream's callback
//! that falls due, on the calling thread. That makes the loss, return, reorder, re-id and drift cases of the engine
//! tests exact and repeatable.
//!
//! A stream's callbacks fall due every `buffer_frames / (rate * (1 + ppm / 1e6))` seconds of the engine timeline, so a
//! device running fast by 200 ppm delivers 200 more frames per million. The [`CallbackTime`] of each callback is exact:
//! for an input, `callback - device` is the device's latency; for an output, `device - callback` is.
//!
//! Callbacks run with no backend lock held, so a callback can call back into the backend. A stream must not be
//! dropped from inside its own callback.

mod device;
mod script;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_audio_api::backend::{
    AudioBackend, BackendError, CallbackTime, DeviceStream, ErrorCallback, InputCallback, OutputCallback, StreamConfig,
};
use omuzaki_audio_api::device::{DeviceId, DeviceInfo, Direction};
use omuzaki_audio_api::node::SAMPLE_RATE;
use omuzaki_core::time::Seconds;

pub use device::{FAKE_HOST, FakeDevice, InputSource, fake_id};
pub use script::{Event, Script};

/// The scripted fake host. Cloning gives another handle to the same host, so a test can keep one while the engine owns
/// another.
#[derive(Clone)]
pub struct FakeBackend {
    shared: Arc<Mutex<Shared>>,
}

struct Shared {
    /// The engine timeline, in samples at 48 kHz.
    now: u64,
    /// In enumeration order. A vanished device stays here, marked, so it can return.
    devices: Vec<DeviceState>,
    script: Script,
    streams: Vec<Arc<Stream>>,
}

struct DeviceState {
    device: FakeDevice,
    present: bool,
    played: Vec<f32>,
}

enum Callback {
    Input(InputCallback),
    Output(OutputCallback),
}

struct Stream {
    device: DeviceId,
    /// What the stream really runs with.
    config: StreamConfig,
    closed: AtomicBool,
    state: Mutex<StreamState>,
}

struct StreamState {
    alive: bool,
    /// When the next callback runs, in seconds of the engine timeline: `anchor + count * period`, so repeated additions
    /// do not accumulate rounding error while the drift stays the same.
    next_at: f64,
    anchor: f64,
    count: u64,
    ppm: f64,
    /// Device frames delivered so far.
    frames_done: u64,
    callback: Option<Callback>,
    on_error: Option<ErrorCallback>,
    /// Reused for every callback so a step allocates nothing.
    buffer: Vec<f32>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A callback that panicked poisons the lock; the state is still consistent, and the panic is the test's failure.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn seconds_of(sample: u64) -> f64 {
    // Test timelines stay far below 2^52 samples.
    #[allow(clippy::cast_precision_loss)]
    {
        sample as f64 / f64::from(SAMPLE_RATE)
    }
}

/// Seconds between two callbacks of a stream.
fn period(config: StreamConfig, ppm: f64) -> f64 {
    f64::from(config.buffer_frames) / (f64::from(config.rate) * (1.0 + ppm * 1e-6))
}

/// A callback that falls within this many seconds before a boundary counts as at the boundary: sums of periods are not
/// exact in floating point, and a callback at exactly `k / 48000` must not move to the other side of an event at `k`.
const BOUNDARY_SLACK: f64 = 1e-9;

impl Shared {
    fn find(&self, id: &DeviceId) -> Option<usize> {
        self.devices.iter().position(|d| &d.device.info.id == id)
    }

    /// Applies one event; returns the streams it killed.
    fn apply(&mut self, event: Event) -> Vec<Arc<Stream>> {
        let mut dead = Vec::new();
        match event {
            Event::Vanish(id) => {
                if let Some(at) = self.find(&id) {
                    self.devices[at].present = false;
                    dead = self.streams_on(&id);
                }
            }
            Event::Return(id) => {
                if let Some(at) = self.find(&id) {
                    self.devices[at].present = true;
                }
            }
            Event::Reorder(order) => {
                let mut ordered = Vec::with_capacity(self.devices.len());
                for id in &order {
                    if let Some(at) = self.devices.iter().position(|d| &d.device.info.id == id) {
                        ordered.push(self.devices.remove(at));
                    }
                }
                ordered.append(&mut self.devices);
                self.devices = ordered;
            }
            Event::Reid { from, to } => {
                if let Some(at) = self.find(&from) {
                    dead = self.streams_on(&from);
                    let state = &mut self.devices[at];
                    state.device.info.id = to;
                    state.present = true;
                }
            }
            Event::Drift { device, ppm } => {
                if let Some(at) = self.find(&device) {
                    self.devices[at].device.drift_ppm = ppm;
                }
            }
            Event::Latency { device, latency } => {
                if let Some(at) = self.find(&device) {
                    self.devices[at].device.latency = latency;
                }
            }
            Event::SetDefault(id) => {
                if let Some(direction) = self.find(&id).map(|at| self.devices[at].device.info.direction) {
                    for state in self.devices.iter_mut().filter(|d| d.device.info.direction == direction) {
                        state.device.info.is_default = state.device.info.id == id;
                    }
                }
            }
        }
        dead
    }

    fn streams_on(&self, id: &DeviceId) -> Vec<Arc<Stream>> {
        self.streams.iter().filter(|s| &s.device == id).cloned().collect()
    }
}

impl FakeBackend {
    /// A host with `devices` in this enumeration order, and the events of `script`. Time starts at sample 0.
    #[must_use]
    pub fn new(devices: Vec<FakeDevice>, script: Script) -> Self {
        let devices =
            devices.into_iter().map(|device| DeviceState { device, present: true, played: Vec::new() }).collect();
        Self { shared: Arc::new(Mutex::new(Shared { now: 0, devices, script, streams: Vec::new() })) }
    }

    /// The engine timeline's position, in samples at 48 kHz.
    #[must_use]
    pub fn now(&self) -> u64 {
        locked(&self.shared).now
    }

    /// Adds an event to the script. An index that is already past happens at the start of the next step.
    pub fn schedule(&self, index: u64, event: Event) {
        locked(&self.shared).script.push(index, event);
    }

    /// Sets what a capture device hears from now on. Streams already open keep their position in it.
    pub fn set_input(&self, id: &DeviceId, input: InputSource) {
        let mut shared = locked(&self.shared);
        if let Some(at) = shared.find(id) {
            shared.devices[at].device.input = input;
        }
    }

    /// How many streams are open and alive.
    #[must_use]
    pub fn open_streams(&self) -> usize {
        let shared = locked(&self.shared);
        shared.streams.iter().filter(|s| locked(&s.state).alive).count()
    }

    /// What the playback device `id` has been given so far, interleaved, if it was built with
    /// [`FakeDevice::recording`].
    #[must_use]
    pub fn played(&self, id: &DeviceId) -> Vec<f32> {
        let shared = locked(&self.shared);
        shared.find(id).map(|at| shared.devices[at].played.clone()).unwrap_or_default()
    }

    /// Advances the engine timeline by `frames` samples: applies the script's events at their indices and runs every
    /// callback that falls due, in time order (streams opened earlier first on a tie), on this thread.
    pub fn step(&self, frames: u64) {
        let target = locked(&self.shared).now.saturating_add(frames);
        loop {
            // Events due at the current index happen first, so after a step the host already shows the events at its end.
            let (segment_end, killed) = {
                let mut shared = locked(&self.shared);
                let mut killed = Vec::new();
                loop {
                    let now = shared.now;
                    let Some(event) = shared.script.pop_due(now) else { break };
                    killed.extend(shared.apply(event));
                }
                (shared.script.next_index().map_or(target, |i| i.min(target)), killed)
            };
            for stream in killed {
                stream.fail();
            }
            if segment_end <= locked(&self.shared).now {
                break;
            }
            self.run_callbacks_before(seconds_of(segment_end));
            locked(&self.shared).now = segment_end;
        }
    }

    /// Runs every callback due before `limit` seconds, earliest first.
    fn run_callbacks_before(&self, limit: f64) {
        while let Some(stream) = self.earliest_due(limit) {
            // The device's state is read before the stream's lock is taken: the two locks are never nested.
            let params = {
                let shared = locked(&self.shared);
                shared.find(&stream.device).filter(|at| shared.devices[*at].present).map(|at| {
                    let d = &shared.devices[at].device;
                    (d.drift_ppm, d.latency, d.input.clone(), d.info.direction, d.record)
                })
            };
            let mut state = locked(&stream.state);
            if !state.alive || stream.closed.load(Ordering::Acquire) {
                continue;
            }
            let Some((ppm, latency, input, direction, record)) = params else {
                // The device is gone but the script has not killed this stream: close it so the loop cannot spin.
                state.alive = false;
                continue;
            };
            let at = state.next_at;
            let channels = usize::from(stream.config.channels);
            let StreamState { callback, buffer, frames_done, .. } = &mut *state;
            match callback {
                Some(Callback::Input(on_data)) => {
                    for (frame, chunk) in buffer.chunks_mut(channels).enumerate() {
                        chunk.fill(input.sample(*frames_done + frame as u64));
                    }
                    on_data(buffer, CallbackTime { callback: Seconds(at), device: Seconds(at - latency.0) });
                }
                Some(Callback::Output(on_data)) => {
                    buffer.fill(0.0);
                    on_data(buffer, CallbackTime { callback: Seconds(at), device: Seconds(at + latency.0) });
                }
                None => {
                    state.alive = false;
                    continue;
                }
            }
            *frames_done += u64::from(stream.config.buffer_frames);
            if state.ppm.to_bits() == ppm.to_bits() {
                state.count += 1;
            } else {
                // The drift changed: the new rate counts from this callback.
                state.anchor = at;
                state.count = 1;
                state.ppm = ppm;
            }
            #[allow(clippy::cast_precision_loss)] // a count of callbacks, far below 2^52
            {
                state.next_at = state.anchor + state.count as f64 * period(stream.config, ppm);
            }
            if record && direction == Direction::Output {
                let samples = state.buffer.clone();
                drop(state);
                let mut shared = locked(&self.shared);
                if let Some(at) = shared.find(&stream.device) {
                    shared.devices[at].played.extend_from_slice(&samples);
                }
            }
        }
    }

    /// The live stream whose next callback is the earliest one before `limit`.
    fn earliest_due(&self, limit: f64) -> Option<Arc<Stream>> {
        let shared = locked(&self.shared);
        let mut best: Option<(f64, &Arc<Stream>)> = None;
        for stream in &shared.streams {
            let state = locked(&stream.state);
            if state.alive && state.next_at < limit - BOUNDARY_SLACK && best.is_none_or(|(at, _)| state.next_at < at) {
                best = Some((state.next_at, stream));
            }
        }
        best.map(|(_, stream)| Arc::clone(stream))
    }

    fn open(
        &self,
        id: &DeviceId,
        direction: Direction,
        config: StreamConfig,
        callback: Callback,
        on_error: ErrorCallback,
    ) -> Result<Box<dyn DeviceStream>, BackendError> {
        let mut shared = locked(&self.shared);
        let at = shared.find(id).ok_or_else(|| BackendError::UnknownDevice(id.clone()))?;
        let state = &shared.devices[at];
        if !state.present {
            return Err(BackendError::DeviceNotAvailable);
        }
        let info = &state.device.info;
        if info.direction != direction {
            return Err(BackendError::Unsupported(format!("{} is not a {direction:?} device", info.label)));
        }
        if !info.rates.contains(&config.rate) {
            return Err(BackendError::Unsupported(format!("{} does not run at {} Hz", info.label, config.rate)));
        }
        if config.channels == 0 || config.channels > info.channels || config.buffer_frames == 0 {
            return Err(BackendError::Unsupported(format!(
                "{} cannot run {} channels with {} frames",
                info.label, config.channels, config.buffer_frames
            )));
        }
        let actual =
            StreamConfig { buffer_frames: state.device.callback_frames.unwrap_or(config.buffer_frames), ..config };
        let ppm = state.device.drift_ppm;
        let stream = Arc::new(Stream {
            device: id.clone(),
            config: actual,
            closed: AtomicBool::new(false),
            state: Mutex::new(StreamState {
                alive: true,
                next_at: seconds_of(shared.now) + period(actual, ppm),
                anchor: seconds_of(shared.now),
                count: 1,
                ppm,
                frames_done: 0,
                callback: Some(callback),
                on_error: Some(on_error),
                buffer: vec![0.0; usize::try_from(actual.buffer_frames).unwrap_or(0) * usize::from(actual.channels)],
            }),
        });
        shared.streams.push(Arc::clone(&stream));
        Ok(Box::new(FakeStream { stream, shared: Arc::clone(&self.shared) }))
    }
}

impl Stream {
    /// The device went away under a running stream: report it once and stop.
    fn fail(&self) {
        let mut state = locked(&self.state);
        if state.alive {
            state.alive = false;
            if let Some(on_error) = state.on_error.as_mut() {
                on_error(BackendError::DeviceNotAvailable);
            }
        }
    }
}

/// An open fake stream. Dropping it closes it.
struct FakeStream {
    stream: Arc<Stream>,
    shared: Arc<Mutex<Shared>>,
}

impl DeviceStream for FakeStream {
    fn config(&self) -> StreamConfig {
        self.stream.config
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.stream.closed.store(true, Ordering::Release);
        locked(&self.shared).streams.retain(|s| !Arc::ptr_eq(s, &self.stream));
        // Waits for a callback that is running on another thread, so none runs after this returns.
        let mut state = locked(&self.stream.state);
        state.alive = false;
        state.callback = None;
        state.on_error = None;
    }
}

impl AudioBackend for FakeBackend {
    fn devices(&self) -> Result<Vec<DeviceInfo>, BackendError> {
        Ok(locked(&self.shared).devices.iter().filter(|d| d.present).map(|d| d.device.info.clone()).collect())
    }

    fn default_device(&self, direction: Direction) -> Result<Option<DeviceId>, BackendError> {
        let shared = locked(&self.shared);
        let mut of_direction = shared.devices.iter().filter(|d| d.present && d.device.info.direction == direction);
        let first = of_direction.next();
        Ok(shared
            .devices
            .iter()
            .find(|d| d.present && d.device.info.direction == direction && d.device.info.is_default)
            .or(first)
            .map(|d| d.device.info.id.clone()))
    }

    fn open_input(
        &self,
        id: &DeviceId,
        config: StreamConfig,
        on_data: InputCallback,
        on_error: ErrorCallback,
    ) -> Result<Box<dyn DeviceStream>, BackendError> {
        self.open(id, Direction::Input, config, Callback::Input(on_data), on_error)
    }

    fn open_output(
        &self,
        id: &DeviceId,
        config: StreamConfig,
        on_data: OutputCallback,
        on_error: ErrorCallback,
    ) -> Result<Box<dyn DeviceStream>, BackendError> {
        self.open(id, Direction::Output, config, Callback::Output(on_data), on_error)
    }
}
