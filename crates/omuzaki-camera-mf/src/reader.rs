//! Media Foundation capture: device enumeration and an asynchronous source reader that hands out raw frames.
//!
//! The reader runs in asynchronous mode so `next_frame` can wait with a timeout: Media Foundation delivers each
//! sample to a callback, which puts it in a [`Slot`]; the caller takes from the slot with a timed wait.
//!
//! This is the one place with `unsafe`: every call here crosses into COM, whose bindings are `unsafe` by nature.
//!
//! ## Lifetimes
//!
//! - Media Foundation is started once and shared (`Arc<MfRuntime>`): the camera and every stream hold it, and it is
//!   shut down when the last of them is gone. A stream declares it as its last field so it outlives every interface.
//! - COM is per thread, and a stream may move between threads, so the apartment is a thread-local guard
//!   ([`enter_com`]): every entry point calls it, the first call on a thread runs `CoInitializeEx` and the guard
//!   keeps the apartment until the thread exits. `next_frame` therefore pays one thread-local read per frame, not an
//!   apartment set-up and tear-down. Every interface is released on a thread whose guard is still alive: streams drop
//!   their interfaces explicitly inside the reaper thread's work, never from a thread-local destructor.
//! - The callback owns its own `Arc<Shared>`, so a late callback after the stream is gone touches only memory it owns.
//!   The one thing it borrows, the reader it re-arms, is detached (and awaited) before the reader is released.
//!
//! ## From callback to caller (the real-time path)
//!
//! 1. Media Foundation calls `OnReadSample` on a worker thread; the callback turns the sample into a token (an
//!    `AddRef`, no allocation) and puts it in the [`TokenSlot`]: one atomic swap that displaces and releases the older,
//!    unread frame (newest wins).
//! 2. It sets the slot's [`EventSignal`] (a kernel event, no lock of ours) and re-arms the next `ReadSample` itself, so
//!    capture never waits for the caller and a late frame is simply replaced.
//! 3. `next_frame` swaps the token out (or waits on the event with the timeout), rebuilds the sample, borrows its
//!    single buffer directly, checks it against the real allocation and returns the borrow.
//!
//! No step locks or allocates. **The rule:** everything `next_frame` can use is sized once, at open, for the largest
//! mode the device offers ([`Capacity`]): the gather buffer for samples delivered in several pieces, and the top-down
//! copy for bottom-up BGRA frames. A renegotiation only changes how much of them is used; a mode beyond them (a driver
//! that lied about its modes) fails the stream with an error. Nothing reachable from `next_frame` creates a buffer,
//! grows a `Vec` or resizes anything.
#![allow(unsafe_code)] // Win32/COM FFI has no safe binding; each block carries its own `// SAFETY:` comment.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use omuzaki_track_api::camera::{
    Camera, CameraFormat, CameraFormatRequest, CameraFrame, CameraId, CameraInfo, CameraStream, DeviceTime, PixelFormat,
};
use omuzaki_track_api::error::TrackError;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMF2DBuffer2, IMFActivate, IMFAttributes, IMFMediaBuffer, IMFMediaSource, IMFMediaType, IMFSample,
    IMFSourceReader, IMFSourceReaderCallback, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
    MF_E_NO_MORE_TYPES, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE, MF_READWRITE_DISABLE_CONVERTERS, MF_SOURCE_READER_ASYNC_CALLBACK,
    MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
    MF2DBuffer_LockFlags_Read, MFCreateAttributes, MFCreateMemoryBuffer, MFCreateSourceReaderFromMediaSource,
    MFEnumDeviceSources, MFGetStrideForBitmapInfoHeader, MFSTARTUP_FULL, MFSampleExtension_DeviceTimestamp,
    MFSampleExtension_Discontinuity, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::core::{GUID, Interface, PCWSTR, PWSTR};

use crate::modes::{
    Capacity, CaptureClock, ClockSource, FrameSpan, ModeInfo, NativeSizes, RowFlip, SeqTracker, Signal, SlotFailure,
    Take, TokenSlot, choose_mode, frame_span, is_progressive, map_hresult, reap, resolve_stride, subtype_format,
    take_after_retype,
};

/// The reader's first video stream, as the `u32` the calls take.
const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned();
/// Sample times are in units of 100 ns.
const NS_PER_TICK: u64 = 100;
/// How long dropping a stream waits for its camera to be released before it returns anyway. A tracker has 100 ms to
/// stop (`track-api.md` §9), so this stays well inside it: a healthy driver releases in a few milliseconds, and a stalled
/// one is finished by the reaper thread while `open` of the same camera waits and then fails with [`TrackError::Busy`].
const DROP_WAIT: Duration = Duration::from_millis(50);
/// How long `open` waits for an earlier stream of the same camera, still being released, before it reports the camera
/// busy. More than the two [`FLUSH_WAIT`]s (callbacks in flight, then the flush) that bound the reaper's work, so a
/// release that is going to finish does so inside it.
const REOPEN_WAIT: Duration = Duration::from_secs(5);
/// How long a dropping stream waits for Media Foundation to confirm its flush. The camera is released either way;
/// the wait only lets in-flight callbacks finish first.
const FLUSH_WAIT: Duration = Duration::from_secs(2);

/// Pairs `MFStartup` with `MFShutdown`. Startup is process-wide and reference-counted, so the guard may drop on any
/// thread. Shared by `Arc` so the last owner, camera or stream, shuts it down.
struct MfRuntime;

impl MfRuntime {
    fn start() -> windows::core::Result<Self> {
        // SAFETY: plain call; a failed startup returns an error and needs no shutdown.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL)? };
        Ok(Self)
    }
}

impl Drop for MfRuntime {
    fn drop(&mut self) {
        // SAFETY: pairs the successful `MFStartup` in `start`. A shutdown failure leaves nothing to recover.
        let _ = unsafe { MFShutdown() };
    }
}

/// How a thread stands with COM.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Standing {
    /// `CoInitializeEx` has not succeeded on this thread yet.
    Outside,
    /// This guard holds a `CoInitializeEx` reference, released when the thread exits.
    Owner,
    /// The thread was in another apartment at the last probe (`RPC_E_CHANGED_MODE`): usable as it is, nothing of ours
    /// to release, and probed again on the next call because that apartment may be gone by then.
    Guest,
}

/// The calling thread's COM apartment, entered once and kept until the thread exits (see [`enter_com`]).
struct Apartment {
    standing: Cell<Standing>,
    /// How many times `CoInitializeEx` ran on this thread: the counting seam behind the "once per thread" test (a guest
    /// thread counts every probe).
    inits: Cell<u32>,
}

impl Apartment {
    const fn new() -> Self {
        Self { standing: Cell::new(Standing::Outside), inits: Cell::new(0) }
    }

    fn enter(&self) -> Result<(), TrackError> {
        // Only a reference of ours is cached. A guest apartment is somebody else's and can be torn down under us, so it
        // is probed again every time: `CoInitializeEx` on a thread already in another apartment just returns
        // `RPC_E_CHANGED_MODE`, and if that apartment is gone it now succeeds and the guard becomes the owner.
        if self.standing.get() == Standing::Owner {
            return Ok(());
        }
        self.inits.set(self.inits.get().saturating_add(1));
        // SAFETY: plain call. `S_OK` and `S_FALSE` both add a reference that `Drop` releases; a thread already in
        // another apartment (`RPC_E_CHANGED_MODE`) is usable as it is and takes no reference.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            self.standing.set(Standing::Guest);
            return Ok(());
        }
        hr.ok().map_err(|e| map_hresult(e.code().0, &CameraId(String::new())))?;
        self.standing.set(Standing::Owner);
        Ok(())
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.standing.get() == Standing::Owner {
            // SAFETY: pairs the `CoInitializeEx` that took a reference in `enter`, on the same thread (a
            // thread-local is destroyed on the thread that owns it).
            unsafe { CoUninitialize() };
        }
    }
}

thread_local! {
    static APARTMENT: Apartment = const { Apartment::new() };
}

/// Puts the calling thread in the multithreaded apartment, once: the first call on a thread runs `CoInitializeEx`, every
/// later call is a thread-local read, and the apartment lives until the thread exits. A thread that is a guest in someone else's apartment is probed on every call instead, because that apartment is not ours to keep alive. COM is per thread, so every entry
/// point that touches Media Foundation calls this; the guard is thread-local rather than owned by the stream because a
/// stream may be moved to another thread (`next_frame` on a capture loop's thread, the drop on the reaper's). A scope
/// per call would tear the apartment down after every frame whenever the thread has no apartment of its own.
fn enter_com() -> Result<(), TrackError> {
    APARTMENT
        .try_with(Apartment::enter)
        .unwrap_or_else(|_| Err(TrackError::Backend("the thread is exiting; COM can no longer be entered".into())))
}

/// How many times `CoInitializeEx` ran on the calling thread.
fn com_init_calls() -> u32 {
    APARTMENT.try_with(|a| a.inits.get()).unwrap_or(0)
}

/// Says when a dropped stream's camera is released. Shared by the dropping thread, which waits a little, and the reaper,
/// which finishes it; `open` of the same camera waits on it too.
struct Release {
    done: Mutex<bool>,
    changed: Condvar,
}

impl Release {
    /// Waits up to `timeout`; whether the camera was released by then.
    fn wait(&self, timeout: Duration) -> bool {
        let guard = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        let (done, _) =
            self.changed.wait_timeout_while(guard, timeout, |done| !*done).unwrap_or_else(PoisonError::into_inner);
        *done
    }
}

/// The releases in progress, by camera id. A stream that is dropped adds one; its reaper's [`ReleaseGuard`] removes it.
static RELEASING: Mutex<Vec<(CameraId, Arc<Release>)>> = Mutex::new(Vec::new());

