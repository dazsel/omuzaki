//! Media Foundation capture: device enumeration and a synchronous source reader that hands out raw frames.
//!
//! This is the one place with `unsafe`: every call here crosses into COM, whose bindings are `unsafe` by nature.
#![allow(unsafe_code)] // Win32/COM FFI has no safe binding; each block carries its own `// SAFETY:` comment.

use std::time::Duration;

use omuzaki_track_api::camera::{
    Camera, CameraFormat, CameraFormatRequest, CameraFrame, CameraId, CameraInfo, CameraStream, DeviceTime, PixelFormat,
};
use omuzaki_track_api::error::TrackError;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFAttributes, IMFMediaBuffer, IMFMediaSource, IMFMediaType, IMFSample, IMFSourceReader,
    MF_API_VERSION, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
    MF_E_NO_MORE_TYPES, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_SUBTYPE,
    MF_READWRITE_DISABLE_CONVERTERS, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR, MFCreateAttributes, MFCreateSourceReaderFromMediaSource,
    MFEnumDeviceSources, MFSTARTUP_FULL, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::core::{GUID, Interface, PWSTR};

use crate::modes::{ModeInfo, choose_mode, copy_top_down, map_hresult, subtype_format};

/// The reader's first video stream, as the `u32` the calls take.
const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned();
/// Sample times are in units of 100 ns.
const NS_PER_TICK: u64 = 100;

/// Pairs `MFStartup` with `MFShutdown`. Startup is process-wide and reference-counted, so the guard may drop on any
/// thread.
struct MfRuntime;

