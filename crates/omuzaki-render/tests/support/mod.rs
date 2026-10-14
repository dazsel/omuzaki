//! Shared helpers for the integration tests: a software-rasteriser renderer, so every test is deterministic and none
//! competes with a real GPU.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};

/// A fresh headless renderer on the software adapter (or the one `WGPU_ADAPTER_NAME` names).
pub fn renderer() -> WgpuRenderer {
    WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .expect("a software Vulkan adapter (install mesa-vulkan-drivers) or WGPU_ADAPTER_NAME")
}