/// Held by whoever releases a camera; dropping it (when the release is done, or the work is abandoned or panics)
/// finishes the release and wakes the waiters. If the reaper thread could not be started the guard is leaked with the
/// stream and the camera stays "being released": `open` then reports it busy, which is the truth.
struct ReleaseGuard {
    id: CameraId,
    release: Arc<Release>,
}

impl ReleaseGuard {
    /// Registers a release of camera `id` as under way.
    fn begin(id: &CameraId) -> Self {
        let release = Arc::new(Release { done: Mutex::new(false), changed: Condvar::new() });
        RELEASING.lock().unwrap_or_else(PoisonError::into_inner).push((id.clone(), Arc::clone(&release)));
        Self { id: id.clone(), release }
    }
}

impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        RELEASING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(id, release)| !(*id == self.id && Arc::ptr_eq(release, &self.release)));
        *self.release.done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.release.changed.notify_all();
    }
}

/// Waits up to `timeout` for every release of camera `id` that is under way.
///
/// # Errors
/// [`TrackError::Busy`] if one is still running at the deadline: the earlier stream's driver has not let go yet.
fn await_released(id: &CameraId, timeout: Duration) -> Result<(), TrackError> {
    let pending: Vec<Arc<Release>> = RELEASING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|(held, _)| held == id)
        .map(|(_, release)| Arc::clone(release))
        .collect();
    let deadline = Instant::now() + timeout;
    for release in pending {
        if !release.wait(deadline.saturating_duration_since(Instant::now())) {
            return Err(TrackError::Busy(id.clone()));
        }
    }
    Ok(())
}

/// The Media Foundation camera source.
pub struct MfCamera {
    runtime: Arc<MfRuntime>,
}

impl MfCamera {
    /// Starts Media Foundation.
    ///
    /// # Errors
    /// [`TrackError::Backend`] if Media Foundation is missing or fails to start.
    pub fn new() -> Result<Self, TrackError> {
        let runtime = MfRuntime::start().map_err(|e| map_hresult(e.code().0, &CameraId(String::new())))?;
        Ok(Self { runtime: Arc::new(runtime) })
    }
}

/// A device's symbolic link and friendly name, with the activation object that opens it.
struct Device {
    activate: IMFActivate,
    id: CameraId,
    name: String,
}

fn err(hr: &windows::core::Error, id: &CameraId) -> TrackError {
    map_hresult(hr.code().0, id)
}

fn no_object() -> windows::core::Error {
    windows::core::Error::from(windows::Win32::Foundation::E_POINTER)
}

/// Reads a string attribute; `None` if the object has none.
fn string_attr(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    let mut text = PWSTR::null();
    let mut len = 0_u32;
    // SAFETY: `text` and `len` outlive the call; on success Media Foundation fills them with a CoTaskMem string of
    // `len` UTF-16 units.
    unsafe { attrs.GetAllocatedString(key, &raw mut text, &raw mut len) }.ok()?;
    // SAFETY: on success `text` points at `len` valid UTF-16 units, owned by us until freed below.
    let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text.0, len as usize) });
    // SAFETY: `text` came from `GetAllocatedString`, which allocates with CoTaskMemAlloc, and is not used again.
    unsafe { CoTaskMemFree(Some(text.0.cast())) };
    Some(value)
}

/// Every video capture device present. Needs the thread to have entered COM ([`enter_com`]).
fn devices() -> windows::core::Result<Vec<Device>> {
    let mut query = None;
    // SAFETY: `query` outlives the call and receives the new attribute store.
    unsafe { MFCreateAttributes(&raw mut query, 1)? };
    let query: IMFAttributes = query.ok_or_else(no_object)?;
    // SAFETY: both GUIDs are valid statics for the call.
    unsafe { query.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)? };

    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0_u32;
    // SAFETY: `list` and `count` outlive the call; on success `list` is a CoTaskMem array of `count` activators.
    unsafe { MFEnumDeviceSources(&query, &raw mut list, &raw mut count)? };
    let mut out = Vec::with_capacity(count as usize);
    if !list.is_null() {
        // SAFETY: Media Foundation returned `count` initialised entries at `list`.
        let slice = unsafe { std::slice::from_raw_parts_mut(list, count as usize) };
        for slot in slice.iter_mut() {
            // Taking the entry moves its reference out, so freeing the array below releases nothing twice.
            if let Some(activate) = slot.take() {
                // An activator is an attribute store, so its attributes are read through it.
                let id = string_attr(&activate, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK);
                let name = string_attr(&activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME);
                if let Some(id) = id {
                    let name = name.unwrap_or_else(|| id.clone());
                    out.push(Device { activate, id: CameraId(id), name });
                }
            }
        }
        // SAFETY: `list` is the CoTaskMem array from `MFEnumDeviceSources`; every entry was taken above.
        unsafe { CoTaskMemFree(Some(list.cast())) };
    }
    Ok(out)
}

/// A kernel event the capture callback sets and the caller waits on: the [`Signal`] of the slot. Auto-reset, so a
/// `notify` before the `wait` is kept (latched) and wakes it at once.
struct EventSignal(HANDLE);

impl EventSignal {
    fn new() -> windows::core::Result<Self> {
        // SAFETY: plain call; no security attributes, auto-reset, initially clear, unnamed.
        unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.map(Self)
    }
}

// SAFETY: an event handle may be set and waited on from any thread; it is closed once, when the last owner drops.
unsafe impl Send for EventSignal {}
// SAFETY: as above.
unsafe impl Sync for EventSignal {}

impl Signal for EventSignal {
    fn notify(&self) {
        // SAFETY: the handle is a live event owned by `self`. A failure leaves the taker to its timeout.
        let _ = unsafe { SetEvent(self.0) };
    }

    fn wait(&self, timeout: Duration) {
        // Round up so a wait never ends early, and stay below `INFINITE` (`u32::MAX`); the slot loops for the rest.
        let ms = u32::try_from(timeout.as_millis().saturating_add(1)).unwrap_or(u32::MAX - 1).min(u32::MAX - 1);
        // SAFETY: the handle is a live event owned by `self`.
        let _ = unsafe { WaitForSingleObject(self.0, ms) };
    }
}

