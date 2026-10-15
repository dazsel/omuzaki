//! Capture modes, media subtypes and error codes: the parts of the camera that need no Windows to build or test.

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
