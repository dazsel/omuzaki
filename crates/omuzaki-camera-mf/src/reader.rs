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
//! - COM is per thread. Every entry point enters a [`ComScope`] first, and every interface is released while the
//!   scope is still alive: streams drop their interfaces explicitly inside `Drop`, not as fields after it.
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
//! No step locks or allocates. A multi-buffer sample is copied into one reusable buffer that grows only if a frame is
//! larger than any before; a bottom-up BGRA frame goes through a buffer sized at open; both change only when the
//! camera renegotiates its media type.
#![allow(unsafe_code)] // Win32/COM FFI has no safe binding; each block carries its own `// SAFETY:` comment.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::time::Duration;

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
    MF_E_NO_MORE_TYPES, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_SUBTYPE,
    MF_READWRITE_DISABLE_CONVERTERS, MF_SOURCE_READER_ASYNC_CALLBACK, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION, MF2DBuffer_LockFlags_Read, MFCreateAttributes,
    MFCreateMemoryBuffer, MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFSTARTUP_FULL,
    MFSampleExtension_Discontinuity, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::core::{GUID, Interface, PCWSTR, PWSTR};

use crate::modes::{
    FrameSpan, ModeInfo, SeqTracker, Signal, SlotFailure, Take, TokenSlot, choose_mode, copy_top_down, frame_span,
    map_hresult, subtype_format,
};

/// The reader's first video stream, as the `u32` the calls take.
const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned();
/// Sample times are in units of 100 ns.
const NS_PER_TICK: u64 = 100;
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

/// Puts the calling thread in the multithreaded apartment for a scope. COM is per thread, so every entry point
/// that touches Media Foundation holds one: the camera and its streams may move between threads. Declare it before
/// every interface it must outlive.
struct ComScope(bool);

impl ComScope {
    fn enter() -> Result<Self, TrackError> {
        // SAFETY: plain call. `S_OK` and `S_FALSE` both add a reference that `Drop` releases; a thread already in
        // another apartment (`RPC_E_CHANGED_MODE`) is usable as it is and takes no reference.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self(false));
        }
        hr.ok().map_err(|e| map_hresult(e.code().0, &CameraId(String::new())))?;
        Ok(Self(true))
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: pairs the `CoInitializeEx` that took a reference in `enter`, on the same thread.
            unsafe { CoUninitialize() };
        }
    }
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

/// Every video capture device present. Needs a [`ComScope`] that outlives the returned devices.
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

/// The mode a native media type describes, or `None` for a subtype we do not carry.
fn mode_of(media: &IMFMediaType) -> windows::core::Result<Option<ModeInfo>> {
    // SAFETY: the keys are valid statics and `media` is live.
    let (subtype, size, rate) = unsafe {
        (media.GetGUID(&MF_MT_SUBTYPE)?, media.GetUINT64(&MF_MT_FRAME_SIZE)?, media.GetUINT64(&MF_MT_FRAME_RATE)?)
    };
    // Both attributes pack two `u32`s into a `u64`: width and height, rate numerator and denominator.
    #[allow(clippy::cast_possible_truncation)] // each half is shifted or masked to 32 bits first
    let halves = |v: u64| ((v >> 32) as u32, (v & u64::from(u32::MAX)) as u32);
    let ((width, height), (fps_num, fps_den)) = (halves(size), halves(rate));
    Ok(subtype_format(subtype.data1).map(|format| ModeInfo { format, width, height, fps_num, fps_den }))
}

