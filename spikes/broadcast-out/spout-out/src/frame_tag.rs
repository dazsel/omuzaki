//! A machine-readable tag drawn into every test frame, so the receiving process can tell which frame it got, when the
//! sender started it, and whether the frame arrived whole.
//!
//! Layout, in 16-pixel cells from the top-left corner of a BGRA frame:
//! - row 0: the sender's `QueryPerformanceCounter` ticks, 64 cells, most significant bit first
//! - row 1: the frame number in cells 0..32, then its bitwise complement in cells 32..64
//! - last row of the image: a copy of row 1 (a frame torn between two renders disagrees with itself)
//!
//! The picture rows also carry a stamp column (x in 1024..1040): every row holds the frame number's low byte as a grey
//! level, so where two frames meet inside one image (a tear line) can be located to the row.
//!
//! A cell is opaque white for a 1 and opaque black for a 0, so reading it does not depend on how alpha is handled.

/// Cell edge in pixels. Big enough that scaling or a stray bad pixel cannot flip a bit.
pub const CELL: u32 = 16;
/// Cells in a tag row.
pub const CELLS: u32 = 64;
/// The narrowest frame that holds a tag row.
pub const MIN_WIDTH: u32 = CELL * CELLS;
/// Rows above the picture area: the ticks row and the frame row.
pub const HEADER_HEIGHT: u32 = 2 * CELL;
/// Rows below the picture area: the footer copy of the frame row.
pub const FOOTER_HEIGHT: u32 = CELL;
/// The smallest frame height that fits a header, a footer and some picture between them.
pub const MIN_HEIGHT: u32 = 16 * CELL;

/// First pixel column of the stamp column (right after the tag rows, which end at `MIN_WIDTH`).
pub const STAMP_X: u32 = MIN_WIDTH;
/// Width of the stamp column in pixels.
pub const STAMP_WIDTH: u32 = 16;

/// What the sender wrote into a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameTag {
    /// Frame counter, from 0.
    pub frame: u32,
    /// `QueryPerformanceCounter` ticks at the start of the frame.
    pub ticks: u64,
}

/// A tag read back from pixels, with the integrity checks that passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedTag {
    /// The values read from the header rows.
    pub tag: FrameTag,
    /// The complement cells matched the frame number cells.
    pub header_ok: bool,
    /// The footer row carried the same frame number as the header.
    pub footer_ok: bool,
}

/// Why a frame could not be read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TagError {
    /// The pixel buffer is not `width * height * 4` bytes, or the frame is smaller than a tag.
    #[error("frame {width}x{height} with {len} bytes cannot hold a tag")]
    BadShape {
        /// Frame width.
        width: u32,
        /// Frame height.
        height: u32,
        /// Buffer length.
        len: usize,
    },
}

/// The cells of the ticks row, most significant bit first. The shader draws the same bits.
#[must_use]
pub fn ticks_cells(ticks: u64) -> [bool; CELLS as usize] {
    std::array::from_fn(|cell| (ticks >> (63 - cell)) & 1 == 1)
}

/// The cells of the frame row: the frame number then its complement.
#[must_use]
pub fn frame_cells(frame: u32) -> [bool; CELLS as usize] {
    std::array::from_fn(
        |cell| {
            if cell < 32 { (frame >> (31 - cell)) & 1 == 1 } else { (frame >> (63 - cell)) & 1 == 0 }
        },
    )
}

/// Draws `tag` into a BGRA frame the way the shader does. Used by tests and by the CPU fallback path.
///
/// # Errors
/// Fails when the buffer does not match the stated size or the frame is too small.
pub fn encode(tag: FrameTag, bgra: &mut [u8], width: u32, height: u32) -> Result<(), TagError> {
    check_shape(bgra.len(), width, height)?;
    let mut paint = |row_y: u32, cells: &[bool; CELLS as usize]| {
        for (cell, &on) in (0u32..).zip(cells) {
            let value = if on { 255 } else { 0 };
            for y in row_y..row_y + CELL {
                for x in cell * CELL..(cell + 1) * CELL {
                    let at = pixel_offset(x, y, width);
                    bgra[at..at + 4].copy_from_slice(&[value, value, value, 255]);
                }
            }
        }
    };
    paint(0, &ticks_cells(tag.ticks));
    paint(CELL, &frame_cells(tag.frame));
    paint(height - FOOTER_HEIGHT, &frame_cells(tag.frame));
    Ok(())
}

/// Reads the tag back out of a BGRA frame.
///
/// # Errors
/// Fails when the buffer does not match the stated size or the frame is too small to carry a tag.
pub fn decode(bgra: &[u8], width: u32, height: u32) -> Result<DecodedTag, TagError> {
    check_shape(bgra.len(), width, height)?;
    let read_row = |row_y: u32| -> [bool; CELLS as usize] {
        std::array::from_fn(|cell| {
            let (x, y) = (u32::try_from(cell).unwrap_or(0) * CELL + CELL / 2, row_y + CELL / 2);
            // Green is the middle channel in both BGRA and RGBA, so the channel order cannot matter.
            bgra[pixel_offset(x, y, width) + 1] >= 128
        })
    };
    let to_u64 = |cells: &[bool]| cells.iter().fold(0u64, |acc, &on| (acc << 1) | u64::from(on));
    let ticks = to_u64(&read_row(0));
    let header = read_row(CELL);
    let footer = read_row(height - FOOTER_HEIGHT);
    let frame = u32::try_from(to_u64(&header[..32])).unwrap_or(0);
    let complement = u32::try_from(to_u64(&header[32..])).unwrap_or(0);
    let footer_frame = u32::try_from(to_u64(&footer[..32])).unwrap_or(0);
    Ok(DecodedTag {
        tag: FrameTag { frame, ticks },
        header_ok: frame == !complement,
        footer_ok: footer_frame == frame && to_u64(&footer[32..]) == to_u64(&header[32..]),
    })
}

