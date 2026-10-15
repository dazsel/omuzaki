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
//! - The callback owns its own `Arc<Slot>`, so a late callback after the stream is gone touches only memory it owns.
#![allow(unsafe_code)] // Win32/COM FFI has no safe binding; each block carries its own `// SAFETY:` comment.

use std::sync::Arc;
use std::time::Duration;

use omuzaki_track_api::camera::{
    Camera, CameraFormat, CameraFormatRequest, CameraFrame, CameraId, CameraInfo, CameraStream, DeviceTime, PixelFormat,
};
use omuzaki_track_api::error::TrackError;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer2, IMFActivate, IMFAttributes, IMFMediaBuffer, IMFMediaSource, IMFMediaType, IMFSample, IMFSourceReader,
    IMFSourceReaderCallback, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
    MF_E_NO_MORE_TYPES, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_SUBTYPE,
    MF_READWRITE_DISABLE_CONVERTERS, MF_SOURCE_READER_ASYNC_CALLBACK, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION, MF2DBuffer_LockFlags_Read, MFCreateAttributes,
    MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFSTARTUP_FULL, MFSampleExtension_Discontinuity,
    MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::core::{GUID, Interface, PWSTR};

use crate::modes::{
    FrameSpan, ModeInfo, SeqTracker, Slot, SlotFailure, Take, choose_mode, copy_top_down, frame_span, map_hresult,
    subtype_format,
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

/// A capture sample on its way from the callback to the caller.
struct Delivery {
    sample: Option<SendSample>,
    time: i64,
}

/// An `IMFSample` that may change threads.
struct SendSample(IMFSample);
// SAFETY: Media Foundation samples are free-threaded (multithreaded-apartment) objects; one thread uses a sample at
// a time here, since the callback hands it over through the slot and keeps no copy.
unsafe impl Send for SendSample {}

mod callback {
    //! The `implement` macro expands to code that trips two pedantic lints; the expansion is not ours to change.
    #![allow(clippy::ref_as_ptr, clippy::inline_always)]

    use std::sync::Arc;

    use windows::Win32::Media::MediaFoundation::{
        IMFMediaEvent, IMFSample, IMFSourceReaderCallback, IMFSourceReaderCallback_Impl, MF_SOURCE_READERF_ENDOFSTREAM,
        MF_SOURCE_READERF_ERROR,
    };
    use windows::core::{HRESULT, Ref, implement};

    use super::{Delivery, SendSample};
    use crate::modes::{Slot, SlotFailure};

    /// Receives samples from the source reader on a Media Foundation worker thread.
    #[implement(IMFSourceReaderCallback)]
    pub(super) struct Callback {
        pub(super) slot: Arc<Slot<Delivery>>,
    }

    impl IMFSourceReaderCallback_Impl for Callback_Impl {
        fn OnReadSample(
            &self,
            status: HRESULT,
            _stream: u32,
            flags: u32,
            time: i64,
            sample: Ref<'_, IMFSample>,
        ) -> windows::core::Result<()> {
            if status.is_err() {
                self.slot.fail(SlotFailure::Hresult(status.0));
            } else if flags & (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ENDOFSTREAM.0).cast_unsigned() != 0 {
                self.slot.fail(SlotFailure::Lost);
            } else {
                let sample = sample.as_ref().cloned().map(SendSample);
                // The older sample (or this one, after the stream closed) is released here, on the worker thread.
                drop(self.slot.put(Delivery { sample, time }));
            }
            Ok(())
        }

        fn OnFlush(&self, _stream: u32) -> windows::core::Result<()> {
            self.slot.mark_flushed();
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
    let slot = Arc::new(Slot::new());
    let callback: IMFSourceReaderCallback = Callback { slot: Arc::clone(&slot) }.into();
    let configured = configure(&device, &callback, id, request);
    let (reader, mode, bottom_up) = match configured {
        Ok(done) => done,
        Err(e) => {
            // SAFETY: plain call; releases the camera `configure` opened before it failed.
            let _ = unsafe { device.activate.ShutdownObject() };
            return Err(e);
        }
    };
    // One top-down copy buffer for the life of the stream, only for the layout that arrives bottom-up.
    let flip_len = match (mode.format, mode.format.tight_stride(mode.width)) {
        (PixelFormat::Bgra8, Some(row)) => row.saturating_mul(mode.height as usize),
        _ => 0,
    };
    Ok(MfStream {
        live: Some(Live { reader, activate: device.activate, held: None, pending: false }),
        slot,
        id: id.clone(),
        mode,
        flip: vec![0; flip_len],
        bottom_up,
        seq: SeqTracker::new(mode.fps_num, mode.fps_den),
        _runtime: Arc::clone(runtime),
    })
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
    // SAFETY: plain call on a live media type; an absent attribute means top-down.
    let bottom_up = unsafe { media.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.is_ok_and(|v| v.cast_signed() < 0);
    Ok((reader, mode, bottom_up))
}

/// The locked buffer of the frame last handed out.
struct Held {
    buffer: IMFMediaBuffer,
    two_d: Option<IMF2DBuffer2>,
    /// The start of the locked allocation, and where the frame's lowest row begins within it (`span_len` bytes).
    base: *const u8,
    span_start: usize,
    span_len: usize,
    /// Bytes from one row to the next, top row first; negative when the top row is last in memory.
    pitch: isize,
}

impl Held {
    fn unlock(&self) {
        // SAFETY: the buffer was locked by the matching call in `lock_sample`, and is unlocked exactly once (the
        // caller drops this value right after).
        let _ = unsafe {
            match &self.two_d {
                Some(two_d) => two_d.Unlock2D(),
                None => self.buffer.Unlock(),
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
}

/// Locks `sample`'s pixels and checks them against the real allocation. The buffer stays locked until
/// [`Held::unlock`].
fn lock_sample(
    sample: &IMFSample,
    mode: &ModeInfo,
    bottom_up: bool,
    can_flip: bool,
    id: &CameraId,
) -> Result<Held, TrackError> {
    let format = mode.format;
    // SAFETY: plain call on a live sample.
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.map_err(|e| err(&e, id))?;
    let two_d = if format == PixelFormat::Mjpg { None } else { buffer.cast::<IMF2DBuffer2>().ok() };
    let mut held = Held { buffer, two_d, base: std::ptr::null(), span_start: 0, span_len: 0, pitch: 0 };
    // The allocation length, and the offset of the top row in it.
    let (alloc_len, scanline0) = if let Some(two_d) = &held.two_d {
        let (mut top, mut pitch, mut start, mut len) =
            (std::ptr::null_mut::<u8>(), 0_i32, std::ptr::null_mut::<u8>(), 0);
        // SAFETY: the out-pointers outlive the call; a success locks the buffer until `Unlock2D` and reports the
        // whole allocation (`start`, `len`), which `top` lies in.
        unsafe {
            two_d.Lock2DSize(MF2DBuffer_LockFlags_Read, &raw mut top, &raw mut pitch, &raw mut start, &raw mut len)
        }
        .map_err(|e| err(&e, id))?;
        held.base = start;
        held.pitch = pitch as isize;
        (len as usize, top.addr().wrapping_sub(start.addr()))
    } else {
        let (mut start, mut len) = (std::ptr::null_mut::<u8>(), 0_u32);
        // SAFETY: the out-pointers outlive the call; a success locks the buffer until `Unlock`.
        unsafe { held.buffer.Lock(&raw mut start, None, Some(&raw mut len)) }.map_err(|e| err(&e, id))?;
        held.base = start;
        // `Lock` hands over packed rows: the default stride, which is negative for a bottom-up type.
        let tight = format.tight_stride(mode.width).unwrap_or(0);
        let tight = isize::try_from(tight).unwrap_or(0);
        held.pitch = if bottom_up { -tight } else { tight };
        let top = if held.pitch < 0 { (mode.height as usize).saturating_sub(1) * tight.unsigned_abs() } else { 0 };
        (len as usize, top)
    };
    let span = if held.base.is_null() {
        Err("the buffer has no address")
    } else if format == PixelFormat::Mjpg {
        // An image has no rows; the whole locked length is the payload.
        if alloc_len == 0 { Err("empty image") } else { Ok(FrameSpan { start: 0, len: alloc_len }) }
    } else if held.pitch < 0 && !can_flip {
        Err("bottom-up frames of this layout are not supported")
    } else {
        frame_span(format, mode.width, mode.height, held.pitch, scanline0, alloc_len)
    };
    match span {
        Ok(span) => {
            held.span_start = span.start;
            held.span_len = span.len;
            Ok(held)
        }
        Err(why) => {
            held.unlock();
            Err(TrackError::Backend(format!(
                "unusable {format:?} buffer: {why} (pitch {}, {alloc_len} bytes)",
                held.pitch
            )))
        }
    }
}

/// The interfaces a stream owns. Dropped by `MfStream::drop` inside its [`ComScope`], never as a bare field.
struct Live {
    reader: IMFSourceReader,
    /// Owns the camera; shut down on drop so it is released at once.
    activate: IMFActivate,
    held: Option<Held>,
    /// A `ReadSample` is outstanding, so the callback will deliver one more item.
    pending: bool,
}

// SAFETY: the reader and activator are free-threaded (multithreaded-apartment) objects; `held` points into a buffer
// that moves with them. All of it is touched only through `&mut MfStream`, one thread at a time, each time inside a
// `ComScope`.
unsafe impl Send for Live {}

struct MfStream {
    live: Option<Live>,
    slot: Arc<Slot<Delivery>>,
    id: CameraId,
    mode: ModeInfo,
    /// Where bottom-up frames are turned top-down; allocated once at open, only for BGRA (the layout Windows
    /// delivers bottom-up). Empty for the other layouts.
    flip: Vec<u8>,
    /// The mode's default stride is negative, so a plain `Lock` hands over rows bottom-up.
    bottom_up: bool,
    seq: SeqTracker,
    /// Last, so Media Foundation outlives every interface above.
    _runtime: Arc<MfRuntime>,
}

impl CameraStream for MfStream {
    fn format(&self) -> CameraFormat {
        self.mode.to_format()
    }

    /// The newest frame, waiting up to `timeout` for the device. `Ok(None)` means none arrived in time (the request
    /// stays outstanding, so a frame arriving later is returned by the next call) or the driver sent an empty tick.
    /// A bottom-up BGRA frame is copied top-down into a buffer allocated at open; no other layout copies.
    /// `seq` follows the capture times (see [`SeqTracker`]), so a skipped capture is a gap.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
        let _com = ComScope::enter()?;
        let live = self.live.as_mut().ok_or(TrackError::Lost)?;
        // The previous frame's borrow ended with this call, so its buffer can be released.
        if let Some(held) = live.held.take() {
            held.unlock();
        }
        if !live.pending {
            // SAFETY: plain call; the result arrives at the callback, which owns what it touches.
            unsafe { live.reader.ReadSample(VIDEO, 0, None, None, None, None) }.map_err(|e| err(&e, &self.id))?;
            live.pending = true;
        }
        let delivery = match self.slot.take(timeout) {
            Take::Item(delivery) => delivery,
            Take::Timeout => return Ok(None),
            Take::Failed(SlotFailure::Lost) | Take::Closed => return Err(TrackError::Lost),
            Take::Failed(SlotFailure::Hresult(hr)) => return Err(map_hresult(hr, &self.id)),
        };
        live.pending = false;
        let Some(sample) = delivery.sample else { return Ok(None) };
        // SAFETY: plain call on a live sample; an absent attribute means no discontinuity.
        let lost_before = unsafe { sample.0.GetUINT32(&MFSampleExtension_Discontinuity) }.is_ok_and(|v| v != 0);
        let held = lock_sample(&sample.0, &self.mode, self.bottom_up, !self.flip.is_empty(), &self.id)?;
        let (width, height, pixel) = (self.mode.width, self.mode.height, self.mode.format);
        let captured = DeviceTime(u64::try_from(delivery.time).unwrap_or(0).saturating_mul(NS_PER_TICK));
        let seq = self.seq.next(delivery.time, lost_before);
        // SAFETY: the lock is held until the next call to this method, which needs `&mut self` and so ends the
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

impl Drop for MfStream {
    /// Releases the camera even with a read outstanding: flushes the reader, waits (bounded) for the flush to finish,
    /// shuts the source down, then drops every interface while COM is still initialised.
    fn drop(&mut self) {
        // Entered first, dropped last: after every interface below. Without COM we still release what we can.
        let _com = ComScope::enter().ok();
        if let Some(mut live) = self.live.take() {
            if let Some(held) = live.held.take() {
                held.unlock();
            }
            // Anything the callback delivered but nobody took is released here, then late deliveries are refused.
            drop(self.slot.take(Duration::ZERO));
            self.slot.close();
            // SAFETY: plain call on a live reader; completion is reported to `OnFlush`.
            if unsafe { live.reader.Flush(VIDEO) }.is_ok() {
                self.slot.wait_flushed(FLUSH_WAIT);
            }
            // SAFETY: plain call; asks Media Foundation to release the camera now rather than at the last reference.
            let _ = unsafe { live.activate.ShutdownObject() };
            drop(live);
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
