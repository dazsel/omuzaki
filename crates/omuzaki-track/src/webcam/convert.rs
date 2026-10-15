//! Camera frames of any [`PixelFormat`] to packed RGB8 in a buffer the converter owns.

use omuzaki_pixfmt::{
    ConvertError, MjpgDecoder, Plane, RgbOut, Yuv, bgra_to_rgb, nv12_to_rgb, rgb_to_rgb, yuy2_to_rgb,
};
use omuzaki_track_api::camera::{CameraFrame, PixelFormat};
use omuzaki_track_api::error::TrackError;

use crate::frame::RgbFrame;

/// [`CameraFrame`] (any [`PixelFormat`]) to packed RGB8 in a buffer it owns, allocated at the first frame or when the
/// frame size changes. NV12 and YUY2 are read as BT.601 limited range, which is what UVC webcams send. MJPG decoding
/// allocates inside the JPEG library on every frame; the raw formats allocate nothing.
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

    /// Converts `frame`; the result borrows the converter's buffer until the next call.
    ///
    /// # Errors
    /// [`TrackError::Backend`] for a frame the converter cannot read: a zero or odd side where the format needs an
    /// even one, data shorter than its layout, a damaged JPEG, or a pixel format this version does not know.
    pub fn convert(&mut self, frame: &CameraFrame<'_>) -> Result<RgbFrame<'_>, TrackError> {
        let (w, h) = (frame.width, frame.height);
        let len = (w as usize).saturating_mul(h as usize).saturating_mul(3);
        if self.rgb.len() != len {
            self.rgb.resize(len, 0);
        }
        let mut out = RgbOut { data: &mut self.rgb, width: w, height: h };
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
            PixelFormat::Mjpg => self.mjpg.decode(frame.data, &mut out),
            other => return Err(TrackError::Backend(format!("no converter for {other:?}"))),
        };
        converted.map_err(|e| TrackError::Backend(format!("pixel conversion: {e}")))?;
        Ok(RgbFrame { data: &self.rgb, width: w, height: h, stride: 3 * w as usize })
    }
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