impl Drop for EventSignal {
    fn drop(&mut self) {
        // SAFETY: closes the handle `new` created, once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// What a stream and its callback share. The callback holds its own `Arc`, so it never touches freed state.
struct Shared {
    slot: TokenSlot<EventSignal>,
    /// The reader the callback re-arms, borrowed (no reference of ours): the stream detaches it and waits for
    /// `in_flight` to reach 0 before it releases the reader.
    reader: AtomicPtr<c_void>,
    in_flight: AtomicUsize,
}

impl Shared {
    /// Asks the reader for the next sample, if it is still attached.
    fn rearm(&self) -> windows::core::Result<()> {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        let raw = self.reader.load(Ordering::SeqCst);
        let result = if raw.is_null() {
            Ok(())
        } else {
            // SAFETY: `raw` is the stream's live reader: the stream detaches it, then waits for `in_flight` to drop
            // to 0 (which this call holds above 0) before releasing it.
            match unsafe { IMFSourceReader::from_raw_borrowed(&raw) } {
                // SAFETY: plain call; the result arrives at this callback.
                Some(reader) => unsafe { reader.ReadSample(VIDEO, 0, None, None, None, None) },
                None => Ok(()),
            }
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        result
    }

    /// Releases the sample a token stands for.
    fn release(token: NonNull<c_void>) {
        // SAFETY: `into_token` made this token from a sample's raw pointer (`IUnknown::into_raw`, which keeps its
        // provenance) and it is released exactly once, by the slot (`put`, `drain`) or by the caller that took it.
        drop(unsafe { IMFSample::from_raw(token.as_ptr()) });
    }
}

/// A sample as a slot token: its raw COM pointer, which the token now owns a reference to.
fn into_token(sample: IMFSample) -> Option<NonNull<c_void>> {
    NonNull::new(sample.into_raw())
}

mod callback {
    //! The `implement` macro expands to code that trips two pedantic lints; the expansion is not ours to change.
    #![allow(clippy::ref_as_ptr, clippy::inline_always)]

    use std::sync::Arc;

    use windows::Win32::Foundation::E_POINTER;
    use windows::Win32::Media::MediaFoundation::{
        IMFMediaEvent, IMFSample, IMFSourceReaderCallback, IMFSourceReaderCallback_Impl,
        MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR,
        MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED,
    };
    use windows::core::{HRESULT, Ref, implement};

    use super::{Shared, into_token};
    use crate::modes::SlotFailure;

    /// Receives samples from the source reader on a Media Foundation worker thread, files the newest in the slot and
    /// asks for the next, so capture keeps running whether or not anyone is waiting.
    #[implement(IMFSourceReaderCallback)]
    pub(super) struct Callback {
        pub(super) shared: Arc<Shared>,
    }

    impl IMFSourceReaderCallback_Impl for Callback_Impl {
        fn OnReadSample(
            &self,
            status: HRESULT,
            _stream: u32,
            flags: u32,
            _time: i64,
            sample: Ref<'_, IMFSample>,
        ) -> windows::core::Result<()> {
            let shared = &self.shared;
            let ended = (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ENDOFSTREAM.0).cast_unsigned();
            let retyped = (MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 | MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED.0)
                .cast_unsigned();
            if status.is_err() {
                shared.slot.fail(SlotFailure::Hresult(status.0));
                return Ok(());
            }
            if flags & ended != 0 {
                shared.slot.fail(SlotFailure::Lost);
                return Ok(());
            }
            if shared.slot.is_closed() {
                return Ok(()); // the stream is going away; the sample is released on return
            }
            if flags & retyped != 0 {
                // Before the sample is put, so whoever takes it (or a later one) sees the change.
                shared.slot.set_retype();
            }
            // A stream tick has no sample: nothing to deliver.
            if let Some(sample) = sample.as_ref().cloned() {
                let Some(token) = into_token(sample) else {
                    shared.slot.fail(SlotFailure::Hresult(E_POINTER.0));
                    return Ok(());
                };
                shared.slot.put(token, Shared::release);
            }
            if let Err(e) = shared.rearm() {
                shared.slot.fail(SlotFailure::Hresult(e.code().0));
            }
            Ok(())
        }

        fn OnFlush(&self, _stream: u32) -> windows::core::Result<()> {
            self.shared.slot.mark_flushed();
            Ok(())
        }

        fn OnEvent(&self, _stream: u32, _event: Ref<'_, IMFMediaEvent>) -> windows::core::Result<()> {
            Ok(())
        }
    }
}

use callback::Callback;

/// Opens a source reader on the device, with video processing and converters off so frames stay raw. With a
/// `callback` the reader is asynchronous.
fn open_reader(device: &Device, callback: Option<&IMFSourceReaderCallback>) -> windows::core::Result<IMFSourceReader> {
    let mut attrs = None;
    // SAFETY: `attrs` outlives the call and receives the new attribute store.
    unsafe { MFCreateAttributes(&raw mut attrs, 3)? };
    let attrs: IMFAttributes = attrs.ok_or_else(no_object)?;
    // SAFETY: the keys are valid statics and `callback`, when present, is a live callback object.
    unsafe {
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 0)?;
        attrs.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1)?;
        if let Some(callback) = callback {
            attrs.SetUnknown(&MF_SOURCE_READER_ASYNC_CALLBACK, callback)?;
        }
    }
    // SAFETY: `activate` is a live activator from the enumeration.
    let source: IMFMediaSource = unsafe { device.activate.ActivateObject()? };
    // SAFETY: `source` and `attrs` are live for the call.
    unsafe { MFCreateSourceReaderFromMediaSource(&source, &attrs) }
}

/// The mode a native media type describes, with what it declares about its sample sizes; `None` for a subtype we do
/// not carry and for any mode that is not progressive (this reader cannot deinterlace).
fn mode_of(media: &IMFMediaType) -> windows::core::Result<Option<(ModeInfo, NativeSizes)>> {
    // SAFETY: the keys are valid statics and `media` is live.
    let (subtype, size, rate) = unsafe {
        (media.GetGUID(&MF_MT_SUBTYPE)?, media.GetUINT64(&MF_MT_FRAME_SIZE)?, media.GetUINT64(&MF_MT_FRAME_RATE)?)
    };
    // SAFETY: as above; each attribute may be absent, which is an error we read as "not declared".
    let (interlace, sample_size, stride) = unsafe {
        (
            media.GetUINT32(&MF_MT_INTERLACE_MODE).ok(),
            media.GetUINT32(&MF_MT_SAMPLE_SIZE).ok(),
            media.GetUINT32(&MF_MT_DEFAULT_STRIDE).ok(),
        )
    };
    // Both attributes pack two `u32`s into a `u64`: width and height, rate numerator and denominator.
    #[allow(clippy::cast_possible_truncation)] // each half is shifted or masked to 32 bits first
    let halves = |v: u64| ((v >> 32) as u32, (v & u64::from(u32::MAX)) as u32);
    let ((width, height), (fps_num, fps_den)) = (halves(size), halves(rate));
    // An absent interlace mode is progressive (see `is_progressive`).
    let mode = subtype_format(subtype.data1).filter(|_| is_progressive(interlace)).map(|format| ModeInfo {
        format,
        width,
        height,
        fps_num,
        fps_den,
    });
    let sizes = NativeSizes { sample_size, default_stride: stride.map(u32::cast_signed) };
    Ok(mode.map(|mode| (mode, sizes)))
}

/// Every progressive mode the reader's first video stream offers, with what its type says about sample sizes and the
/// media type that selects it.
fn modes(reader: &IMFSourceReader) -> windows::core::Result<Vec<(ModeInfo, NativeSizes, IMFMediaType)>> {
    let mut out = Vec::new();
    for index in 0.. {
        // SAFETY: plain call on a live reader.
        match unsafe { reader.GetNativeMediaType(VIDEO, index) } {
            Ok(media) => {
                if let Some((mode, sizes)) = mode_of(&media)? {
                    out.push((mode, sizes, media));
                }
            }
            Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

impl Camera for MfCamera {
    /// A device that cannot be probed (busy, or refusing access) is listed without formats.
    fn enumerate(&self) -> Vec<CameraInfo> {
        if enter_com().is_err() {
            return Vec::new();
        }
        let Ok(found) = devices() else { return Vec::new() };
        found
            .into_iter()
            .map(|d| {
                let formats = open_reader(&d, None)
                    .and_then(|r| modes(&r))
                    .map(|m| m.iter().map(|(mode, _, _)| mode.to_format()).collect())
                    .unwrap_or_default();
                // SAFETY: plain call; releases the camera the probe opened.
                let _ = unsafe { d.activate.ShutdownObject() };
                CameraInfo::new(d.id, d.name, formats)
            })
            .collect()
    }

    fn open(&mut self, id: &CameraId, request: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
        enter_com()?;
        // A stream of this camera dropped a moment ago may still be letting go of the device.
        await_released(id, REOPEN_WAIT)?;
        let stream = open_stream(&self.runtime, id, request)?;
        Ok(Box::new(stream))
    }
}

/// Opens `id` in the mode `request` picks. Runs on a thread that has entered COM ([`enter_com`]); everything it creates and does not
/// hand to the stream is released before it returns.
fn open_stream(runtime: &Arc<MfRuntime>, id: &CameraId, request: &CameraFormatRequest) -> Result<MfStream, TrackError> {
    let device = devices()
        .map_err(|e| err(&e, id))?
        .into_iter()
        .find(|d| d.id == *id)
        .ok_or_else(|| TrackError::NoDevice(id.clone()))?;
    let signal = EventSignal::new().map_err(|e| err(&e, id))?;
    let shared = Arc::new(Shared {
        slot: TokenSlot::new(signal),
        reader: AtomicPtr::new(std::ptr::null_mut()),
        in_flight: AtomicUsize::new(0),
    });
    let callback: IMFSourceReaderCallback = Callback { shared: Arc::clone(&shared) }.into();
    let configured = configure(&device, &callback, id, request);
    let (reader, mode, stride, capacity) = match configured {
        Ok(done) => done,
        Err(e) => {
            // SAFETY: plain call; releases the camera `configure` opened before it failed.
            let _ = unsafe { device.activate.ShutdownObject() };
            return Err(e);
        }
    };
    // The gather buffer is made now, once, for the largest sample of any mode.
    // SAFETY: plain call; the buffer is owned by the stream from here.
    let gather = match unsafe { MFCreateMemoryBuffer(u32::try_from(capacity.gather).unwrap_or(u32::MAX)) } {
        Ok(buffer) => buffer,
        Err(e) => {
            // SAFETY: plain call; releases the camera `configure` opened.
            let _ = unsafe { device.activate.ShutdownObject() };
            return Err(err(&e, id));
        }
    };
    let mut stream = MfStream {
        live: Some(Live { reader, activate: device.activate, held: None, gather, gather_len: capacity.gather }),
        shared,
        id: id.clone(),
        mode,
        flip: RowFlip::new(capacity.flip),
        capacity,
        clock: CaptureClock::new(),
        stride,
        seq: SeqTracker::new(mode.fps_num, mode.fps_den),
        packed_locks: 0,
        oversize_logged: false,
        failed: None,
        runtime: Arc::clone(runtime),
    };
    stream.size_flip()?;
    // Start capture: from here the callback re-arms itself. If this fails the stream is dropped, which shuts the
    // camera down.
    let live = stream.live.as_ref().ok_or(TrackError::Lost)?;
    stream.shared.reader.store(live.reader.as_raw(), Ordering::SeqCst);
    // SAFETY: plain call on the live reader; the result arrives at the callback.
    unsafe { live.reader.ReadSample(VIDEO, 0, None, None, None, None) }.map_err(|e| err(&e, id))?;
    Ok(stream)
}

/// Opens the device's reader and sets the mode `request` picks: the reader, the mode, whether its rows arrive
/// bottom-up, and the storage that every mode the device offers needs.
fn configure(
    device: &Device,
    callback: &IMFSourceReaderCallback,
    id: &CameraId,
    request: &CameraFormatRequest,
) -> Result<(IMFSourceReader, ModeInfo, isize, Capacity), TrackError> {
    let reader = open_reader(device, Some(callback)).map_err(|e| err(&e, id))?;
    let available = modes(&reader).map_err(|e| err(&e, id))?;
    let list: Vec<ModeInfo> = available.iter().map(|(m, _, _)| *m).collect();
    let mode = choose_mode(&list, request).ok_or(TrackError::FormatUnsupported(*request))?;
    let media = available.iter().find(|(m, _, _)| *m == mode).map(|(_, _, t)| t);
    let media = media.ok_or_else(|| TrackError::Backend("chosen mode vanished from the list".into()))?;
    // SAFETY: `media` is one of the reader's own native types, so no conversion is needed.
    unsafe { reader.SetCurrentMediaType(VIDEO, None, media) }.map_err(|e| err(&e, id))?;
    Ok((
        reader,
        mode,
        default_stride(media, &mode),
        Capacity::for_native(&available.iter().map(|(m, n, _)| (*m, *n)).collect::<Vec<_>>()),
    ))
}

/// The signed stride of a media type's rows (see [`resolve_stride`]), resolved once when the type is set. For a type
/// without `MF_MT_DEFAULT_STRIDE` it is what `MFGetStrideForBitmapInfoHeader` computes, so an RGB32 type that omits the
/// attribute still comes out negative (bottom-up).
fn default_stride(media: &IMFMediaType, mode: &ModeInfo) -> isize {
    // SAFETY: plain call on a live media type; an absent attribute is an error we read as "not declared".
    let declared = unsafe { media.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.ok();
    let compute = || {
        // The subtype's first 32 bits are the format code this call takes.
        // SAFETY: plain call; the subtype is read from the live type.
        let subtype = unsafe { media.GetGUID(&MF_MT_SUBTYPE) }.ok()?;
        // SAFETY: plain call with values read from the live type.
        unsafe { MFGetStrideForBitmapInfoHeader(subtype.data1, mode.width) }.ok()
    };
    resolve_stride(declared, compute, mode.format.tight_stride(mode.width).unwrap_or(0))
}

/// How a buffer was locked, which is also how it is unlocked.
enum Lock {
    /// `IMF2DBuffer2::Lock2DSize`: the real pitch and the real extent of the allocation.
    Sized(IMF2DBuffer2),
    /// `IMF2DBuffer::Lock2D`: the real pitch, for an unpadded frame (see `unpadded_back`).
    Surface(IMF2DBuffer),
    /// `IMFMediaBuffer::Lock`: packed rows; may copy a padded surface, so it is the last resort for one.
    Packed,
}

/// The locked buffer of the frame last handed out.
struct Held {
    buffer: IMFMediaBuffer,
    lock: Lock,
    /// The start of the locked allocation, and where the frame's lowest row begins within it (`span_len` bytes).
    base: *const u8,
    span_start: usize,
    span_len: usize,
    /// Bytes from one row to the next, top row first; negative when the top row is last in memory.
    pitch: isize,
}

impl Held {
    fn unlock(&self) {
        // SAFETY: the buffer was locked by the matching call in `lock_buffer`, and is unlocked exactly once (the
        // caller drops this value right after).
        let _ = unsafe {
            match &self.lock {
                Lock::Sized(b) => b.Unlock2D(),
                Lock::Surface(b) => b.Unlock2D(),
                Lock::Packed => self.buffer.Unlock(),
            }
        };
    }

    /// The frame's bytes, lowest row first.
    ///
    /// # Safety
    /// The lock must still be held, and the slice must not outlive it.
    unsafe fn bytes<'a>(&self) -> &'a [u8] {
        // SAFETY: `frame_span` checked that `span_start + span_len` lies inside the locked allocation at `base`; the
        // caller keeps the lock for `'a`.
        unsafe { std::slice::from_raw_parts(self.base.add(self.span_start), self.span_len) }
    }

    /// Checks the locked frame against its allocation, `alloc_len` bytes with the top row `scanline0` bytes in, and
    /// records where it lies.
    fn fit(&mut self, mode: &ModeInfo, can_flip: bool, alloc_len: usize, scanline0: usize) -> Result<(), &'static str> {
        if self.base.is_null() {
            return Err("the buffer has no address");
        }
        let span = if mode.format == PixelFormat::Mjpg {
            // An image has no rows; the whole locked length is the payload.
            if alloc_len == 0 { Err("empty image") } else { Ok(FrameSpan { start: 0, len: alloc_len }) }
        } else if self.pitch < 0 && !can_flip {
            Err("bottom-up frames of this layout are not supported")
        } else {
            frame_span(mode.format, mode.width, mode.height, self.pitch, scanline0, alloc_len)
        }?;
        self.span_start = span.start;
        self.span_len = span.len;
        Ok(())
    }
}

/// For a frame locked through `Lock2D` at `pitch`: how far the lowest row lies before the top row (0 unless the image is
/// bottom-up) if the frame is one run of pixel bytes. `None` for padded rows, a zero pitch, a layout with no rows and
/// every planar layout (NV12): `Lock2D` gives the first plane's pointer only, and `GetContiguousLength` is the size of
/// a packed copy, not a promise that the planes sit next to each other, so a planar frame without an extent is read
/// through the packed lock instead.
fn unpadded_back(mode: &ModeInfo, pitch: isize) -> Option<usize> {
    let step = pitch.unsigned_abs();
    if mode.format == PixelFormat::Nv12 || mode.format.tight_stride(mode.width) != Some(step) || step == 0 {
        return None;
    }
    if pitch < 0 { (mode.height as usize).checked_sub(1)?.checked_mul(step) } else { Some(0) }
}

/// What locking a buffer cost.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Cost {
    /// A 2-D lock, or a buffer with no 2-D interface: no copy.
    Free,
    /// The buffer has a 2-D interface that could not be used, so the packed `Lock` ran and may have copied the frame.
    MayCopy,
}

/// Locks `buffer`'s pixels and checks them against the allocation, in Microsoft's order: `Lock2DSize`, then
/// `Lock2D`, then the packed `Lock` as the last resort. The buffer stays locked until [`Held::unlock`].
fn lock_buffer(
    buffer: IMFMediaBuffer,
    mode: &ModeInfo,
    stride: isize,
    can_flip: bool,
    id: &CameraId,
) -> Result<(Held, Cost), TrackError> {
    let format = mode.format;
    let fail = |held: &Held, why: &str, alloc_len: usize| {
        held.unlock();
        TrackError::Backend(format!("unusable {format:?} buffer: {why} (pitch {}, {alloc_len} bytes)", held.pitch))
    };
    let mut held = Held { buffer, lock: Lock::Packed, base: std::ptr::null(), span_start: 0, span_len: 0, pitch: 0 };
    let mut cost = Cost::Free;
    if format != PixelFormat::Mjpg {
        if let Ok(sized) = held.buffer.cast::<IMF2DBuffer2>() {
            let (mut top, mut pitch, mut start, mut len) =
                (std::ptr::null_mut::<u8>(), 0_i32, std::ptr::null_mut::<u8>(), 0);
            // SAFETY: the out-pointers outlive the call; a success locks the buffer until `Unlock2D` and reports the
            // whole allocation (`start`, `len`), which `top` lies in.
            unsafe {
                sized.Lock2DSize(MF2DBuffer_LockFlags_Read, &raw mut top, &raw mut pitch, &raw mut start, &raw mut len)
            }
            .map_err(|e| err(&e, id))?;
            held.lock = Lock::Sized(sized);
            held.base = start;
            held.pitch = pitch as isize;
            // SAFETY: `top` and `start` both point into the allocation just locked.
            let scanline0 = usize::try_from(unsafe { top.offset_from(start) }).unwrap_or(usize::MAX);
            return match held.fit(mode, can_flip, len as usize, scanline0) {
                Ok(()) => Ok((held, cost)),
                Err(why) => Err(fail(&held, why, len as usize)),
            };
        }
        if let Ok(surface) = held.buffer.cast::<IMF2DBuffer>() {
            cost = Cost::MayCopy; // unless the surface turns out to be usable as it is
            let (mut top, mut pitch) = (std::ptr::null_mut::<u8>(), 0_i32);
            // SAFETY: the out-pointers outlive the call; a success locks the buffer until `Unlock2D`.
            unsafe { surface.Lock2D(&raw mut top, &raw mut pitch) }.map_err(|e| err(&e, id))?;
            // SAFETY: plain call on a live buffer.
            let contiguous = unsafe { surface.GetContiguousLength() }.unwrap_or(0) as usize;
            held.lock = Lock::Surface(surface);
            held.pitch = pitch as isize;
            // `Lock2D` gives the top row and the pitch but not the allocation, and padding is not promised readable, so
            // only an unpadded frame is borrowed: every byte of its span is pixel data, checked against the packed
            // length. A padded frame (or a pitch that cannot describe one) takes the packed lock below, which hands
            // over tight rows; `Lock2DSize`, above, is what borrows a padded frame, since it reports the extent.
            if let Some(back) = unpadded_back(mode, held.pitch) {
                held.base = top.wrapping_sub(back);
                if held.fit(mode, can_flip, contiguous, back).is_ok() {
                    return Ok((held, Cost::Free));
                }
            }
            held.unlock();
            held.lock = Lock::Packed;
            held.pitch = 0;
        }
    }
    let (mut start, mut len) = (std::ptr::null_mut::<u8>(), 0_u32);
    // SAFETY: the out-pointers outlive the call; a success locks the buffer until `Unlock`.
    unsafe { held.buffer.Lock(&raw mut start, None, Some(&raw mut len)) }.map_err(|e| err(&e, id))?;
    held.base = start;
    // `Lock` hands over rows at the type's default stride, negative for a bottom-up type.
    held.pitch = stride;
    let top = if held.pitch < 0 { (mode.height as usize).saturating_sub(1) * held.pitch.unsigned_abs() } else { 0 };
    match held.fit(mode, can_flip, len as usize, top) {
        Ok(()) => Ok((held, cost)),
        Err(why) => Err(fail(&held, why, len as usize)),
    }
}

/// The interfaces a stream owns. Released by the reaper thread after it entered COM, never as a bare field.
struct Live {
    reader: IMFSourceReader,
    /// Owns the camera; shut down on drop so it is released at once.
    activate: IMFActivate,
    held: Option<Held>,
    /// Where a sample with several buffers is gathered into one: made at open, `gather_len` bytes, never replaced.
    gather: IMFMediaBuffer,
    gather_len: usize,
}

impl Live {
    /// See [`contiguous`].
    fn contiguous(&mut self, sample: &IMFSample, id: &CameraId) -> Result<IMFMediaBuffer, TrackError> {
        contiguous(sample, &self.gather, self.gather_len, id)
    }
}

/// The sample's pixels as one buffer: its only buffer, borrowed as it is, or, for a sample in pieces, a copy in the
/// gather buffer made at open. A sample too big for that buffer is an error, never a new allocation.
fn contiguous(
    sample: &IMFSample,
    gather: &IMFMediaBuffer,
    gather_len: usize,
    id: &CameraId,
) -> Result<IMFMediaBuffer, TrackError> {
    // SAFETY: plain calls on a live sample.
    let count = unsafe { sample.GetBufferCount() }.map_err(|e| err(&e, id))?;
    if count == 1 {
        // SAFETY: as above; index 0 exists since the count is 1.
        return unsafe { sample.GetBufferByIndex(0) }.map_err(|e| err(&e, id));
    }
    // SAFETY: plain call on a live sample.
    let total = unsafe { sample.GetTotalLength() }.map_err(|e| err(&e, id))?;
    if total as usize > gather_len {
        return Err(TrackError::Backend(format!(
            "a sample of {total} bytes does not fit the {gather_len} bytes sized at open"
        )));
    }
    // SAFETY: both are live; the gather buffer is unlocked (the previous frame was released) and big enough.
    unsafe { sample.CopyToBuffer(gather) }.map_err(|e| err(&e, id))?;
    Ok(gather.clone())
}

// SAFETY: the reader and activator are free-threaded (multithreaded-apartment) objects; `held` points into a buffer
// that moves with them. All of it is touched only through `&mut MfStream`, one thread at a time, each time after `enter_com`.
unsafe impl Send for Live {}

struct MfStream {
    live: Option<Live>,
    shared: Arc<Shared>,
    id: CameraId,
    mode: ModeInfo,
    /// Where bottom-up frames are turned top-down: allocated at open for the largest BGRA mode, in use (by length)
    /// only while the mode is BGRA.
    flip: RowFlip,
    /// What was sized at open; a renegotiated mode must fit it.
    capacity: Capacity,
    clock: CaptureClock,
    /// The signed default stride of the current type: what a plain `Lock` hands over (negative: bottom-up).
    stride: isize,
    seq: SeqTracker,
    /// Frames locked the last-resort way (see [`Cost::MayCopy`]); logged when the first happens and again at drop.
    packed_locks: u64,
    /// The oversize-sample warning has been logged.
    oversize_logged: bool,
    /// Set when the camera changed to something we cannot hand over; every later call repeats it.
    failed: Option<TrackError>,
    /// Last, so Media Foundation outlives every interface above.
    runtime: Arc<MfRuntime>,
}

impl MfStream {
    /// Points the flip buffer at the current mode. It was sized at open; nothing is allocated here.
    fn size_flip(&mut self) -> Result<(), TrackError> {
        self.flip.set_len(Capacity::flip_len(&self.mode)).map_err(|e| TrackError::Backend(e.into()))
    }

    /// Re-reads the current media type after the camera renegotiated, so no sample is described by the old one. A
    /// mode that needs more storage than was sized at open is refused.
    fn retype(&mut self) -> Result<(), TrackError> {
        let live = self.live.as_ref().ok_or(TrackError::Lost)?;
        // SAFETY: plain call on the live reader.
        let media = unsafe { live.reader.GetCurrentMediaType(VIDEO) }.map_err(|e| err(&e, &self.id))?;
        // A format we do not carry, or a scan mode that is not progressive, ends the stream: fail closed.
        let (mode, _) = mode_of(&media).map_err(|e| err(&e, &self.id))?.ok_or_else(|| {
            TrackError::Backend("the camera changed to a format or scan mode that cannot be handed over".into())
        })?;
        self.capacity.admits(&mode).map_err(|e| TrackError::Backend(e.into()))?;
        self.stride = default_stride(&media, &mode);
        self.seq.set_rate(mode.fps_num, mode.fps_den);
        if mode != self.mode {
            self.mode = mode;
            self.size_flip()?;
        }
        Ok(())
    }

    /// Says which clock the capture times come from, when it is first known or changes.
    fn log_clock(id: &CameraId, source: ClockSource) {
        match source {
            ClockSource::Device => {
                tracing::info!(camera = %id.0, "capture times from the driver's device timestamp");
            }
            ClockSource::Sample => tracing::warn!(
                camera = %id.0,
                "the driver gives no device timestamp: capture times are the sample presentation times"
            ),
            ClockSource::SampleAligned => tracing::warn!(
                camera = %id.0,
                "a frame lacked the device timestamp: its sample time was moved onto the device clock"
            ),
        }
    }

    /// One frame from `token`, which this call now owns.
    fn frame_from(&mut self, token: NonNull<c_void>) -> Result<Option<CameraFrame<'_>>, TrackError> {
        // SAFETY: the token was made from a sample's raw pointer by `into_token`; this call owns it.
        let sample = unsafe { IMFSample::from_raw(token.as_ptr()) };
        let live = self.live.as_mut().ok_or(TrackError::Lost)?;
        // SAFETY: plain calls on a live sample; an absent attribute means no discontinuity or no driver timestamp.
        let sample_time = unsafe { sample.GetSampleTime() }.map_err(|e| err(&e, &self.id))?;
        let lost_before = unsafe { sample.GetUINT32(&MFSampleExtension_Discontinuity) }.is_ok_and(|v| v != 0);
        let device_time = unsafe { sample.GetUINT64(&MFSampleExtension_DeviceTimestamp) }.ok();
        let stamp = self.clock.stamp(device_time, sample_time);
        if stamp.first_of_source {
            Self::log_clock(&self.id, stamp.source);
        }
        let buffer = match live.contiguous(&sample, &self.id) {
            Ok(buffer) => buffer,
            Err(e) => {
                if !self.oversize_logged {
                    self.oversize_logged = true;
                    tracing::warn!(camera = %self.id.0, mode = ?self.mode, error = %e, "a sample did not fit the gather buffer");
                }
                return Err(e);
            }
        };
        let (held, cost) = lock_buffer(buffer, &self.mode, self.stride, !self.flip.is_empty(), &self.id)?;
        if cost == Cost::MayCopy {
            self.packed_locks += 1;
            if self.packed_locks == 1 {
                tracing::warn!(camera = %self.id.0, "camera buffers fell back to the packed Lock, which may copy every frame");
            }
        }
        let (width, height, pixel) = (self.mode.width, self.mode.height, self.mode.format);
        let captured = DeviceTime(stamp.ticks.saturating_mul(NS_PER_TICK));
        let seq = self.seq.next(i64::try_from(stamp.ticks).unwrap_or(i64::MAX), lost_before);
        // SAFETY: the lock is held until the next call to `next_frame`, which needs `&mut self` and so ends the
        // borrow this slice lives in first.
        let bytes = unsafe { held.bytes() };
        if held.pitch < 0 {
            let row_bytes = self.flip.len() / height as usize;
            let copied = self.flip.fill(bytes, held.pitch, row_bytes, height as usize);
            held.unlock();
            let top_down = copied.map_err(|e| TrackError::Backend(e.into()))?;
            return Ok(Some(CameraFrame::new(top_down, width, height, row_bytes, pixel, captured, seq)));
        }
        let stride = if pixel == PixelFormat::Mjpg { 0 } else { held.pitch.unsigned_abs() };
        live.held = Some(held);
        Ok(Some(CameraFrame::new(bytes, width, height, stride, pixel, captured, seq)))
    }
}

impl CameraStream for MfStream {
    fn format(&self) -> CameraFormat {
        self.mode.to_format()
    }

    /// The newest frame, waiting up to `timeout` for the device. `Ok(None)` means none arrived in time, or the driver
    /// sent an empty tick. A media-type change does not end the wait: the frame that came with it is dropped and the wait
    /// goes on, to the original deadline, for one in the new format. Capture does not depend on this call: the callback
    /// keeps the next read armed, so a frame that arrives after a timeout is replaced by newer ones until you ask.
    /// A bottom-up BGRA frame is copied top-down into a buffer allocated at open; no other layout copies, except a
    /// sample the driver delivers in several buffers. `seq` follows the capture times (see [`SeqTracker`]), so a
    /// skipped capture is a gap. If the camera switches to a format that cannot be handed over, this and every later
    /// call return that error.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
        enter_com()?;
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        let live = self.live.as_mut().ok_or(TrackError::Lost)?;
        // The previous frame's borrow ended with this call, so its buffer can be released.
        if let Some(held) = live.held.take() {
            held.unlock();
        }
        // A media-type change costs the wait nothing: after re-reading the type the wait goes on, up to the original
        // deadline, for a frame in the new format. (An `Arc` clone is a counter, not an allocation.)
        let shared = Arc::clone(&self.shared);
        let outcome = take_after_retype(&shared.slot, timeout, |stale| {
            // Whatever was waiting may predate the change, so it is dropped unused.
            if let Some(token) = stale {
                Shared::release(token);
            }
            self.retype().inspect_err(|e| self.failed = Some(e.clone()))
        })?;
        match outcome {
            Take::Item(token) => self.frame_from(token),
            Take::Timeout | Take::Retype(_) => Ok(None),
            Take::Failed(SlotFailure::Lost) | Take::Closed => Err(TrackError::Lost),
            Take::Failed(SlotFailure::Hresult(hr)) => Err(map_hresult(hr, &self.id)),
        }
    }
}

/// Hands a camera's resources to a reaper thread and waits up to `wait` for it to finish: `work` must drop the
/// [`ReleaseGuard`] that `make` puts in the resource once the camera is let go. Returns whether it had by then.
/// Separate from the stream so the drop-then-reopen sequence can be tested with a fake source.
fn release_and_wait<T: Send + 'static>(
    id: &CameraId,
    make: impl FnOnce(ReleaseGuard) -> T,
    work: fn(T),
    wait: Duration,
) -> bool {
    let release = ReleaseGuard::begin(id);
    let waiter = Arc::clone(&release.release);
    reap(make(release), work);
    waiter.wait(wait)
}

