//! The stream service: the one place rendered frames leave the app (spec v3 §25; `docs/design/record.md` D2, D9;
//! `docs/design/supervisor.md` §12).
//!
//! The frame thread hands each rendered frame to [`StreamService::push`], which copies it into a free slot of a small
//! fixed pool and returns: it never waits for an output, a lock an output holds, the disk or the GPU. A pump thread
//! takes the slot, converts the picture once per distinct [`AlphaNeed`] (and scales it where an output runs at
//! another size), paces it onto each output's own frame grid, and calls [`Output::submit`] on every running output.
//! Outputs copy what they keep and return at once, so one slow output cannot hold up another.
//!
//! The service is an ops service (`ctx.service::<StreamService>()`): `output.*` and `record.*` start, list and stop
//! outputs through it, and the app's frame loop calls [`push`](StreamService::push).
//!
//! **Pacing.** Each output has a grid at its own frame rate, anchored at the first frame it sees. A frame lands on
//! the grid slot nearest its capture time; a frame whose slot is already filled is not submitted to that output, and slots that pass with no frame are skipped, so the output sees a jump in
//! [`Frame::index`] and repeats the previous frame itself (a recorder keeps its timeline exact that way, D2).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream_api::encoder::Frame;
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::output::{AlphaNeed, Output, OutputConfig, OutputInfo, OutputState, OutputStatus};
use omuzaki_stream_api::time::Ticks;
use omuzaki_stream_api::window::DEFAULT_KEY;
use serde::{Deserialize, Serialize};

use crate::alpha::convert_into;
use crate::record::runtime::ActiveTake;

/// Frame slots between the frame thread and the pump: one being filled, one being pumped, one spare.
pub const POOL_SLOTS: usize = 3;
/// The most outputs that run at once.
pub const MAX_OUTPUTS: usize = 16;
/// How long the pump waits for a frame before it looks at the stop flag again.
const PUMP_POLL: Duration = Duration::from_millis(50);
/// Nanoseconds per second, the unit of [`Ticks`].
const NANOS: i128 = 1_000_000_000;

/// A running output's id, unique for the life of the service.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct OutputId(pub u64);

/// What [`StreamService::push`] did with a frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// No output is running, so nothing was copied.
    Idle,
    /// The frame is queued for the outputs.
    Queued,
    /// Every slot was busy (the pump is behind), so the frame was dropped and counted.
    Dropped,
}

/// A snapshot of one running output, for `output.status` and the Live workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OutputEntry {
    /// Its id.
    pub id: OutputId,
    /// Its registered kind (`recorder`, `vcam`).
    pub kind: String,
    /// The name receivers see.
    pub name: String,
    /// Frame size in pixels.
    pub size: [u32; 2],
    /// Frames per second as `[numerator, denominator]`.
    pub fps: [u32; 2],
    /// `running`, `stopped` or `failed`.
    pub state: String,
    /// Frames accepted.
    pub frames: u64,
    /// Frames dropped.
    pub dropped: u64,
    /// How full its queue is, `0` to `1`.
    pub queue_fill: f32,
    /// Its last error, if any.
    pub error: Option<String>,
}

/// Counters for the whole service.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServiceCounters {
    /// Frames the frame thread handed over while an output ran.
    pub pushed: u64,
    /// Frames dropped because every slot was busy.
    pub dropped: u64,
    /// Frames the pump took and fanned out.
    pub pumped: u64,
}

/// The program picture's format: what outputs run at unless they ask for another size or rate.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// The rendered size.
    pub size: UVec2,
    /// The rendered frame rate.
    pub fps: (u32, u32),
}

impl Default for Program {
    /// 1920 × 1080 at 30 fps, the budget scene's format (spec §10).
    fn default() -> Self {
        Self { size: UVec2::new(1920, 1080), fps: (30, 1) }
    }
}

/// One frame slot: pixels copied off the frame thread, and when they were captured.
struct Slot {
    image: Image,
    capture: Ticks,
}

/// One output and its place on its own frame grid.
struct Running {
    id: OutputId,
    info: OutputInfo,
    cfg: OutputConfig,
    output: Box<dyn Output>,
    pacer: Pacer,
}

