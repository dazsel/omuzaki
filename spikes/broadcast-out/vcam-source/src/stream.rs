//! The camera's one video stream: answers `RequestSample` with frames paced at the camera's frame rate.
//!
//! A worker thread owns the pacing. Requests queue up tokens; once a frame is due the worker renders it, stamps it with the
//! system-time clock Frame Server expects, and posts it as an `MEMediaSample` event. A consumer that is slower than the
//! camera simply sees later frames (the worker skips ahead rather than replaying a backlog), like a real webcam.

#![allow(unsafe_code)]
// The `#[implement]` macro expands to `inline(always)`, reference-to-pointer casts and raw-pointer trait methods, and the
// COM trait signatures are fixed by windows-rs, so these lints fire on code we do not write.
#![allow(
    clippy::inline_always,
    clippy::borrow_as_ptr,
    clippy::ref_as_ptr,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::needless_pass_by_value
)]

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{E_INVALIDARG, S_OK};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncResult, IMFMediaEvent, IMFMediaEventGenerator_Impl, IMFMediaEventQueue, IMFMediaSource,
    IMFMediaStream_Impl, IMFMediaStream2, IMFMediaStream2_Impl, IMFStreamDescriptor,
    MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MEMediaSample, MEStreamStarted, MEStreamStopped,
    MF_E_MEDIA_SOURCE_WRONGSTATE, MF_E_SHUTDOWN, MF_STREAM_STATE, MF_STREAM_STATE_RUNNING, MFCreateEventQueue,
    MFCreateMemoryBuffer, MFCreateSample, MFGetSystemTime, MFSampleExtension_Token,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{GUID, HRESULT, IUnknown, Ref, Result, implement};

use crate::com::{Agile, LiveGuard};
use crate::format::{FRAME_DURATION_100NS, HEIGHT, PixelFormat, WIDTH};
use crate::pattern;