/// Everything a dropped stream hands to its reaper thread.
struct Reaped {
    live: Option<Live>,
    shared: Arc<Shared>,
    /// Kept so Media Foundation outlives the interfaces being released (and is never shut down under a leaked one).
    runtime: Arc<MfRuntime>,
    /// Dropped last, when the camera is let go (or the release is abandoned).
    release: ReleaseGuard,
}

impl Drop for MfStream {
    /// Waits up to [`DROP_WAIT`] for the camera to be released, so a drop followed by an `open` of the same camera
    /// finds it free. The release itself runs on a detached reaper thread (see [`reap`]): releasing a camera can take
    /// as long as its driver likes (a read stalled inside the driver, a flush that is never answered), and a tracker has
    /// 100 ms to stop (`track-api.md` §9), so this never waits longer. If the driver is slower than that, the drop
    /// returns, the reaper carries on, and `open` of the same camera waits for it (up to [`REOPEN_WAIT`]) and then
    /// fails with [`TrackError::Busy`] instead of racing the driver.
    fn drop(&mut self) {
        if self.packed_locks > 0 {
            tracing::warn!(camera = %self.id.0, frames = self.packed_locks, "frames locked through the packed Lock fallback");
        }
        let (live, shared, runtime) = (self.live.take(), Arc::clone(&self.shared), Arc::clone(&self.runtime));
        let released =
            release_and_wait(&self.id, |release| Reaped { live, shared, runtime, release }, reap_stream, DROP_WAIT);
        if !released {
            tracing::warn!(camera = %self.id.0, "the camera is still being released; reopening it waits for that");
        }
    }
}