struct Inner {
    outputs: Mutex<Vec<Running>>,
    /// Set while at least one output runs, so `push` skips the copy when nobody reads.
    active: AtomicBool,
    free: Mutex<Receiver<Slot>>,
    full: SyncSender<Slot>,
    stop: AtomicBool,
    next_id: AtomicU64,
    pushed: AtomicU64,
    dropped: AtomicU64,
    pumped: AtomicU64,
    program: Mutex<Program>,
    key: Mutex<Colour>,
    epoch: Instant,
}

/// The stream service. Cheap to share: clone the `Arc` it lives in.
pub struct StreamService {
    inner: Arc<Inner>,
    pump: Mutex<Option<JoinHandle<()>>>,
    /// The take in progress (`record::runtime`).
    pub(crate) recording: Mutex<Option<ActiveTake>>,
}

impl StreamService {
    /// A service with no outputs, its pump thread started. Cold: allocates the slot pool lazily, on the first frames.
    ///
    /// # Errors
    /// [`StreamError::Io`] if the pump thread cannot be started.
    pub fn new() -> Result<Self, StreamError> {
        Self::with_epoch(Instant::now())
    }

    /// As [`new`](Self::new), with the pipeline clock's zero at `epoch`. The app gives the stream service and the
    /// audio engine the same epoch, so a frame's capture time and the engine clock's time are on one timeline and a
    /// take's audio lines up with its video.
    ///
    /// # Errors
    /// [`StreamError::Io`] if the pump thread cannot be started.
    pub fn with_epoch(epoch: Instant) -> Result<Self, StreamError> {
        let (full_tx, full_rx) = sync_channel::<Slot>(POOL_SLOTS);
        let (free_tx, free_rx) = sync_channel::<Slot>(POOL_SLOTS);
        for _ in 0..POOL_SLOTS {
            let empty = Slot { image: empty_image(), capture: Ticks(0) };
            // The channel holds exactly `POOL_SLOTS`, so these sends never block or fail.
            free_tx.try_send(empty).map_err(|_| StreamError::Io("the frame pool could not be filled".into()))?;
        }
        let inner = Arc::new(Inner {
            outputs: Mutex::new(Vec::new()),
            active: AtomicBool::new(false),
            free: Mutex::new(free_rx),
            full: full_tx,
            stop: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            pushed: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            pumped: AtomicU64::new(0),
            program: Mutex::new(Program::default()),
            key: Mutex::new(DEFAULT_KEY),
            epoch,
        });
        let pump_inner = Arc::clone(&inner);
        let pump = std::thread::Builder::new()
            .name("omuzaki-stream-pump".into())
            .spawn(move || pump(&pump_inner, &full_rx, &free_tx))
            .map_err(|e| StreamError::Io(format!("the stream pump thread could not start: {e}")))?;
        Ok(Self { inner, pump: Mutex::new(Some(pump)), recording: Mutex::new(None) })
    }

    /// The time now on the service's clock, for callers with no capture time of their own.
    pub fn now(&self) -> Ticks {
        Ticks(i64::try_from(self.inner.epoch.elapsed().as_nanos()).unwrap_or(i64::MAX))
    }

    /// The pipeline clock's zero.
    pub fn epoch(&self) -> Instant {
        self.inner.epoch
    }

    /// Sets the program picture's format, which new outputs use unless they ask otherwise.
    pub fn set_program(&self, program: Program) {
        *lock(&self.inner.program) = program;
    }

    /// The program picture's format.
    pub fn program(&self) -> Program {
        *lock(&self.inner.program)
    }

    /// Sets the colour opaque outputs (a virtual camera) composite the picture over. Default `#00b140`, chroma green,
    /// so a capture tool can key it out.
    pub fn set_backdrop(&self, key: Colour) {
        *lock(&self.inner.key) = key;
    }