/// Where in the picture area two different frames meet, if they do: the first row whose stamp differs from the stamp of
/// the first picture row. `None` for a coherent frame, or one too narrow to carry the stamp column.
#[must_use]
pub fn tear_row(bgra: &[u8], width: u32, height: u32) -> Option<u32> {
    if check_shape(bgra.len(), width, height).is_err() || width < STAMP_X + STAMP_WIDTH {
        return None;
    }
    let stamp = |row: u32| bgra[pixel_offset(STAMP_X + STAMP_WIDTH / 2, row, width) + 1];
    let first = stamp(HEADER_HEIGHT);
    (HEADER_HEIGHT..height - FOOTER_HEIGHT).find(|&row| stamp(row) != first)
}

fn check_shape(len: usize, width: u32, height: u32) -> Result<(), TagError> {
    let expected = (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4));
    if width < MIN_WIDTH || height < MIN_HEIGHT || expected != Some(len) {
        return Err(TagError::BadShape { width, height, len });
    }
    Ok(())
}

fn pixel_offset(x: u32, y: u32, width: u32) -> usize {
    (y as usize * width as usize + x as usize) * 4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(width: u32, height: u32) -> Vec<u8> {
        vec![0; (width * height * 4) as usize]
    }

    #[test]
    fn round_trips_a_tag() {
        let tag = FrameTag { frame: 0xDEAD_BEEF, ticks: 0x0123_4567_89AB_CDEF };
        let mut frame = blank(1280, 720);
        encode(tag, &mut frame, 1280, 720).unwrap();
        let read = decode(&frame, 1280, 720).unwrap();
        assert_eq!(read.tag, tag);
        assert!(read.header_ok && read.footer_ok);
    }

    #[test]
    fn extremes_round_trip() {
        for tag in [FrameTag { frame: 0, ticks: 0 }, FrameTag { frame: u32::MAX, ticks: u64::MAX }] {
            let mut frame = blank(1024, 256);
            encode(tag, &mut frame, 1024, 256).unwrap();
            assert_eq!(decode(&frame, 1024, 256).unwrap().tag, tag);
        }
    }

    #[test]
    fn a_frame_torn_between_renders_is_flagged() {
        let mut frame = blank(1280, 720);
        encode(FrameTag { frame: 41, ticks: 1 }, &mut frame, 1280, 720).unwrap();
        // The footer comes from a later render than the header.
        let mut later = blank(1280, 720);
        encode(FrameTag { frame: 42, ticks: 2 }, &mut later, 1280, 720).unwrap();
        let footer_start = pixel_offset(0, 720 - FOOTER_HEIGHT, 1280);
        frame[footer_start..].copy_from_slice(&later[footer_start..]);
        let read = decode(&frame, 1280, 720).unwrap();
        assert!(read.header_ok);
        assert!(!read.footer_ok);
    }

    #[test]
    fn a_corrupt_header_is_flagged() {
        let mut frame = blank(1280, 720);
        encode(FrameTag { frame: 7, ticks: 9 }, &mut frame, 1280, 720).unwrap();
        // Flip the lowest frame-number cell (cell 31) to black.
        for y in CELL..2 * CELL {
            for x in 31 * CELL..32 * CELL {
                let at = pixel_offset(x, y, 1280);
                frame[at..at + 3].fill(0);
            }
        }
        assert!(!decode(&frame, 1280, 720).unwrap().header_ok);
    }

    #[test]
    fn colour_channel_order_does_not_matter() {
        let tag = FrameTag { frame: 5, ticks: 77 };
        let mut frame = blank(1280, 720);
        encode(tag, &mut frame, 1280, 720).unwrap();
        for px in frame.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        assert_eq!(decode(&frame, 1280, 720).unwrap().tag, tag);
    }

    #[test]
    fn a_tear_line_is_located_to_the_row() {
        let (w, h) = (1280, 720);
        let mut frame = blank(w, h);
        let stamp_pixel = |frame: &mut Vec<u8>, row: u32, value: u8| {
            for x in STAMP_X..STAMP_X + STAMP_WIDTH {
                let at = pixel_offset(x, row, w);
                frame[at..at + 4].copy_from_slice(&[value, value, value, 255]);
            }
        };
        for row in HEADER_HEIGHT..h - FOOTER_HEIGHT {
            stamp_pixel(&mut frame, row, if row < 400 { 17 } else { 18 });
        }
        assert_eq!(tear_row(&frame, w, h), Some(400));
        for row in HEADER_HEIGHT..h - FOOTER_HEIGHT {
            stamp_pixel(&mut frame, row, 17);
        }
        assert_eq!(tear_row(&frame, w, h), None);
        assert_eq!(tear_row(&frame[..16], 2, 2), None);
    }

    #[test]
    fn rejects_bad_shapes() {
        assert!(matches!(decode(&[0; 16], 2, 2), Err(TagError::BadShape { .. })));
        assert!(matches!(decode(&blank(1280, 720), 1280, 719), Err(TagError::BadShape { .. })));
    }
}