/// The reaper thread's work: stop the callback re-arming, wait (bounded) for any in flight, flush the reader, wait
/// (bounded) for the flush to finish, shut the source down, and release every interface while COM is initialised.
/// If a callback is stuck inside the reader, releasing it would be a use after free, so it is leaked instead.
fn reap_stream(reaped: Reaped) {
    let Reaped { live, shared, runtime, release } = reaped;
    // The reaper is a fresh thread whose apartment outlives everything below. Without COM we still release what we can.
    if let Err(e) = enter_com() {
        tracing::warn!(error = %e, "releasing a camera without COM");
    }
    shared.slot.close();
    shared.reader.store(std::ptr::null_mut(), Ordering::SeqCst);
    // A callback that read the reader pointer before it was cleared is still using the reader: wait it out.
    let deadline = std::time::Instant::now() + FLUSH_WAIT;
    while shared.in_flight.load(Ordering::SeqCst) != 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    if let Some(mut live) = live {
        if shared.in_flight.load(Ordering::SeqCst) != 0 {
            // Leak the reader, and with it Media Foundation: shutting that down under a live reader is no better.
            std::mem::forget(live);
            std::mem::forget(runtime);
            // The camera is still held, so the release never finishes: `open` of it reports it busy.
            std::mem::forget(release);
            return;
        }
        if let Some(held) = live.held.take() {
            held.unlock();
        }
        // SAFETY: plain call on a live reader; completion is reported to `OnFlush`.
        if unsafe { live.reader.Flush(VIDEO) }.is_ok() {
            shared.slot.wait_flushed(FLUSH_WAIT);
        }
        // SAFETY: plain call; asks Media Foundation to release the camera now rather than at the last reference.
        let _ = unsafe { live.activate.ShutdownObject() };
        drop(live);
    }
    // Anything the callback delivered but nobody took.
    if let Some(token) = shared.slot.drain() {
        Shared::release(token);
    }
    drop(runtime);
}

