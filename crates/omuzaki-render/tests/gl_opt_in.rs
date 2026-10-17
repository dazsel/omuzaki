//! `WGPU_BACKEND=gl` still reaches a GL adapter: the instance starts only the backends the environment names, and the
//! adapter choice must not filter them out again.
//!
//! GL is opt-in because starting it crashes the process at exit on WSL with NVIDIA's D3D12 driver, so this test runs only
//! when the caller opts in: `WGPU_BACKEND=gl cargo test -p omuzaki-render --test gl_opt_in`. Without that, or when the
//! machine has no GL software adapter, it prints why and passes. The named-adapter path (`WGPU_ADAPTER_NAME`) enumerates
//! through the same call, so it is covered with the software one.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};

#[test]
fn a_gl_opt_in_still_finds_a_software_gl_adapter() {
    let opted_in = std::env::var("WGPU_BACKEND").is_ok_and(|v| v.to_lowercase().split(',').any(|b| b.trim() == "gl"));
    if !opted_in {
        eprintln!("skipped: GL is opt-in; run with WGPU_BACKEND=gl to check that its adapters are still found");
        return;
    }
    // Whether this machine has a GL software adapter at all, asked of a GL-only instance (only reached when opted in).
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::GL;
    let present = pollster::block_on(wgpu::Instance::new(descriptor).enumerate_adapters(wgpu::Backends::GL))
        .iter()
        .any(|a| a.get_info().device_type == wgpu::DeviceType::Cpu);
    if !present {
        eprintln!("skipped: this machine has no GL software adapter");
        return;
    }
    let _renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .expect("WGPU_BACKEND=gl is set and a GL software adapter exists, so the product's opener must find it");
    eprintln!("found a software adapter under WGPU_BACKEND=gl");
}
