//! Capture modes, media subtypes and error codes, plus the pieces of the reader that need no Windows to build or test:
//! frame bounds, row flipping, the device sequence and the timed hand-off between the capture callback and the caller.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use omuzaki_track_api::camera::{CameraFormat, CameraFormatRequest, CameraId, PixelFormat};
use omuzaki_track_api::error::TrackError;

/// `E_ACCESSDENIED`: Windows' camera privacy setting (or a policy) refuses the app.
pub const E_ACCESSDENIED: i32 = 0x8007_0005_u32.cast_signed();
/// `MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED`: the camera was unplugged or its driver reset.
pub const MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED: i32 = 0xC00D_3EA2_u32.cast_signed();
/// `MF_E_HW_MFT_FAILED_START_STREAMING`: the camera could not start, usually because another app holds it.
pub const MF_E_HW_MFT_FAILED_START_STREAMING: i32 = 0xC00D_3704_u32.cast_signed();

/// One capture mode a device offers, as Media Foundation states it (frame rate as a fraction).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModeInfo {
    /// Byte layout of the frames.
    pub format: PixelFormat,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Frame rate numerator.
    pub fps_num: u32,
    /// Frame rate denominator (0 means the device gave none).
    pub fps_den: u32,
}

impl ModeInfo {
    /// The mode in the contract's terms: the frame rate rounded to whole frames per second (29.97 is 30).
    pub fn to_format(&self) -> CameraFormat {
        let fps = if self.fps_den == 0 {
            0
        } else {
            let (n, d) = (u64::from(self.fps_num), u64::from(self.fps_den));
            u32::try_from((2 * n + d) / (2 * d)).unwrap_or(u32::MAX)
        };
        CameraFormat::new(self.width, self.height, fps, self.format)
    }
}

/// The mode closest to `request`, by [`CameraFormatRequest::pick`]: nearest pixel count, then nearest frame rate,
/// then the cheapest layout to convert (NV12 before YUY2 before MJPG). Of modes that tie, the device's first.
/// `None` if `modes` has none with the required layout.
pub fn choose_mode(modes: &[ModeInfo], request: &CameraFormatRequest) -> Option<ModeInfo> {
    let formats: Vec<CameraFormat> = modes.iter().map(ModeInfo::to_format).collect();
    let picked = request.pick(&formats)?;
    // `pick` keeps the first of equal formats, so the first mode that converts to it is the one it chose.
    modes.iter().copied().find(|m| m.to_format() == picked)
}

/// The Media Foundation subtype (its GUID's first 32 bits) as a layout: NV12, YUY2, MJPG and RGB32 (`BGRA` in memory,
/// the alpha byte unused). `None` for anything else. RGB24 is `None` too: Media Foundation stores it B, G, R, which
/// [`PixelFormat::Rgb8`] (R, G, B) cannot describe.
pub const fn subtype_format(fourcc: u32) -> Option<PixelFormat> {
    match fourcc {
        NV12 => Some(PixelFormat::Nv12),
        YUY2 => Some(PixelFormat::Yuy2),
        MJPG => Some(PixelFormat::Mjpg),
        RGB32 => Some(PixelFormat::Bgra8),
        _ => None,
    }
}

const fn fourcc(tag: [u8; 4]) -> u32 {
    u32::from_le_bytes(tag)
}

/// `MFVideoFormat_NV12`.
pub const NV12: u32 = fourcc(*b"NV12");
/// `MFVideoFormat_YUY2`.
pub const YUY2: u32 = fourcc(*b"YUY2");
/// `MFVideoFormat_MJPG`.
pub const MJPG: u32 = fourcc(*b"MJPG");
/// `MFVideoFormat_RGB32` (`D3DFMT_X8R8G8B8`).
pub const RGB32: u32 = 22;

/// A Media Foundation `HRESULT` as the contract's error, for the camera `id`. Access denied is the person's privacy
/// setting, an invalidated recording device is a camera that went away, a failed start is a busy one; anything else
/// is [`TrackError::Backend`] carrying the code in hex.
pub fn map_hresult(hr: i32, id: &CameraId) -> TrackError {
    match hr {
        E_ACCESSDENIED => TrackError::PermissionDenied,
        MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED => TrackError::Lost,
        MF_E_HW_MFT_FAILED_START_STREAMING => TrackError::Busy(id.clone()),
        other => TrackError::Backend(format!("Media Foundation error {:#010X}", other.cast_unsigned())),
    }
}