/// Every mode the reader's first video stream offers, with the media type that selects it.
fn modes(reader: &IMFSourceReader) -> windows::core::Result<Vec<(ModeInfo, IMFMediaType)>> {
    let mut out = Vec::new();
    for index in 0.. {
        // SAFETY: plain call on a live reader.
        match unsafe { reader.GetNativeMediaType(VIDEO, index) } {
            Ok(media) => {
                if let Some(mode) = mode_of(&media)? {
                    out.push((mode, media));
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
        // Declared first so it is released last: every interface below goes before it.
        let Ok(_com) = ComScope::enter() else { return Vec::new() };
        let Ok(found) = devices() else { return Vec::new() };
        found
            .into_iter()
            .map(|d| {
                let formats = open_reader(&d, None)
                    .and_then(|r| modes(&r))
                    .map(|m| m.iter().map(|(mode, _)| mode.to_format()).collect())
                    .unwrap_or_default();
                // SAFETY: plain call; releases the camera the probe opened.
                let _ = unsafe { d.activate.ShutdownObject() };
                CameraInfo::new(d.id, d.name, formats)
            })
            .collect()
    }

    fn open(&mut self, id: &CameraId, request: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
        // Declared first so it is released last, after every interface `open_stream` created and dropped.
        let _com = ComScope::enter()?;
        let stream = open_stream(&self.runtime, id, request)?;
        Ok(Box::new(stream))
    }
}

/// Opens `id` in the mode `request` picks. Runs inside the caller's [`ComScope`]; everything it creates and does not
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
    let (reader, mode, bottom_up) = match configured {
        Ok(done) => done,
        Err(e) => {
            // SAFETY: plain call; releases the camera `configure` opened before it failed.
            let _ = unsafe { device.activate.ShutdownObject() };
            return Err(e);
        }
    };
    let mut stream = MfStream {
        live: Some(Live { reader, activate: device.activate, held: None, gather: None }),
        shared,
        id: id.clone(),
        mode,
        flip: Vec::new(),
        bottom_up,
        seq: SeqTracker::new(mode.fps_num, mode.fps_den),
        packed_locks: 0,
        failed: None,
        _runtime: Arc::clone(runtime),
    };
    stream.size_flip();
    // Start capture: from here the callback re-arms itself. If this fails the stream is dropped, which shuts the
    // camera down.
    let live = stream.live.as_ref().ok_or(TrackError::Lost)?;
    stream.shared.reader.store(live.reader.as_raw(), Ordering::SeqCst);
    // SAFETY: plain call on the live reader; the result arrives at the callback.
    unsafe { live.reader.ReadSample(VIDEO, 0, None, None, None, None) }.map_err(|e| err(&e, id))?;
    Ok(stream)
}

/// Opens the device's reader and sets the mode `request` picks: the reader, the mode and whether its rows arrive
/// bottom-up.
fn configure(
    device: &Device,
    callback: &IMFSourceReaderCallback,
    id: &CameraId,
    request: &CameraFormatRequest,
) -> Result<(IMFSourceReader, ModeInfo, bool), TrackError> {
    let reader = open_reader(device, Some(callback)).map_err(|e| err(&e, id))?;
    let available = modes(&reader).map_err(|e| err(&e, id))?;
    let list: Vec<ModeInfo> = available.iter().map(|(m, _)| *m).collect();
    let mode = choose_mode(&list, request).ok_or(TrackError::FormatUnsupported(*request))?;
    let media = available.iter().find(|(m, _)| *m == mode).map(|(_, t)| t);
    let media = media.ok_or_else(|| TrackError::Backend("chosen mode vanished from the list".into()))?;
    // SAFETY: `media` is one of the reader's own native types, so no conversion is needed.
    unsafe { reader.SetCurrentMediaType(VIDEO, None, media) }.map_err(|e| err(&e, id))?;
    Ok((reader, mode, is_bottom_up(media)))
}

/// Whether a media type's default stride is negative, so a plain `Lock` hands over rows bottom-up. An absent
/// attribute means top-down.
fn is_bottom_up(media: &IMFMediaType) -> bool {
    // SAFETY: plain call on a live media type.
    unsafe { media.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.is_ok_and(|v| v.cast_signed() < 0)
}

/// How a buffer was locked, which is also how it is unlocked.
enum Lock {
    /// `IMF2DBuffer2::Lock2DSize`: the real pitch and the real extent of the allocation.
    Sized(IMF2DBuffer2),
    /// `IMF2DBuffer::Lock2D`: the real pitch, without the extent.
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

/// What locking a buffer cost.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Cost {
    /// A 2-D lock, or a buffer with no 2-D interface: no copy.
    Free,
    /// The buffer has a 2-D interface that could not be used, so the packed `Lock` ran and may have copied the frame.
    MayCopy,
}

/// Locks `buffer`'s pixels and checks them against the real allocation, in Microsoft's order: `Lock2DSize`, then
/// `Lock2D`, then the packed `Lock` as the last resort. The buffer stays locked until [`Held::unlock`].
fn lock_buffer(
    buffer: IMFMediaBuffer,
    mode: &ModeInfo,
    bottom_up: bool,
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
            held.base = top;
            held.pitch = pitch as isize;
            held.lock = Lock::Surface(surface);
            // Without the allocation's extent only the packed size is known, so only an unpadded top-down frame
            // is trusted; anything else is left to the packed lock below.
            if held.pitch > 0 && held.fit(mode, can_flip, contiguous, 0).is_ok() {
                return Ok((held, Cost::Free));
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
    // `Lock` hands over packed rows: the default stride, which is negative for a bottom-up type.
    let tight = format.tight_stride(mode.width).unwrap_or(0);
    let tight = isize::try_from(tight).unwrap_or(0);
    held.pitch = if bottom_up { -tight } else { tight };
    let top = if held.pitch < 0 { (mode.height as usize).saturating_sub(1) * tight.unsigned_abs() } else { 0 };
    match held.fit(mode, can_flip, len as usize, top) {
        Ok(()) => Ok((held, cost)),
        Err(why) => Err(fail(&held, why, len as usize)),
    }
}

/// The interfaces a stream owns. Dropped by `MfStream::drop` inside its [`ComScope`], never as a bare field.
struct Live {
    reader: IMFSourceReader,
    /// Owns the camera; shut down on drop so it is released at once.
    activate: IMFActivate,
    held: Option<Held>,
    /// Where a sample with several buffers is gathered into one; made when the first such sample arrives and kept.
    gather: Option<(IMFMediaBuffer, u32)>,
}

impl Live {
    /// The sample's pixels as one buffer: its only buffer, borrowed as it is, or, for a sample in pieces, a copy in
    /// the reusable gather buffer (grown, rarely, if a frame is bigger than any before).
    fn contiguous(&mut self, sample: &IMFSample, id: &CameraId) -> Result<IMFMediaBuffer, TrackError> {
        // SAFETY: plain calls on a live sample.
        let count = unsafe { sample.GetBufferCount() }.map_err(|e| err(&e, id))?;
        if count == 1 {
            // SAFETY: as above; index 0 exists since the count is 1.
            return unsafe { sample.GetBufferByIndex(0) }.map_err(|e| err(&e, id));
        }
        // SAFETY: plain call on a live sample.
        let total = unsafe { sample.GetTotalLength() }.map_err(|e| err(&e, id))?;
        let fits = self.gather.as_ref().is_some_and(|(_, capacity)| *capacity >= total);
        if !fits {
            // SAFETY: plain call; the buffer is owned by this stream from here.
            let buffer = unsafe { MFCreateMemoryBuffer(total) }.map_err(|e| err(&e, id))?;
            self.gather = Some((buffer, total));
        }
        let Some((buffer, _)) = &self.gather else { return Err(TrackError::Backend("gather buffer missing".into())) };
        // SAFETY: both are live; the gather buffer is unlocked (the previous frame was released) and big enough.
        unsafe { sample.CopyToBuffer(buffer) }.map_err(|e| err(&e, id))?;
        Ok(buffer.clone())
    }
}

// SAFETY: the reader and activator are free-threaded (multithreaded-apartment) objects; `held` points into a buffer
// that moves with them. All of it is touched only through `&mut MfStream`, one thread at a time, each time inside a
// `ComScope`.
unsafe impl Send for Live {}

struct MfStream {
    live: Option<Live>,
    shared: Arc<Shared>,
    id: CameraId,
    mode: ModeInfo,
    /// Where bottom-up frames are turned top-down; sized at open (and when the camera changes its media type), only
    /// for BGRA, the layout Windows delivers bottom-up. Empty for the other layouts.
    flip: Vec<u8>,
    /// The mode's default stride is negative, so a plain `Lock` hands over rows bottom-up.
    bottom_up: bool,
    seq: SeqTracker,
    /// Frames locked the last-resort way (see [`Cost::MayCopy`]); logged when the first happens and again at drop.
    packed_locks: u64,
    /// Set when the camera changed to something we cannot hand over; every later call repeats it.
    failed: Option<TrackError>,
    /// Last, so Media Foundation outlives every interface above.
    _runtime: Arc<MfRuntime>,
}

impl MfStream {
    /// Sizes the flip buffer for the current mode.
    fn size_flip(&mut self) {
        let len = match (self.mode.format, self.mode.format.tight_stride(self.mode.width)) {
            (PixelFormat::Bgra8, Some(row)) => row.saturating_mul(self.mode.height as usize),
            _ => 0,
        };
        self.flip.clear();
        self.flip.resize(len, 0);
    }

    /// Re-reads the current media type after the camera renegotiated, so no sample is described by the old one.
    fn retype(&mut self) -> Result<(), TrackError> {
        let live = self.live.as_ref().ok_or(TrackError::Lost)?;
        // SAFETY: plain call on the live reader.
        let media = unsafe { live.reader.GetCurrentMediaType(VIDEO) }.map_err(|e| err(&e, &self.id))?;
        let mode = mode_of(&media)
            .map_err(|e| err(&e, &self.id))?
            .ok_or_else(|| TrackError::Backend("the camera changed to a format that cannot be handed over".into()))?;
        self.bottom_up = is_bottom_up(&media);
        self.seq.set_rate(mode.fps_num, mode.fps_den);
        if mode != self.mode {
            self.mode = mode;
            self.size_flip();
        }
        Ok(())
    }

    /// One frame from `token`, which this call now owns.
    fn frame_from(&mut self, token: NonNull<c_void>) -> Result<Option<CameraFrame<'_>>, TrackError> {
        // SAFETY: the token was made from a sample's raw pointer by `into_token`; this call owns it.
        let sample = unsafe { IMFSample::from_raw(token.as_ptr()) };
        let live = self.live.as_mut().ok_or(TrackError::Lost)?;
        // SAFETY: plain calls on a live sample; an absent attribute means no discontinuity.
        let time = unsafe { sample.GetSampleTime() }.map_err(|e| err(&e, &self.id))?;
        let lost_before = unsafe { sample.GetUINT32(&MFSampleExtension_Discontinuity) }.is_ok_and(|v| v != 0);
        let buffer = live.contiguous(&sample, &self.id)?;
        let (held, cost) = lock_buffer(buffer, &self.mode, self.bottom_up, !self.flip.is_empty(), &self.id)?;
        if cost == Cost::MayCopy {
            self.packed_locks += 1;
            if self.packed_locks == 1 {
                tracing::warn!(camera = %self.id.0, "camera buffers fell back to the packed Lock, which may copy every frame");
            }
        }
        let (width, height, pixel) = (self.mode.width, self.mode.height, self.mode.format);
        let captured = DeviceTime(u64::try_from(time).unwrap_or(0).saturating_mul(NS_PER_TICK));
        let seq = self.seq.next(time, lost_before);
        // SAFETY: the lock is held until the next call to `next_frame`, which needs `&mut self` and so ends the
        // borrow this slice lives in first.
        let bytes = unsafe { held.bytes() };
        if held.pitch < 0 {
            let row_bytes = self.flip.len() / height as usize;
            let copied = copy_top_down(bytes, held.pitch, row_bytes, height as usize, &mut self.flip);
            held.unlock();
            copied.map_err(|e| TrackError::Backend(e.into()))?;
            return Ok(Some(CameraFrame::new(&self.flip, width, height, row_bytes, pixel, captured, seq)));
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
    /// sent an empty tick, or announced a new media type (the frame that came with it is dropped). Capture does not depend on this call: the callback
    /// keeps the next read armed, so a frame that arrives after a timeout is replaced by newer ones until you ask.
    /// A bottom-up BGRA frame is copied top-down into a buffer allocated at open; no other layout copies, except a
    /// sample the driver delivers in several buffers. `seq` follows the capture times (see [`SeqTracker`]), so a
    /// skipped capture is a gap. If the camera switches to a format that cannot be handed over, this and every later
    /// call return that error.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
        let _com = ComScope::enter()?;
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        let live = self.live.as_mut().ok_or(TrackError::Lost)?;
        // The previous frame's borrow ended with this call, so its buffer can be released.
        if let Some(held) = live.held.take() {
            held.unlock();
        }
        match self.shared.slot.take(timeout) {
            Take::Item(token) => self.frame_from(token),
            Take::Retype(token) => {
                // The camera changed its media type. Whatever was waiting may predate the change, so it is dropped;
                // the next frame is read under the new type.
                if let Some(token) = token {
                    Shared::release(token);
                }
                if let Err(e) = self.retype() {
                    self.failed = Some(e.clone());
                    return Err(e);
                }
                Ok(None)
            }
            Take::Timeout => Ok(None),
            Take::Failed(SlotFailure::Lost) | Take::Closed => Err(TrackError::Lost),
            Take::Failed(SlotFailure::Hresult(hr)) => Err(map_hresult(hr, &self.id)),
        }
    }
}

impl Drop for MfStream {
    /// Releases the camera even with a read outstanding: stops the callback re-arming, waits for any in flight,
    /// flushes the reader, waits (bounded) for the flush to finish, shuts the source down, then drops every
    /// interface while COM is still initialised.
    fn drop(&mut self) {
        // Entered first, dropped last: after every interface below. Without COM we still release what we can.
        let _com = ComScope::enter().ok();
        if self.packed_locks > 0 {
            tracing::warn!(camera = %self.id.0, frames = self.packed_locks, "frames locked through the packed Lock fallback");
        }
        self.shared.slot.close();
        self.shared.reader.store(std::ptr::null_mut(), Ordering::SeqCst);
        // A callback that read the reader pointer before it was cleared is still using the reader: wait it out.
        let deadline = std::time::Instant::now() + FLUSH_WAIT;
        while self.shared.in_flight.load(Ordering::SeqCst) != 0 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let safe_to_release = self.shared.in_flight.load(Ordering::SeqCst) == 0;
        if let Some(mut live) = self.live.take() {
            if let Some(held) = live.held.take() {
                held.unlock();
            }
            if safe_to_release {
                // SAFETY: plain call on a live reader; completion is reported to `OnFlush`.
                if unsafe { live.reader.Flush(VIDEO) }.is_ok() {
                    self.shared.slot.wait_flushed(FLUSH_WAIT);
                }
                // SAFETY: plain call; asks Media Foundation to release the camera now rather than at the last
                // reference.
                let _ = unsafe { live.activate.ShutdownObject() };
                drop(live);
            } else {
                // A callback is stuck inside the reader: releasing it would be a use after free. Leak it instead.
                std::mem::forget(live);
            }
        }
        // Anything the callback delivered but nobody took.
        if let Some(token) = self.shared.slot.drain() {
            Shared::release(token);
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
}
