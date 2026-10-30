//! The slice of libopus's C interface the Opus encoder and decoder call, loaded at run time.
//!
//! Signatures follow `opus.h` and `opus_defines.h` (stable since 1.0): `opus_int32` is a 32-bit `int` on every platform
//! libopus builds for. The encoder and decoder states are opaque; libopus allocates and frees them.

use std::ffi::{c_char, c_int, c_uchar};

/// `OPUS_OK`.
pub const OK: c_int = 0;
/// `OPUS_APPLICATION_AUDIO`: the general-purpose mode, for a voice over music or effects as much as for speech alone.
pub const APPLICATION_AUDIO: c_int = 2049;
/// `OPUS_SET_BITRATE_REQUEST`, with one `opus_int32` in bits per second.
pub const SET_BITRATE: c_int = 4002;
/// `OPUS_SET_COMPLEXITY_REQUEST`, with one `opus_int32` from 0 to 10.
pub const SET_COMPLEXITY: c_int = 4010;
/// `OPUS_GET_LOOKAHEAD_REQUEST`, with one `opus_int32 *`: the samples the decoder must skip at the start.
pub const GET_LOOKAHEAD: c_int = 4027;

/// `OpusEncoder`, opaque.
#[repr(C)]
pub struct Encoder {
    _private: [u8; 0],
}

/// `OpusDecoder`, opaque.
#[repr(C)]
pub struct Decoder {
    _private: [u8; 0],
}

/// The functions, looked up once when the library loads.
#[derive(Clone, Copy)]
pub struct Api {
    /// `opus_encoder_create`.
    pub encoder_create: unsafe extern "C" fn(i32, c_int, c_int, *mut c_int) -> *mut Encoder,
    /// `opus_encode_float`.
    pub encode_float: unsafe extern "C" fn(*mut Encoder, *const f32, c_int, *mut c_uchar, i32) -> i32,
    /// `opus_encoder_ctl`, variadic with one `opus_int32` or one `opus_int32 *` for every request used here.
    pub encoder_ctl: unsafe extern "C" fn(*mut Encoder, c_int, ...) -> c_int,
    /// `opus_encoder_destroy`.
    pub encoder_destroy: unsafe extern "C" fn(*mut Encoder),
    /// `opus_decoder_create`.
    pub decoder_create: unsafe extern "C" fn(i32, c_int, *mut c_int) -> *mut Decoder,
    /// `opus_decode_float`.
    pub decode_float: unsafe extern "C" fn(*mut Decoder, *const c_uchar, i32, *mut f32, c_int, c_int) -> c_int,
    /// `opus_decoder_destroy`.
    pub decoder_destroy: unsafe extern "C" fn(*mut Decoder),
    /// `opus_strerror`.
    pub strerror: unsafe extern "C" fn(c_int) -> *const c_char,
    /// `opus_get_version_string`.
    pub version_string: unsafe extern "C" fn() -> *const c_char,
}