/// Copies `rows` rows of `row_bytes` pixels each from `block` into `dst`, top row first.
///
/// `block` holds the rows `pitch` bytes apart. A positive pitch means the top row is first in memory; a negative
/// one means it is last (a bottom-up image), and the rows are read from the end. Only the first `row_bytes` of
/// each row are copied, so padding never reaches `dst`.
///
/// # Errors
/// If `pitch` is 0 or smaller than `row_bytes`, or `block` or `dst` is too short for `rows` rows.
pub fn copy_top_down(
    block: &[u8],
    pitch: isize,
    row_bytes: usize,
    rows: usize,
    dst: &mut [u8],
) -> Result<(), &'static str> {
    let step = pitch.unsigned_abs();
    if step < row_bytes || step == 0 {
        return Err("row pitch is smaller than a row");
    }
    let (Some(src_len), Some(dst_len)) = (step.checked_mul(rows), row_bytes.checked_mul(rows)) else {
        return Err("frame size overflows");
    };
    if block.len() < src_len || dst.len() < dst_len {
        return Err("buffer is shorter than the frame");
    }
    for (row, out) in dst[..dst_len].chunks_exact_mut(row_bytes.max(1)).enumerate() {
        let at = if pitch >= 0 { row } else { rows - 1 - row } * step;
        out.copy_from_slice(&block[at..at + row_bytes]);
    }
    Ok(())
}

/// Where a raw frame's bytes lie within a locked allocation: `start..start + len`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FrameSpan {
    /// Offset of the frame's first byte (its lowest-addressed row) from the start of the allocation.
    pub start: usize,
    /// Bytes from there to the end of the last row.
    pub len: usize,
}

/// Checks that a raw `format` frame of `width` x `height` pixels, whose top row starts `scanline0` bytes into an
/// allocation of `alloc_len` bytes with rows `pitch` bytes apart, lies wholly inside it, and says where.
///
/// A negative `pitch` is a bottom-up image: the top row is the highest-addressed one. The length is
/// [`PixelFormat::frame_len`], so NV12 counts its chroma plane (1,382,400 bytes at 1280x720). Padding after rows
/// counts, so a padded BGRA or YUY2 frame is judged by its real extent and not by its packed size. Planar NV12 must
/// be top-down.
///
/// # Errors
/// If the frame has no size, the pitch is 0 or narrower than a row, the layout is MJPG (it has no rows), or the
/// frame does not fit in the allocation.
pub fn frame_span(
    format: PixelFormat,
    width: u32,
    height: u32,
    pitch: isize,
    scanline0: usize,
    alloc_len: usize,
) -> Result<FrameSpan, &'static str> {
    let step = pitch.unsigned_abs();
    let Some(row_bytes) = format.tight_stride(width) else { return Err("this layout has no rows") };
    if width == 0 || height == 0 || step == 0 || step < row_bytes {
        return Err("row pitch is smaller than a row");
    }
    if pitch < 0 && format == PixelFormat::Nv12 {
        return Err("planar frames cannot be bottom-up");
    }
    let rows = height as usize;
    let len = format.frame_len(height, step).ok_or("frame size overflows")?;
    let start = if pitch < 0 {
        let back = (rows - 1).checked_mul(step).ok_or("frame size overflows")?;
        scanline0.checked_sub(back).ok_or("frame starts before its allocation")?
    } else {
        scanline0
    };
    match start.checked_add(len) {
        Some(end) if end <= alloc_len => Ok(FrameSpan { start, len }),
        _ => Err("frame does not fit in its allocation"),
    }
}

/// Numbers frames like a device counter would, from capture times: a gap in the numbers is a skipped capture.
///
/// Media Foundation gives no frame counter, so the step between two frames is their time apart in whole frame
/// intervals (at least 1). Counting steps, not elapsed time from the start, keeps a camera whose real rate is a
/// little off its nominal one from showing invented gaps.
#[derive(Copy, Clone, Debug)]
pub struct SeqTracker {
    interval: i64,
    last: Option<(u64, i64)>,
}

impl SeqTracker {
    /// For a camera running at `fps_num / fps_den` frames per second; a rate of 0 steps by 1 per frame (a
    /// discontinuity still skips one).
    pub fn new(fps_num: u32, fps_den: u32) -> Self {
        let interval = if fps_num == 0 { 0 } else { i64::from(fps_den) * 10_000_000 / i64::from(fps_num) };
        Self { interval, last: None }
    }

