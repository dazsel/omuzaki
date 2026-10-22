//! VP9 through libvpx (BSD-3-Clause), loaded at run time: the codec of the `WebM` take with alpha (`docs/design/record.md`).
//!
//! The library is the system's own (`libvpx.so.*` on Linux, `libvpx*.dylib` on macOS, `vpx.dll` on Windows) or the
//! file `OMUZAKI_LIBVPX` names. Nothing links it at build time, so a machine without it still runs the app: the
//! recorder falls back to the PNG sequence and says why. The encoder ABI version is found by asking: libvpx refuses
//! an init call with another version before it reads anything else, so trying the versions releases have used is
//! safe.
//!
//! Alpha is coded the way `FFmpeg`, Chrome and OBS read it: two encoders, the colour as I420 and the alpha as the luma
//! of a second I420 picture, its packets carried beside the colour's in Matroska `BlockAdditional` id 1.
#![allow(unsafe_code)] // loading libvpx and calling its C functions; each call says why it is sound

pub mod ffi;

use std::ffi::{CStr, c_int, c_uint, c_void};
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};

use libloading::Library;
use omuzaki_stream_api::error::StreamError;

use ffi::{Api, CODEC_ABI_MISMATCH, CODEC_OK, Ctx, EncCfg};

/// The environment variable that names a libvpx file to load instead of the system's.
pub const LIBRARY_ENV: &str = "OMUZAKI_LIBVPX";

/// The file names tried, newest soname first.
#[cfg(target_os = "linux")]
const CANDIDATES: &[&str] = &["libvpx.so.9", "libvpx.so.8", "libvpx.so.7", "libvpx.so.6", "libvpx.so"];
#[cfg(target_os = "macos")]
const CANDIDATES: &[&str] = &[
    "libvpx.9.dylib",
    "libvpx.8.dylib",
    "libvpx.dylib",
    "/opt/homebrew/lib/libvpx.dylib",
    "/usr/local/lib/libvpx.dylib",
];
#[cfg(target_os = "windows")]
const CANDIDATES: &[&str] = &["vpx.dll", "libvpx-1.dll", "libvpx.dll"];
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const CANDIDATES: &[&str] = &[];

/// Encoder ABI versions to try after the 1.14 value. Every release since 1.8 lies in this range.
const ENCODER_ABI: std::ops::RangeInclusive<c_int> = 20..=48;
/// Decoder ABI versions to try.
const DECODER_ABI: std::ops::RangeInclusive<c_int> = 8..=20;
/// The encoder ABI of libvpx 1.14, tried first.
const ENCODER_ABI_1_14: c_int = 34;
/// The decoder ABI of libvpx 1.14, tried first.
const DECODER_ABI_1_14: c_int = 12;

/// The loaded library: its handle, kept for the life of the process, and its functions.
struct Loaded {
    _library: Library,
    api: Api,
    version: String,
    encoder_abi: AtomicI32,
    decoder_abi: AtomicI32,
}

static LIBRARY: OnceLock<Result<Loaded, String>> = OnceLock::new();

fn symbol<F: Copy>(library: &Library, name: &str) -> Result<F, String> {
    // SAFETY: each `F` is the signature `name` has in libvpx's public headers (the table in `ffi`); `Option<F>` reads a
    // null address as `None`.
    let symbol = unsafe { library.get::<Option<F>>(name.as_bytes()) }.map_err(|e| format!("no `{name}`: {e}"))?;
    (*symbol).ok_or_else(|| format!("`{name}` is null"))
}

