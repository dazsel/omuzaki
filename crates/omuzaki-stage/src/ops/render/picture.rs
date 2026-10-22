//! The pictures the `render.*` ops read back and lay out: a stored PNG named by its reference, and straight-alpha canvases
//! that cells are copied into.

use std::io::Cursor;

use image::ImageReader;
use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::{JobCtx, OpError};
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_store_api::{ContentHash, Store};

use super::job::{MAX_SIDE, StoredRender, store_png};
use super::still::encode_png;

/// What a render op's URI starts with; the hash and `.png` follow.
pub const RENDER_URI: &str = "omuzaki://render/";
/// The widest or tallest picture a render op lays out or writes: room for a sheet of 4096-pixel renders four abreast.
pub const MAX_CANVAS_SIDE: u32 = 4 * MAX_SIDE;
/// The most pixels a picture a render op writes may have (256 MiB of RGBA).
pub const MAX_CANVAS_PIXELS: u64 = 64 << 20;

/// The content hash `reference` names: a bare hash, or a render URI (`omuzaki://render/<hash>.png`).
///
/// # Errors
/// `Invalid` for `field` when it is neither.
pub fn hash_of(field: &str, reference: &str) -> Result<ContentHash, OpError> {
    let hex = reference.strip_prefix(RENDER_URI).and_then(|rest| rest.strip_suffix(".png")).unwrap_or(reference);
    hex.parse().map_err(|_| {
        OpError::invalid(field, format!("'{reference}' is not a render: give a content hash or an {RENDER_URI}<hash>.png URI"))
            .hint("use the uri a render.* op returned")
    })
}

/// The stored PNG `reference` names, decoded as straight-alpha RGBA. Its header is checked against [`MAX_SIDE`] before
/// any pixels are decoded.
///
/// # Errors
/// `Invalid` for a reference that isn't a hash or render URI, `NotFound` for one the store doesn't hold, `Invalid` for
/// stored bytes that aren't a PNG or are larger than a render can be; `Unavailable` without a store.
pub fn load(job: &JobCtx<'_>, field: &str, reference: &str) -> Result<Image, OpError> {
    let hash = hash_of(field, reference)?;
    let store = job.service::<dyn Store>()?;
    let bytes = store.get(&hash).map_err(|e| {
        OpError::not_found(format!("there is no stored picture '{reference}': {e}")).hint("render it again")
    })?;
    decode(field, reference, &bytes)
}

/// `bytes` as a straight-alpha image, if they are a PNG no larger than a render.
fn decode(field: &str, reference: &str, bytes: &[u8]) -> Result<Image, OpError> {
    let not_png = |why: String| OpError::invalid(field, format!("'{reference}' is not a PNG a render made: {why}"));
    let reader = ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let (width, height) = reader.into_dimensions().map_err(|e| not_png(e.to_string()))?;
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(not_png(format!("it is {width}x{height}, and a render is at most {MAX_SIDE} a side")));
    }
    let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| not_png(e.to_string()))?
        .to_rgba8();
    straight(UVec2::new(width, height), decoded.into_raw())
}

/// A transparent canvas of `size`.
///
/// # Errors
/// `Invalid` for `field` when either side passes [`MAX_CANVAS_SIDE`] or the area passes [`MAX_CANVAS_PIXELS`].
pub fn canvas(field: &str, size: UVec2) -> Result<Image, OpError> {
    let pixels = u64::from(size.x) * u64::from(size.y);
    if size.x > MAX_CANVAS_SIDE || size.y > MAX_CANVAS_SIDE || pixels > MAX_CANVAS_PIXELS {
        return Err(OpError::invalid(
            field,
            format!(
                "the picture would be {}x{} pixels; at most {MAX_CANVAS_SIDE} a side and {MAX_CANVAS_PIXELS} pixels",
                size.x, size.y
            ),
        )
        .hint("use fewer or smaller pictures"));
    }
    let bytes = usize::try_from(pixels * 4).map_err(|_| OpError::invalid(field, "the picture is too large to hold"))?;
    straight(size, vec![0; bytes])
}

/// A straight-alpha image of `size` over `rgba8`.
///
/// # Errors
/// `Internal` when `rgba8` doesn't hold `size` (a bug in the caller).
pub fn straight(size: UVec2, rgba8: Vec<u8>) -> Result<Image, OpError> {
    Image::new(size, AlphaEncoding::Straight, rgba8).map_err(|e| OpError::internal(format!("a picture: {e}")))
}

/// Copies `cell` into `canvas` with its top-left corner at `at`, clipped to the canvas.
pub fn paste(canvas: &mut Image, cell: &Image, at: UVec2) {
    let (cw, ch) = (canvas.size.x as usize, canvas.size.y as usize);
    let (w, h) = (cell.size.x as usize, cell.size.y as usize);
    let (x0, y0) = (at.x as usize, at.y as usize);
    if x0 >= cw || y0 >= ch {
        return;
    }
    let row = (w.min(cw - x0)) * 4;
    for y in 0..h.min(ch - y0) {
        let from = y * w * 4;
        let to = ((y0 + y) * cw + x0) * 4;
        canvas.rgba8[to..to + row].copy_from_slice(&cell.rgba8[from..from + row]);
    }
}

/// `image` encoded as a PNG and put in the store.
///
/// # Errors
/// `Internal` when it doesn't encode; what [`store_png`] reports.
pub fn store(job: &JobCtx<'_>, image: &Image) -> Result<StoredRender, OpError> {
    let png = encode_png(image).map_err(|e| OpError::internal(e.to_string()))?;
    store_png(job, &png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_is_a_hash_or_a_render_uri() {
        let hash = ContentHash::of(b"x");
        assert_eq!(hash_of("a", &hash.to_string()).unwrap(), hash);
        assert_eq!(hash_of("a", &format!("{RENDER_URI}{hash}.png")).unwrap(), hash);
        for bad in ["", "omuzaki://render/zz.png", "file:///etc/passwd", "omuzaki://asset/abc"] {
            assert!(hash_of("a", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn paste_clips_at_the_canvas_edge() {
        let mut canvas = canvas("c", UVec2::new(3, 2)).unwrap();
        let cell = straight(UVec2::new(2, 2), vec![9; 16]).unwrap();
        paste(&mut canvas, &cell, UVec2::new(2, 1));
        let lit: Vec<usize> = canvas.rgba8.chunks(4).enumerate().filter(|(_, p)| p[0] == 9).map(|(i, _)| i).collect();
        assert_eq!(lit, [5], "only the pixel at (2, 1) is inside");
        paste(&mut canvas, &cell, UVec2::new(5, 5));
    }

    #[test]
    fn a_huge_canvas_is_refused_before_it_is_allocated() {
        assert!(canvas("c", UVec2::new(MAX_CANVAS_SIDE + 1, 1)).is_err());
        assert!(canvas("c", UVec2::new(MAX_CANVAS_SIDE, MAX_CANVAS_SIDE)).is_err());
    }

    #[test]
    fn stored_bytes_that_are_not_a_png_are_refused() {
        assert!(decode("a", "x", b"GIF89a").is_err());
        let mut png = Vec::new();
        image::RgbaImage::new(2, 1).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let image = decode("a", "x", &png).unwrap();
        assert_eq!((image.size, image.rgba8.len()), (UVec2::new(2, 1), 8));
    }
}
