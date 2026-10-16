//! The one way the GPU tests, benches and examples open a wgpu instance (`cargo xtask lint-gpu-instances` enforces it).
//!
//! Why not wgpu's default (every backend, GL included): on WSL with NVIDIA's D3D12 driver, starting GL makes Mesa's EGL
//! load that driver, and a thread-exit destructor it registers outlives the library, so the process dies with SIGSEGV
//! at exit after every test has passed. The product opens `Backends::PRIMARY` (`omuzaki-render`'s `gpu.rs`); this does
//! the same, and `WGPU_BACKEND` can still opt in to GL.
//!
//! Include it with `#[path = "..."] mod gpu_instance;`: the test-support crate is an App crate that impl crates may not
//! depend on (ADR 0054), so one source file is shared instead of one copy per crate.

#![allow(dead_code)]

/// A headless instance on the product's backends (`WGPU_BACKEND` overrides).
pub fn instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::PRIMARY;
    wgpu::Instance::new(descriptor.with_env())
}
