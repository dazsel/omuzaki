//! The slice of libvpx's C interface the VP9 encoder and decoder call, loaded at run time.
//!
//! Layouts follow libvpx's public headers (`vpx_codec.h`, `vpx_image.h`, `vpx_encoder.h`, `vpx_decoder.h`, `vp8cx.h`;
//! checked against 1.14). Only the stable prefix of the encoder config is named: libvpx fills the whole struct and reads
//! it back, so the rest is opaque padding larger than any release's tail. The codec context is opaque for the same
//! reason; errors are read through `vpx_codec_error`.

use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void};

/// `vpx_codec_err_t`.
pub type CodecErr = c_int;
/// `VPX_CODEC_OK`.
pub const CODEC_OK: CodecErr = 0;
/// `VPX_CODEC_ABI_MISMATCH`: the ABI version passed to an init call is not the library's.
pub const CODEC_ABI_MISMATCH: CodecErr = 3;

/// `VPX_IMG_FMT_I420`.
pub const IMG_FMT_I420: c_int = 0x100 | 2;
/// `VPX_CS_UNKNOWN`.
pub const CS_UNKNOWN: c_int = 0;
/// `VPX_CS_BT_709`.
pub const CS_BT_709: c_int = 2;
/// `VPX_CR_STUDIO_RANGE`: Y 16 to 235, UV 16 to 240.
pub const CR_STUDIO: c_int = 0;
/// `VPX_CR_FULL_RANGE`.
pub const CR_FULL: c_int = 1;

/// `VPX_RC_MODE` `VPX_Q`: constant quality.
pub const RC_Q: c_int = 3;
/// `VPX_KF_DISABLED`: the encoder places no keyframes; the writer forces them.
pub const KF_DISABLED: c_int = 0;
/// `VPX_EFLAG_FORCE_KF`.
pub const EFLAG_FORCE_KF: c_long = 1;
/// `VPX_FRAME_IS_KEY`.
pub const FRAME_IS_KEY: u32 = 1;
/// `VPX_CODEC_CX_FRAME_PKT`.
pub const CX_FRAME_PKT: c_int = 0;
/// `VPX_DL_REALTIME`: libvpx's real-time mode.
pub const DL_REALTIME: c_ulong = 1;
/// `VPX_DL_GOOD_QUALITY`.
pub const DL_GOOD_QUALITY: c_ulong = 1_000_000;

/// `VP8E_SET_CPUUSED`.
pub const SET_CPUUSED: c_int = 13;
/// `VP8E_SET_CQ_LEVEL`.
pub const SET_CQ_LEVEL: c_int = 25;
/// `VP9E_SET_TILE_COLUMNS` (log2).
pub const SET_TILE_COLUMNS: c_int = 33;
/// `VP9E_SET_COLOR_SPACE`: the colour space written in the bitstream (the image's `cs` is not).
pub const SET_COLOR_SPACE: c_int = 46;
/// `VP9E_SET_COLOR_RANGE`.
pub const SET_COLOR_RANGE: c_int = 51;
/// `VP9E_SET_ROW_MT`.
pub const SET_ROW_MT: c_int = 55;

/// `vpx_rational_t`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Rational {
    /// Numerator.
    pub num: c_int,
    /// Denominator.
    pub den: c_int,
}

/// `vpx_fixed_buf_t`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FixedBuf {
    /// The bytes.
    pub buf: *mut c_void,
    /// Their length.
    pub sz: usize,
}

/// `vpx_codec_enc_cfg_t` up to `kf_max_dist`, then room for the rest of any release's struct.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncCfg {
    /// `g_usage`.
    pub g_usage: c_uint,
    /// `g_threads`.
    pub g_threads: c_uint,
    /// `g_profile`.
    pub g_profile: c_uint,
    /// `g_w`.
    pub g_w: c_uint,
    /// `g_h`.
    pub g_h: c_uint,
    /// `g_bit_depth`.
    pub g_bit_depth: c_int,
    /// `g_input_bit_depth`.
    pub g_input_bit_depth: c_uint,
    /// `g_timebase`: seconds per pts tick.
    pub g_timebase: Rational,
    /// `g_error_resilient`.
    pub g_error_resilient: u32,
    /// `g_pass`.
    pub g_pass: c_int,
    /// `g_lag_in_frames`.
    pub g_lag_in_frames: c_uint,
    /// `rc_dropframe_thresh`.
    pub rc_dropframe_thresh: c_uint,
    /// `rc_resize_allowed`.
    pub rc_resize_allowed: c_uint,
    /// `rc_scaled_width`.
    pub rc_scaled_width: c_uint,
    /// `rc_scaled_height`.
    pub rc_scaled_height: c_uint,
    /// `rc_resize_up_thresh`.
    pub rc_resize_up_thresh: c_uint,
    /// `rc_resize_down_thresh`.
    pub rc_resize_down_thresh: c_uint,
    /// `rc_end_usage`.
    pub rc_end_usage: c_int,
    /// `rc_twopass_stats_in`.
    pub rc_twopass_stats_in: FixedBuf,
    /// `rc_firstpass_mb_stats_in`.
    pub rc_firstpass_mb_stats_in: FixedBuf,
    /// `rc_target_bitrate`.
    pub rc_target_bitrate: c_uint,
    /// `rc_min_quantizer`.
    pub rc_min_quantizer: c_uint,
    /// `rc_max_quantizer`.
    pub rc_max_quantizer: c_uint,
    /// `rc_undershoot_pct`.
    pub rc_undershoot_pct: c_uint,
    /// `rc_overshoot_pct`.
    pub rc_overshoot_pct: c_uint,
    /// `rc_buf_sz`.
    pub rc_buf_sz: c_uint,
    /// `rc_buf_initial_sz`.
    pub rc_buf_initial_sz: c_uint,
    /// `rc_buf_optimal_sz`.
    pub rc_buf_optimal_sz: c_uint,
    /// `rc_2pass_vbr_bias_pct`.
    pub rc_2pass_vbr_bias_pct: c_uint,
    /// `rc_2pass_vbr_minsection_pct`.
    pub rc_2pass_vbr_minsection_pct: c_uint,
    /// `rc_2pass_vbr_maxsection_pct`.
    pub rc_2pass_vbr_maxsection_pct: c_uint,
    /// `rc_2pass_vbr_corpus_complexity`.
    pub rc_2pass_vbr_corpus_complexity: c_uint,
    /// `kf_mode`.
    pub kf_mode: c_int,
    /// `kf_min_dist`.
    pub kf_min_dist: c_uint,
    /// `kf_max_dist`.
    pub kf_max_dist: c_uint,
    /// The layer, temporal and rate-control fields after `kf_max_dist`, which libvpx sets and reads itself (about 300
    /// bytes in 1.14).
    pub rest: [u64; 256],
}

