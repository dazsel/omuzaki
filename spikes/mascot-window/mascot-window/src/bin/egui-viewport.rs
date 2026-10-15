//! `egui-viewport`: the same transparent picture hosted by eframe, with a transparent root or a transparent secondary
//! viewport, to answer whether egui's multi-viewport can host the desktop mascot or whether it needs its own window and
//! surface (research page section 9, item 3).

// Pixel arithmetic on window and image sizes, far inside the exact range of f32 and u32.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use eframe::egui::{
    self, Color32, ColorImage, Sense, TextureHandle, TextureOptions, ViewportBuilder, ViewportCommand, ViewportId,
};
use mascot_window::cli::{DEFAULT_SIZE, Dx12Arg, LevelArg, parse_size};
use mascot_window::host::native_options;
use mascot_window::scene::Fit;
use mascot_window::scene_pass;

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Mode {
    /// The transparent window is eframe's root viewport.
    Root,
    /// The root is an ordinary control window; the mascot is a deferred child viewport.
    Child,
}

// One bool per command line flag: a struct of switches is what a CLI is.
#[allow(clippy::struct_excessive_bools)]
#[derive(Parser)]
#[command(
    about = "The mascot as an egui (eframe, wgpu) viewport; run with RUST_LOG=warn to see egui-wgpu's alpha warnings"
)]
struct Args {
    /// Which viewport is the mascot.
    #[arg(long, value_enum, default_value = "root")]
    mode: Mode,
    /// Child mode only: also make the root window transparent. egui-wgpu picks its alpha mode from the root's flag.
    #[arg(long)]
    root_transparent: bool,
    /// Make the mascot viewport click-through as a whole (`ViewportBuilder::with_mouse_passthrough`).
    #[arg(long)]
    passthrough: bool,
    /// Hide the mascot from the taskbar (`ViewportBuilder::with_taskbar(false)`; egui-winit applies it on Windows only).
    #[arg(long)]
    skip_taskbar: bool,
    /// Stacking level of the mascot viewport.
    #[arg(long, value_enum, default_value = "top")]
    level: LevelArg,
    /// Windows DX12 only: which swap chain kind wgpu builds (hwnd is opaque only).
    #[arg(long, value_enum, default_value = "visual")]
    dx12_swapchain: Dx12Arg,
    /// Size of the mascot viewport in points and of the picture in pixels.
    #[arg(long, default_value = DEFAULT_SIZE, value_parser = parse_size)]
    size: (u32, u32),
    /// Mascot opacity, 0 to 1.
    #[arg(long, default_value_t = 1.0)]
    opacity: f32,
}

fn mascot_viewport(args: &Args) -> ViewportBuilder {
    ViewportBuilder::default()
        .with_title("Omuzaki mascot egui spike")
        .with_inner_size([args.size.0 as f32, args.size.1 as f32])
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(false)
        .with_window_level(match args.level {
            LevelArg::Normal => egui::WindowLevel::Normal,
            LevelArg::Top => egui::WindowLevel::AlwaysOnTop,
            LevelArg::Bottom => egui::WindowLevel::AlwaysOnBottom,
        })
        .with_taskbar(!args.skip_taskbar)
        .with_mouse_passthrough(args.passthrough)
}

/// Draws the mascot into `ui`, fitted to its area, and lets the user drag the window by it.
fn paint_mascot(ui: &mut egui::Ui, texture: &TextureHandle, opacity: f32) {
    let area = ui.max_rect();
    let fit =
        Fit::new((area.width() as u32, area.height() as u32), (texture.size()[0] as u32, texture.size()[1] as u32));
    let side = fit.image.0 as f32 * fit.scale;
    let rect = egui::Rect::from_min_size(
        area.min + egui::vec2(fit.origin.0, fit.origin.1),
        egui::vec2(side, fit.image.1 as f32 * fit.scale),
    );
    // A white tint with alpha scales every channel of the premultiplied image, which keeps it premultiplied.
    let tint = Color32::from_white_alpha((opacity.clamp(0.0, 1.0) * 255.0).round() as u8);
    ui.painter().image(texture.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
    if ui.interact(rect, ui.id().with("drag"), Sense::drag()).drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
    }
}

struct MascotApp {
    args: Args,
    texture: Option<TextureHandle>,
}

impl MascotApp {
    fn texture(&mut self, ctx: &egui::Context) -> TextureHandle {
        self.texture
            .get_or_insert_with(|| {
                // The picture is drawn once on a device of its own and uploaded as an egui texture.
                let scene =
                    scene_pass::render_reference(wgpu::Backends::all(), false, self.args.size).unwrap_or_else(|e| {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    });
                let size = [scene.width as usize, scene.height as usize];
                ctx.load_texture(
                    "mascot",
                    ColorImage::from_rgba_premultiplied(size, &scene.pixels),
                    TextureOptions::LINEAR,
                )
            })
            .clone()
    }
}

impl eframe::App for MascotApp {
    /// Fully transparent: whatever the panels do not cover shows the desktop.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let texture = self.texture(ui.ctx());
        let opacity = self.args.opacity;
        match self.args.mode {
            Mode::Root => paint_mascot(ui, &texture, opacity),
            Mode::Child => {
                ui.label("Control window. The mascot is a separate deferred viewport.");
                ui.label(if self.args.root_transparent {
                    "Root viewport: transparent"
                } else {
                    "Root viewport: opaque"
                });
                let builder = mascot_viewport(&self.args);
                ui.ctx().show_viewport_deferred(ViewportId::from_hash_of("mascot"), builder, move |ui, _class| {
                    paint_mascot(ui, &texture, opacity);
                });
            }
        }
    }
}

fn main() -> ExitCode {
    env_logger::init();
    let args = Args::parse();
    let mut viewport = match args.mode {
        Mode::Root => mascot_viewport(&args),
        Mode::Child => {
            ViewportBuilder::default().with_inner_size([420.0, 120.0]).with_transparent(args.root_transparent)
        }
    };
    // egui-wgpu chooses PreMultiplied, then PostMultiplied, from this flag of the root viewport only.
    viewport.transparent = Some(args.mode == Mode::Root || args.root_transparent);
    let options = native_options(viewport, matches!(args.dx12_swapchain, Dx12Arg::Visual));
    match eframe::run_native(
        "Omuzaki mascot egui spike",
        options,
        Box::new(|_cc| Ok(Box::new(MascotApp { args, texture: None }))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
