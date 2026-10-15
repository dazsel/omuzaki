//! The camera seam: device lists, format choice, and the borrowed frames a stream hands out.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::TrackError;

/// A camera as its backend names it (a device path, a symbolic link). It may name the machine, so it lives in the
/// person's settings, never in a project.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct CameraId(pub String);

/// How a frame's bytes are laid out. Rows are `stride` bytes apart; only the first bytes of each row are pixels.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    /// A `height`-row Y plane, then a `height / 2`-row plane of interleaved U, V pairs, both at `stride`.
    Nv12,
    /// Packed Y0 U Y1 V, two bytes per pixel.
    Yuy2,
    /// One JPEG image per frame; `stride` is 0 and `data` is the whole compressed image.
    Mjpg,
    /// Packed R, G, B.
    Rgb8,
    /// Packed B, G, R, A.
    Bgra8,
}

impl PixelFormat {
    /// The bytes of one row with no padding, or `None` for [`PixelFormat::Mjpg`].
    pub const fn tight_stride(self, width: u32) -> Option<usize> {
        let w = width as usize;
        match self {
            Self::Nv12 => Some(w),
            Self::Yuy2 => Some(2 * w),
            Self::Rgb8 => Some(3 * w),
            Self::Bgra8 => Some(4 * w),
            Self::Mjpg => None,
        }
    }

    /// The bytes of a whole frame at `stride`, or `None` for [`PixelFormat::Mjpg`] (and on overflow).
    pub fn frame_len(self, height: u32, stride: usize) -> Option<usize> {
        let rows = match self {
            Self::Nv12 => height as usize + (height as usize).div_ceil(2),
            Self::Mjpg => return None,
            _ => height as usize,
        };
        rows.checked_mul(stride)
    }

    /// Relative cost of turning this format into RGB, lowest first: the tie-break in [`CameraFormatRequest::pick`].
    const fn convert_cost(self) -> u8 {
        match self {
            Self::Rgb8 => 0,
            Self::Bgra8 => 1,
            Self::Nv12 => 2,
            Self::Yuy2 => 3,
            Self::Mjpg => 4,
        }
    }
}

/// One mode a camera can run in.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct CameraFormat {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Nominal frames per second. Real frame times come from [`CameraFrame::captured`].
    pub fps: u32,
    /// Byte layout.
    pub pixel: PixelFormat,
}

/// A camera in a device list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    /// What to pass to [`Camera::open`].
    pub id: CameraId,
    /// The name the OS shows, for the picker.
    pub name: String,
    /// Every mode it offers, in the backend's order.
    pub formats: Vec<CameraFormat>,
}

/// The mode a person asked for; [`pick`](Self::pick) finds the closest one a camera has.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct CameraFormatRequest {
    /// Wanted width in pixels.
    pub width: u32,
    /// Wanted height in pixels.
    pub height: u32,
    /// Wanted frames per second.
    pub fps: u32,
    /// A required byte layout, or `None` for any.
    #[serde(default)]
    pub pixel: Option<PixelFormat>,
}

impl Default for CameraFormatRequest {
    /// 1280 x 720 at 60 fps, any layout (PF-01: 60 fps when the camera allows).
    fn default() -> Self {
        Self { width: 1280, height: 720, fps: 60, pixel: None }
    }
}

impl CameraFormatRequest {
    /// The closest format: nearest pixel count, then nearest frame rate, then the cheapest to convert, then the
    /// backend's order. Every backend picks with this, so one request means the same mode everywhere. `None` if no
    /// format has the required layout.
    pub fn pick(&self, formats: &[CameraFormat]) -> Option<CameraFormat> {
        let area = u64::from(self.width) * u64::from(self.height);
        formats.iter().copied().filter(|f| self.pixel.is_none_or(|p| p == f.pixel)).min_by_key(|f| {
            let f_area = u64::from(f.width) * u64::from(f.height);
            (f_area.abs_diff(area), f.fps.abs_diff(self.fps), f.pixel.convert_cost())
        })
    }
}

/// A timestamp on the camera's own clock, in nanoseconds from an epoch the device chooses. Only differences mean
/// anything; [`ClockMap`](crate::clock::ClockMap) puts it on the stage clock.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceTime(pub u64);

/// One captured frame, borrowed from the stream's own buffers until the next call to
/// [`CameraStream::next_frame`], so steady-state capture allocates nothing.
#[derive(Copy, Clone, Debug)]
pub struct CameraFrame<'a> {
    /// The pixel bytes, laid out as [`PixelFormat`] says.
    pub data: &'a [u8],
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bytes from one row to the next (0 for [`PixelFormat::Mjpg`]).
    pub stride: usize,
    /// Byte layout.
    pub pixel: PixelFormat,
    /// When the sensor captured it, on the device clock.
    pub captured: DeviceTime,
    /// The device's frame counter; a gap means frames were skipped (the newest frame wins at capture too).
    pub seq: u64,
}

/// A source of cameras: one per OS backend, plus the fake.
pub trait Camera: Send {
    /// The cameras present now. Cold: may allocate and may take tens of milliseconds.
    fn enumerate(&self) -> Vec<CameraInfo>;

    /// Opens a camera in the format [`CameraFormatRequest::pick`] chooses. Cold. Only person-initiated paths call it
    /// (see the crate docs).
    ///
    /// # Errors
    /// [`TrackError::NoDevice`], [`TrackError::Busy`], [`TrackError::FormatUnsupported`],
    /// [`TrackError::PermissionDenied`] or [`TrackError::Backend`].
    fn open(&mut self, id: &CameraId, request: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError>;
}

/// A running camera. Dropping it releases the device.
pub trait CameraStream: Send {
    /// The mode it runs in.
    fn format(&self) -> CameraFormat;

    /// The newest frame the device has, waiting up to `timeout` for one. `Ok(None)` means none arrived in time.
    /// Blocks: call it on the tracker thread only. Hot path otherwise: it may not allocate.
    ///
    /// # Errors
    /// [`TrackError::Lost`] once the device is gone; [`TrackError::Backend`] for a driver failure.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(width: u32, height: u32, fps: u32, pixel: PixelFormat) -> CameraFormat {
        CameraFormat { width, height, fps, pixel }
    }

    #[test]
    fn pick_prefers_size_then_rate_then_cheap_layouts() {
        let formats = [
            fmt(640, 480, 30, PixelFormat::Yuy2),
            fmt(1280, 720, 30, PixelFormat::Mjpg),
            fmt(1280, 720, 60, PixelFormat::Mjpg),
            fmt(1280, 720, 60, PixelFormat::Nv12),
        ];
        let request = CameraFormatRequest::default();
        assert_eq!(request.pick(&formats), Some(formats[3]));
        let yuy2 = CameraFormatRequest { pixel: Some(PixelFormat::Yuy2), ..request };
        assert_eq!(yuy2.pick(&formats), Some(formats[0]));
        let bgra = CameraFormatRequest { pixel: Some(PixelFormat::Bgra8), ..request };
        assert_eq!(bgra.pick(&formats), None);
    }

    #[test]
    fn frame_lengths_follow_the_layouts() {
        assert_eq!(PixelFormat::Nv12.frame_len(480, 640), Some(640 * 720));
        assert_eq!(PixelFormat::Yuy2.tight_stride(640), Some(1280));
        assert_eq!(PixelFormat::Bgra8.frame_len(2, 16), Some(32));
        assert_eq!(PixelFormat::Mjpg.frame_len(480, 0), None);
    }
}