impl MfRuntime {
    fn start() -> windows::core::Result<Self> {
        // SAFETY: plain call; a failed startup returns an error and needs no shutdown.
        unsafe { MFStartup(MF_API_VERSION, MFSTARTUP_FULL)? };
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
/// that touches Media Foundation holds one: the camera and its streams may move between threads.
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
    _runtime: MfRuntime,
}

impl MfCamera {
    /// Starts Media Foundation.
    ///
    /// # Errors
    /// [`TrackError::Backend`] if Media Foundation is missing or fails to start.
    pub fn new() -> Result<Self, TrackError> {
        let runtime = MfRuntime::start().map_err(|e| map_hresult(e.code().0, &CameraId(String::new())))?;
        Ok(Self { _runtime: runtime })
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

/// Every video capture device present. Needs a [`ComScope`].
fn devices() -> windows::core::Result<Vec<Device>> {
    let mut query = None;
    // SAFETY: `query` outlives the call and receives the new attribute store.
    unsafe { MFCreateAttributes(&raw mut query, 1)? };
    let query: IMFAttributes =
        query.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))?;
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
                let id = string_attr(
                    &activate.cast::<IMFAttributes>()?,
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                );
                let name = string_attr(&activate.cast::<IMFAttributes>()?, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME);
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

/// Opens a source reader on the device, with video processing and converters off so frames stay raw.
fn open_reader(device: &Device) -> windows::core::Result<IMFSourceReader> {
    let mut attrs = None;
    // SAFETY: `attrs` outlives the call and receives the new attribute store.
    unsafe { MFCreateAttributes(&raw mut attrs, 2)? };
    let attrs: IMFAttributes =
        attrs.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))?;
    // SAFETY: the keys are valid statics.
    unsafe {
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 0)?;
        attrs.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1)?;
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
        let Ok(_com) = ComScope::enter() else { return Vec::new() };
        let Ok(found) = devices() else { return Vec::new() };
        found
            .into_iter()
            .map(|d| {
                let formats = open_reader(&d)
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
        let com = ComScope::enter()?;
        let device = devices()
            .map_err(|e| err(&e, id))?
            .into_iter()
            .find(|d| d.id == *id)
            .ok_or_else(|| TrackError::NoDevice(id.clone()))?;
        let reader = open_reader(&device).map_err(|e| err(&e, id))?;
        let available = modes(&reader).map_err(|e| err(&e, id))?;
        let list: Vec<ModeInfo> = available.iter().map(|(m, _)| *m).collect();
        let mode = choose_mode(&list, request).ok_or(TrackError::FormatUnsupported(*request))?;
        let media = available.iter().find(|(m, _)| *m == mode).map(|(_, t)| t);
        let media = media.ok_or_else(|| TrackError::Backend("chosen mode vanished from the list".into()))?;
        // SAFETY: `media` is one of the reader's own native types, so no conversion is needed.
        unsafe { reader.SetCurrentMediaType(VIDEO, None, media) }.map_err(|e| err(&e, id))?;
        // SAFETY: plain call on a live media type; an absent attribute means top-down.
        let bottom_up = unsafe { media.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.is_ok_and(|v| v.cast_signed() < 0);
        // One top-down copy buffer for the life of the stream, only for the layout that arrives bottom-up.
        let flip_len = match (mode.format, mode.format.tight_stride(mode.width)) {
            (PixelFormat::Bgra8, Some(row)) => row.saturating_mul(mode.height as usize),
            _ => 0,
        };
        drop(com);
        Ok(Box::new(MfStream {
            reader: SendReader(reader),
            id: id.clone(),
            mode,
            held: None,
            flip: vec![0; flip_len],
            bottom_up,
            seq: 0,
            device: SendActivate(device.activate),
        }))
    }
}

/// A source reader that can move between threads.
struct SendReader(IMFSourceReader);
// SAFETY: a source reader is a free-threaded (multithreaded-apartment) object; it is used by one thread at a time
// here, since the stream's methods take `&mut self`, and each use holds a `ComScope`.
unsafe impl Send for SendReader {}

/// The activator that owns the camera, kept to release it when the stream drops.
struct SendActivate(IMFActivate);
// SAFETY: as for `SendReader`; only `Drop` calls it, with the stream exclusively borrowed.
unsafe impl Send for SendActivate {}

/// The locked buffer of the frame last handed out.
struct Held {
    buffer: IMFMediaBuffer,
    two_d: Option<IMF2DBuffer>,
    /// The lowest address of the frame's rows, and the bytes from there to the end of the last row.
    block: *const u8,
    len: usize,
    /// Bytes from one row to the next, top row first; negative when the top row is last in memory.
    pitch: isize,
}

impl Held {
    fn unlock(&self) {
        // SAFETY: the buffer was locked by the matching call in `lock`, and is unlocked exactly once (the caller
        // drops this value right after).
        let _ = unsafe {
            match &self.two_d {
                Some(two_d) => two_d.Unlock2D(),
                None => self.buffer.Unlock(),
            }
        };
    }
}

struct MfStream {
    reader: SendReader,
    id: CameraId,
    mode: ModeInfo,
    held: Option<Held>,
    /// Where bottom-up frames are turned top-down; allocated once at open, only for BGRA (the layout Windows
    /// delivers bottom-up). Empty for the other layouts.
    flip: Vec<u8>,
    /// The mode's default stride is negative, so a plain `Lock` hands over rows bottom-up.
    bottom_up: bool,
    seq: u64,
    device: SendActivate,
}

// SAFETY: `ptr` points into the locked buffer owned by `buffer`, which moves with the stream and is only touched
// through `&mut self`.
unsafe impl Send for Held {}

impl MfStream {
    /// Locks `sample`'s pixels and keeps them locked until the next read.
    fn lock(&mut self, sample: &IMFSample) -> Result<(), TrackError> {
        let id = &self.id;
        let (format, height) = (self.mode.format, self.mode.height as usize);
        // SAFETY: plain calls on a live sample and its buffer.
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }.map_err(|e| err(&e, id))?;
        let two_d = if format == PixelFormat::Mjpg { None } else { buffer.cast::<IMF2DBuffer>().ok() };
        let (mut ptr, mut pitch, mut len) = (std::ptr::null_mut::<u8>(), 0_i32, 0_u32);
        if let Some(two_d) = &two_d {
            // SAFETY: the out-pointers outlive the calls; a success locks the buffer until `Unlock2D`.
            unsafe { two_d.Lock2D(&raw mut ptr, &raw mut pitch) }.map_err(|e| err(&e, id))?;
            // SAFETY: plain call on a live buffer.
            len = unsafe { two_d.GetContiguousLength() }.unwrap_or(0);
        } else {
            // SAFETY: as above; a success locks the buffer until `Unlock`.
            unsafe { buffer.Lock(&raw mut ptr, None, Some(&raw mut len)) }.map_err(|e| err(&e, id))?;
            let tight = format.tight_stride(self.mode.width).unwrap_or(0);
            pitch = i32::try_from(tight).unwrap_or(0);
            if self.bottom_up {
                pitch = -pitch;
            }
        }
        let mut held = Held { buffer, two_d, block: ptr, len: len as usize, pitch: pitch as isize };
        let rows_len =
            if format == PixelFormat::Mjpg { Some(held.len) } else { height.checked_mul(held.pitch.unsigned_abs()) };
        let usable = !ptr.is_null() && rows_len.is_some_and(|n| n > 0 && n <= held.len);
        // Planar NV12 cannot be flipped row by row; nothing delivers it bottom-up.
        let flippable = held.pitch >= 0 || !self.flip.is_empty();
        if !usable || !flippable {
            held.unlock();
            return Err(TrackError::Backend(format!("unusable {format:?} buffer: pitch {pitch}, {} bytes", held.len)));
        }
        if held.pitch < 0 && held.two_d.is_some() {
            // `Lock2D` points at the top row; the block starts `height - 1` rows lower in memory.
            // SAFETY: the buffer holds `height` rows of `|pitch|` bytes ending at the top row's end (checked above), so
            // the lowest row starts inside the allocation.
            let back = rows_len.unwrap_or(0).saturating_sub(held.pitch.unsigned_abs());
            held.block = unsafe { ptr.sub(back) };
        }
        held.len = rows_len.unwrap_or(0);
        self.held = Some(held);
        Ok(())
    }
}

