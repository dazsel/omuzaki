//! BGRA and strided RGB to packed RGB8.

use crate::{ConvertError, Plane, RgbOut, check_out, check_plane, rows};

/// Drops alpha and swaps B and R for every pixel of the rows `src` yields, top row first.
fn bgra_rows<'a>(src: impl Iterator<Item = &'a [u8]>, out: &mut [u8], out_row: usize) {
    for (row, out) in src.zip(out.chunks_exact_mut(out_row)) {
        for (px, out) in row.as_chunks::<4>().0.iter().zip(out.as_chunks_mut::<3>().0.iter_mut()) {
            out.copy_from_slice(&[px[2], px[1], px[0]]);
        }
    }
}

/// Converts BGRA (bytes `B G R A`, as Media Foundation and GDI deliver it) to RGB8, dropping alpha.
///
/// With `bottom_up` the first row in `src` is the bottom of the image (a DIB with a positive height), and the rows
/// are flipped so the output is top-down.
///
/// # Errors
/// [`ConvertError::BadSize`] for a zero width or height, [`ConvertError::ShortPlane`] or
/// [`ConvertError::ShortOutput`] for a buffer or stride that cannot hold the frame.
pub fn bgra_to_rgb(src: Plane<'_>, bottom_up: bool, out: &mut RgbOut<'_>) -> Result<(), ConvertError> {
    let (w, h, out_row) = check_out(out, false, false)?;
    check_plane("src", src, w * 4, h)?;
    let src_rows = rows(src, w * 4, h);
    if bottom_up {
        bgra_rows(src_rows.rev(), out.data, out_row);
    } else {
        bgra_rows(src_rows, out.data, out_row);
    }
    Ok(())
}

/// Copies a strided RGB8 frame (rows `stride` bytes apart, at least `3 * width`) into the packed output.
///
/// # Errors
/// [`ConvertError::BadSize`] for a zero width or height, [`ConvertError::ShortPlane`] or
/// [`ConvertError::ShortOutput`] for a buffer or stride that cannot hold the frame.
pub fn rgb_to_rgb(src: Plane<'_>, out: &mut RgbOut<'_>) -> Result<(), ConvertError> {
    let (w, h, out_row) = check_out(out, false, false)?;
    check_plane("src", src, w * 3, h)?;
    for (row, out) in rows(src, w * 3, h).zip(out.data.chunks_exact_mut(out_row)) {
        out.copy_from_slice(row);
    }
    Ok(())
}
