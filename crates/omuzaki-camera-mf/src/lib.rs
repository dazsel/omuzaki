//! The Windows Media Foundation camera behind [`omuzaki_track_api::camera::Camera`] (PF-01).
//!
//! Frames are handed over raw: the source reader runs with video processing and converters off, so the tracker
//! converts them itself (`omuzaki-pixfmt`). [`modes`] holds the platform-neutral tables (mode choice, subtype and
//! `HRESULT` mapping) and builds everywhere; `MfCamera` needs Media Foundation and exists on Windows only.

pub mod modes;
#[cfg(windows)]
pub mod reader;
