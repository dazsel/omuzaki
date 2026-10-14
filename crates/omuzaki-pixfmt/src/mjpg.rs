//! MJPG (a JPEG per frame) to RGB8 through zune-jpeg.

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::{ConvertError, RgbOut, check_out};

/// Decodes MJPG frames straight into the caller's RGB buffer.
///
/// zune-jpeg builds a decoder per image, so each [`decode`](Self::decode) makes a few small allocations inside the
/// library (tables and the MCU row buffers); the pixels themselves go straight into `out`. The type exists so a
/// caller keeps one decoder per camera, and so a decoder that can reuse its scratch can slot in without an API change.
#[derive(Debug)]
pub struct MjpgDecoder {
    options: DecoderOptions,
}

impl Default for MjpgDecoder {
    /// The same as [`MjpgDecoder::new`]: a derived default would leave strict mode off.
    fn default() -> Self {
        Self::new()
    }
}

impl MjpgDecoder {
    /// A decoder set to emit RGB and to reject damaged frames (strict mode) rather than return half an image.
    pub fn new() -> Self {
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB).set_strict_mode(true);
        Self { options }
    }

    /// Decodes one JPEG into `out`.
    ///
    /// # Errors
    /// [`ConvertError::BadSize`] or [`ConvertError::ShortOutput`] for an unusable output, [`ConvertError::Jpeg`] when
    /// the data is truncated or corrupt, and [`ConvertError::JpegSize`] when the image is not `out.width` x
    /// `out.height` (nothing is decoded in that case). A decode that fails part-way can leave partial pixels in `out`.
    pub fn decode(&mut self, jpeg: &[u8], out: &mut RgbOut<'_>) -> Result<(), ConvertError> {
        let (w, h, _) = check_out(out, false, false)?;
        let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), self.options);
        decoder.decode_headers().map_err(|e| ConvertError::Jpeg(e.to_string()))?;
        let (jw, jh) = decoder.dimensions().unwrap_or((0, 0));
        if (jw, jh) != (w, h) {
            let clamp = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
            return Err(ConvertError::JpegSize { expected: (out.width, out.height), got: (clamp(jw), clamp(jh)) });
        }
        let need = w * h * 3;
        decoder.decode_into(&mut out.data[..need]).map_err(|e| ConvertError::Jpeg(e.to_string()))
    }
}