/// What the benchmark (`benches/stream.rs`) drives: the per-frame path of `next_frame` on a synthetic sample, with no
/// device. It runs the same functions the stream does: the token hand-off through the event-woken slot, the buffer
/// choice, the lock, the bounds check, and the top-down copy. Not part of the API.
#[doc(hidden)]
pub mod bench_support {
    use super::{
        Arc, AtomicPtr, AtomicUsize, CameraId, EventSignal, IMFMediaBuffer, IMFSample, MFCreateMemoryBuffer, MfRuntime,
        ModeInfo, PixelFormat, RowFlip, Shared, Take, TokenSlot, TrackError, com_init_calls, contiguous, enter_com,
        err, into_token, lock_buffer,
    };
    use std::time::Duration;

    use windows::Win32::Media::MediaFoundation::MFCreateSample;

    /// How many times `CoInitializeEx` ran on the calling thread.
    pub fn com_inits() -> u32 {
        com_init_calls()
    }

    /// A synthetic one-buffer sample and everything the frame path needs around it.
    pub struct Harness {
        sample: IMFSample,
        gather: IMFMediaBuffer,
        gather_len: usize,
        shared: Arc<Shared>,
        mode: ModeInfo,
        stride: isize,
        flip: RowFlip,
        id: CameraId,
        // Dropped last: Media Foundation after the interfaces.
        _runtime: MfRuntime,
    }

    impl Harness {
        /// A `width` x `height` sample of `format` filled with a pattern, as a bottom-up image if `stride` is negative.
        ///
        /// # Errors
        /// If Media Foundation cannot start or build the sample.
        pub fn new(format: PixelFormat, width: u32, height: u32, stride: isize) -> Result<Self, TrackError> {
            let id = CameraId("bench".into());
            enter_com()?;
            let runtime = MfRuntime::start().map_err(|e| err(&e, &id))?;
            let mode = ModeInfo { format, width, height, fps_num: 30, fps_den: 1 };
            let row = format.tight_stride(width).unwrap_or(0);
            let len = format.frame_len(height, stride.unsigned_abs().max(row)).unwrap_or(row * height as usize);
            let bytes = u32::try_from(len).map_err(|_| TrackError::Backend("frame too large".into()))?;
            // SAFETY: plain calls; the buffer and sample are owned by this harness.
            let (gather, sample) = unsafe {
                let buffer = MFCreateMemoryBuffer(bytes).map_err(|e| err(&e, &id))?;
                let (mut start, mut max) = (std::ptr::null_mut::<u8>(), 0_u32);
                buffer.Lock(&raw mut start, Some(&raw mut max), None).map_err(|e| err(&e, &id))?;
                // SAFETY: the lock gives `max` writable bytes at `start`.
                for (i, byte) in std::slice::from_raw_parts_mut(start, max as usize).iter_mut().enumerate() {
                    *byte = (i & 0xFF).try_into().unwrap_or(0); // a repeating pattern
                }
                buffer.Unlock().map_err(|e| err(&e, &id))?;
                buffer.SetCurrentLength(bytes).map_err(|e| err(&e, &id))?;
                let sample = MFCreateSample().map_err(|e| err(&e, &id))?;
                sample.AddBuffer(&buffer).map_err(|e| err(&e, &id))?;
                (MFCreateMemoryBuffer(bytes).map_err(|e| err(&e, &id))?, sample)
            };
            let flip_len = if stride < 0 { row * height as usize } else { 0 };
            let mut flip = RowFlip::new(flip_len);
            flip.set_len(flip_len).map_err(|e| TrackError::Backend(e.into()))?;
            let signal = EventSignal::new().map_err(|e| err(&e, &id))?;
            let shared = Arc::new(Shared {
                slot: TokenSlot::new(signal),
                reader: AtomicPtr::new(std::ptr::null_mut()),
                in_flight: AtomicUsize::new(0),
            });
            Ok(Self { sample, gather, gather_len: len, shared, mode, stride, flip, id, _runtime: runtime })
        }

        /// One `next_frame` without a device: enter COM as the real call does (a thread-local read once the thread
        /// has an apartment), then the hand-off and the lock. Returns the bytes of the frame.
        ///
        /// # Errors
        /// As `next_frame` would.
        pub fn next_frame(&mut self) -> Result<usize, TrackError> {
            enter_com()?;
            if !self.hand_off() {
                return Err(TrackError::Lost);
            }
            self.lock_and_read()
        }

        /// The callback's side and the caller's: put the sample in the slot (waking the event) and take it back.
        /// Returns whether it came out.
        pub fn hand_off(&self) -> bool {
            let Some(token) = into_token(self.sample.clone()) else { return false };
            self.shared.slot.put(token, Shared::release);
            match self.shared.slot.take(Duration::ZERO) {
                Take::Item(token) => {
                    Shared::release(token);
                    true
                }
                _ => false,
            }
        }

        /// The caller's side after the hand-off: choose the buffer, lock it, check it against its allocation, take
        /// the top-down copy if the image is bottom-up, and release the lock. Returns the bytes of the frame.
        ///
        /// # Errors
        /// As `next_frame` would.
        pub fn lock_and_read(&mut self) -> Result<usize, TrackError> {
            let buffer = contiguous(&self.sample, &self.gather, self.gather_len, &self.id)?;
            let (held, _) = lock_buffer(buffer, &self.mode, self.stride, !self.flip.is_empty(), &self.id)?;
            // SAFETY: the lock is held until `unlock` below, after the slice's last use.
            let bytes = unsafe { held.bytes() };
            let used = if held.pitch < 0 {
                let row = self.flip.len() / self.mode.height as usize;
                match self.flip.fill(bytes, held.pitch, row, self.mode.height as usize) {
                    Ok(top_down) => top_down.len(),
                    Err(e) => {
                        held.unlock();
                        return Err(TrackError::Backend(e.into()));
                    }
                }
            } else {
                bytes.len()
            };
            held.unlock();
            Ok(used)
        }
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::E_ACCESSDENIED;
    use windows::Win32::Media::MediaFoundation::{
        MF_E_HW_MFT_FAILED_START_STREAMING, MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED,
    };

    use super::*;
    use crate::modes;

    /// The tables spell the codes out so they build on Linux; here they are checked against the bindings.
    #[test]
    fn hresult_constants_match_the_bindings() {
        assert_eq!(modes::E_ACCESSDENIED, E_ACCESSDENIED.0);
        assert_eq!(modes::MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED, MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED.0);
        assert_eq!(modes::MF_E_HW_MFT_FAILED_START_STREAMING, MF_E_HW_MFT_FAILED_START_STREAMING.0);
    }

    /// Media Foundation starts with the SDK version, twice over (the count is shared), and enumeration works with or
    /// without cameras.
    #[test]
    fn startup_and_enumeration_smoke() {
        let first = MfCamera::new().expect("Media Foundation starts");
        let second = MfCamera::new().expect("it starts again");
        drop(first);
        let _ = second.enumerate();
    }

    /// A stream keeps Media Foundation alive after its camera is gone (only when a camera is present).
    #[test]
    fn a_stream_outlives_its_camera() {
        let mut camera = MfCamera::new().expect("Media Foundation starts");
        let Some(info) = camera.enumerate().into_iter().find(|c| !c.formats.is_empty()) else { return };
        let Ok(mut stream) = camera.open(&info.id, &CameraFormatRequest::default()) else { return };
        drop(camera);
        let _ = stream.next_frame(Duration::from_millis(500));
    }

