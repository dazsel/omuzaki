//! Shared by the GPU-rendering tests: the headless egui renderer's wgpu setup on the product's backends.
//!
//! Why: `egui_kittest`'s default setup starts every wgpu backend, GL included. On WSL with NVIDIA's `D3D12` driver, starting
//! GL loads that driver through Mesa's EGL, and a thread-exit destructor it registers outlives the library, so the
//! process dies with SIGSEGV at exit after the test has passed (#2232). The product opens `Backends::PRIMARY`; so does
//! this, and it keeps kittest's own software-first adapter choice.

use eframe::egui_wgpu::{WgpuSetup, wgpu::Backends};

/// kittest's default setup, restricted to `Backends::PRIMARY`.
pub fn wgpu_setup() -> WgpuSetup {
    let mut setup = egui_kittest::wgpu::default_wgpu_setup();
    if let WgpuSetup::CreateNew(create) = &mut setup {
        create.instance_descriptor.backends = Backends::PRIMARY;
    }
    setup
}

/// An egui render state on that setup: the device, queue and renderer the app shares with the stage when the stage
/// draws into egui's own textures (`NativePresenter`). Fresh, so a `WgpuTestRenderer` can take it.
#[allow(dead_code)] // the tests that draw into egui's own textures use it; the others don't
pub fn render_state() -> eframe::egui_wgpu::RenderState {
    egui_kittest::wgpu::create_render_state(wgpu_setup(), eframe::egui_wgpu::RendererOptions::PREDICTABLE)
}
