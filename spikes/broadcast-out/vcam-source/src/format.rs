//! What the camera offers: one stream, 1280x720 at 30 frames per second, as NV12 or YUY2.
//!
//! NV12 is what most apps and the Windows camera pipeline prefer; YUY2 is what most USB webcams expose and what legacy
//! `DirectShow` apps assume exists (Microsoft's Frame Server guidance recommends offering it for those apps). Neither
//! carries alpha: a virtual camera is opaque, so the app must flatten transparency onto a background before it reaches
//! this stage.

/// Frame width in pixels.
pub const WIDTH: u32 = 1280;
/// Frame height in pixels.
pub const HEIGHT: u32 = 720;
/// Frames per second.
pub const FPS: u32 = 30;
/// One frame's duration in 100-nanosecond units, the Media Foundation time base.
pub const FRAME_DURATION_100NS: i64 = 10_000_000 / FPS as i64;

/// The pixel layouts the camera can produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// 4:2:0, a full-size Y plane then interleaved half-size U/V: 12 bits per pixel.
    Nv12,
    /// 4:2:2, packed Y0 U Y1 V: 16 bits per pixel.
    Yuy2,
}

impl PixelFormat {
    /// Bytes in one frame of `width` x `height` pixels (both even).
    #[must_use]
    pub const fn frame_bytes(self, width: u32, height: u32) -> usize {
        let pixels = width as usize * height as usize;
        match self {
            Self::Nv12 => pixels * 3 / 2,
            Self::Yuy2 => pixels * 2,
        }
    }

    /// Bytes per row of the first (or only) plane.
    #[must_use]
    pub const fn stride(self, width: u32) -> u32 {
        match self {
            Self::Nv12 => width,
            Self::Yuy2 => width * 2,
        }
    }
}

/// The COM class id the DLL serves and the virtual camera is created from (generated for this spike; a product gets its own).
pub const CLSID_TEXT: &str = "{1536BDA7-78F9-482B-BCDA-1C5321D5F4E8}";
/// [`CLSID_TEXT`] as a number, for `windows::core::GUID::from_u128`.
pub const CLSID_U128: u128 = 0x1536_BDA7_78F9_482B_BCDA_1C53_21D5_F4E8;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_sizes() {
        assert_eq!(PixelFormat::Nv12.frame_bytes(1280, 720), 1_382_400);
        assert_eq!(PixelFormat::Yuy2.frame_bytes(1280, 720), 1_843_200);
        assert_eq!(FRAME_DURATION_100NS, 333_333);
    }

    #[test]
    fn clsid_text_and_number_agree() {
        let hex: String = CLSID_TEXT.chars().filter(char::is_ascii_hexdigit).collect();
        assert_eq!(u128::from_str_radix(&hex, 16).unwrap(), CLSID_U128);
    }
}
