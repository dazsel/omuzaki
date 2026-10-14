//! The seam between the engine and the operating system's audio (design doc section 2.1, ADR 0018 decision 1).
//!
//! The engine talks to devices only through [`AudioBackend`]. `omuzaki-audio` implements it with cpal; the mock
//! implements it with scripted fake devices (devices that vanish, return, reorder and drift); a native WASAPI backend
//! can be added later as a sibling crate without touching the graph.

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::device::{DeviceId, DeviceInfo};

/// What a stream was opened with.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StreamConfig {
    /// Sample rate in Hz. The graph runs at 48 kHz; a device at another rate is resampled at its bridge.
    pub rate: u32,
    /// Interleaved channel count of the callback buffers.
    pub channels: u16,
    /// The requested callback size in frames (the engine asks for 256). The real size can differ, and a callback may
    /// deliver fewer or more frames than requested.
    pub buffer_frames: u32,
}

/// The timestamps of one device callback, on the engine's monotonic timeline (the timebase of
/// [`omuzaki_core::time::Clock`]).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CallbackTime {
    /// When the callback ran.
    pub callback: Seconds,
    /// For an input, when the first frame of the buffer was captured; for an output, when the first frame will be
    /// played. The bridge places samples on the engine timeline by this instant, not by when they arrived.
    pub device: Seconds,
}

/// Why the backend failed. Streams report these through their error callback; `open_*` returns them directly.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
pub enum BackendError {
    /// The device is gone (unplugged, disabled, or its driver restarted).
    #[error("the device is not available")]
    DeviceNotAvailable,
    /// The device's format changed under the stream (WASAPI `DeviceChanged`).
    #[error("the device changed its format")]
    DeviceChanged,
    /// No device has this id on this host.
    #[error("no device {0:?}")]
    UnknownDevice(DeviceId),
    /// The device cannot run this configuration.
    #[error("unsupported stream configuration: {0}")]
    Unsupported(String),
    /// Anything else the host reports.
    #[error("audio backend error: {0}")]
    Other(String),
}

/// Called on the device's thread with captured interleaved samples. Must be wait-free and allocation-free.
pub type InputCallback = Box<dyn FnMut(&[f32], CallbackTime) + Send + 'static>;

/// Called on the device's thread to fill an interleaved buffer for playback (it arrives zeroed). Must be wait-free and
/// allocation-free.
pub type OutputCallback = Box<dyn FnMut(&mut [f32], CallbackTime) + Send + 'static>;

/// Called when a running stream fails. It may run on any thread and must not block.
pub type ErrorCallback = Box<dyn FnMut(BackendError) + Send + 'static>;

/// An open stream. Dropping it closes the stream; no callback runs after the drop returns.
pub trait DeviceStream: Send {
    /// The configuration the stream really runs with (the host may differ from what was requested).
    fn config(&self) -> StreamConfig;
}

/// The operating system's audio, as the engine needs it.
pub trait AudioBackend: Send + Sync {
    /// Every device the host currently has. Cheap: the engine calls it once a second to watch for changes, because
    /// there is no portable hotplug notification.
    ///
    /// # Errors
    /// [`BackendError::Other`] when the host cannot enumerate.
    fn devices(&self) -> Result<Vec<DeviceInfo>, BackendError>;

    /// The device the OS currently names the default for `direction`, for `Binding::SystemDefault`.
    ///
    /// # Errors
    /// [`BackendError::Other`] when the host cannot enumerate.
    fn default_device(&self, direction: crate::device::Direction) -> Result<Option<DeviceId>, BackendError>;

    /// Opens a capture stream on `id` and starts it.
    ///
    /// # Errors
    /// [`BackendError::UnknownDevice`], [`BackendError::DeviceNotAvailable`] or [`BackendError::Unsupported`].
    fn open_input(
        &self,
        id: &DeviceId,
        config: StreamConfig,
        on_data: InputCallback,
        on_error: ErrorCallback,
    ) -> Result<Box<dyn DeviceStream>, BackendError>;

    /// Opens a playback stream on `id` and starts it.
    ///
    /// # Errors
    /// [`BackendError::UnknownDevice`], [`BackendError::DeviceNotAvailable`] or [`BackendError::Unsupported`].
    fn open_output(
        &self,
        id: &DeviceId,
        config: StreamConfig,
        on_data: OutputCallback,
        on_error: ErrorCallback,
    ) -> Result<Box<dyn DeviceStream>, BackendError>;
}
