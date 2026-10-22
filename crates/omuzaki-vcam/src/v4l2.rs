//! The Linux virtual camera: frames written to a v4l2loopback device, which apps then open like a webcam.
//!
//! v4l2loopback is a kernel module the person installs (a separate GPL program, `docs/licensing/policy.md`); we only
//! open its device, set the format and write frames. The device is the one named by `OMUZAKI_VCAM_DEVICE`, else the
//! first `/dev/video*` whose driver reports itself as `v4l2 loopback`.

// The device is configured with two ioctls (`VIDIOC_QUERYCAP`, `VIDIOC_S_FMT`) on structs laid out exactly as the
// kernel's UAPI headers declare them; the layouts are checked by size below and in the tests.
#![allow(unsafe_code)]
#![allow(
    clippy::disallowed_methods,
    reason = "the virtual camera opens only `/dev/video*` device nodes, never a path an op passes in"
)]

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};

use omuzaki_stream_api::error::StreamError;

/// The environment variable that names the device to write to, overriding the search.
pub const DEVICE_ENV: &str = "OMUZAKI_VCAM_DEVICE";
/// The driver name v4l2loopback reports.
pub const LOOPBACK_DRIVER: &str = "v4l2 loopback";
/// What to tell a person whose machine has no loopback device.
pub const INSTALL_HINT: &str =
    "install v4l2loopback, then load it: sudo modprobe v4l2loopback exclusive_caps=1 card_label=\"Omuzaki Camera\"";

/// `struct v4l2_capability`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

/// `struct v4l2_pix_format`.
#[repr(C)]
#[derive(Clone, Copy)]
struct PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    private: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

/// The union in `struct v4l2_format`: 200 bytes, 8-aligned on 64-bit targets because some members hold pointers.
#[repr(C)]
#[derive(Clone, Copy)]
union FormatUnion {
    pix: PixFormat,
    raw: [u8; 200],
    align: [usize; 200 / std::mem::size_of::<usize>()],
}

/// `struct v4l2_format`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Format {
    kind: u32,
    fmt: FormatUnion,
}

const _: () = assert!(std::mem::size_of::<Capability>() == 104);
const _: () = assert!(std::mem::size_of::<PixFormat>() == 48);

/// `_IOC(dir, type, nr, size)` from `asm-generic/ioctl.h`.
const fn ioc(dir: u64, kind: u8, nr: u8, size: usize) -> u64 {
    (dir << 30) | ((size as u64) << 16) | ((kind as u64) << 8) | nr as u64
}
const IOC_READ: u64 = 2;
const IOC_READ_WRITE: u64 = 3;
const VIDIOC_QUERYCAP: u64 = ioc(IOC_READ, b'V', 0, std::mem::size_of::<Capability>());
const VIDIOC_S_FMT: u64 = ioc(IOC_READ_WRITE, b'V', 5, std::mem::size_of::<Format>());

const BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const FIELD_NONE: u32 = 1;
const COLORSPACE_SMPTE170M: u32 = 1;
const YCBCR_ENC_601: u32 = 1;
const QUANTIZATION_LIM_RANGE: u32 = 2;
const XFER_FUNC_709: u32 = 1;
/// `v4l2_fourcc('Y', 'U', 'Y', 'V')`.
const PIX_FMT_YUYV: u32 = u32::from_le_bytes(*b"YUYV");

fn text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn query(file: &File) -> Option<Capability> {
    // SAFETY: an all-zero `v4l2_capability` is valid, and `VIDIOC_QUERYCAP` writes exactly that struct (its size is
    // encoded in the request number) into the pointer we pass; the fd is open for the call's whole duration.
    let mut cap: Capability = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), VIDIOC_QUERYCAP as _, &raw mut cap) };
    (rc == 0).then_some(cap)
}

/// A loopback device found on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Its node, `/dev/videoN`.
    pub path: PathBuf,
    /// Its card label, the name apps show in their camera list.
    pub card: String,
}