/// `vpx_image_t`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Image {
    /// `fmt`.
    pub fmt: c_int,
    /// `cs`.
    pub cs: c_int,
    /// `range`.
    pub range: c_int,
    /// `w`: stored width.
    pub w: c_uint,
    /// `h`: stored height.
    pub h: c_uint,
    /// `bit_depth`.
    pub bit_depth: c_uint,
    /// `d_w`: displayed width.
    pub d_w: c_uint,
    /// `d_h`: displayed height.
    pub d_h: c_uint,
    /// `r_w`.
    pub r_w: c_uint,
    /// `r_h`.
    pub r_h: c_uint,
    /// `x_chroma_shift`.
    pub x_chroma_shift: c_uint,
    /// `y_chroma_shift`.
    pub y_chroma_shift: c_uint,
    /// `planes`: Y, U, V, alpha.
    pub planes: [*mut u8; 4],
    /// `stride`.
    pub stride: [c_int; 4],
    /// `bps`.
    pub bps: c_int,
    /// `user_priv`.
    pub user_priv: *mut c_void,
    /// `img_data`.
    pub img_data: *mut u8,
    /// `img_data_owner`.
    pub img_data_owner: c_int,
    /// `self_allocd`.
    pub self_allocd: c_int,
    /// `fb_priv`.
    pub fb_priv: *mut c_void,
}

/// The `frame` member of `vpx_codec_cx_pkt_t`'s data union, with its kind before it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CxPkt {
    /// `kind`.
    pub kind: c_int,
    /// `data.frame.buf`.
    pub buf: *mut c_void,
    /// `data.frame.sz`.
    pub sz: usize,
    /// `data.frame.pts`.
    pub pts: i64,
    /// `data.frame.duration`.
    pub duration: c_ulong,
    /// `data.frame.flags`.
    pub flags: u32,
}

/// `vpx_codec_ctx_t`, opaque: 56 bytes on 64-bit systems in every release, held in room for twice that.
#[repr(C)]
pub struct Ctx {
    _opaque: [usize; 16],
}

impl Ctx {
    /// A zeroed context, as libvpx expects before an init call.
    pub fn zeroed() -> Self {
        Self { _opaque: [0; 16] }
    }
}

/// `vpx_codec_iface_t`, opaque.
#[repr(C)]
pub struct Iface {
    _private: [u8; 0],
}

/// The functions, looked up once when the library loads.
#[derive(Clone, Copy)]
pub struct Api {
    /// `vpx_codec_vp9_cx`.
    pub vp9_cx: unsafe extern "C" fn() -> *const Iface,
    /// `vpx_codec_vp9_dx`.
    pub vp9_dx: unsafe extern "C" fn() -> *const Iface,
    /// `vpx_codec_enc_config_default`.
    pub enc_config_default: unsafe extern "C" fn(*const Iface, *mut EncCfg, c_uint) -> CodecErr,
    /// `vpx_codec_enc_init_ver`.
    pub enc_init_ver: unsafe extern "C" fn(*mut Ctx, *const Iface, *const EncCfg, c_long, c_int) -> CodecErr,
    /// `vpx_codec_dec_init_ver`.
    pub dec_init_ver: unsafe extern "C" fn(*mut Ctx, *const Iface, *const c_void, c_long, c_int) -> CodecErr,
    /// `vpx_codec_control_`, variadic with one int argument for every control used here.
    pub control: unsafe extern "C" fn(*mut Ctx, c_int, ...) -> CodecErr,
    /// `vpx_codec_encode`.
    pub encode: unsafe extern "C" fn(*mut Ctx, *const Image, i64, c_ulong, c_long, c_ulong) -> CodecErr,
    /// `vpx_codec_get_cx_data`.
    pub get_cx_data: unsafe extern "C" fn(*mut Ctx, *mut *const c_void) -> *const CxPkt,
    /// `vpx_codec_decode`.
    pub decode: unsafe extern "C" fn(*mut Ctx, *const u8, c_uint, *mut c_void, c_long) -> CodecErr,
    /// `vpx_codec_get_frame`.
    pub get_frame: unsafe extern "C" fn(*mut Ctx, *mut *const c_void) -> *const Image,
    /// `vpx_codec_destroy`.
    pub destroy: unsafe extern "C" fn(*mut Ctx) -> CodecErr,
    /// `vpx_codec_error`.
    pub error: unsafe extern "C" fn(*const Ctx) -> *const c_char,
    /// `vpx_codec_error_detail`.
    pub error_detail: unsafe extern "C" fn(*const Ctx) -> *const c_char,
    /// `vpx_codec_version_str`.
    pub version_str: unsafe extern "C" fn() -> *const c_char,
}