fn open(path: &str) -> Result<Loaded, String> {
    // SAFETY: loading libvpx runs its initialisers, which only set up its own tables; the handle is kept for the life of
    // the process, so no function pointer taken from it outlives it.
    let library = unsafe { Library::new(path) }.map_err(|e| e.to_string())?;
    let api = Api {
        vp9_cx: symbol(&library, "vpx_codec_vp9_cx")?,
        vp9_dx: symbol(&library, "vpx_codec_vp9_dx")?,
        enc_config_default: symbol(&library, "vpx_codec_enc_config_default")?,
        enc_init_ver: symbol(&library, "vpx_codec_enc_init_ver")?,
        dec_init_ver: symbol(&library, "vpx_codec_dec_init_ver")?,
        control: symbol(&library, "vpx_codec_control_")?,
        encode: symbol(&library, "vpx_codec_encode")?,
        get_cx_data: symbol(&library, "vpx_codec_get_cx_data")?,
        decode: symbol(&library, "vpx_codec_decode")?,
        get_frame: symbol(&library, "vpx_codec_get_frame")?,
        destroy: symbol(&library, "vpx_codec_destroy")?,
        error: symbol(&library, "vpx_codec_error")?,
        error_detail: symbol(&library, "vpx_codec_error_detail")?,
        version_str: symbol(&library, "vpx_codec_version_str")?,
    };
    // SAFETY: `vpx_codec_version_str` takes nothing and returns a static NUL-terminated string.
    let version = unsafe { c_string((api.version_str)()) };
    Ok(Loaded { _library: library, api, version, encoder_abi: AtomicI32::new(0), decoder_abi: AtomicI32::new(0) })
}

fn load() -> Result<&'static Loaded, StreamError> {
    let loaded = LIBRARY.get_or_init(|| {
        if let Ok(path) = std::env::var(LIBRARY_ENV) {
            return open(&path).map_err(|e| format!("{LIBRARY_ENV}: {e}"));
        }
        let mut last = String::from("no candidate names for this system");
        for name in CANDIDATES {
            match open(name) {
                Ok(loaded) => return Ok(loaded),
                Err(e) => last = e,
            }
        }
        Err(last)
    });
    loaded.as_ref().map_err(|e| StreamError::Unavailable {
        what: "libvpx (VP9)".into(),
        hint: format!(
            "install libvpx (Debian and Ubuntu: libvpx9; Fedora: libvpx; Homebrew: libvpx) or set {LIBRARY_ENV} to the library file; recording falls back to a PNG sequence ({e})"
        ),
    })
}

/// Whether libvpx loads on this machine.
///
/// # Errors
/// [`StreamError::Unavailable`] with what a person can do about it.
pub fn available() -> Result<(), StreamError> {
    load().map(|_| ())
}

/// The loaded library's version string, such as `v1.14.0`.
///
/// # Errors
/// [`StreamError::Unavailable`] when libvpx does not load.
pub fn version() -> Result<String, StreamError> {
    load().map(|l| l.version.clone())
}

/// # Safety
/// `p` is null or points to a NUL-terminated string that lives for this call.
unsafe fn c_string(p: *const std::ffi::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: non-null and NUL-terminated by the caller's contract.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// The context's last error as text.
fn ctx_error(api: &Api, ctx: &Ctx) -> String {
    // SAFETY: `ctx` was passed to an init call; libvpx returns static or context-owned strings that live until the
    // next call on it, and they are copied here.
    let (error, detail) = unsafe { (c_string((api.error)(ctx)), c_string((api.error_detail)(ctx))) };
    if detail.is_empty() { error } else { format!("{error}: {detail}") }
}

/// What an encoder makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderSettings {
    /// Picture width in pixels.
    pub width: u32,
    /// Picture height in pixels.
    pub height: u32,
    /// Frames per second as numerator and denominator: one pts tick per frame.
    pub fps: (u32, u32),
    /// Constant-quality level, 0 (best) to 63.
    pub cq_level: u32,
    /// Speed, 0 (slowest, best) to 9 (fastest; above 5 in real-time mode only).
    pub cpu_used: i32,
    /// Encoder threads.
    pub threads: u32,
    /// Whether the picture is studio-range colour (BT.709) rather than full-range data such as an alpha plane.
    pub studio_range: bool,
    /// Whether to encode in libvpx's real-time mode (speeds 5 to 9, for live takes) rather than its good-quality mode
    /// (speeds 0 to 5, for exports).
    pub realtime: bool,
}

/// One compressed frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    /// The frame's pts.
    pub pts: i64,
    /// Whether it is a keyframe.
    pub key: bool,
    /// The VP9 frame.
    pub data: Vec<u8>,
}

/// A planar 4:2:0 picture: `y` is `stride_y × height`, `u` and `v` are `stride_uv × ⌈height / 2⌉`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct I420 {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Luma, one byte per pixel, rows `width` apart.
    pub y: Vec<u8>,
    /// Blue-difference chroma, `⌈width / 2⌉` per row.
    pub u: Vec<u8>,
    /// Red-difference chroma, `⌈width / 2⌉` per row.
    pub v: Vec<u8>,
}