    /// A release that finishes lets a waiting `open` through; nothing releasing costs nothing.
    #[test]
    fn open_waits_for_a_release_in_progress() {
        let id = CameraId("release-finishes".into());
        assert!(await_released(&id, Duration::ZERO).is_ok(), "nothing is being released");
        let guard = ReleaseGuard::begin(&id);
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(guard);
        });
        assert!(await_released(&id, Duration::from_secs(10)).is_ok(), "the release finished inside the wait");
        worker.join().expect("the worker finished");
        assert!(RELEASING.lock().expect("registry").iter().all(|(held, _)| *held != id), "the entry is removed");
    }

    /// A release that does not finish within the wait is reported as a busy camera, after about the wait and no longer.
    #[test]
    fn open_reports_a_stuck_release_as_busy() {
        let id = CameraId("release-stuck".into());
        let _stuck = ReleaseGuard::begin(&id);
        let started = Instant::now();
        let result = await_released(&id, Duration::from_millis(100));
        assert!(matches!(result, Err(TrackError::Busy(ref busy)) if *busy == id), "got {result:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "the wait is bounded");
        // Another camera is not held up by it.
        assert!(await_released(&CameraId("another".into()), Duration::ZERO).is_ok());
    }

    /// The drop waits for a quick release, and gives up at `DROP_WAIT` on a slow one.
    #[test]
    fn a_waiting_drop_is_bounded() {
        let quick = ReleaseGuard::begin(&CameraId("drop-quick".into()));
        let waiter = Arc::clone(&quick.release);
        drop(quick);
        assert!(waiter.wait(DROP_WAIT), "a finished release is seen at once");
        let slow = ReleaseGuard::begin(&CameraId("drop-slow".into()));
        let started = Instant::now();
        assert!(!slow.release.wait(DROP_WAIT), "an unfinished release times out");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// With a camera present: every drop-then-reopen of the same camera succeeds. The test skips only when there is
    /// no camera or the first open is unavailable; after that a reopen that fails, `Busy` included, is a regression.
    #[test]
    fn a_dropped_camera_can_be_reopened_at_once() {
        let mut camera = MfCamera::new().expect("Media Foundation starts");
        let Some(info) = camera.enumerate().into_iter().find(|c| !c.formats.is_empty()) else { return };
        let Ok(first) = camera.open(&info.id, &CameraFormatRequest::default()) else { return };
        drop(first);
        for round in 0..3 {
            let stream = camera
                .open(&info.id, &CameraFormatRequest::default())
                .unwrap_or_else(|e| panic!("reopen {round} after a drop failed: {e}"));
            drop(stream);
        }
    }

    /// A fake source: `held` is true while it holds the device, and its reaper lets go after a delay.
    struct FakeSource {
        held: Arc<std::sync::atomic::AtomicBool>,
        delay: Duration,
        release: ReleaseGuard,
    }

    fn let_go(source: FakeSource) {
        std::thread::sleep(source.delay);
        source.held.store(false, Ordering::SeqCst);
        drop(source.release);
    }

    /// Drop, then reopen at once, with a source that takes a little while to let go: the drop waits for it, so the
    /// device is free when the reopen looks, and the reopen has nothing to wait for. No hardware needed.
    #[test]
    fn drop_then_reopen_finds_the_device_free() {
        let id = CameraId("fake-quick".into());
        let held = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let source = |release| FakeSource { held: Arc::clone(&held), delay: Duration::from_millis(20), release };
        assert!(release_and_wait(&id, source, let_go, Duration::from_secs(10)), "the drop saw the release");
        assert!(!held.load(Ordering::SeqCst), "the device was still held when the drop returned");
        assert!(await_released(&id, Duration::ZERO).is_ok(), "nothing is left to wait for");
    }

    /// A source slower than the drop's wait: the drop returns without it, the reopen then waits and finds the device
    /// free, and a reopen that cannot wait that long gets `Busy` rather than a racing open.
    #[test]
    fn a_slow_release_makes_the_reopen_wait_or_report_busy() {
        let id = CameraId("fake-slow".into());
        let held = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let source = |release| FakeSource { held: Arc::clone(&held), delay: Duration::from_millis(400), release };
        assert!(!release_and_wait(&id, source, let_go, Duration::from_millis(10)), "the drop gave up at its bound");
        assert!(held.load(Ordering::SeqCst), "the fake is still holding the device");
        assert!(matches!(await_released(&id, Duration::from_millis(10)), Err(TrackError::Busy(ref b)) if *b == id));
        assert!(await_released(&id, Duration::from_secs(10)).is_ok(), "a patient reopen is let through");
        assert!(!held.load(Ordering::SeqCst), "the device is free once the reopen is let through");
    }

    /// A thread that is a guest in another apartment is probed again on every call, so when that apartment goes the
    /// guard takes over instead of trusting a stale answer: STA up, use, STA down, use again.
    #[test]
    fn a_guest_thread_is_probed_again_after_its_apartment_goes() {
        use windows::Win32::System::Com::COINIT_APARTMENTTHREADED;

        std::thread::spawn(|| {
            let standing = || APARTMENT.with(|a| a.standing.get());
            // SAFETY: plain calls on this test's own thread; the STA is torn down below, once.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().expect("the STA starts");
            enter_com().expect("a guest thread is usable");
            assert!(standing() == Standing::Guest, "the STA is not ours");
            let before = com_init_calls();
            enter_com().expect("still usable");
            assert_eq!(com_init_calls(), before + 1, "a guest is probed on every call");
            // SAFETY: pairs the `CoInitializeEx` above.
            unsafe { CoUninitialize() };
            enter_com().expect("the thread is usable once the STA is gone");
            assert!(standing() == Standing::Owner, "the guard took over");
            let mut harness =
                bench_support::Harness::new(PixelFormat::Nv12, 64, 48, 64).expect("Media Foundation works again");
            harness.next_frame().expect("a frame after the STA went");
        })
        .join()
        .expect("the thread finished");
    }

    /// Two doubles of a 2-D buffer: both have `Lock2D`; `SizedSurface` also has `Lock2DSize` (the readable extent). The
    /// packed `Lock` of a `Surface` hands over tight rows, as Media Foundation's does; a `SizedSurface` refuses it.
    mod surface {
        #![allow(clippy::ref_as_ptr, clippy::inline_always)] // the `implement` expansion

        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL};
        use windows::Win32::Media::MediaFoundation::{
            IMF2DBuffer, IMF2DBuffer_Impl, IMF2DBuffer2, IMF2DBuffer2_Impl, IMFMediaBuffer, IMFMediaBuffer_Impl,
            MF2DBuffer_LockFlags,
        };
        use windows::core::{BOOL, Ref, implement};

        /// The memory, where the top row starts and the signed pitch, the tight copy a packed `Lock` hands over (empty
        /// to refuse it), and how many packed locks happened.
        pub(super) struct Layout {
            pub(super) memory: Vec<u8>,
            pub(super) top: usize,
            pub(super) pitch: i32,
            pub(super) packed: Vec<u8>,
            pub(super) packed_locks: Arc<AtomicUsize>,
        }

        macro_rules! buffer_impls {
            ($ty:ident, $imp:ident) => {
                impl IMFMediaBuffer_Impl for $imp {
                    fn Lock(&self, start: *mut *mut u8, _: *mut u32, len: *mut u32) -> windows::core::Result<()> {
                        self.0.packed_locks.fetch_add(1, Ordering::SeqCst);
                        if self.0.packed.is_empty() {
                            return Err(E_FAIL.into());
                        }
                        // SAFETY: the caller passes valid out-pointers; the copy is only read.
                        unsafe {
                            *start = self.0.packed.as_ptr().cast_mut();
                            *len = u32::try_from(self.0.packed.len()).unwrap_or(u32::MAX);
                        }
                        Ok(())
                    }
                    fn Unlock(&self) -> windows::core::Result<()> {
                        Ok(())
                    }
                    fn GetCurrentLength(&self) -> windows::core::Result<u32> {
                        Ok(u32::try_from(self.0.memory.len()).unwrap_or(u32::MAX))
                    }
                    fn SetCurrentLength(&self, _: u32) -> windows::core::Result<()> {
                        Ok(())
                    }
                    fn GetMaxLength(&self) -> windows::core::Result<u32> {
                        Ok(u32::try_from(self.0.memory.len()).unwrap_or(u32::MAX))
                    }
                }

                impl IMF2DBuffer_Impl for $imp {
                    fn Lock2D(&self, scanline0: *mut *mut u8, pitch: *mut i32) -> windows::core::Result<()> {
                        // SAFETY: valid out-pointers; `top` lies inside `memory`, which is only read.
                        unsafe {
                            *scanline0 = self.0.memory.as_ptr().add(self.0.top).cast_mut();
                            *pitch = self.0.pitch;
                        }
                        Ok(())
                    }
                    fn Unlock2D(&self) -> windows::core::Result<()> {
                        Ok(())
                    }
                    fn GetScanline0AndPitch(
                        &self,
                        scanline0: *mut *mut u8,
                        pitch: *mut i32,
                    ) -> windows::core::Result<()> {
                        self.Lock2D(scanline0, pitch)
                    }
                    fn IsContiguousFormat(&self) -> windows::core::Result<BOOL> {
                        Ok(BOOL(0))
                    }
                    fn GetContiguousLength(&self) -> windows::core::Result<u32> {
                        Ok(u32::try_from(self.0.packed.len()).unwrap_or(u32::MAX))
                    }
                    fn ContiguousCopyTo(&self, _: *mut u8, _: u32) -> windows::core::Result<()> {
                        Err(E_NOTIMPL.into())
                    }
                    fn ContiguousCopyFrom(&self, _: *const u8, _: u32) -> windows::core::Result<()> {
                        Err(E_NOTIMPL.into())
                    }
                }
            };
        }

        #[implement(IMFMediaBuffer, IMF2DBuffer)]
        pub(super) struct Surface(pub(super) Layout);
        buffer_impls!(Surface, Surface_Impl);

        #[implement(IMFMediaBuffer, IMF2DBuffer, IMF2DBuffer2)]
        pub(super) struct SizedSurface(pub(super) Layout);
        buffer_impls!(SizedSurface, SizedSurface_Impl);

        impl IMF2DBuffer2_Impl for SizedSurface_Impl {
            fn Lock2DSize(
                &self,
                _: MF2DBuffer_LockFlags,
                scanline0: *mut *mut u8,
                pitch: *mut i32,
                start: *mut *mut u8,
                len: *mut u32,
            ) -> windows::core::Result<()> {
                self.Lock2D(scanline0, pitch)?;
                // SAFETY: valid out-pointers; the whole of `memory` is the readable extent.
                unsafe {
                    *start = self.0.memory.as_ptr().cast_mut();
                    *len = u32::try_from(self.0.memory.len()).unwrap_or(u32::MAX);
                }
                Ok(())
            }
            fn Copy2DTo(&self, _: Ref<'_, IMF2DBuffer2>) -> windows::core::Result<()> {
                Err(E_NOTIMPL.into())
            }
        }
    }

    const IMAGE_ROWS: usize = 4;
    /// 32 YUY2 pixels: a row of 64 pixel bytes.
    const ROW_BYTES: usize = 64;

    /// Row `row` of the test image: every pixel byte is `row + 1`, so a misplaced row or a padding byte shows.
    fn marker(row: usize) -> u8 {
        u8::try_from(row + 1).unwrap_or(u8::MAX)
    }

    /// A 32x4 YUY2 image in memory with rows `pitch` bytes apart (the padding 0xEE), bottom-up if asked, as a layout
    /// whose packed copy has tight rows top-down.
    fn layout(
        pitch: usize,
        bottom_up: bool,
        packed_locks: &Arc<std::sync::atomic::AtomicUsize>,
        offer_packed: bool,
    ) -> surface::Layout {
        let mut memory = vec![0xEE_u8; pitch * IMAGE_ROWS];
        for r in 0..IMAGE_ROWS {
            let image_row = if bottom_up { IMAGE_ROWS - 1 - r } else { r };
            memory[r * pitch..r * pitch + ROW_BYTES].fill(marker(image_row));
        }
        let packed = if offer_packed {
            (0..IMAGE_ROWS).flat_map(|r| std::iter::repeat_n(marker(r), ROW_BYTES)).collect()
        } else {
            Vec::new()
        };
        let signed = i32::try_from(pitch).expect("a small pitch");
        let (top, signed) = if bottom_up { ((IMAGE_ROWS - 1) * pitch, -signed) } else { (0, signed) };
        surface::Layout { memory, top, pitch: signed, packed, packed_locks: Arc::clone(packed_locks) }
    }

    /// Locks `buffer` the way `next_frame` does and returns the cost, the pitch of the lock and the image's rows
    /// top-down, each only its pixel bytes (what a consumer reads through the stride).
    fn read_rows(buffer: IMFMediaBuffer) -> (Cost, isize, Vec<Vec<u8>>) {
        let mode = ModeInfo { format: PixelFormat::Yuy2, width: 32, height: 4, fps_num: 30, fps_den: 1 };
        let id = CameraId("surface".into());
        // The packed lock's default stride is the tight, top-down one.
        let (held, cost) = lock_buffer(buffer, &mode, ROW_BYTES.cast_signed(), true, &id).expect("the buffer locks");
        // SAFETY: the lock is held until `unlock` below, after the slice's last use.
        let bytes = unsafe { held.bytes() };
        let top_down: Vec<u8> = if held.pitch < 0 {
            let mut flip = RowFlip::new(ROW_BYTES * IMAGE_ROWS);
            flip.set_len(ROW_BYTES * IMAGE_ROWS).expect("the flip buffer is sized");
            flip.fill(bytes, held.pitch, ROW_BYTES, IMAGE_ROWS).expect("the rows flip").to_vec()
        } else {
            let step = held.pitch.unsigned_abs();
            (0..IMAGE_ROWS).flat_map(|r| bytes[r * step..r * step + ROW_BYTES].iter().copied()).collect()
        };
        let pitch = held.pitch;
        held.unlock();
        (cost, pitch, top_down.chunks(ROW_BYTES).map(<[u8]>::to_vec).collect())
    }

    fn assert_rows(rows: &[Vec<u8>]) {
        assert_eq!(rows.len(), IMAGE_ROWS);
        for (r, pixels) in rows.iter().enumerate() {
            assert!(pixels.iter().all(|&b| b == marker(r)), "row {r} has foreign bytes: {pixels:?}");
        }
    }

    /// A padded surface with a readable extent (`Lock2DSize`) is borrowed through its pitch: no packed lock, no copy,
    /// right pixels, either way up.
    #[test]
    fn a_padded_sized_surface_is_borrowed_through_its_pitch() {
        for bottom_up in [false, true] {
            let packed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let buffer: IMFMediaBuffer = surface::SizedSurface(layout(80, bottom_up, &packed, false)).into();
            let (cost, pitch, rows) = read_rows(buffer);
            assert!(cost == Cost::Free);
            assert_eq!(packed.load(Ordering::SeqCst), 0, "the packed Lock was used");
            assert_eq!(pitch, if bottom_up { -80 } else { 80 });
            assert_rows(&rows);
        }
    }

    /// With `Lock2D` alone (no extent), padding is not promised readable: a padded surface takes the packed lock and
    /// its tight rows, so no padding byte is ever read or handed over as a pixel.
    #[test]
    fn a_padded_surface_without_an_extent_takes_the_packed_lock() {
        let packed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let buffer: IMFMediaBuffer = surface::Surface(layout(80, false, &packed, true)).into();
        let (cost, pitch, rows) = read_rows(buffer);
        assert!(cost == Cost::MayCopy, "the fallback is counted");
        assert_eq!(packed.load(Ordering::SeqCst), 1);
        assert_eq!(pitch, ROW_BYTES.cast_signed());
        assert_rows(&rows);
    }

    /// An unpadded `Lock2D` surface is borrowed as it lies, either way up: no packed lock, no copy.
    #[test]
    fn an_unpadded_surface_is_borrowed_through_lock_2d() {
        for bottom_up in [false, true] {
            let packed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let buffer: IMFMediaBuffer = surface::Surface(layout(ROW_BYTES, bottom_up, &packed, true)).into();
            let (cost, pitch, rows) = read_rows(buffer);
            assert!(cost == Cost::Free);
            assert_eq!(packed.load(Ordering::SeqCst), 0, "the packed Lock was used");
            assert_eq!(pitch, if bottom_up { -(ROW_BYTES.cast_signed()) } else { ROW_BYTES.cast_signed() });
            assert_rows(&rows);
        }
    }

    /// Only unpadded rows are borrowed through `Lock2D` alone; a bottom-up one starts `height - 1` pitches back.
    #[test]
    fn unpadded_back_accepts_only_tight_rows() {
        let yuy2 = ModeInfo { format: PixelFormat::Yuy2, width: 32, height: 4, fps_num: 30, fps_den: 1 };
        assert_eq!(unpadded_back(&yuy2, 64), Some(0));
        assert_eq!(unpadded_back(&yuy2, -64), Some(192));
        assert_eq!(unpadded_back(&yuy2, 80), None);
        assert_eq!(unpadded_back(&yuy2, 0), None);
        assert_eq!(unpadded_back(&ModeInfo { format: PixelFormat::Mjpg, ..yuy2 }, 64), None);
        // A planar frame is never one run of bytes behind a single pointer.
        assert_eq!(unpadded_back(&ModeInfo { format: PixelFormat::Nv12, width: 64, ..yuy2 }, 64), None);
    }

    /// An NV12 surface with only `Lock2D`, whose Y pitch equals the width but whose UV plane is not next to the Y plane
    /// in memory: it is not borrowed as one span (that would read the gap as chroma); the packed lock's copy is used.
    #[test]
    fn a_planar_surface_with_apart_planes_takes_the_packed_lock() {
        const WIDTH: usize = 32;
        const HEIGHT: usize = 4;
        let mode = ModeInfo { format: PixelFormat::Nv12, width: 32, height: 4, fps_num: 30, fps_den: 1 };
        let (y_len, uv_len, gap) = (WIDTH * HEIGHT, WIDTH * HEIGHT / 2, 64);
        // Y, then a gap that is not chroma, then UV.
        let mut memory = vec![0xEE_u8; y_len + gap + uv_len];
        memory[..y_len].fill(0x10);
        memory[y_len + gap..].fill(0x80);
        let packed: Vec<u8> = std::iter::repeat_n(0x10, y_len).chain(std::iter::repeat_n(0x80, uv_len)).collect();
        let locks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let layout = surface::Layout {
            memory,
            top: 0,
            pitch: i32::try_from(WIDTH).expect("a small pitch"),
            packed: packed.clone(),
            packed_locks: Arc::clone(&locks),
        };
        let buffer: IMFMediaBuffer = surface::Surface(layout).into();
        let id = CameraId("planar".into());
        let (held, cost) = lock_buffer(buffer, &mode, WIDTH.cast_signed(), true, &id).expect("the buffer locks");
        // SAFETY: the lock is held until `unlock` below, after the slice's last use.
        let bytes = unsafe { held.bytes() }.to_vec();
        held.unlock();
        assert!(cost == Cost::MayCopy, "the fallback is counted");
        assert_eq!(locks.load(Ordering::SeqCst), 1, "the packed Lock was not used");
        assert_eq!(bytes, packed, "the chroma plane was read from the wrong place");
    }
}
