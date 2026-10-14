//! Safe wrappers over the Spout2 SDK's DirectX 11 sender and receiver classes (through the C shim).
//!
//! The SDK keeps its own registry of senders in shared memory, a named mutex around the shared texture, and a frame
//! counter plus sync event; this module only owns the handles and turns the shim's int returns into `Result`s.

#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_void};
use std::ptr::NonNull;

use crate::ffi;

/// Why a Spout call failed.
#[derive(Debug, thiserror::Error)]
pub enum SpoutError {
    /// The name contains an interior NUL byte.
    #[error("sender name must not contain NUL")]
    BadName,
    /// The SDK could not create the sender or receiver (no D3D11 device, out of memory).
    #[error("Spout could not create the {0}")]
    Create(&'static str),
    /// A send failed: no shared texture could be made, or the source texture was empty.
    #[error("Spout send failed")]
    Send,
    /// A pixel readback from the received texture failed.
    #[error("Spout receive readback failed")]
    Read,
    /// The GPU did not finish the shared-texture copy in time.
    #[error("the GPU did not finish the Spout copy within a second")]
    GpuTimeout,
    /// The caller's pixel buffer is the wrong size.
    #[error("buffer holds {got} bytes, the frame needs {want}")]
    BufferSize {
        /// Bytes supplied.
        got: usize,
        /// Bytes needed.
        want: usize,
    },
}

/// A Spout sender. Dropping it unregisters the sender, so receivers see it disappear.
pub struct SpoutSender {
    raw: NonNull<ffi::BoSender>,
}

impl SpoutSender {
    /// Creates a sender called `name` that shares textures made on `d3d11_device` (an `ID3D11Device*`, which for the
    /// D3D12 route is the `D3D11On12` device).
    ///
    /// # Safety
    /// `d3d11_device` must be a valid `ID3D11Device*` that outlives the sender.
    ///
    /// # Errors
    /// Fails on a bad name or when the SDK cannot create the sender.
    pub unsafe fn on_device(d3d11_device: *mut c_void, name: &str) -> Result<Self, SpoutError> {
        let name = CString::new(name).map_err(|_| SpoutError::BadName)?;
        // SAFETY: `name` is a valid NUL-terminated string for the call; the device pointer is the caller's contract.
        let raw = unsafe { ffi::bo_sender_new(d3d11_device, name.as_ptr()) };
        NonNull::new(raw).map(|raw| Self { raw }).ok_or(SpoutError::Create("sender"))
    }

    /// Creates a sender on a Direct3D 11 device that Spout makes for itself, for sending CPU pixels.
    ///
    /// # Errors
    /// Fails on a bad name or when the SDK cannot create the sender.
    pub fn standalone(name: &str) -> Result<Self, SpoutError> {
        // SAFETY: a null device asks Spout to create and own one.
        unsafe { Self::on_device(std::ptr::null_mut(), name) }
    }

    /// Sends the contents of an `ID3D11Texture2D*` made on the sender's device. Spout copies it into its own shared
    /// texture (creating or resizing that on first use) and flushes the device.
    ///
    /// # Safety
    /// `texture` must be a valid `ID3D11Texture2D*` on the device the sender was created with.
    ///
    /// # Errors
    /// Fails when Spout cannot create its shared texture.
    pub(crate) unsafe fn send_texture(&mut self, texture: *mut c_void) -> Result<(), SpoutError> {
        // SAFETY: `self.raw` is live until drop; `texture` is the caller's contract.
        if unsafe { ffi::bo_sender_send_texture(self.raw.as_ptr(), texture) } == 1 {
            Ok(())
        } else {
            Err(SpoutError::Send)
        }
    }

    /// Sends tightly packed BGRA8 pixels (the CPU path).
    ///
    /// # Errors
    /// Fails when the buffer is the wrong size or Spout cannot create its shared texture.
    pub fn send_image(&mut self, bgra: &[u8], width: u32, height: u32) -> Result<(), SpoutError> {
        let want = width as usize * height as usize * 4;
        if bgra.len() != want {
            return Err(SpoutError::BufferSize { got: bgra.len(), want });
        }
        // SAFETY: the buffer holds `width * height * 4` bytes (checked above) and the call only reads it.
        let ok = unsafe { ffi::bo_sender_send_image(self.raw.as_ptr(), bgra.as_ptr(), width, height, 0) };
        if ok == 1 { Ok(()) } else { Err(SpoutError::Send) }
    }

    /// Blocks until the sender's D3D11 device has finished everything it was given, so the shared texture is complete
    /// before receivers are told about it.
    ///
    /// # Errors
    /// Fails when the GPU does not finish within a second.
    pub fn wait_gpu(&mut self) -> Result<(), SpoutError> {
        // SAFETY: `self.raw` is live until drop.
        if unsafe { ffi::bo_sender_wait_gpu(self.raw.as_ptr()) } == 1 { Ok(()) } else { Err(SpoutError::GpuTimeout) }
    }