impl I420 {
    /// A picture of `width × height` with its planes sized and zeroed.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let mut picture = Self::default();
        picture.resize(width, height);
        picture
    }

    /// Resizes the planes for `width × height`, keeping their allocations.
    pub fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        self.width = width;
        self.height = height;
        self.y.resize(w * h, 0);
        self.u.resize(cw * ch, 0);
        self.v.resize(cw * ch, 0);
    }

    /// Bytes per chroma row.
    #[must_use]
    pub fn chroma_width(&self) -> u32 {
        self.width.div_ceil(2)
    }

    fn planes_fit(&self) -> bool {
        let (w, h) = (self.width as usize, self.height as usize);
        let c = w.div_ceil(2) * h.div_ceil(2);
        self.y.len() == w * h && self.u.len() == c && self.v.len() == c
    }
}

/// A VP9 encoder producing one packet per frame (no look-ahead), keyframes only where asked.
pub struct Vp9Encoder {
    api: Api,
    ctx: Box<Ctx>,
    settings: EncoderSettings,
}

// SAFETY: the context is used from one thread at a time (`&mut self` on every call); libvpx keeps no thread-local
// state for a context, so moving it between threads is sound.
unsafe impl Send for Vp9Encoder {}

impl std::fmt::Debug for Vp9Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vp9Encoder").field("settings", &self.settings).finish_non_exhaustive()
    }
}

fn encode_error(what: &str, e: impl std::fmt::Display) -> StreamError {
    StreamError::Encode(format!("VP9 {what}: {e}"))
}

impl Vp9Encoder {
    /// An encoder for `settings`.
    ///
    /// # Errors
    /// [`StreamError::Unavailable`] when libvpx does not load; [`StreamError::Config`] for a size or rate libvpx
    /// refuses; [`StreamError::Encode`] when libvpx fails to start.
    pub fn new(settings: EncoderSettings) -> Result<Self, StreamError> {
        let loaded = load()?;
        let api = loaded.api;
        if settings.width == 0 || settings.height == 0 || settings.width > 16_384 || settings.height > 16_384 {
            return Err(StreamError::Config(format!(
                "VP9 takes 1 to 16384 pixels a side; got {}x{}",
                settings.width, settings.height
            )));
        }
        let (Ok(fps_num), Ok(fps_den)) = (c_int::try_from(settings.fps.0), c_int::try_from(settings.fps.1)) else {
            return Err(StreamError::Config(format!(
                "frame rate {}/{} is out of range",
                settings.fps.0, settings.fps.1
            )));
        };
        if fps_num == 0 || fps_den == 0 {
            return Err(StreamError::Config("the frame rate must be above zero".into()));
        }
        // SAFETY: `vpx_codec_vp9_cx` takes nothing and returns a static interface.
        let iface = unsafe { (api.vp9_cx)() };
        // SAFETY: `EncCfg` is plain integers and pointers, all-zero valid; libvpx overwrites it next.
        let mut cfg: EncCfg = unsafe { std::mem::zeroed() };
        // SAFETY: `cfg` is larger than any release's `vpx_codec_enc_cfg_t`, so libvpx's write stays inside it.
        let err = unsafe { (api.enc_config_default)(iface, &raw mut cfg, 0) };
        if err != CODEC_OK {
            return Err(encode_error("default config", err));
        }
        cfg.g_w = settings.width;
        cfg.g_h = settings.height;
        cfg.g_timebase = ffi::Rational { num: fps_den, den: fps_num };
        cfg.g_threads = settings.threads.clamp(1, 64);
        cfg.g_lag_in_frames = 0;
        cfg.g_error_resilient = 0;
        cfg.g_pass = 0;
        cfg.rc_dropframe_thresh = 0;
        cfg.rc_resize_allowed = 0;
        cfg.rc_end_usage = ffi::RC_Q;
        cfg.rc_min_quantizer = 0;
        cfg.rc_max_quantizer = 63;
        cfg.kf_mode = ffi::KF_DISABLED;
        cfg.kf_min_dist = 0;
        cfg.kf_max_dist = 1 << 30;

        let mut ctx = Box::new(Ctx::zeroed());
        let cached = loaded.encoder_abi.load(Ordering::Relaxed);
        let order = std::iter::once(if cached == 0 { ENCODER_ABI_1_14 } else { cached })
            .chain(ENCODER_ABI.rev().filter(|v| *v != ENCODER_ABI_1_14 && *v != cached));
        let mut started = false;
        for abi in order {
            // SAFETY: `ctx` is zeroed and boxed (libvpx may keep its address); `cfg` came from libvpx's own defaults.
            let err = unsafe { (api.enc_init_ver)(&raw mut *ctx, iface, &raw const cfg, 0, abi) };
            if err == CODEC_ABI_MISMATCH {
                continue;
            }
            if err != CODEC_OK {
                return Err(StreamError::Config(format!("libvpx refused the encoder: {}", ctx_error(&api, &ctx))));
            }
            loaded.encoder_abi.store(abi, Ordering::Relaxed);
            started = true;
            break;
        }
        if !started {
            return Err(StreamError::Unavailable {
                what: format!("libvpx {} (VP9 encoder)", loaded.version),
                hint: "this libvpx uses an encoder ABI this build does not know; install libvpx 1.8 or later".into(),
            });
        }
        let encoder = Self { api, ctx, settings };
        let speed_max = if settings.realtime { 9 } else { 5 };
        // Each tile column needs 256 pixels; more columns let more threads work.
        let tiles = (settings.width / 256).max(1).ilog2().min(settings.threads.max(1).ilog2() + 1);
        encoder.control(ffi::SET_CPUUSED, settings.cpu_used.clamp(0, speed_max))?;
        encoder.control(ffi::SET_CQ_LEVEL, c_int::try_from(settings.cq_level.min(63)).unwrap_or(63))?;
        encoder.control(ffi::SET_TILE_COLUMNS, c_int::try_from(tiles).unwrap_or(0))?;
        encoder.control(ffi::SET_ROW_MT, 1)?;
        encoder.control(ffi::SET_COLOR_SPACE, if settings.studio_range { ffi::CS_BT_709 } else { ffi::CS_UNKNOWN })?;
        encoder.control(ffi::SET_COLOR_RANGE, if settings.studio_range { ffi::CR_STUDIO } else { ffi::CR_FULL })?;
        Ok(encoder)
    }

