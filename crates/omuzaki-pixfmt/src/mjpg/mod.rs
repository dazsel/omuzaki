//! MJPG (a JPEG per frame) to RGB8: our own allocation-free baseline decoder for what webcams send, and zune-jpeg for
//! everything else.

mod baseline;
mod color;
mod huffman;
mod idct;

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::{ConvertError, RgbOut, check_out};
use baseline::{Baseline, Parsed};

/// Decodes MJPG frames straight into the caller's RGB buffer.
///
/// Webcam MJPG is baseline JPEG: 8-bit, Huffman-coded, YCbCr at 4:2:2 or 4:2:0, often without its own Huffman tables.
/// Those frames go through a decoder that keeps its tables and sample planes between frames, so after the first frame
/// of a size a stream allocates nothing. It uses libjpeg's integer IDCT, fancy upsampling and colour maths, so its
/// pixels match libjpeg-turbo's. Any other JPEG (progressive, 12-bit, CMYK, unusual sampling), and any frame the fast
/// path rejects, is decoded by zune-jpeg instead, which allocates per frame; [`fallbacks`](Self::fallbacks) counts
/// those. Keep one decoder per camera.
#[derive(Debug)]
pub struct MjpgDecoder {
    options: DecoderOptions,
    baseline: Baseline,
    fallbacks: u64,
}

impl Default for MjpgDecoder {
    /// The same as [`MjpgDecoder::new`]: a derived default would leave strict mode off.
    fn default() -> Self {
        Self::new()
    }
}

impl MjpgDecoder {
    /// A decoder that rejects damaged frames rather than return half an image.
    pub fn new() -> Self {
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB).set_strict_mode(true);
        Self { options, baseline: Baseline::new(), fallbacks: 0 }
    }

    /// How many frames went to zune-jpeg rather than the allocation-free path.
    pub fn fallbacks(&self) -> u64 {
        self.fallbacks
    }

    /// Decodes one JPEG into `out`.
    ///
    /// # Errors
    /// [`ConvertError::BadSize`] or [`ConvertError::ShortOutput`] for an unusable output, [`ConvertError::Jpeg`] when
    /// the data is truncated or corrupt, and [`ConvertError::JpegSize`] when the image is not `out.width` x
    /// `out.height` (nothing is decoded in that case). A decode that fails part-way can leave partial pixels in `out`.
    pub fn decode(&mut self, jpeg: &[u8], out: &mut RgbOut<'_>) -> Result<(), ConvertError> {
        let (w, h, _) = check_out(out, false, false)?;
        if let Ok(Parsed::Frame(frame)) = self.baseline.parse(jpeg) {
            if (frame.width, frame.height) != (w, h) {
                return Err(size_error(out, frame.width, frame.height));
            }
            if self.baseline.decode(&frame, jpeg, out.data).is_ok() {
                return Ok(());
            }
            // A frame the fast path rejects gets a second opinion from zune-jpeg, which reports the error if it
            // rejects it too.
        }
        self.fallbacks += 1;
        let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), self.options);
        decoder.decode_headers().map_err(|e| ConvertError::Jpeg(e.to_string()))?;
        let (jw, jh) = decoder.dimensions().unwrap_or((0, 0));
        if (jw, jh) != (w, h) {
            return Err(size_error(out, jw, jh));
        }
        let need = w * h * 3;
        decoder.decode_into(&mut out.data[..need]).map_err(|e| ConvertError::Jpeg(e.to_string()))
    }
}

fn size_error(out: &RgbOut<'_>, w: usize, h: usize) -> ConvertError {
    let clamp = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    ConvertError::JpegSize { expected: (out.width, out.height), got: (clamp(w), clamp(h)) }
}
