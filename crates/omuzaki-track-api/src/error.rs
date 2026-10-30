//! The one error type of cameras and trackers.

use crate::camera::{CameraFormatRequest, CameraId};

/// Why a camera or tracker call failed. Backends map every OS error to one of these; nothing here panics.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TrackError {
    /// No camera has this id (unplugged, or never there).
    #[error("no camera {0:?}")]
    NoDevice(CameraId),
    /// Another stream, in this process or another app, holds the camera.
    #[error("camera {0:?} is in use")]
    Busy(CameraId),
    /// The camera offers no format that satisfies the request.
    #[error("the camera has no format for {0:?}")]
    FormatUnsupported(CameraFormatRequest),
    /// The OS refused camera access; only the person can grant it, in the OS settings.
    #[error("camera access was denied")]
    PermissionDenied,
    /// The stream ended: the device was unplugged or the driver stopped it. Reopen to recover.
    #[error("the camera stream was lost")]
    Lost,
    /// Anything else the backend reported, as text for the log.
    #[error("camera backend: {0}")]
    Backend(String),
    /// This tracker cannot do what was asked (body tracking without body models, say).
    #[error("not supported by this tracker: {0}")]
    Unsupported(String),
}