    /// Hands a rendered frame to the outputs. Called on the frame thread once per rendered frame.
    ///
    /// Copies the pixels into a free slot and returns; allocates only when the picture size changes. With no output
    /// running it copies nothing.
    pub fn push(&self, image: &Image, capture: Ticks) -> PushOutcome {
        if !self.inner.active.load(Ordering::Acquire) {
            return PushOutcome::Idle;
        }
        self.inner.pushed.fetch_add(1, Ordering::Relaxed);
        let slot = match lock(&self.inner.free).try_recv() {
            Ok(slot) => slot,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                return PushOutcome::Dropped;
            }
        };
        let mut slot = slot;
        slot.image.size = image.size;
        slot.image.alpha = image.alpha;
        slot.image.rgba8.clear();
        slot.image.rgba8.extend_from_slice(&image.rgba8);
        slot.capture = capture;
        // The pool and the channel are the same size, so a slot taken from the pool always fits.
        if self.inner.full.try_send(slot).is_err() {
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
            return PushOutcome::Dropped;
        }
        PushOutcome::Queued
    }

    /// Starts `output` with `cfg` and returns its id. The output is started before it joins the fan-out, so a failed
    /// start leaves nothing behind.
    ///
    /// # Errors
    /// What [`Output::start`] reports, or [`StreamError::Busy`] at [`MAX_OUTPUTS`].
    pub fn start(&self, mut output: Box<dyn Output>, cfg: OutputConfig) -> Result<OutputId, StreamError> {
        if lock(&self.inner.outputs).len() >= MAX_OUTPUTS {
            return Err(StreamError::Busy);
        }
        output.start(&cfg)?;
        let info = output.info();
        let id = OutputId(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let pacer = Pacer::new(cfg.fps);
        let mut outputs = lock(&self.inner.outputs);
        outputs.push(Running { id, info, cfg, output, pacer });
        self.inner.active.store(true, Ordering::Release);
        Ok(id)
    }

    /// Stops the output `id` and returns its last status. The output leaves the fan-out first, so its `stop` (which
    /// may finish files) runs without holding up the frames of the others.
    ///
    /// # Errors
    /// [`StreamError::Config`] if no output has that id.
    pub fn stop(&self, id: OutputId) -> Result<OutputStatus, StreamError> {
        let mut running = {
            let mut outputs = lock(&self.inner.outputs);
            let at = outputs
                .iter()
                .position(|r| r.id == id)
                .ok_or_else(|| StreamError::Config(format!("no output {}", id.0)))?;
            let running = outputs.remove(at);
            self.inner.active.store(!outputs.is_empty(), Ordering::Release);
            running
        };
        running.output.stop();
        Ok(running.output.status())
    }

    /// Runs `f` on the running output `id`, for a caller that knows its concrete type (the recorder's report).
    ///
    /// # Errors
    /// [`StreamError::Config`] if no output has that id.
    pub fn with_output<R>(&self, id: OutputId, f: impl FnOnce(&mut dyn Output) -> R) -> Result<R, StreamError> {
        let mut outputs = lock(&self.inner.outputs);
        let running = outputs
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| StreamError::Config(format!("no output {}", id.0)))?;
        Ok(f(running.output.as_mut()))
    }

    /// Every running output, in the order they started.
    pub fn outputs(&self) -> Vec<OutputEntry> {
        lock(&self.inner.outputs).iter().map(entry).collect()
    }

    /// The service's counters.
    pub fn counters(&self) -> ServiceCounters {
        ServiceCounters {
            pushed: self.inner.pushed.load(Ordering::Relaxed),
            dropped: self.inner.dropped.load(Ordering::Relaxed),
            pumped: self.inner.pumped.load(Ordering::Acquire),
        }
    }

    /// Stops every output and the pump. Idempotent; dropping the service does it too.
    pub fn shutdown(&self) {
        // A take still recording is finished properly (its manifest written) before everything else stops.
        if lock(&self.recording).is_some() {
            let _ = self.record_stop();
        }
        let drained: Vec<Running> = {
            let mut outputs = lock(&self.inner.outputs);
            self.inner.active.store(false, Ordering::Release);
            outputs.drain(..).collect()
        };
        for mut running in drained {
            running.output.stop();
        }
        self.inner.stop.store(true, Ordering::Release);
        if let Some(handle) = lock(&self.pump).take() {
            // A pump that panicked has already stopped feeding; nothing more to do for it here.
            let _ = handle.join();
        }
    }
}