/// The device to write to: `OMUZAKI_VCAM_DEVICE`, else the first loopback device.
///
/// # Errors
/// [`StreamError::Unavailable`] with the install hint when there is none.
pub fn find() -> Result<Found, StreamError> {
    let unavailable = || StreamError::Unavailable { what: "a v4l2loopback device".into(), hint: INSTALL_HINT.into() };
    if let Some(path) = std::env::var_os(DEVICE_ENV) {
        let path = PathBuf::from(path);
        let file = OpenOptions::new().write(true).open(&path).map_err(|e| StreamError::Unavailable {
            what: format!("the camera device {}", path.display()),
            hint: format!("check {DEVICE_ENV} and the device's permissions: {e}"),
        })?;
        let card = query(&file).map(|c| text(&c.card)).unwrap_or_default();
        return Ok(Found { path, card });
    }
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev")
        .map_err(|_| unavailable())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("video")))
        .collect();
    nodes.sort();
    for path in nodes {
        let Ok(file) = OpenOptions::new().write(true).open(&path) else { continue };
        if let Some(cap) = query(&file).filter(|c| text(&c.driver) == LOOPBACK_DRIVER) {
            return Ok(Found { path, card: text(&cap.card) });
        }
    }
    Err(unavailable())
}

/// An open loopback device, set to YUYV at one size.
pub struct Device {
    file: File,
    path: PathBuf,
}

impl Device {
    /// Opens `path` and sets its output format to YUYV `width` × `height`.
    ///
    /// # Errors
    /// [`StreamError::Unavailable`] if it cannot be opened, [`StreamError::Config`] if it refuses the format.
    pub fn open(path: &Path, width: u32, height: u32) -> Result<Self, StreamError> {
        let file = OpenOptions::new().write(true).open(path).map_err(|e| StreamError::Unavailable {
            what: format!("the camera device {}", path.display()),
            hint: format!("check that it exists and that you may write to it: {e}"),
        })?;
        let pix = PixFormat {
            width,
            height,
            pixelformat: PIX_FMT_YUYV,
            field: FIELD_NONE,
            bytesperline: width * 2,
            sizeimage: width * height * 2,
            colorspace: COLORSPACE_SMPTE170M,
            private: 0,
            flags: 0,
            ycbcr_enc: YCBCR_ENC_601,
            quantization: QUANTIZATION_LIM_RANGE,
            xfer_func: XFER_FUNC_709,
        };
        let mut format = Format { kind: BUF_TYPE_VIDEO_OUTPUT, fmt: FormatUnion { raw: [0; 200] } };
        format.fmt.pix = pix;
        // SAFETY: `format` is a valid `v4l2_format` (the union written through its `pix` member over zeroed bytes);
        // `VIDIOC_S_FMT` reads and writes exactly that struct, and the fd is open for the call.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), VIDIOC_S_FMT as _, &raw mut format) };
        if rc != 0 {
            return Err(StreamError::Config(format!(
                "{} refused YUYV {width}x{height}: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: the kernel filled the union through the `pix` member for a video output buffer type.
        let set = unsafe { format.fmt.pix };
        if (set.width, set.height, set.pixelformat) != (width, height, PIX_FMT_YUYV) {
            return Err(StreamError::Config(format!(
                "{} set {}x{} in another format instead of YUYV {width}x{height}",
                path.display(),
                set.width,
                set.height
            )));
        }
        Ok(Self { file, path: path.to_owned() })
    }

    /// Writes one YUYV frame.
    ///
    /// # Errors
    /// [`StreamError::Io`] if the write fails (the module was unloaded, the device closed).
    pub fn write(&mut self, yuyv: &[u8]) -> Result<(), StreamError> {
        self.file.write_all(yuyv).map_err(|e| StreamError::Io(format!("writing to {}: {e}", self.path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layouts_and_request_numbers_match_the_kernel_headers() {
        assert_eq!(std::mem::size_of::<Format>(), if cfg!(target_pointer_width = "64") { 208 } else { 204 });
        // The values `strace` prints for these requests on x86_64.
        if cfg!(target_pointer_width = "64") {
            assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
            assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
        }
        assert_eq!(PIX_FMT_YUYV, 0x5659_5559);
    }

    #[test]
    fn text_stops_at_the_first_nul() {
        assert_eq!(text(b"v4l2 loopback\0\0\0"), LOOPBACK_DRIVER);
        assert_eq!(text(b"no nul"), "no nul");
    }
}
