//! Raw declarations for the C shim in `csrc/bo_shim.h`. Nothing here is safe to call directly; use [`crate::spout`].

#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

/// Opaque sender handle.
#[repr(C)]
pub struct BoSender {
    _private: [u8; 0],
}

/// Opaque receiver handle.
#[repr(C)]
pub struct BoReceiver {
    _private: [u8; 0],
}

/// Mirrors `BoReceiveInfo` in `bo_shim.h`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BoReceiveInfo {
    pub connected: u32,
    pub updated: u32,
    pub new_frame: u32,
    pub width: u32,
    pub height: u32,
    pub dxgi_format: u32,
    pub sender_frame: u64,
    pub sender_fps: f64,
}

unsafe extern "C" {
    pub fn bo_sender_new(d3d11_device: *mut c_void, name: *const c_char) -> *mut BoSender;
    pub fn bo_sender_free(sender: *mut BoSender);
    pub fn bo_sender_send_texture(sender: *mut BoSender, texture: *mut c_void) -> c_int;
    pub fn bo_sender_send_image(sender: *mut BoSender, pixels: *const u8, width: u32, height: u32, pitch: u32)
    -> c_int;
    pub fn bo_sender_wait_gpu(sender: *mut BoSender) -> c_int;
    pub fn bo_sender_signal_sync(sender: *mut BoSender);
    pub fn bo_sender_name(sender: *const BoSender) -> *const c_char;

    pub fn bo_receiver_new(name: *const c_char) -> *mut BoReceiver;
    pub fn bo_receiver_free(receiver: *mut BoReceiver);
    pub fn bo_receiver_wait_sync(receiver: *mut BoReceiver, timeout_ms: u32) -> c_int;
    pub fn bo_receiver_poll(receiver: *mut BoReceiver, info: *mut BoReceiveInfo) -> c_int;
    pub fn bo_receiver_read_pixels(receiver: *mut BoReceiver, dst: *mut u8, dst_len: usize) -> c_int;
}
