//! A synthetic test picture and the RGB to YUV conversions the camera needs.
//!
//! The picture is eight colour bars (the 75% bars of the SMPTE pattern), a grey ramp, a square that moves one step per
//! frame and a row of cells carrying the frame number, so a consumer can check both colour and ordering.
//! Conversion uses BT.709 coefficients and limited (16 to 235) range, matching what the media types declare.

use crate::format::{HEIGHT, PixelFormat, WIDTH};

/// The eight bars, left to right, as straight RGB: white, yellow, cyan, green, magenta, red, blue, black at 75%.
pub const BARS: [[u8; 3]; 8] =
    [[191, 191, 191], [191, 191, 0], [0, 191, 191], [0, 191, 0], [191, 0, 191], [191, 0, 0], [0, 0, 191], [0, 0, 0]];

/// Height of the bar area as a fraction of the frame: the top two thirds.
const BAR_ROWS_DIVISOR: u32 = 3;
/// Edge of one frame-number cell in pixels.
const COUNTER_CELL: u32 = 16;
/// Cells in the frame-number row, most significant bit first.
const COUNTER_CELLS: u32 = 16;

/// Draws frame `index` as BGRA (`width * height * 4` bytes) into `bgra`.
pub fn render_bgra(index: u64, width: u32, height: u32, bgra: &mut [u8]) {
    let bar_height = height - height / BAR_ROWS_DIVISOR;
    let square = u32::try_from(index % u64::from(width - 64)).unwrap_or(0);
    for y in 0..height {
        for x in 0..width {
            let rgb = if y < bar_height {
                BARS[(x * 8 / width) as usize]
            } else if y < bar_height + (height - bar_height) / 2 {
                let grey = u8::try_from(x * 255 / (width - 1)).unwrap_or(255);
                [grey, grey, grey]
            } else {
                [32, 32, 32]
            };
            let inside_square = x >= square && x < square + 64 && y >= height - 112 && y < height - 48;
            let in_counter = y >= height - COUNTER_CELL && x < COUNTER_CELL * COUNTER_CELLS;
            let rgb = if in_counter {
                let bit = (index >> (COUNTER_CELLS - 1 - x / COUNTER_CELL)) & 1;
                if bit == 1 { [255, 255, 255] } else { [0, 0, 0] }
            } else if inside_square {
                [255, 128, 0]
            } else {
                rgb
            };
            let at = (y as usize * width as usize + x as usize) * 4;
            bgra[at..at + 4].copy_from_slice(&[rgb[2], rgb[1], rgb[0], 255]);
        }
    }
}

/// Reads the frame number back from the counter row of a decoded RGB-ish frame, given a luma sampler.
/// `luma(x, y)` returns 0..=255 brightness; a cell is a 1 when it is brighter than mid-grey.
pub fn read_counter(width: u32, height: u32, luma: impl Fn(u32, u32) -> u8) -> u64 {
    debug_assert!(width >= COUNTER_CELL * COUNTER_CELLS);
    (0..COUNTER_CELLS).fold(0u64, |acc, cell| {
        let on = luma(cell * COUNTER_CELL + COUNTER_CELL / 2, height - COUNTER_CELL / 2) > 128;
        (acc << 1) | u64::from(on)
    })
}

/// BT.709, limited range: RGB (0..=255) to Y, Cb, Cr (as 8-bit codes).
#[must_use]
pub fn rgb_to_ycbcr(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
    let y = 16.0 + 219.0 / 255.0 * (0.2126 * r + 0.7152 * g + 0.0722 * b);
    let cb = 128.0 + 224.0 / 255.0 * (-0.1146 * r - 0.3854 * g + 0.5 * b);
    let cr = 128.0 + 224.0 / 255.0 * (0.5 * r - 0.4542 * g - 0.0458 * b);
    (code(y), code(cb), code(cr))
}

fn code(value: f32) -> u8 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to the byte range first
    let byte = value.round().clamp(0.0, 255.0) as u8;
    byte
}