    /// Signals the per-sender sync event that [`SpoutReceiver::wait_sync`] waits on.
    pub fn signal_sync(&mut self) {
        // SAFETY: `self.raw` is live until drop.
        unsafe { ffi::bo_sender_signal_sync(self.raw.as_ptr()) };
    }

    /// The name Spout registered (it appends `_1`, `_2` ... when the requested one is taken). `None` before the first send.
    #[must_use]
    pub fn name(&self) -> Option<String> {
        // SAFETY: `self.raw` is live; the returned pointer is null or a NUL-terminated string owned by the SDK object,
        // copied out before any further call.
        let ptr = unsafe { ffi::bo_sender_name(self.raw.as_ptr()) };
        (!ptr.is_null()).then(|| unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
    }
}

impl Drop for SpoutSender {
    fn drop(&mut self) {
        // SAFETY: `self.raw` came from `bo_sender_new` and is freed exactly once, here.
        unsafe { ffi::bo_sender_free(self.raw.as_ptr()) };
    }
}

/// What one receive poll found.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReceiveInfo {
    /// A sender with the wanted name exists.
    pub connected: bool,
    /// The sender appeared or changed; poll again before reading.
    pub updated: bool,
    /// A frame newer than the last one has been copied and can be read.
    pub new_frame: bool,
    /// Sender texture width.
    pub width: u32,
    /// Sender texture height.
    pub height: u32,
    /// Sender texture `DXGI_FORMAT`.
    pub dxgi_format: u32,
    /// The sender's frame counter.
    pub sender_frame: u64,
    /// The sender's frame rate as it publishes it.
    pub sender_fps: f64,
}

/// A Spout receiver bound to one sender name.
pub struct SpoutReceiver {
    raw: NonNull<ffi::BoReceiver>,
    name: CString,
}

impl SpoutReceiver {
    /// Creates a receiver that connects to the sender called `name`.
    ///
    /// # Errors
    /// Fails on a bad name or when the SDK cannot create the receiver.
    pub fn new(name: &str) -> Result<Self, SpoutError> {
        let name = CString::new(name).map_err(|_| SpoutError::BadName)?;
        // SAFETY: `name` is a valid NUL-terminated string for the call.
        let raw = unsafe { ffi::bo_receiver_new(name.as_ptr()) };
        NonNull::new(raw).map(|raw| Self { raw, name }).ok_or(SpoutError::Create("receiver"))
    }

    /// The sender name this receiver looks for.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name.to_str().unwrap_or_default()
    }

    /// Waits up to `timeout_ms` for the sender's sync event; true when it fired.
    pub fn wait_sync(&mut self, timeout_ms: u32) -> bool {
        // SAFETY: `self.raw` is live until drop.
        unsafe { ffi::bo_receiver_wait_sync(self.raw.as_ptr(), timeout_ms) == 1 }
    }

    /// Connects if needed and copies the newest frame into the receiver's own texture.
    pub fn poll(&mut self) -> ReceiveInfo {
        let mut info = ffi::BoReceiveInfo::default();
        // SAFETY: `self.raw` is live and `info` is a valid, writable struct matching the shim's layout.
        unsafe { ffi::bo_receiver_poll(self.raw.as_ptr(), &raw mut info) };
        ReceiveInfo {
            connected: info.connected == 1,
            updated: info.updated == 1,
            new_frame: info.new_frame == 1,
            width: info.width,
            height: info.height,
            dxgi_format: info.dxgi_format,
            sender_frame: info.sender_frame,
            sender_fps: info.sender_fps,
        }
    }

    /// Copies the texture the last poll received into `dst` as tightly packed rows. Blocks until the GPU copy is done.
    ///
    /// # Errors
    /// Fails when `dst` is too small or the GPU readback fails.
    pub fn read_pixels(&mut self, dst: &mut [u8]) -> Result<(), SpoutError> {
        // SAFETY: `self.raw` is live; the shim writes at most `dst.len()` bytes into `dst`.
        let ok = unsafe { ffi::bo_receiver_read_pixels(self.raw.as_ptr(), dst.as_mut_ptr(), dst.len()) };
        if ok == 1 { Ok(()) } else { Err(SpoutError::Read) }
    }
}

impl Drop for SpoutReceiver {
    fn drop(&mut self) {
        // SAFETY: `self.raw` came from `bo_receiver_new` and is freed exactly once, here.
        unsafe { ffi::bo_receiver_free(self.raw.as_ptr()) };
    }
}
