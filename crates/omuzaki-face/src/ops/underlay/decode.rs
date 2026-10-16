//! Reading the sheet's image with its size bounded before anything is allocated.

use std::io::{self, BufRead, Cursor, Read, Seek, SeekFrom};

use image::{DynamicImage, ImageDecoder, ImageError, ImageReader, Limits, RgbaImage};
use omuzaki_ops_api::OpError;

/// The longest side a sheet image may have, in pixels. The bound `omuzaki-io-ops` puts on every texture it reads.
const MAX_SIDE: u32 = 16_384;

/// The most memory a decode may allocate, in bytes: the decoder's own buffer and the RGBA8 copy made from it. The same
/// bound as for textures in `omuzaki-io-ops`.
const MAX_DECODE_BYTES: u64 = 1 << 30;

/// The bytes of an image that stop being handed out once `stop` says so, so a decoder that pulls its input as it goes
/// (PNG inflates a chunk at a time) ends promptly when a job is cancelled, instead of after the whole decode.
struct Interruptible<'a> {
    inner: Cursor<&'a [u8]>,
    stop: &'a dyn Fn() -> bool,
}

impl Interruptible<'_> {
    fn check(&self) -> io::Result<()> {
        if (self.stop)() { Err(io::Error::other(CANCELLED)) } else { Ok(()) }
    }
}

/// The text of the I/O error a stopped read returns, so the decode's caller can tell a stop from a bad image.
const CANCELLED: &str = "the decode was cancelled";

impl Read for Interruptible<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        self.inner.read(buf)
    }
}

impl BufRead for Interruptible<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if (self.stop)() {
            return Err(io::Error::other(CANCELLED));
        }
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

impl Seek for Interruptible<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

/// Decodes `bytes` (the asset `asset`) to RGBA8. The header's dimensions are read and checked first, and the decoder is
/// built with allocation limits, so a stored compression bomb is refused instead of exhausting memory. `stop` is asked
/// each time the decoder asks for more of the input; once it says `true` the decode ends and the answer is `Ok(None)`.
/// A decoder that takes its whole input in one read cannot be stopped part-way, which the size bounds keep short.
///
/// # Errors
/// [`OpError`] naming `asset` when the bytes are not an image this build reads, or the image is larger than the bounds.
pub fn rgba(bytes: &[u8], asset: &str, stop: &dyn Fn() -> bool) -> Result<Option<RgbaImage>, OpError> {
    rgba_within(bytes, asset, MAX_DECODE_BYTES, stop)
}

/// [`rgba`] with the memory bound as a parameter, so a test can reach it with a small image.
fn rgba_within(
    bytes: &[u8],
    asset: &str,
    max_bytes: u64,
    stop: &dyn Fn() -> bool,
) -> Result<Option<RgbaImage>, OpError> {
    let stopped = |e: &ImageError| matches!(e, ImageError::IoError(io) if io.to_string() == CANCELLED);
    let not_an_image = |e: &dyn std::fmt::Display| {
        OpError::invalid("asset", format!("asset {asset} is not an image this build reads: {e}"))
    };
    let mut limits = Limits::default();
    limits.max_alloc = Some(max_bytes);
    let input = Interruptible { inner: Cursor::new(bytes), stop };
    let mut reader = match ImageReader::new(input).with_guessed_format() {
        Ok(reader) => reader,
        Err(e) if e.to_string() == CANCELLED => return Ok(None),
        Err(e) => return Err(not_an_image(&e)),
    };
    reader.limits(limits);
    let decoder = match reader.into_decoder() {
        Ok(decoder) => decoder,
        Err(e) if stopped(&e) => return Ok(None),
        Err(ImageError::Limits(_)) => return Err(too_large(asset, None, max_bytes)),
        Err(other) => return Err(not_an_image(&other)),
    };
    let (width, height) = decoder.dimensions();
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(too_large(asset, Some((width, height)), max_bytes));
    }
    // `from_decoder` does not count its output buffer against the limit, and a 16-bit image needs twice the bytes of
    // an 8-bit one, so the decoder's whole output and the RGBA8 copy are reserved here, before anything is allocated.
    let needed = decoder.total_bytes().saturating_add(u64::from(width) * u64::from(height) * 4);
    if needed > max_bytes {
        return Err(too_large(asset, Some((width, height)), max_bytes));
    }
    match DynamicImage::from_decoder(decoder) {
        Ok(image) if stop() => {
            drop(image);
            Ok(None)
        }
        Ok(image) => Ok(Some(image.into_rgba8())),
        Err(e) if stopped(&e) => Ok(None),
        Err(ImageError::Limits(_)) => Err(too_large(asset, Some((width, height)), max_bytes)),
        Err(other) => Err(not_an_image(&other)),
    }
}

