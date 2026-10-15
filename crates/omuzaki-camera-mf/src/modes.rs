//! Capture modes, media subtypes and error codes, plus the pieces of the reader that need no Windows to build or test:
//! frame bounds, row flipping, the device sequence and the timed hand-off between the capture callback and the caller.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
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

/// Why a wait on a [`Slot`] ended without a frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SlotFailure {
    /// The stream ended or errored.
    Lost,
    /// Media Foundation reported this `HRESULT`.
    Hresult(i32),
}

/// What [`Slot::take`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Take<T> {
    /// The newest item.
    Item(T),
    /// Nothing arrived within the time.
    Timeout,
    /// The producer failed; stays so until the slot is dropped.
    Failed(SlotFailure),
    /// The slot was closed.
    Closed,
}

/// A one-item mailbox with a timed wait: the capture callback puts, the caller takes, the newest item wins.
#[derive(Debug)]
pub struct Slot<T> {
    state: Mutex<SlotState<T>>,
    signal: Condvar,
}

#[derive(Debug)]
struct SlotState<T> {
    item: Option<T>,
    failure: Option<SlotFailure>,
    closed: bool,
    flushed: bool,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Slot<T> {
    /// An empty, open slot.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(SlotState { item: None, failure: None, closed: false, flushed: false }),
            signal: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SlotState<T>> {
        // A panic elsewhere cannot leave this state half-written: every update is a single assignment.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stores `item` for the taker. Returns what the caller must drop: the item it replaced, or `item` itself if the
    /// slot is closed. Dropping outside the lock keeps releases off the hand-off path.
    pub fn put(&self, item: T) -> Option<T> {
        let mut state = self.lock();
        if state.closed {
            return Some(item);
        }
        let old = state.item.replace(item);
        drop(state);
        self.signal.notify_all();
        old
    }

    /// Records a failure for the taker.
    pub fn fail(&self, failure: SlotFailure) {
        self.lock().failure.get_or_insert(failure);
        self.signal.notify_all();
    }

    /// Closes the slot: later puts are handed back and takes return [`Take::Closed`].
    pub fn close(&self) {
        self.lock().closed = true;
        self.signal.notify_all();
    }

    /// Records that the producer finished flushing (see [`wait_flushed`](Self::wait_flushed)).
    pub fn mark_flushed(&self) {
        self.lock().flushed = true;
        self.signal.notify_all();
    }

    /// Waits up to `timeout` for the producer's flush to finish; whether it did.
    pub fn wait_flushed(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        let mut state = self.lock();
        while !state.flushed {
            let Some(left) = remaining(deadline) else { return false };
            state = self.signal.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        true
    }

    /// The newest item, waiting up to `timeout` for one. An item that arrived is returned before a failure or a
    /// close. A zero `timeout` only looks.
    pub fn take(&self, timeout: Duration) -> Take<T> {
        let deadline = Instant::now().checked_add(timeout);
        let mut state = self.lock();
        loop {
            if let Some(item) = state.item.take() {
                return Take::Item(item);
            }
            if let Some(failure) = state.failure {
                return Take::Failed(failure);
            }
            if state.closed {
                return Take::Closed;
            }
            let Some(left) = remaining(deadline) else { return Take::Timeout };
            state = self.signal.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
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