    /// Changes the nominal rate after the camera renegotiated, keeping the numbering going.
    pub fn set_rate(&mut self, fps_num: u32, fps_den: u32) {
        self.interval = Self::new(fps_num, fps_den).interval;
    }

    /// The number of the frame captured at `time` (100 ns units). The first is 0. `discontinuity` is the driver's
    /// flag that frames were lost before this one, so it steps by at least 2 even if the times look regular.
    pub fn next(&mut self, time: i64, discontinuity: bool) -> u64 {
        let seq = match self.last {
            None => 0,
            Some((seq, before)) => {
                let apart = time.saturating_sub(before);
                let mut step = if self.interval > 0 && apart > 0 {
                    u64::try_from(apart.saturating_add(self.interval / 2) / self.interval).unwrap_or(u64::MAX)
                } else {
                    1
                };
                step = step.max(if discontinuity { 2 } else { 1 });
                seq.saturating_add(step)
            }
        };
        self.last = Some((seq, time));
        seq
    }
}

/// Why a wait on a [`TokenSlot`] ended without a frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SlotFailure {
    /// The stream ended or errored.
    Lost,
    /// Media Foundation reported this `HRESULT`.
    Hresult(i32),
}

impl SlotFailure {
    const LOST: u64 = 1;
    const HRESULT_TAG: u64 = 2 << 32;

    const fn encode(self) -> u64 {
        match self {
            Self::Lost => Self::LOST,
            Self::Hresult(hr) => Self::HRESULT_TAG | hr.cast_unsigned() as u64,
        }
    }

    #[allow(clippy::cast_possible_truncation)] // the low 32 bits are the code
    const fn decode(bits: u64) -> Option<Self> {
        match bits {
            0 => None,
            Self::LOST => Some(Self::Lost),
            _ => Some(Self::Hresult((bits as u32).cast_signed())),
        }
    }
}

/// What [`TokenSlot::take`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Take<T> {
    /// The newest item.
    Item(T),
    /// The producer reported a change that the taker must handle before it uses any item (a new media type); the
    /// item that was waiting, if any, comes back to be released unused. The flag stays set until this is returned, so
    /// replacing the item cannot lose it.
    Retype(Option<T>),
    /// Nothing arrived within the time.
    Timeout,
    /// The producer failed; stays so until the slot is dropped.
    Failed(SlotFailure),
    /// The slot was closed.
    Closed,
}

/// How a [`TokenSlot`] wakes its taker. `notify` must be latched: a `notify` with nobody waiting makes the next
/// `wait` return at once (a Win32 auto-reset event, or a flag and a condition variable in a test).
pub trait Signal: Send + Sync {
    /// Wakes the taker, now or at its next `wait`. Called from the producer's thread; must not block.
    fn notify(&self);
    /// Waits up to `timeout` for a `notify`, consuming it. May return early; the slot rechecks.
    fn wait(&self, timeout: Duration);
}

/// A lock-free one-item mailbox with a timed wait: the capture callback puts, the caller takes, the newest wins.
///
/// The item is a pointer (a COM object the owner turns into a raw pointer and back); the slot only moves it, never
/// dereferences it, so it needs no `unsafe` and the pointer keeps its provenance. Every state is one atomic word, so
/// neither side ever takes a lock or allocates; the only blocking is the taker's [`Signal::wait`]. Whoever holds a
/// pointer owns what it stands for: `put` hands back every pointer it displaces through `release`, and `drain` hands
/// back the one left.
#[derive(Debug)]
pub struct TokenSlot<S: Signal> {
    item: AtomicPtr<c_void>,
    retype: AtomicBool,
    failure: AtomicU64,
    closed: AtomicBool,
    flushed: AtomicBool,
    signal: S,
}

impl<S: Signal> TokenSlot<S> {
    /// An empty, open slot that wakes its taker through `signal`.
    pub const fn new(signal: S) -> Self {
        Self {
            item: AtomicPtr::new(std::ptr::null_mut()),
            retype: AtomicBool::new(false),
            failure: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            flushed: AtomicBool::new(false),
            signal,
        }
    }

    /// Stores `token`, replacing any older one. Every token the slot lets go of, the older one or (once the slot is
    /// closed) `token` itself, goes to `release` exactly once, so nothing leaks and nothing is released twice.
    pub fn put(&self, token: NonNull<c_void>, mut release: impl FnMut(NonNull<c_void>)) {
        if let Some(old) = NonNull::new(self.item.swap(token.as_ptr(), Ordering::AcqRel)) {
            release(old);
        }
        // Checked after the swap: a close that lands after the check also drains, so one of the two reclaims it.
        if self.closed.load(Ordering::SeqCst) {
            if let Some(mine) = self.drain() {
                release(mine);
            }
        } else {
            self.signal.notify();
        }
    }