/// State shared between the COM object, the source and the worker thread.
pub struct StreamShared {
    state: Mutex<State>,
    wake: Condvar,
    events: Agile<IMFMediaEventQueue>,
    descriptor: Agile<IMFStreamDescriptor>,
    /// The owning source, cleared on shutdown so the two do not keep each other alive.
    source: Mutex<Option<Agile<IMFMediaSource>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct State {
    format: PixelFormat,
    /// `Start` has been called and `Stop` has not.
    running: bool,
    playback: MF_STREAM_STATE,
    shutdown: bool,
    /// Tokens of outstanding `RequestSample` calls, oldest first (`None` when the caller passed none).
    tokens: VecDeque<Option<Agile<IUnknown>>>,
    /// Number of the next frame to deliver.
    frame: u64,
    /// System time (100 ns) at which frame 0 was due.
    start_time: i64,
}

/// A frame that is due now: which request it answers, which picture, and when it was due.
struct Job {
    token: Option<Agile<IUnknown>>,
    frame: u64,
    due: i64,
    format: PixelFormat,
}

/// A frame already drawn in the camera's pixel format.
struct Prepared {
    frame: u64,
    format: PixelFormat,
    pixels: Vec<u8>,
}

impl Prepared {
    fn draw(frame: u64, format: PixelFormat, scratch: &mut Vec<u8>) -> Self {
        let mut pixels = vec![0; format.frame_bytes(WIDTH, HEIGHT)];
        pattern::render_frame(frame, format, scratch, &mut pixels);
        Self { frame, format, pixels }
    }
}

fn lock(shared: &StreamShared) -> MutexGuard<'_, State> {
    // A panic on the worker leaves the state consistent enough to shut down, so a poisoned lock is still usable.
    shared.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl StreamShared {
    /// Creates the shared state and starts the (idle) worker.
    pub fn spawn(descriptor: IMFStreamDescriptor, source: IMFMediaSource) -> Result<Arc<Self>> {
        // SAFETY: plain object creation.
        let events = unsafe { MFCreateEventQueue() }?;
        let shared = Arc::new(Self {
            state: Mutex::new(State {
                format: PixelFormat::Nv12,
                running: false,
                playback: MF_STREAM_STATE_RUNNING,
                shutdown: false,
                tokens: VecDeque::new(),
                frame: 0,
                start_time: 0,
            }),
            wake: Condvar::new(),
            events: Agile(events),
            descriptor: Agile(descriptor),
            source: Mutex::new(Some(Agile(source))),
            worker: Mutex::new(None),
        });
        let for_worker = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("omuzaki-vcam-stream".to_owned())
            .spawn(move || for_worker.run_worker())
            .map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        *shared.worker.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
        Ok(shared)
    }

    /// Begins (or restarts) delivery in `format` and announces `MEStreamStarted`.
    pub fn start(&self, format: PixelFormat) -> Result<()> {
        {
            let mut state = lock(self);
            if state.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            state.format = format;
            state.running = true;
            state.tokens.clear();
            state.frame = 0;
            // SAFETY: no arguments.
            state.start_time = unsafe { MFGetSystemTime() };
        }
        self.wake.notify_all();
        let empty = PROPVARIANT::default();
        // SAFETY: `empty` is a valid VT_EMPTY value for the call.
        unsafe { self.events.0.QueueEventParamVar(MEStreamStarted.0 as u32, &GUID::zeroed(), S_OK, &raw const empty) }
    }

    /// Stops delivery and announces `MEStreamStopped`.
    pub fn stop(&self) -> Result<()> {
        {
            let mut state = lock(self);
            if state.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            state.running = false;
            state.tokens.clear();
        }
        let empty = PROPVARIANT::default();
        // SAFETY: `empty` is a valid VT_EMPTY value for the call.
        unsafe { self.events.0.QueueEventParamVar(MEStreamStopped.0 as u32, &GUID::zeroed(), S_OK, &raw const empty) }
    }

    /// Ends the worker and the event queue. Idempotent.
    pub fn shutdown(&self) {
        {
            let mut state = lock(self);
            if state.shutdown {
                return;
            }
            state.shutdown = true;
            state.tokens.clear();
        }
        self.wake.notify_all();
        *self.source.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        // SAFETY: the queue is owned by `self`. A failure here means it is already shut down.
        let _ = unsafe { self.events.0.Shutdown() };
        let handle = self.worker.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(handle) = handle
            && handle.thread().id() != std::thread::current().id()
        {
            let _ = handle.join();
        }
    }

    fn request(&self, token: Option<IUnknown>) -> Result<()> {
        {
            let mut state = lock(self);
            if state.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            if !state.running {
                return Err(MF_E_MEDIA_SOURCE_WRONGSTATE.into());
            }
            state.tokens.push_back(token.map(Agile));
        }
        self.wake.notify_all();
        Ok(())
    }

    /// The worker: wait for a request, wait for the frame to be due, post it, then draw the next frame ahead of time.
    ///
    /// Drawing takes tens of milliseconds for this synthetic picture (a real source would copy a finished frame), so
    /// doing it right after a delivery rather than when the next frame falls due keeps the timestamp honest.
    fn run_worker(&self) {
        let mut scratch = Vec::new();
        let mut prepared: Option<Prepared> = None;
        while let Some(job) = self.next_job() {
            let ready = match prepared.take() {
                Some(frame) if frame.frame == job.frame && frame.format == job.format => frame,
                _ => Prepared::draw(job.frame, job.format, &mut scratch),
            };
            // A failed delivery means the queue is shut down; the next `next_job` sees that and ends the loop.
            let _ = self.deliver(job.token.as_ref(), job.due, &ready);
            prepared = Some(Prepared::draw(job.frame + 1, job.format, &mut scratch));
        }
    }

    /// Blocks until a frame is due and a request is waiting; `None` when shutting down.
    fn next_job(&self) -> Option<Job> {
        let mut state = lock(self);
        loop {
            if state.shutdown {
                return None;
            }
            let ready = state.running && state.playback == MF_STREAM_STATE_RUNNING && !state.tokens.is_empty();
            if !ready {
                state = self.wake.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            }
            // SAFETY: no arguments.
            let now = unsafe { MFGetSystemTime() };
            let mut due = state.start_time + i64::try_from(state.frame).unwrap_or(0) * FRAME_DURATION_100NS;
            if now > due + 2 * FRAME_DURATION_100NS {
                // Far behind (a slow consumer): jump to the frame that is current rather than replaying the backlog.
                state.frame = u64::try_from((now - state.start_time) / FRAME_DURATION_100NS).unwrap_or(0);
                due = state.start_time + i64::try_from(state.frame).unwrap_or(0) * FRAME_DURATION_100NS;
            }
            if now < due {
                // `thread::sleep` rather than a condvar timeout: on Windows a condvar timeout rounds up to the 15.6 ms
                // system timer tick, while `sleep` uses a high-resolution timer, and a frame due in under 33 ms cannot
                // tolerate the former. Shutdown waits at most one frame period for the sleep to end.
                let wait = Duration::from_nanos(u64::try_from((due - now) * 100).unwrap_or(0));
                drop(state);
                std::thread::sleep(wait);
                state = lock(self);
                continue;
            }
            let frame = state.frame;
            state.frame += 1;
            let token = state.tokens.pop_front().flatten();
            return Some(Job { token, frame, due, format: state.format });
        }
    }

    fn deliver(&self, token: Option<&Agile<IUnknown>>, due: i64, frame: &Prepared) -> Result<()> {
        let size = u32::try_from(frame.pixels.len()).map_err(|_| windows::core::Error::from(E_INVALIDARG))?;
        // A new buffer per frame keeps the spike simple; a product pools them (MFCreateVideoSampleAllocatorEx).
        // SAFETY: the buffer is locked for exactly the span of the write and its length is `size`.
        let sample = unsafe {
            let buffer = MFCreateMemoryBuffer(size)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&raw mut data, None, None)?;
            std::slice::from_raw_parts_mut(data, size as usize).copy_from_slice(&frame.pixels);
            buffer.Unlock()?;
            buffer.SetCurrentLength(size)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(due)?;
            sample.SetSampleDuration(FRAME_DURATION_100NS)?;
            if let Some(Agile(token)) = token {
                sample.SetUnknown(&MFSampleExtension_Token, token)?;
            }
            sample
        };
        // SAFETY: the sample is a valid COM object for the call.
        unsafe { self.events.0.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample) }
    }

    fn playback_state(&self) -> MF_STREAM_STATE {
        lock(self).playback
    }

    fn set_playback(&self, value: MF_STREAM_STATE) {
        lock(self).playback = value;
        self.wake.notify_all();
    }

    fn events(&self) -> Result<IMFMediaEventQueue> {
        if lock(self).shutdown { Err(MF_E_SHUTDOWN.into()) } else { Ok(self.events.0.clone()) }
    }
}