fn too_large(asset: &str, size: Option<(u32, u32)>, max_bytes: u64) -> OpError {
    let size = size.map_or_else(String::new, |(w, h)| format!(" ({w}x{h})"));
    OpError::invalid(
        "asset",
        format!("asset {asset} is too large{size}: a side may be at most {MAX_SIDE} px and a decode at most {max_bytes} bytes"),
    )
    .hint("give a smaller image of the sheet")
}

#[cfg(test)]
mod tests {
    use image::{ImageFormat, Rgba};

    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
        bytes
    }

    #[test]
    fn a_small_png_decodes() {
        let image = rgba(&png(4, 3), "abc", &|| false).unwrap().unwrap();
        assert_eq!(image.dimensions(), (4, 3));
        assert_eq!(image.get_pixel(0, 0).0, [10, 20, 30, 255]);
    }

    #[test]
    fn a_header_over_the_side_bound_is_refused_before_decoding() {
        // One pixel high, so the file is small whatever the header says; the width alone is over the bound.
        let err = rgba(&png(MAX_SIDE + 1, 1), "abc", &|| false).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("asset"));
        assert!(err.message.contains("too large") && err.message.contains(&format!("{}x1", MAX_SIDE + 1)), "{err:?}");
        // The bound itself is allowed.
        assert_eq!(rgba(&png(MAX_SIDE, 1), "abc", &|| false).unwrap().unwrap().dimensions(), (MAX_SIDE, 1));
    }

    #[test]
    fn a_sixteen_bit_image_is_counted_at_its_decoded_size() {
        use image::{ImageBuffer, Rgba};
        let wide = ImageBuffer::<Rgba<u16>, Vec<u16>>::from_pixel(8, 8, Rgba([1, 2, 3, 65535]));
        let mut sixteen = Vec::new();
        wide.write_to(&mut Cursor::new(&mut sixteen), ImageFormat::Png).unwrap();
        let eight = png(8, 8);
        // 8 x 8: the 8-bit decode is 256 bytes plus the 256-byte RGBA8 copy, the 16-bit decode 512 plus 256.
        assert_eq!(rgba_within(&eight, "abc", 600, &|| false).unwrap().unwrap().dimensions(), (8, 8));
        let err = rgba_within(&sixteen, "abc", 600, &|| false).unwrap_err();
        assert!(err.message.contains("too large") && err.message.contains("600 bytes"), "{err:?}");
        assert_eq!(rgba_within(&sixteen, "abc", 800, &|| false).unwrap().unwrap().dimensions(), (8, 8));
    }

    #[test]
    fn bytes_that_are_not_an_image_name_the_asset() {
        let err = rgba(b"not an image", "abc", &|| false).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("asset"));
        assert!(err.message.contains("not an image this build reads"), "{err:?}");
    }

    #[test]
    fn a_decode_stops_as_soon_as_it_is_told_to() {
        use std::cell::Cell;
        let bytes = png(256, 256);
        // Stopped from the start: nothing is decoded and the answer is "stopped", not an error.
        assert!(rgba(&bytes, "abc", &|| true).unwrap().is_none());
        // Stopped part-way: the first reads pass, a later one is refused.
        let asked = Cell::new(0_u32);
        let after_two = || {
            asked.set(asked.get() + 1);
            asked.get() > 2
        };
        let outcome = rgba(&bytes, "abc", &after_two).unwrap();
        assert!(outcome.is_none(), "a stop while the decoder was still reading ends the decode");
        assert!(asked.get() >= 3);
    }
}