/// Converts a BGRA frame to `format` into `out` (which must be `format.frame_bytes(width, height)` long).
/// Chroma is the average of each 2x2 (NV12) or 2x1 (YUY2) group.
pub fn convert(bgra: &[u8], width: u32, height: u32, format: PixelFormat, out: &mut [u8]) {
    let (w, h) = (width as usize, height as usize);
    let px = |x: usize, y: usize| -> (u8, u8, u8) {
        let at = (y * w + x) * 4;
        rgb_to_ycbcr(bgra[at + 2], bgra[at + 1], bgra[at])
    };
    match format {
        PixelFormat::Nv12 => {
            let (luma, chroma) = out.split_at_mut(w * h);
            for y in 0..h {
                for x in 0..w {
                    luma[y * w + x] = px(x, y).0;
                }
            }
            for cy in 0..h / 2 {
                for cx in 0..w / 2 {
                    let block = [
                        px(2 * cx, 2 * cy),
                        px(2 * cx + 1, 2 * cy),
                        px(2 * cx, 2 * cy + 1),
                        px(2 * cx + 1, 2 * cy + 1),
                    ];
                    let average = |pick: fn(&(u8, u8, u8)) -> u8| {
                        let sum: u32 = block.iter().map(|p| u32::from(pick(p))).sum();
                        u8::try_from((sum + 2) / 4).unwrap_or(255)
                    };
                    let at = cy * w + cx * 2;
                    chroma[at] = average(|p| p.1);
                    chroma[at + 1] = average(|p| p.2);
                }
            }
        }
        PixelFormat::Yuy2 => {
            for y in 0..h {
                for cx in 0..w / 2 {
                    let (left, right) = (px(2 * cx, y), px(2 * cx + 1, y));
                    let at = (y * w + 2 * cx) * 2;
                    let mean = |a: u8, b: u8| u8::try_from((u16::from(a) + u16::from(b)).div_ceil(2)).unwrap_or(255);
                    out[at] = left.0;
                    out[at + 1] = mean(left.1, right.1);
                    out[at + 2] = right.0;
                    out[at + 3] = mean(left.2, right.2);
                }
            }
        }
    }
}

/// Renders frame `index` straight into `format`, using `scratch` (a BGRA buffer of the camera's size) as the work area.
pub fn render_frame(index: u64, format: PixelFormat, scratch: &mut Vec<u8>, out: &mut [u8]) {
    scratch.resize(WIDTH as usize * HEIGHT as usize * 4, 0);
    render_bgra(index, WIDTH, HEIGHT, scratch);
    convert(scratch, WIDTH, HEIGHT, format, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_black_and_red_have_the_bt709_limited_range_codes() {
        assert_eq!(rgb_to_ycbcr(255, 255, 255), (235, 128, 128));
        assert_eq!(rgb_to_ycbcr(0, 0, 0), (16, 128, 128));
        let (y, cb, cr) = rgb_to_ycbcr(255, 0, 0);
        assert_eq!((y, cb, cr), (63, 102, 240));
    }

    #[test]
    fn the_counter_row_reads_back_the_frame_number() {
        let (w, h) = (WIDTH, HEIGHT);
        let mut bgra = vec![0; (w * h * 4) as usize];
        for index in [0u64, 1, 29, 1234, 65_535] {
            render_bgra(index, w, h, &mut bgra);
            let luma = |x: u32, y: u32| bgra[((y * w + x) * 4 + 1) as usize];
            assert_eq!(read_counter(w, h, luma), index & 0xFFFF);
        }
    }

    #[test]
    fn nv12_and_yuy2_have_the_right_size_and_a_white_bar_in_the_first_column() {
        let mut bgra = vec![0; (WIDTH * HEIGHT * 4) as usize];
        render_bgra(7, WIDTH, HEIGHT, &mut bgra);
        for format in [PixelFormat::Nv12, PixelFormat::Yuy2] {
            let mut out = vec![0; format.frame_bytes(WIDTH, HEIGHT)];
            convert(&bgra, WIDTH, HEIGHT, format, &mut out);
            // 75% white is (191,191,191): Y = 16 + 219/255 * 191 = 180, neutral chroma.
            assert_eq!(out[0], 180, "{format:?}");
        }
    }

    #[test]
    fn nv12_chroma_is_neutral_for_grey_and_saturated_for_a_bar() {
        let mut bgra = vec![0; (WIDTH * HEIGHT * 4) as usize];
        render_bgra(0, WIDTH, HEIGHT, &mut bgra);
        let mut out = vec![0; PixelFormat::Nv12.frame_bytes(WIDTH, HEIGHT)];
        convert(&bgra, WIDTH, HEIGHT, PixelFormat::Nv12, &mut out);
        let chroma = &out[(WIDTH * HEIGHT) as usize..];
        assert_eq!((chroma[0], chroma[1]), (128, 128)); // white bar
        // Sixth bar is red: columns 5/8 of the width.
        let at = (WIDTH as usize * 5 / 8 + 8) & !1;
        assert_eq!(
            (chroma[at], chroma[at + 1]),
            (code(128.0 + 224.0 / 255.0 * -0.1146 * 191.0), code(128.0 + 224.0 / 255.0 * 0.5 * 191.0))
        );
    }
}