    /// Tells the taker that the media type changed. Call it before putting the first item of the new type: the taker
    /// that sees such an item then also sees this.
    pub fn set_retype(&self) {
        self.retype.store(true, Ordering::SeqCst);
        self.signal.notify();
    }

    /// Records a failure for the taker (the first one stays).
    pub fn fail(&self, failure: SlotFailure) {
        let _ = self.failure.compare_exchange(0, failure.encode(), Ordering::SeqCst, Ordering::SeqCst);
        self.signal.notify();
    }

    /// Closes the slot: takes return [`Take::Closed`] and puts hand their token straight back.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.signal.notify();
    }

    /// Whether [`close`](Self::close) was called.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// The token left in the slot, if any, for its owner to release.
    pub fn drain(&self) -> Option<NonNull<c_void>> {
        NonNull::new(self.item.swap(std::ptr::null_mut(), Ordering::AcqRel))
    }

    /// Records that the producer finished flushing (see [`wait_flushed`](Self::wait_flushed)).
    pub fn mark_flushed(&self) {
        self.flushed.store(true, Ordering::SeqCst);
        self.signal.notify();
    }

    /// Waits up to `timeout` for the producer's flush to finish; whether it did.
    pub fn wait_flushed(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        while !self.flushed.load(Ordering::SeqCst) {
            let Some(left) = remaining(deadline) else { return false };
            self.signal.wait(left);
        }
        true
    }

    /// The newest token, waiting up to `timeout` for one. A pending type change is returned first, with the token
    /// that was waiting (see [`Take::Retype`]); then a token that arrived before a failure or a close. A zero
    /// `timeout` only looks.
    pub fn take(&self, timeout: Duration) -> Take<NonNull<c_void>> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            // The token first, then the flag: a token put after the flag was set shows the flag.
            let token = self.drain();
            if self.retype.swap(false, Ordering::SeqCst) {
                return Take::Retype(token);
            }
            if let Some(token) = token {
                return Take::Item(token);
            }
            if let Some(failure) = SlotFailure::decode(self.failure.load(Ordering::SeqCst)) {
                return Take::Failed(failure);
            }
            if self.closed.load(Ordering::SeqCst) {
                return Take::Closed;
            }
            let Some(left) = remaining(deadline) else { return Take::Timeout };
            self.signal.wait(left);
        }
    }
}

/// Time left until `deadline`, or `None` when it has passed. A deadline too far off to represent waits in one-hour
/// pieces.
fn remaining(deadline: Option<Instant>) -> Option<Duration> {
    match deadline {
        Some(at) => Some(at.saturating_duration_since(Instant::now())).filter(|d| !d.is_zero()),
        None => Some(Duration::from_secs(3600)),
    }
}

/// The least a sample of `mode` can take when its pieces are gathered into one buffer, from the mode alone: the
/// frame at its tight stride, or, for MJPG (whose size varies), twice the pixel count, which a compressed image of
/// that many pixels stays under.
fn gather_bytes(mode: &ModeInfo) -> usize {
    let pixels = (mode.width as usize).saturating_mul(mode.height as usize);
    match mode.format.tight_stride(mode.width) {
        None => pixels.saturating_mul(2),
        Some(row) => mode.format.frame_len(mode.height, row).unwrap_or(usize::MAX),
    }
}

/// What a native media type says about the size of its samples, when it says anything: `MF_MT_SAMPLE_SIZE` and
/// `MF_MT_DEFAULT_STRIDE` (negative for a bottom-up image).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeSizes {
    /// `MF_MT_SAMPLE_SIZE`: the bytes of one sample, padding included.
    pub sample_size: Option<u32>,
    /// `MF_MT_DEFAULT_STRIDE`, as the signed value Media Foundation stores in an unsigned attribute.
    pub default_stride: Option<i32>,
}

