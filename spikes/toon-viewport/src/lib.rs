//! A wgpu viewport that renders an exported character GLB with a port of the JS prototype's toon shading.
//!
//! This spike checks that the same renderer drives an egui window ([`app`]) and a headless mode
//! ([`headless`]) that writes screenshots and measures frame times, so the evidence images and numbers come from the
//! exact code the window shows.

pub mod app;
pub mod camera;
pub mod gpu;
pub mod headless;
pub mod lighting;
pub mod model;
pub mod renderer;
pub mod stats;
mod texture;
mod timing;
