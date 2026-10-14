//! The egui window: an orbit-camera viewport with light controls and a frame-time readout.

use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use eframe::egui_wgpu;

use crate::camera::OrbitCamera;
use crate::headless::save_png;
use crate::lighting::Lighting;
use crate::model::Model;
use crate::renderer::{FrameParams, RenderOptions, Renderer};
use crate::stats::Summary;

/// Frames of history behind the readout.
const HISTORY: usize = 120;

/// An unattended run for smoke tests: render `frames` frames, optionally save a screenshot of the window, print the
/// timings, exit.
#[derive(Clone, Debug)]
pub struct Smoke {
    pub frames: u32,
    pub screenshot: Option<PathBuf>,
}

/// The viewport application.
pub struct ViewportApp {
    renderer: Renderer,
    camera: OrbitCamera,
    lighting: Lighting,
    preset: &'static str,
    options: RenderOptions,
    texture: Option<(egui::TextureId, u64)>,
    cpu_ms: Vec<f32>,
    frame_ms: Vec<f32>,
    last_frame: Option<Instant>,
    smoke: Option<Smoke>,
    smoke_done: bool,
    frame_count: u32,
    adapter: String,
}

impl ViewportApp {
    /// Builds the app on the GPU eframe opened; `None` when eframe is not running the wgpu renderer.
    #[must_use]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        model: &Model,
        camera: OrbitCamera,
        lighting: Lighting,
        options: RenderOptions,
        smoke: Option<Smoke>,
    ) -> Option<Self> {
        let state = cc.wgpu_render_state.as_ref()?;
        let info = state.adapter.get_info();
        Some(Self {
            renderer: Renderer::new(&state.device, &state.queue, model),
            camera,
            lighting,
            preset: "studio",
            options,
            texture: None,
            cpu_ms: Vec::new(),
            frame_ms: Vec::new(),
            last_frame: None,
            smoke,
            smoke_done: false,
            frame_count: 0,
            adapter: format!("{} ({:?})", info.name, info.backend),
        })
    }

    fn push(history: &mut Vec<f32>, v: f32) {
        if history.len() == HISTORY {
            history.remove(0);
        }
        history.push(v);
    }

    /// Draws the 3D view into the central area and handles the camera gestures.
    fn viewport(&mut self, ui: &mut egui::Ui, state: &egui_wgpu::RenderState) {
        let ppp = ui.ctx().pixels_per_point();
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let height = rect.height();
        if response.dragged_by(egui::PointerButton::Primary) {
            let d = response.drag_delta();
            self.camera.orbit(d.x, d.y, height);
        }
        if response.dragged_by(egui::PointerButton::Secondary) || response.dragged_by(egui::PointerButton::Middle) {
            let d = response.drag_delta();
            self.camera.pan(d.x, d.y, height);
        }
        if response.hovered() {
            self.camera.zoom(ui.input(|i| i.smooth_scroll_delta.y) / 50.0);
        }

        // Pixel counts are small positive numbers, so the casts below cannot truncate or change sign.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let size = ((rect.width() * ppp).round().max(1.0) as u32, (rect.height() * ppp).round().max(1.0) as u32);
        let started = Instant::now();
        self.renderer
            .render(size, &FrameParams { camera: &self.camera, lighting: &self.lighting, options: self.options });
        Self::push(&mut self.cpu_ms, started.elapsed().as_secs_f32() * 1000.0);
        self.renderer.poll_timing();

        let Some(view) = self.renderer.output() else { return };
        let mut egui_renderer = state.renderer.write();
        let generation = self.renderer.generation();
        let id = match self.texture {
            Some((id, g)) if g == generation => id,
            Some((id, _)) => {
                egui_renderer.update_egui_texture_from_wgpu_texture(&state.device, view, wgpu::FilterMode::Linear, id);
                self.texture = Some((id, generation));
                id
            }
            None => {
                let id = egui_renderer.register_native_texture(&state.device, view, wgpu::FilterMode::Linear);
                self.texture = Some((id, generation));
                id
            }
        };
        ui.painter().image(
            id,
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }

    fn readout(&self, ui: &mut egui::Ui) {
        ui.label(format!("GPU: {}", self.adapter));
        for (name, samples) in [("CPU submit", &self.cpu_ms), ("frame interval", &self.frame_ms)] {
            if let Some(s) = Summary::of(samples) {
                ui.label(format!("{name}: {:.2} ms (p95 {:.2})", s.mean, s.p95));
            }
        }
        match self.renderer.gpu_ms() {
            Some(ms) => ui.label(format!("GPU frame: {ms:.3} ms")),
            None => ui.label("GPU frame: no timestamp queries"),
        };
        if let Some(s) = Summary::of(&self.frame_ms) {
            ui.label(format!("{:.0} fps", 1000.0 / s.mean.max(f32::EPSILON)));
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("Light");
        ui.horizontal(|ui| {
            for name in ["studio", "night", "warm"] {
                if let (true, Some(l)) =
                    (ui.selectable_label(self.preset == name, name).clicked(), Lighting::preset(name))
                {
                    self.lighting = l;
                    self.preset = name;
                }
            }
        });
        if let Some(key) = self.lighting.lights.iter_mut().find(|l| l.casts_shadow) {
            let (mut az, mut el) = key.angles();
            let (mut az_deg, mut el_deg) = (az.to_degrees(), el.to_degrees());
            ui.add(egui::Slider::new(&mut az_deg, -180.0..=180.0).text("key azimuth"));
            ui.add(egui::Slider::new(&mut el_deg, 0.0..=89.0).text("key elevation"));
            (az, el) = (az_deg.to_radians(), el_deg.to_radians());
            key.set_angles(az, el);
            ui.add(egui::Slider::new(&mut key.intensity, 0.0..=5.0).text("key intensity"));
        }
        ui.add(egui::Slider::new(&mut self.lighting.hemi_intensity, 0.0..=3.0).text("hemisphere"));
        ui.add(egui::Slider::new(&mut self.lighting.exposure, 0.2..=3.0).text("exposure"));
        ui.separator();
        ui.checkbox(&mut self.options.outlines, "outlines");
        ui.checkbox(&mut self.options.shadows, "shadows");
        ui.checkbox(&mut self.options.ground, "ground shadow");
        ui.separator();
        ui.heading("Camera");
        ui.horizontal(|ui| {
            for (name, yaw) in [("front", 0.0), ("3/4", 35.0), ("side", 90.0), ("back", 180.0)] {
                if ui.button(name).clicked() {
                    self.camera = OrbitCamera::js_view(yaw, 2.4, 0.12, 0.5);
                }
            }
        });
        ui.label("drag: orbit, right drag: pan, wheel: zoom");
        ui.separator();
        ui.heading("Frame time");
        self.readout(ui);
    }

    /// Runs the unattended smoke test: after the requested frames, screenshot the window, print the numbers, close.
    fn smoke_step(&mut self, ctx: &egui::Context) {
        let Some(smoke) = self.smoke.clone() else { return };
        self.frame_count += 1;
        if self.frame_count < smoke.frames || self.smoke_done {
            return;
        }
        if let Some(path) = &smoke.screenshot {
            // The window's own pixels (UI and viewport), delivered as an event on a later frame.
            let shot = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            let Some(image) = shot else {
                if self.frame_count == smoke.frames {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                }
                return;
            };
            let size = (u32::try_from(image.width()).unwrap_or(0), u32::try_from(image.height()).unwrap_or(0));
            match save_png(path, image.as_raw().to_vec(), size) {
                Ok(()) => println!("smoke: wrote {} ({}x{})", path.display(), size.0, size.1),
                Err(e) => eprintln!("smoke: screenshot failed: {e}"),
            }
        }
        self.smoke_done = true;
        println!("smoke: adapter {}", self.adapter);
        for (name, samples) in [("cpu submit", &self.cpu_ms), ("frame interval", &self.frame_ms)] {
            if let Some(s) = Summary::of(samples) {
                println!("smoke: {name}: {s}");
            }
        }
        if let Some(ms) = self.renderer.poll_timing() {
            println!("smoke: gpu frame (last): {ms:.3} ms");
        }
        println!("smoke: frames {}", self.frame_count);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for ViewportApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let now = Instant::now();
        if let Some(last) = self.last_frame.replace(now) {
            Self::push(&mut self.frame_ms, now.duration_since(last).as_secs_f32() * 1000.0);
        }
        let Some(state) = frame.wgpu_render_state().cloned() else { return };
        egui::Panel::left("controls").exact_size(260.0).show(ui, |ui| self.controls(ui));
        egui::CentralPanel::no_frame().show(ui, |ui| self.viewport(ui, &state));
        let ctx = ui.ctx().clone();
        self.smoke_step(&ctx);
        // Keep rendering continuously: the readout is the point of the window.
        ctx.request_repaint();
    }
}