/// The COM face of the stream.
#[implement(IMFMediaStream2)]
pub struct MediaStream {
    shared: Arc<StreamShared>,
    _live: LiveGuard,
}

impl MediaStream {
    pub fn new(shared: Arc<StreamShared>) -> Self {
        Self { shared, _live: LiveGuard::new() }
    }
}

impl Drop for MediaStream {
    fn drop(&mut self) {
        self.shared.shutdown();
    }
}

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        // GetEvent can block, so the queue is cloned out and no lock is held while waiting.
        let queue = self.shared.events()?;
        // SAFETY: the queue is a valid COM object.
        unsafe { queue.GetEvent(flags.0) }
    }

    fn BeginGetEvent(&self, callback: Ref<'_, IMFAsyncCallback>, state: Ref<'_, IUnknown>) -> Result<()> {
        // SAFETY: the queue is a valid COM object; the references are valid for the call.
        unsafe { self.shared.events()?.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }

    fn EndGetEvent(&self, result: Ref<'_, IMFAsyncResult>) -> Result<IMFMediaEvent> {
        // SAFETY: as above.
        unsafe { self.shared.events()?.EndGetEvent(result.as_ref()) }
    }

    fn QueueEvent(&self, kind: u32, extended: *const GUID, status: HRESULT, value: *const PROPVARIANT) -> Result<()> {
        // SAFETY: the pointers come from the caller of the COM method and are valid for the call.
        unsafe { self.shared.events()?.QueueEventParamVar(kind, extended, status, value) }
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        let source = self.shared.source.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        source.as_ref().map(|source| source.0.clone()).ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        Ok(self.shared.descriptor.0.clone())
    }

    fn RequestSample(&self, token: Ref<'_, IUnknown>) -> Result<()> {
        self.shared.request(token.as_ref().cloned())
    }
}

impl IMFMediaStream2_Impl for MediaStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> Result<()> {
        self.shared.set_playback(value);
        Ok(())
    }

    fn GetStreamState(&self) -> Result<MF_STREAM_STATE> {
        Ok(self.shared.playback_state())
    }
}