/// The bytes one sample of `mode` can take, as far as its native type tells: the largest of the declared sample
/// size, the declared stride's frame (planes included) and the mode's own tight size. Never less than the tight size.
fn sample_bytes(mode: &ModeInfo, sizes: &NativeSizes) -> usize {
    let declared = sizes.sample_size.map_or(0, |n| n as usize);
    let strided = match (sizes.default_stride, mode.format) {
        (Some(stride), format) if format != PixelFormat::Mjpg => {
            format.frame_len(mode.height, stride.unsigned_abs() as usize).unwrap_or(usize::MAX)
        }
        _ => 0,
    };
    gather_bytes(mode).max(declared).max(strided)
}

/// Bytes of top-down copy a `mode` needs: BGRA, the one layout Windows hands over bottom-up, and nothing else.
fn flip_bytes(mode: &ModeInfo) -> usize {
    match (mode.format, mode.format.tight_stride(mode.width)) {
        (PixelFormat::Bgra8, Some(row)) => row.saturating_mul(mode.height as usize),
        _ => 0,
    }
}

/// The storage a stream sizes once, at open, for the largest mode the device offers, so that no frame call, and no
/// renegotiation, ever allocates or grows anything. A mode beyond it (a driver that lied about its modes) is refused.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Capacity {
    /// Bytes of the gather buffer for samples delivered in several pieces.
    pub gather: usize,
    /// Bytes of the top-down copy buffer for bottom-up BGRA frames.
    pub flip: usize,
}

impl Capacity {
    /// Enough for every one of `modes`, judged from the modes alone.
    pub fn for_modes(modes: &[ModeInfo]) -> Self {
        let native: Vec<(ModeInfo, NativeSizes)> = modes.iter().map(|m| (*m, NativeSizes::default())).collect();
        Self::for_native(&native)
    }

    /// Enough for every one of the device's native types: the largest sample any of them declares or implies.
    pub fn for_native(modes: &[(ModeInfo, NativeSizes)]) -> Self {
        Self {
            gather: modes.iter().map(|(m, sizes)| sample_bytes(m, sizes)).max().unwrap_or(0),
            flip: modes.iter().map(|(m, _)| flip_bytes(m)).max().unwrap_or(0),
        }
    }

    /// Whether `mode` fits.
    ///
    /// # Errors
    /// If it needs more storage than was sized at open.
    pub fn admits(&self, mode: &ModeInfo) -> Result<(), &'static str> {
        if gather_bytes(mode) <= self.gather && flip_bytes(mode) <= self.flip {
            Ok(())
        } else {
            Err("the camera switched to a mode larger than any it offered at open")
        }
    }

    /// Bytes of the top-down copy `mode` needs (0 when its frames are never flipped).
    pub fn flip_len(mode: &ModeInfo) -> usize {
        flip_bytes(mode)
    }
}

/// The top-down copy of a bottom-up frame, fully allocated at open: switching modes only moves the length used.
#[derive(Debug)]
pub struct RowFlip {
    buf: Vec<u8>,
    len: usize,
}

impl RowFlip {
    /// A buffer of `capacity` bytes, none in use. The only allocation it ever makes.
    pub fn new(capacity: usize) -> Self {
        Self { buf: vec![0; capacity], len: 0 }
    }

    /// Uses `len` bytes from now on (0 for a mode that is never flipped).
    ///
    /// # Errors
    /// If `len` is more than was allocated; nothing is allocated to make room.
    pub fn set_len(&mut self, len: usize) -> Result<(), &'static str> {
        if len > self.buf.len() {
            return Err("the top-down buffer sized at open is too small for this mode");
        }
        self.len = len;
        Ok(())
    }

    /// Bytes in use.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is in use (the mode is never flipped).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Copies `block`'s rows into the buffer top row first (see [`copy_top_down`]) and lends the result.
    ///
    /// # Errors
    /// As [`copy_top_down`], and if `row_bytes * rows` is not the length in use.
    pub fn fill(&mut self, block: &[u8], pitch: isize, row_bytes: usize, rows: usize) -> Result<&[u8], &'static str> {
        if row_bytes.checked_mul(rows) != Some(self.len) {
            return Err("frame size does not match the top-down buffer");
        }
        let used = &mut self.buf[..self.len];
        copy_top_down(block, pitch, row_bytes, rows, used)?;
        Ok(used)
    }
}

/// Which clock a frame's capture time came from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ClockSource {
    /// The driver's capture timestamp (`MFSampleExtension_DeviceTimestamp`).
    Device,
    /// The sample's presentation time, because the stream's first sample had no capture timestamp.
    Sample,
    /// The presentation time moved onto the device clock by the last offset between the two, for a frame that lacks
    /// the timestamp in a stream that started with it.
    SampleAligned,
}