    /// The settings it was made with.
    #[must_use]
    pub fn settings(&self) -> EncoderSettings {
        self.settings
    }

    fn deadline(&self) -> std::ffi::c_ulong {
        if self.settings.realtime { ffi::DL_REALTIME } else { ffi::DL_GOOD_QUALITY }
    }

    fn control(&self, id: c_int, value: c_int) -> Result<(), StreamError> {
        let ctx = ptr::from_ref::<Ctx>(&self.ctx).cast_mut();
        // SAFETY: every control used here takes one `int`, which C's variadic promotion passes as is; `ctx` was
        // initialised by `enc_init_ver` and libvpx serialises nothing else on it during the call.
        let err = unsafe { (self.api.control)(ctx, id, value) };
        if err != CODEC_OK {
            return Err(encode_error(&format!("control {id}"), ctx_error(&self.api, &self.ctx)));
        }
        Ok(())
    }

    /// Encodes `picture` as frame `pts`, a keyframe when `key`, and appends what libvpx returns to `out`.
    ///
    /// # Errors
    /// [`StreamError::Config`] when the picture's size is not the encoder's; [`StreamError::Encode`] when libvpx
    /// fails.
    pub fn encode(&mut self, picture: &I420, pts: i64, key: bool, out: &mut Vec<Packet>) -> Result<(), StreamError> {
        if picture.width != self.settings.width || picture.height != self.settings.height || !picture.planes_fit() {
            return Err(StreamError::Config(format!(
                "the encoder takes {}x{} pictures; got {}x{}",
                self.settings.width, self.settings.height, picture.width, picture.height
            )));
        }
        let image = self.wrap(picture);
        let flags = if key { ffi::EFLAG_FORCE_KF } else { 0 };
        // SAFETY: `image` points into `picture`'s planes, sized for its dimensions above and alive across the call;
        // libvpx copies the picture before it returns (no look-ahead is configured).
        let err = unsafe { (self.api.encode)(&raw mut *self.ctx, &raw const image, pts, 1, flags, self.deadline()) };
        if err != CODEC_OK {
            return Err(encode_error("encode", ctx_error(&self.api, &self.ctx)));
        }
        self.drain(out);
        Ok(())
    }

