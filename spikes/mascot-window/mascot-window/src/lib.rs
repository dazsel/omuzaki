//! Spike: a transparent, always-on-top, click-through winit window with a wgpu surface (BR-25 desktop mascot).
//!
//! Four binaries share this library: `caps` (what each wgpu backend offers), `window` (the winit window, alone or on
//! eframe's event loop), `egui-viewport` (the same picture as an egui viewport) and `cost` (the 1080p30 measurement).

// Pixel and window arithmetic: every value is bounded by a window size or the 512 px image, far inside f32's exact range.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_lossless)]

pub mod alpha;
pub mod app;
pub mod cli;
pub mod gpu;
pub mod host;
pub mod platform;
pub mod scene;
pub mod scene_pass;
pub mod stats;
pub mod timing;
