//! The mascot window on eframe's event loop (research page section 6, "shared loop").
//!
//! winit allows one event loop per process, so the mascot cannot start a loop next to eframe's. eframe's
//! `create_native` returns an `ApplicationHandler` for a loop we own; [`Host`] forwards to it and also drives the mascot
//! window, which has its own wgpu surface and is not an egui viewport. The eframe window stays an ordinary opaque
//! control window.

use eframe::UserEvent;
use eframe::egui;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::WindowId;

use crate::app::{App, Config, Outcome, RunError};
use crate::platform;

/// eframe options for the wgpu renderer, with the DX12 swap chain kind chosen the way the mascot's own surface is.
pub fn native_options(viewport: egui::ViewportBuilder, dx12_visual: bool) -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions { viewport, renderer: eframe::Renderer::Wgpu, ..Default::default() };
    if dx12_visual && let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.instance_descriptor.backend_options.dx12.presentation_system =
            eframe::wgpu::Dx12SwapchainKind::DxgiFromVisual;
    }
    options
}

/// The ordinary window the mascot shares its loop with.
struct Control;

impl eframe::App for Control {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.label("eframe control window (opaque). The mascot is a separate winit window on this event loop.");
        ui.label("Esc in the mascot window, or closing either window, ends the run.");
    }
}

struct Host<'a> {
    eframe: eframe::EframeWinitApplication<'a>,
    mascot: App,
}

impl ApplicationHandler<UserEvent> for Host<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.resumed(event_loop);
        self.mascot.resumed(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.mascot.owns(id) {
            self.mascot.window_event(event_loop, event);
        } else {
            self.eframe.window_event(event_loop, id, event);
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.eframe.new_events(event_loop, cause);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        self.eframe.user_event(event_loop, event);
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, id: DeviceId, event: DeviceEvent) {
        self.eframe.device_event(event_loop, id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.about_to_wait(event_loop);
        self.mascot.tick(event_loop, true);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.exiting(event_loop);
    }
}

/// Runs the mascot window and an eframe control window on one event loop.
///
/// # Errors
/// See [`RunError`].
pub fn run_shared(config: Config, dx12_visual: bool) -> Result<Outcome, RunError> {
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    platform::configure_event_loop(&mut builder, &config.platform);
    let event_loop = builder.build()?;
    let viewport =
        egui::ViewportBuilder::default().with_title("Omuzaki eframe control window").with_inner_size([520.0, 90.0]);
    let eframe = eframe::create_native(
        "Omuzaki mascot window spike",
        native_options(viewport, dx12_visual),
        Box::new(|_cc| Ok(Box::new(Control))),
        &event_loop,
    );
    let mut host = Host { eframe, mascot: App::new(config) };
    event_loop.run_app(&mut host)?;
    host.mascot.finish()
}