    /// Flushes the encoder and appends the last packets to `out`.
    ///
    /// # Errors
    /// [`StreamError::Encode`] when libvpx fails.
    pub fn flush(&mut self, out: &mut Vec<Packet>) -> Result<(), StreamError> {
        // SAFETY: a null image asks libvpx to flush; the context is initialised.
        let err = unsafe { (self.api.encode)(&raw mut *self.ctx, ptr::null(), 0, 0, 0, self.deadline()) };
        if err != CODEC_OK {
            return Err(encode_error("flush", ctx_error(&self.api, &self.ctx)));
        }
        self.drain(out);
        Ok(())
    }

    fn drain(&mut self, out: &mut Vec<Packet>) {
        let mut iter: *const c_void = ptr::null();
        loop {
            // SAFETY: `iter` starts null and is only advanced by libvpx; the packet lives until the next call on the
            // context, and its bytes are copied before that.
            let pkt = unsafe { (self.api.get_cx_data)(&raw mut *self.ctx, &raw mut iter) };
            if pkt.is_null() {
                return;
            }
            // SAFETY: non-null packets point at a `vpx_codec_cx_pkt_t`, whose prefix `CxPkt` names.
            let pkt = unsafe { &*pkt };
            if pkt.kind != ffi::CX_FRAME_PKT || pkt.buf.is_null() {
                continue;
            }
            // SAFETY: a frame packet's `buf` holds `sz` bytes until the next call on the context.
            let data = unsafe { std::slice::from_raw_parts(pkt.buf.cast::<u8>(), pkt.sz) }.to_vec();
            out.push(Packet { pts: pkt.pts, key: pkt.flags & ffi::FRAME_IS_KEY != 0, data });
        }
    }

    fn wrap(&self, picture: &I420) -> ffi::Image {
        let range = if self.settings.studio_range { ffi::CR_STUDIO } else { ffi::CR_FULL };
        let cs = if self.settings.studio_range { ffi::CS_BT_709 } else { ffi::CS_UNKNOWN };
        let (w, cw) = (picture.width, picture.chroma_width());
        ffi::Image {
            fmt: ffi::IMG_FMT_I420,
            cs,
            range,
            w: picture.width,
            h: picture.height,
            bit_depth: 8,
            d_w: picture.width,
            d_h: picture.height,
            r_w: 0,
            r_h: 0,
            x_chroma_shift: 1,
            y_chroma_shift: 1,
            // libvpx reads the planes and never writes them.
            planes: [
                picture.y.as_ptr().cast_mut(),
                picture.u.as_ptr().cast_mut(),
                picture.v.as_ptr().cast_mut(),
                ptr::null_mut(),
            ],
            stride: [
                c_int::try_from(w).unwrap_or(0),
                c_int::try_from(cw).unwrap_or(0),
                c_int::try_from(cw).unwrap_or(0),
                0,
            ],
            bps: 12,
            user_priv: ptr::null_mut(),
            img_data: ptr::null_mut(),
            img_data_owner: 0,
            self_allocd: 0,
            fb_priv: ptr::null_mut(),
        }
    }
}

impl Drop for Vp9Encoder {
    fn drop(&mut self) {
        // SAFETY: the context was initialised in `new` and is destroyed once.
        unsafe { (self.api.destroy)(&raw mut *self.ctx) };
    }
}

/// A VP9 decoder, for reading takes back.
pub struct Vp9Decoder {
    api: Api,
    ctx: Box<Ctx>,
}

// SAFETY: as for the encoder: one thread at a time through `&mut self`.
unsafe impl Send for Vp9Decoder {}

impl std::fmt::Debug for Vp9Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vp9Decoder").finish_non_exhaustive()
    }
}

