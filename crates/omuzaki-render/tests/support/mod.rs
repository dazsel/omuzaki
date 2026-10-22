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

/// A headless wgpu instance on the product's backends (`Backends::PRIMARY`; `WGPU_BACKEND` can opt in to GL), the same
/// choice `Gpu::open_headless` makes. Tests that need a raw adapter open it here; wgpu's default (every backend) starts
/// GL, which on WSL with NVIDIA's D3D12 driver crashes the process at exit (`cargo test -p omuzaki-test --test gpu_instances` enforces this).
pub fn instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::PRIMARY;
    wgpu::Instance::new(descriptor.with_env())
}
