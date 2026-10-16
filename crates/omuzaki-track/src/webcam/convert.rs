//! Camera frames of any [`PixelFormat`] to packed RGB8 in a buffer the converter owns.

use omuzaki_pixfmt::{
    ConvertError, MjpgDecoder, Plane, RgbOut, Yuv, bgra_to_rgb, nv12_to_rgb, rgb_to_rgb, yuy2_to_rgb,
};
use omuzaki_track_api::camera::{CameraFrame, PixelFormat};
use omuzaki_track_api::error::TrackError;

use crate::frame::RgbFrame;

/// [`CameraFrame`] (any [`PixelFormat`]) to packed RGB8 in a buffer it owns, allocated at the first frame or when the
/// frame size changes (or at [`with_size`](Self::with_size), for the mode a camera opened with). NV12 and YUY2 are
/// read as BT.601 limited range, which is what UVC webcams send. The raw formats allocate nothing per frame. MJPG
/// decodes into the same buffer, but zune-jpeg builds a small decoder per image (see the note in `convert`).
#[derive(Debug, Default)]
pub struct FrameConverter {
    rgb: Vec<u8>,
    mjpg: MjpgDecoder,
}

impl FrameConverter {
    /// A converter with no buffer yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// A converter whose buffer is already sized for `width` x `height` frames, so the first frame allocates nothing.
    pub fn with_size(width: u32, height: u32) -> Self {
        Self { rgb: vec![0; rgb_len(width, height)], mjpg: MjpgDecoder::new() }
    }

    /// Converts `frame`; the result borrows the converter's buffer until the next call.
    ///
    /// # Errors
    /// [`TrackError::Backend`] for a frame the converter cannot read: a zero or odd side where the format needs an
    /// even one, data shorter than its layout, a damaged JPEG, or a pixel format this version does not know.
    pub fn convert(&mut self, frame: &CameraFrame<'_>) -> Result<RgbFrame<'_>, TrackError> {
        convert_into(&mut self.mjpg, frame, &mut self.rgb).map_err(|e| TrackError::Backend(e.to_string()))?;
        Ok(RgbFrame { data: &self.rgb, width: frame.width, height: frame.height, stride: 3 * frame.width as usize })
    }
}

/// Why a frame could not be converted. Built without allocating, so a stream of bad frames costs the camera thread's
/// hot loop nothing; the public [`FrameConverter::convert`] turns it into text.
#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    /// The pixel code refused the frame.
    #[error("pixel conversion: {0}")]
    Pixel(#[from] ConvertError),
    /// No converter exists for this format.
    #[error("no converter for {0:?}")]
    NoConverter(PixelFormat),
}

/// `frame` as packed RGB8 in `rgb`, resized to fit if it is not that size already (so, no allocation once it has
/// been). The camera thread converts into a buffer it then hands to the tracker, which is why this takes the buffer.
pub(super) fn convert_into(mjpg: &mut MjpgDecoder, frame: &CameraFrame<'_>, rgb: &mut Vec<u8>) -> Result<(), Failure> {
    let (w, h) = (frame.width, frame.height);
    let len = rgb_len(w, h);
    if rgb.len() != len {
        rgb.resize(len, 0);
    }
    let mut out = RgbOut { data: rgb.as_mut_slice(), width: w, height: h };
    let plane = Plane { data: frame.data, stride: frame.stride };
    let converted = match frame.pixel {
        PixelFormat::Nv12 => {
            // The Y plane is `height` rows, the interleaved UV plane follows it.
            let y_len = frame.stride.saturating_mul(h as usize);
            match frame.data.split_at_checked(y_len) {
                Some((y, uv)) => nv12_to_rgb(
                    Plane { data: y, stride: frame.stride },
                    Plane { data: uv, stride: frame.stride },
                    Yuv::default(),
                    &mut out,
                ),
                None => Err(ConvertError::ShortPlane { plane: "y", need: y_len, got: frame.data.len() }),
            }
        }
        PixelFormat::Yuy2 => yuy2_to_rgb(plane, Yuv::default(), &mut out),
        PixelFormat::Rgb8 => rgb_to_rgb(plane, &mut out),
        PixelFormat::Bgra8 => bgra_to_rgb(plane, false, &mut out),
        // The pixels go straight into our buffer. What remains per frame is inside zune-jpeg, which builds its
        // decoder (tables and row buffers, a few small allocations, none of them frame sized) for each image
        // and offers no way to hand one back; `omuzaki-pixfmt` is outside this task's fence. Measured in
        // `tests/webcam.rs` and `benches/webcam.rs`.
        PixelFormat::Mjpg => mjpg.decode(frame.data, &mut out),
        other => return Err(Failure::NoConverter(other)),
    };
    Ok(converted?)
}

/// Bytes of a packed RGB8 frame.
pub(super) fn rgb_len(width: u32, height: u32) -> usize {
    (width as usize).saturating_mul(height as usize).saturating_mul(3)
}

#[cfg(test)]
mod tests {
    use omuzaki_track_api::camera::DeviceTime;

    use super::*;

    fn camera_frame(data: &[u8], pixel: PixelFormat, stride: usize) -> CameraFrame<'_> {
        CameraFrame::new(data, 4, 2, stride, pixel, DeviceTime(0), 0)
    }

    #[test]
    fn strided_rgb_comes_out_packed() {
        // Two rows of 4 pixels with 4 bytes of padding after each.
        let data: Vec<u8> = (0..32).collect();
        let mut converter = FrameConverter::new();
        let rgb = converter.convert(&camera_frame(&data, PixelFormat::Rgb8, 16)).unwrap();
        assert_eq!((rgb.width, rgb.height, rgb.stride), (4, 2, 12));
        let want: Vec<u8> = (0..12).chain(16..28).collect();
        assert_eq!(rgb.data, want);
    }

    #[test]
    fn the_buffer_follows_the_frame_size() {
        let mut converter = FrameConverter::new();
        converter.convert(&camera_frame(&[0; 24], PixelFormat::Rgb8, 12)).unwrap();
        let small = CameraFrame::new(&[0; 6], 2, 1, 6, PixelFormat::Rgb8, DeviceTime(0), 1);
        assert_eq!(converter.convert(&small).unwrap().data.len(), 6);
    }

    #[test]
    fn bad_frames_are_backend_errors_not_panics() {
        let mut converter = FrameConverter::new();
        for (pixel, data) in
            [(PixelFormat::Nv12, &[0_u8; 5][..]), (PixelFormat::Yuy2, &[0; 3]), (PixelFormat::Mjpg, &[1, 2, 3])]
        {
            let result = converter.convert(&camera_frame(data, pixel, 4));
            assert!(matches!(result, Err(TrackError::Backend(_))), "{pixel:?}");
        }
    }
}
