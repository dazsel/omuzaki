//! Camera pixel formats to packed RGB8 (spec v3 §22 PF-01, §10).
//!
//! Webcams deliver NV12, YUY2 or MJPG, and some drivers deliver BGRA. The tracker wants packed RGB8, and the camera
//! capture, the tracker and the UI preview all share this one converter. Everything here is safe Rust over
//! `chunks_exact` row loops (the compiler vectorises them), writes into a caller-owned buffer, and allocates nothing
//! per frame (MJPG excepted, see [`MjpgDecoder`]). Every length is checked before the first write, and a bad input
//! is a [`ConvertError`], never a panic.
//!
//! A frame of `width` x `height` pixels is valid when both are non-zero. NV12 and YUY2 also need an even width and
//! NV12 an even height, because chroma is shared by 2x2 blocks (NV12) or horizontal pairs (YUY2).

mod bgra;
mod mjpg;
mod yuv;

pub use bgra::{bgra_to_rgb, rgb_to_rgb};
pub use mjpg::MjpgDecoder;
pub use yuv::{nv12_to_rgb, yuy2_to_rgb};

/// The YUV to RGB matrix: which luma weights the sender used.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum YuvMatrix {
    /// ITU-R BT.601 (Kr 0.299, Kb 0.114): standard definition, and what UVC webcams send.
    Bt601,
    /// ITU-R BT.709 (Kr 0.2126, Kb 0.0722): high definition.
    Bt709,
}

/// The sample range of the YUV data.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum YuvRange {
    /// Studio swing: Y in 16..=235, U and V in 16..=240.
    Limited,
    /// Full swing: all of 0..=255.
    Full,
}

/// How a YUV frame is to be read. The default is BT.601 and limited range, which is what UVC webcams send.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Yuv {
    /// The luma weights.
    pub matrix: YuvMatrix,
    /// The sample range.
    pub range: YuvRange,
}

impl Default for Yuv {
    fn default() -> Self {
        Self { matrix: YuvMatrix::Bt601, range: YuvRange::Limited }
    }
}

/// A plane of source bytes: `data` holds rows `stride` bytes apart. The last row need not be padded to `stride`.
#[derive(Copy, Clone, Debug)]
pub struct Plane<'a> {
    /// The plane's bytes, starting at its first row.
    pub data: &'a [u8],
    /// Bytes from the start of one row to the start of the next.
    pub stride: usize,
}

/// A packed RGB8 destination: `width * height` pixels of three bytes, rows tightly packed (stride `3 * width`).
/// Only the first `3 * width * height` bytes of `data` are written.
pub struct RgbOut<'a> {
    /// The destination bytes.
    pub data: &'a mut [u8],
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Why a conversion was refused. Nothing is written to the output when one is returned (MJPG excepted: a decode that
/// fails part-way may leave partial pixels).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConvertError {
    /// The width or height is zero, or odd where the format needs an even one.
    #[error("unsupported frame size {width}x{height}")]
    BadSize {
        /// The width given.
        width: u32,
        /// The height given.
        height: u32,
    },
    /// A source plane is too short: a row is wider than its stride, or the data ends before the last row.
    #[error("{plane} plane too short: need {need} bytes, got {got}")]
    ShortPlane {
        /// Which plane (`"y"`, `"uv"` or `"src"`).
        plane: &'static str,
        /// Bytes needed (the row width when the stride is smaller than a row).
        need: usize,
        /// Bytes given (the stride when it is smaller than a row).
        got: usize,
    },
    /// The output buffer holds fewer than `3 * width * height` bytes.
    #[error("output too short: need {need} bytes, got {got}")]
    ShortOutput {
        /// Bytes needed.
        need: usize,
        /// Bytes given.
        got: usize,
    },
    /// The JPEG decoder rejected the data (truncated, corrupt or unsupported).
    #[error("jpeg: {0}")]
    Jpeg(String),
    /// The JPEG decodes, but not to the size the output was made for.
    #[error("jpeg is {}x{}, output expects {}x{}", got.0, got.1, expected.0, expected.1)]
    JpegSize {
        /// The output's (width, height).
        expected: (u32, u32),
        /// The JPEG's (width, height).
        got: (u32, u32),
    },
}

/// Checks the output buffer against its declared size and returns the `(width, height)` in pixels and the number of
/// bytes in one output row. `even_w` and `even_h` say which dimensions the format needs even.
pub(crate) fn check_out(out: &RgbOut<'_>, even_w: bool, even_h: bool) -> Result<(usize, usize, usize), ConvertError> {
    let (width, height) = (out.width, out.height);
    let bad = width == 0 || height == 0 || (even_w && width % 2 != 0) || (even_h && height % 2 != 0);
    if bad {
        return Err(ConvertError::BadSize { width, height });
    }
    let (w, h) = (width as usize, height as usize);
    let need = w.checked_mul(3).and_then(|row| row.checked_mul(h)).unwrap_or(usize::MAX);
    if out.data.len() < need {
        return Err(ConvertError::ShortOutput { need, got: out.data.len() });
    }
    Ok((w, h, w * 3))
}

/// Checks that `plane` holds `rows` rows of `row_bytes` bytes at its stride.
pub(crate) fn check_plane(
    name: &'static str,
    plane: Plane<'_>,
    row_bytes: usize,
    rows: usize,
) -> Result<(), ConvertError> {
    if plane.stride < row_bytes {
        return Err(ConvertError::ShortPlane { plane: name, need: row_bytes, got: plane.stride });
    }
    let need = plane.stride.checked_mul(rows - 1).and_then(|n| n.checked_add(row_bytes)).unwrap_or(usize::MAX);
    if plane.data.len() < need {
        return Err(ConvertError::ShortPlane { plane: name, need, got: plane.data.len() });
    }
    Ok(())
}

/// The first `rows` rows of a checked plane, each trimmed to `row_bytes` (drops the stride padding).
pub(crate) fn rows(
    plane: Plane<'_>,
    row_bytes: usize,
    rows: usize,
) -> impl DoubleEndedIterator<Item = &[u8]> + ExactSizeIterator {
    plane.data.chunks(plane.stride).take(rows).map(move |row| &row[..row_bytes.min(row.len())])
}