impl CameraStream for MfStream {
    fn format(&self) -> CameraFormat {
        self.mode.to_format()
    }

    /// Blocks until Media Foundation delivers a frame or fails; `timeout` is **not** honoured. The reader runs in
    /// synchronous mode (`ReadSample` has no timed wait), so a camera that stays connected but stops sending blocks
    /// the caller for as long as it is silent. Nothing here can interrupt the wait: the stream is borrowed
    /// exclusively for the call, so neither `Drop` nor another method can run meanwhile, and Media Foundation does
    /// not document that `IMFSourceReader::Flush` from another thread cancels a pending synchronous read. An
    /// unplugged camera ends the wait with an error (mapped to [`TrackError::Lost`]). A timed wait needs an
    /// asynchronous-mode reader.
    ///
    /// A bottom-up BGRA frame is copied top-down into a buffer allocated at open; no other layout copies.
    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
        // The previous frame's borrow ended with this call, so its buffer can be released.
        if let Some(held) = self.held.take() {
            held.unlock();
        }
        let _com = ComScope::enter()?;
        let (mut flags, mut stamp) = (0_u32, 0_i64);
        let mut sample: Option<IMFSample> = None;
        // SAFETY: the out-pointers outlive the call; it blocks until a sample, a tick or an error.
        unsafe {
            self.reader.0.ReadSample(VIDEO, 0, None, Some(&raw mut flags), Some(&raw mut stamp), Some(&raw mut sample))
        }
        .map_err(|e| err(&e, &self.id))?;
        if flags & (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ENDOFSTREAM.0).cast_unsigned() != 0 {
            return Err(TrackError::Lost);
        }
        let Some(sample) = sample else { return Ok(None) };
        self.lock(&sample)?;
        let Some(held) = &self.held else { return Err(TrackError::Backend("frame lock was lost".into())) };
        // SAFETY: `block` is the locked buffer's lowest row and `len` bytes from it were checked to be mapped; the
        // lock is held until the next call to this method, which needs `&mut self` and so ends this borrow first.
        let block = unsafe { std::slice::from_raw_parts(held.block, held.len) };
        let (width, height, pixel) = (self.mode.width, self.mode.height, self.mode.format);
        let captured = DeviceTime(u64::try_from(stamp).unwrap_or(0).saturating_mul(NS_PER_TICK));
        self.seq += 1;
        if held.pitch < 0 {
            let row_bytes = self.flip.len() / height as usize;
            copy_top_down(block, held.pitch, row_bytes, height as usize, &mut self.flip)
                .map_err(|e| TrackError::Backend(e.into()))?;
            if let Some(held) = self.held.take() {
                held.unlock();
            }
            return Ok(Some(CameraFrame::new(&self.flip, width, height, row_bytes, pixel, captured, self.seq)));
        }
        let stride = if pixel == PixelFormat::Mjpg { 0 } else { held.pitch.unsigned_abs() };
        Ok(Some(CameraFrame::new(block, width, height, stride, pixel, captured, self.seq)))
    }
}

impl Drop for MfStream {
    fn drop(&mut self) {
        if let Some(held) = self.held.take() {
            held.unlock();
        }
        // SAFETY: plain call; asks Media Foundation to release the camera now rather than at the last reference.
        let _ = unsafe { self.device.0.ShutdownObject() };
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::E_ACCESSDENIED;
    use windows::Win32::Media::MediaFoundation::{
        MF_E_HW_MFT_FAILED_START_STREAMING, MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED,
    };

    use crate::modes;

    fn code(hr: windows::core::HRESULT) -> i32 {
        hr.0
    }

    /// The tables spell the codes out so they build on Linux; here they are checked against the bindings.
    #[test]
    fn hresult_constants_match_the_bindings() {
        assert_eq!(modes::E_ACCESSDENIED, code(E_ACCESSDENIED));
        assert_eq!(modes::MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED, code(MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED));
        assert_eq!(modes::MF_E_HW_MFT_FAILED_START_STREAMING, code(MF_E_HW_MFT_FAILED_START_STREAMING));
    }
}
