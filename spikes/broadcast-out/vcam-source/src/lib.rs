//! Spike: a Media Foundation virtual camera source.
//!
//! `format` and `pattern` are plain Rust (tested on any platform). The COM side (`source`, `stream`, `registry`) builds only
//! for Windows: it is the in-process COM server that Windows' Frame Server loads for a camera made with
//! `MFCreateVirtualCamera`, and it is also built as `omuzaki_vcam_source.dll`.

pub mod format;
pub mod pattern;

#[cfg(windows)]
mod com;
#[cfg(windows)]
pub mod env;
#[cfg(windows)]
pub mod source;
#[cfg(windows)]
mod stream;