impl Vp9Decoder {
    /// A decoder.
    ///
    /// # Errors
    /// [`StreamError::Unavailable`] when libvpx does not load or its decoder ABI is unknown.
    pub fn new() -> Result<Self, StreamError> {
        let loaded = load()?;
        let api = loaded.api;
        // SAFETY: `vpx_codec_vp9_dx` takes nothing and returns a static interface.
        let iface = unsafe { (api.vp9_dx)() };
        let mut ctx = Box::new(Ctx::zeroed());
        let cached = loaded.decoder_abi.load(Ordering::Relaxed);
        let order = std::iter::once(if cached == 0 { DECODER_ABI_1_14 } else { cached })
            .chain(DECODER_ABI.rev().filter(|v| *v != DECODER_ABI_1_14 && *v != cached));
        for abi in order {
            // SAFETY: `ctx` is zeroed and boxed; a null config asks for defaults.
            let err = unsafe { (api.dec_init_ver)(&raw mut *ctx, iface, ptr::null(), 0, abi) };
            if err == CODEC_ABI_MISMATCH {
                continue;
            }
            if err != CODEC_OK {
                return Err(StreamError::Encode(format!("libvpx refused the decoder: {}", ctx_error(&api, &ctx))));
            }
            loaded.decoder_abi.store(abi, Ordering::Relaxed);
            return Ok(Self { api, ctx });
        }
        Err(StreamError::Unavailable {
            what: format!("libvpx {} (VP9 decoder)", loaded.version),
            hint: "this libvpx uses a decoder ABI this build does not know; install libvpx 1.8 or later".into(),
        })
    }

    /// Decodes one VP9 frame into `out`'s planes.
    ///
    /// # Errors
    /// [`StreamError::Encode`] when the frame does not decode or yields no picture.
    pub fn decode(&mut self, data: &[u8], out: &mut I420) -> Result<(), StreamError> {
        let len = c_uint::try_from(data.len()).map_err(|_| encode_error("decode", "frame too large"))?;
        // SAFETY: `data` is `len` readable bytes for the call; the context is initialised.
        let err = unsafe { (self.api.decode)(&raw mut *self.ctx, data.as_ptr(), len, ptr::null_mut(), 0) };
        if err != CODEC_OK {
            return Err(encode_error("decode", ctx_error(&self.api, &self.ctx)));
        }
        let mut iter: *const c_void = ptr::null();
        let mut got = false;
        loop {
            // SAFETY: as for `get_cx_data`: libvpx advances `iter`; the image lives until the next call.
            let image = unsafe { (self.api.get_frame)(&raw mut *self.ctx, &raw mut iter) };
            if image.is_null() {
                break;
            }
            // SAFETY: non-null images point at a `vpx_image_t`.
            let image = unsafe { &*image };
            if image.fmt != ffi::IMG_FMT_I420 {
                return Err(encode_error("decode", format!("format {:#x} is not I420", image.fmt)));
            }
            out.resize(image.d_w, image.d_h);
            let (w, h) = (image.d_w as usize, image.d_h as usize);
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            for (plane, (dst, (pw, ph))) in
                [(&mut out.y, (w, h)), (&mut out.u, (cw, ch)), (&mut out.v, (cw, ch))].into_iter().enumerate()
            {
                let stride =
                    usize::try_from(image.stride[plane]).map_err(|_| encode_error("decode", "negative stride"))?;
                for row in 0..ph {
                    // SAFETY: libvpx's plane holds `ph` rows of `stride ≥ pw` bytes.
                    let src = unsafe { std::slice::from_raw_parts(image.planes[plane].add(row * stride), pw) };
                    dst[row * pw..(row + 1) * pw].copy_from_slice(src);
                }
            }
            got = true;
        }
        if got { Ok(()) } else { Err(encode_error("decode", "the frame yielded no picture")) }
    }
}

impl Drop for Vp9Decoder {
    fn drop(&mut self) {
        // SAFETY: the context was initialised in `new` and is destroyed once.
        unsafe { (self.api.destroy)(&raw mut *self.ctx) };
    }
}

/// The encoder threads to use on this machine: one per core, at most 16.
#[must_use]
pub fn default_threads() -> u32 {
    std::thread::available_parallelism().map_or(2, |n| u32::try_from(n.get()).unwrap_or(2)).clamp(1, 16)
}