/// A frame's capture time, in 100 ns ticks.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// The time.
    pub ticks: u64,
    /// Where it came from.
    pub source: ClockSource,
    /// This is the first frame to use `source`: worth logging.
    pub first_of_source: bool,
}

/// Chooses each frame's capture time.
///
/// The source is chosen **once per stream, from its first sample**: the driver's timestamp if that sample has one,
/// otherwise the sample's presentation time. It never changes, because the two are on different epochs (the device
/// timestamp is in the QPC epoch, the sample time starts near zero with the stream), and a switch would look like an
/// enormous jump, and an enormous number of dropped frames downstream.
/// - Chosen the device clock, a later frame without the timestamp keeps that epoch: its sample time is moved by the
///   offset between the two clocks at the last frame that had both.
/// - Chosen the sample clock, a timestamp that turns up later is ignored.
#[derive(Copy, Clone, Debug, Default)]
pub struct CaptureClock {
    /// `Some(true)` once the stream is on the device clock, `Some(false)` on the sample clock.
    on_device: Option<bool>,
    offset: i64,
    seen: u8,
}

impl CaptureClock {
    /// A clock that has seen no frame.
    pub const fn new() -> Self {
        Self { on_device: None, offset: 0, seen: 0 }
    }

    /// The capture time of a frame with this `device` timestamp (if the driver gave one) and `sample` time.
    pub fn stamp(&mut self, device: Option<u64>, sample: i64) -> Stamp {
        let on_device = *self.on_device.get_or_insert(device.is_some());
        let (ticks, source) = match (on_device, device) {
            (true, Some(d)) => {
                self.offset = i64::try_from(d).unwrap_or(i64::MAX).saturating_sub(sample);
                (d, ClockSource::Device)
            }
            (true, None) => (sample.saturating_add(self.offset).max(0).cast_unsigned(), ClockSource::SampleAligned),
            (false, _) => (sample.max(0).cast_unsigned(), ClockSource::Sample),
        };
        let bit = match source {
            ClockSource::Device => 1,
            ClockSource::Sample => 2,
            ClockSource::SampleAligned => 4,
        };
        let first_of_source = self.seen & bit == 0;
        self.seen |= bit;
        Stamp { ticks, source, first_of_source }
    }
}

/// Whether a native type's `MF_MT_INTERLACE_MODE` (when it has one) describes progressive frames. Absent is
/// progressive: most UVC webcams do not set it. `Unknown` (0) and `Progressive` (2) are accepted; every field-based
/// or mixed mode is not, since the reader runs with video processing off and could only hand over combed or
/// half-height frames.
pub const fn is_progressive(interlace: Option<u32>) -> bool {
    matches!(interlace, None | Some(0 | 2))
}

/// Runs `work(resource)` on a detached thread and returns at once, so a caller that must not wait (a stream being
/// dropped inside a stop deadline) never waits on a stalled driver. If the thread cannot be started the resource is
/// leaked: a leaked camera is better than a blocked tracker.
pub fn reap<T: Send + 'static>(resource: T, work: fn(T)) {
    let cell = std::sync::Arc::new(Mutex::new(Some(resource)));
    let theirs = std::sync::Arc::clone(&cell);
    let spawned = std::thread::Builder::new().name("camera-mf-reaper".into()).spawn(move || {
        let taken = theirs.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(resource) = taken {
            work(resource);
        }
    });
    if spawned.is_err() {
        let taken = cell.lock().unwrap_or_else(PoisonError::into_inner).take();
        std::mem::forget(taken);
    }
}

/// Takes from `slot` until a frame, a failure, a close or the original deadline, handling each media-type change on
/// the way: `retype` gets the token that was waiting (to release, unused) and re-reads the type. A change costs the
/// wait nothing, so `Take::Timeout` means the whole `timeout` passed.
///
/// # Errors
/// What `retype` returns, if it fails.
pub fn take_after_retype<S: Signal, E>(
    slot: &TokenSlot<S>,
    timeout: Duration,
    mut retype: impl FnMut(Option<NonNull<c_void>>) -> Result<(), E>,
) -> Result<Take<NonNull<c_void>>, E> {
    let deadline = Instant::now().checked_add(timeout);
    loop {
        let left = deadline.map_or(timeout, |at| at.saturating_duration_since(Instant::now()));
        match slot.take(left) {
            Take::Retype(token) => retype(token)?,
            other => return Ok(other),
        }
    }
}
