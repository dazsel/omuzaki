//! Spike: a transparent, always-on-top, click-through winit window with a wgpu surface (BR-25 desktop mascot).
//!
//! The binary `mascot-winit` is the whole spike; this library holds the pieces so they can be unit tested and shared
//! with `mascot-egui`.

// Pixel and window arithmetic: every value is bounded by a window size or the 512 px image, far inside f32's exact range.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_lossless)]

pub mod alpha;
pub mod app;
pub mod gpu;
pub mod platform;
pub mod scene;
pub mod stats;
