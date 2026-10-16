//! Reading the sheet's image with its size bounded before anything is allocated.

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder, ImageError, ImageReader, Limits, RgbaImage};
use omuzaki_ops_api::OpError;

/// The longest side a sheet image may have, in pixels. The bound `omuzaki-io-ops` puts on every texture it reads.
const MAX_SIDE: u32 = 16_384;

/// The most memory a decode may allocate, in bytes: the decoder's own buffer and the RGBA8 copy made from it. The same
/// bound as for textures in `omuzaki-io-ops`.
const MAX_DECODE_BYTES: u64 = 1 << 30;

/// Decodes `bytes` (the asset `asset`) to RGBA8. The header's dimensions are read and checked first, and the decoder is
/// built with allocation limits, so a stored compression bomb is refused instead of exhausting memory.
///
/// # Errors
/// [`OpError`] naming `asset` when the bytes are not an image this build reads, or the image is larger than the bounds.
pub fn rgba(bytes: &[u8], asset: &str) -> Result<RgbaImage, OpError> {
    rgba_within(bytes, asset, MAX_DECODE_BYTES)
}

/// [`rgba`] with the memory bound as a parameter, so a test can reach it with a small image.
fn rgba_within(bytes: &[u8], asset: &str, max_bytes: u64) -> Result<RgbaImage, OpError> {
    let not_an_image = |e: &dyn std::fmt::Display| {
        OpError::invalid("asset", format!("asset {asset} is not an image this build reads: {e}"))
    };
    let mut limits = Limits::default();
    limits.max_alloc = Some(max_bytes);
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| not_an_image(&e))?;
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|e| match e {
        ImageError::Limits(_) => too_large(asset, None, max_bytes),
        other => not_an_image(&other),
    })?;
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
    DynamicImage::from_decoder(decoder).map(DynamicImage::into_rgba8).map_err(|e| match e {
        ImageError::Limits(_) => too_large(asset, Some((width, height)), max_bytes),
        other => not_an_image(&other),
    })
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
        let image = rgba(&png(4, 3), "abc").unwrap();
        assert_eq!(image.dimensions(), (4, 3));
        assert_eq!(image.get_pixel(0, 0).0, [10, 20, 30, 255]);
    }

    #[test]
    fn a_header_over_the_side_bound_is_refused_before_decoding() {
        // One pixel high, so the file is small whatever the header says; the width alone is over the bound.
        let err = rgba(&png(MAX_SIDE + 1, 1), "abc").unwrap_err();
        assert_eq!(err.field.as_deref(), Some("asset"));
        assert!(err.message.contains("too large") && err.message.contains(&format!("{}x1", MAX_SIDE + 1)), "{err:?}");
        // The bound itself is allowed.
        assert_eq!(rgba(&png(MAX_SIDE, 1), "abc").unwrap().dimensions(), (MAX_SIDE, 1));
    }

    #[test]
    fn a_sixteen_bit_image_is_counted_at_its_decoded_size() {
        use image::{ImageBuffer, Rgba};
        let wide = ImageBuffer::<Rgba<u16>, Vec<u16>>::from_pixel(8, 8, Rgba([1, 2, 3, 65535]));
        let mut sixteen = Vec::new();
        wide.write_to(&mut Cursor::new(&mut sixteen), ImageFormat::Png).unwrap();
        let eight = png(8, 8);
        // 8 x 8: the 8-bit decode is 256 bytes plus the 256-byte RGBA8 copy, the 16-bit decode 512 plus 256.
        assert_eq!(rgba_within(&eight, "abc", 600).unwrap().dimensions(), (8, 8));
        let err = rgba_within(&sixteen, "abc", 600).unwrap_err();
        assert!(err.message.contains("too large") && err.message.contains("600 bytes"), "{err:?}");
        assert_eq!(rgba_within(&sixteen, "abc", 800).unwrap().dimensions(), (8, 8));
    }

    #[test]
    fn bytes_that_are_not_an_image_name_the_asset() {
        let err = rgba(b"not an image", "abc").unwrap_err();
        assert_eq!(err.field.as_deref(), Some("asset"));
        assert!(err.message.contains("not an image this build reads"), "{err:?}");
    }
}