impl Drop for StreamService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn entry(r: &Running) -> OutputEntry {
    let status = r.output.status();
    let state = match status.state {
        OutputState::Running => "running",
        OutputState::Failed => "failed",
        OutputState::Stopped => "stopped",
        _ => "idle",
    };
    OutputEntry {
        id: r.id,
        kind: r.info.kind.to_owned(),
        name: r.cfg.name.clone(),
        size: [r.cfg.size.x, r.cfg.size.y],
        fps: [r.cfg.fps.0, r.cfg.fps.1],
        state: state.to_owned(),
        frames: status.frames,
        dropped: status.dropped,
        queue_fill: status.queue_fill,
        error: status.last_error.map(|e| e.to_string()),
    }
}

/// Locks a mutex, taking the data even if a holder panicked: every value behind these locks stays valid between
/// statements, so a panic elsewhere never leaves it half-written.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn empty_image() -> Image {
    Image { size: UVec2::ZERO, alpha: AlphaEncoding::Premultiplied, rgba8: Vec::new() }
}

/// One converted picture: the alpha form and size it was made for, its pixels, a buffer for the scaled picture
/// before conversion, and whether it holds the current frame.
struct Picture {
    need: AlphaNeed,
    size: UVec2,
    image: Image,
    scaled: Image,
    fresh: bool,
}

/// Converted pictures, one per alpha need and size, reused across frames so the pump allocates only when a new
/// combination first appears.
#[derive(Default)]
struct Scratch {
    pictures: Vec<Picture>,
}

impl Scratch {
    /// Marks every picture stale for a new frame.
    fn reset(&mut self) {
        for p in &mut self.pictures {
            p.fresh = false;
        }
    }

    /// The frame in the form `need` at `size`, converting (and scaling) once per frame.
    fn get(&mut self, src: &Image, need: AlphaNeed, size: UVec2, key: Colour) -> &Image {
        let at = if let Some(at) = self.pictures.iter().position(|p| p.need == need && p.size == size) {
            at
        } else {
            self.pictures.push(Picture { need, size, image: empty_image(), scaled: empty_image(), fresh: false });
            self.pictures.len() - 1
        };
        let picture = &mut self.pictures[at];
        if !picture.fresh {
            if size == src.size {
                convert_into(src, need, key, &mut picture.image);
            } else {
                resize_bilinear(src, size, &mut picture.scaled);
                convert_into(&picture.scaled, need, key, &mut picture.image);
            }
            picture.fresh = true;
        }
        &picture.image
    }
}

fn pump(inner: &Inner, full: &Receiver<Slot>, free: &SyncSender<Slot>) {
    let mut scratch = Scratch::default();
    while !inner.stop.load(Ordering::Acquire) {
        let slot = match full.recv_timeout(PUMP_POLL) {
            Ok(slot) => slot,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        fan_out(inner, &slot, &mut scratch);
        // Counted after the fan-out, so a caller that waits for the count knows every output has the frame.
        inner.pumped.fetch_add(1, Ordering::Release);
        // The free channel has room for every slot, so this send never fails while the service lives.
        if free.try_send(slot).is_err() {
            return;
        }
    }
}

fn fan_out(inner: &Inner, slot: &Slot, scratch: &mut Scratch) {
    scratch.reset();
    let key = *lock(&inner.key);
    let mut outputs = lock(&inner.outputs);
    for running in outputs.iter_mut() {
        let Some(index) = running.pacer.slot(slot.capture) else { continue };
        let picture = scratch.get(&slot.image, running.info.alpha, running.cfg.size, key);
        let frame = Frame::new(picture.size, index, slot.capture).with_cpu(picture);
        // A failed submit is the output's own state (`OutputStatus::last_error`); the others keep their frames.
        let _ = running.output.submit(&frame);
    }
}

/// An output's frame grid: slot `n` is due at `t0 + n / fps`.
#[derive(Clone, Copy, Debug)]
struct Pacer {
    fps: (u32, u32),
    t0: Option<Ticks>,
    next: u64,
}

impl Pacer {
    fn new(fps: (u32, u32)) -> Self {
        Self { fps, t0: None, next: 0 }
    }

    /// The grid slot a frame captured at `t` fills (the nearest one), or `None` when that slot is already filled.
    fn slot(&mut self, t: Ticks) -> Option<u64> {
        let t0 = *self.t0.get_or_insert(t);
        let elapsed = i128::from(t.0.saturating_sub(t0.0)).max(0);
        let den = i128::from(self.fps.1) * NANOS;
        // The nearest slot, not the one at or before: a frame a little early for its slot (clock jitter, a period
        // that is not a whole number of nanoseconds) still fills it.
        let den = den.max(1);
        let slot = u64::try_from((2 * elapsed * i128::from(self.fps.0) + den) / (2 * den)).unwrap_or(u64::MAX);
        if slot < self.next {
            return None;
        }
        self.next = slot.saturating_add(1);
        Some(slot)
    }
}

/// Resamples premultiplied (or opaque) RGBA8 `src` to `size` with a bilinear filter, which is exact for premultiplied
/// data: colour and alpha average together, so edges keep no fringe.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
fn resize_bilinear(src: &Image, size: UVec2, dst: &mut Image) {
    dst.size = size;
    dst.alpha = src.alpha;
    dst.rgba8.clear();
    dst.rgba8.resize(size.x as usize * size.y as usize * 4, 0);
    if src.size.x == 0 || src.size.y == 0 {
        return;
    }
    let (sw, sh) = (src.size.x as usize, src.size.y as usize);
    let sx = src.size.x as f32 / size.x as f32;
    let sy = src.size.y as f32 / size.y as f32;
    for y in 0..size.y as usize {
        let fy = ((y as f32 + 0.5) * sy - 0.5).max(0.0);
        let y0 = (fy as usize).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let wy = fy - y0 as f32;
        for x in 0..size.x as usize {
            let fx = ((x as f32 + 0.5) * sx - 0.5).max(0.0);
            let x0 = (fx as usize).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let wx = fx - x0 as f32;
            let at = |xx: usize, yy: usize, c: usize| f32::from(src.rgba8[(yy * sw + xx) * 4 + c]);
            for c in 0..4 {
                let top = at(x0, y0, c) * (1.0 - wx) + at(x1, y0, c) * wx;
                let bottom = at(x0, y1, c) * (1.0 - wx) + at(x1, y1, c) * wx;
                let v = top * (1.0 - wy) + bottom * wy;
                dst.rgba8[(y * size.x as usize + x) * 4 + c] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;

    #[test]
    fn the_pacer_fills_each_slot_once_and_skips_empty_ones() {
        let mut p = Pacer::new((30, 1));
        assert_eq!(p.slot(Ticks(1_000 * MS)), Some(0), "the first frame anchors the grid");
        assert_eq!(p.slot(Ticks(1_015 * MS)), None, "a 60 Hz frame nearer slot 0 is not submitted again");
        assert_eq!(p.slot(Ticks(1_030 * MS)), Some(1), "a frame a little early for slot 1 fills it");
        assert_eq!(p.slot(Ticks(1_045 * MS)), None, "still nearest slot 1");
        assert_eq!(p.slot(Ticks(1_134 * MS)), Some(4), "slots 2 and 3 passed with no frame");
        assert_eq!(p.slot(Ticks(1_100 * MS)), None, "an older frame never goes back");
    }

    #[test]
    fn the_pacer_handles_fractional_rates_exactly() {
        let mut p = Pacer::new((30_000, 1_001));
        assert_eq!(p.slot(Ticks(0)), Some(0));
        // Slot 30 of 29.97 fps is due at 1.001 s exactly; the boundary to slot 31 is half a period later.
        assert_eq!(p.slot(Ticks(1_001_000_000)), Some(30));
        assert_eq!(p.slot(Ticks(1_017_683_333)), None, "still nearest slot 30");
        assert_eq!(p.slot(Ticks(1_017_683_334)), Some(31));
    }

    #[test]
    fn bilinear_keeps_a_flat_picture_flat_and_hits_the_size() {
        let src =
            Image { size: UVec2::new(4, 2), alpha: AlphaEncoding::Premultiplied, rgba8: [10, 20, 30, 40].repeat(8) };
        let mut dst = empty_image();
        resize_bilinear(&src, UVec2::new(7, 3), &mut dst);
        assert_eq!(dst.size, UVec2::new(7, 3));
        assert!(dst.rgba8.as_chunks::<4>().0.iter().all(|p| *p == [10, 20, 30, 40]));
    }
}
