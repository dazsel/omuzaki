//! The design-system gallery: every component in every state, in the theme you pick.
//!
//! ```text
//! gallery                                   # a window
//! gallery --theme light --scale 125         # start in a theme and UI scale
//! gallery --screenshot out/dark.png         # render one image without a window and exit
//! gallery --screenshots out/                # dark.png, light.png and high-contrast.png
//! gallery --pseudo                          # the pseudo-locale: accented, 40% longer text (also with --screenshot)
//! gallery --bench 300                       # measure the UI frame time (layout + tessellation) and exit
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use omuzaki_ui::ds::gallery::{Gallery, bench};
use omuzaki_ui::ds::{Density, Scale, Settings, ThemeKind};

#[derive(Clone, Copy, ValueEnum)]
enum ThemeArg {
    Dark,
    Light,
    HighContrast,
}

impl From<ThemeArg> for ThemeKind {
    fn from(arg: ThemeArg) -> Self {
        match arg {
            ThemeArg::Dark => Self::Dark,
            ThemeArg::Light => Self::Light,
            ThemeArg::HighContrast => Self::HighContrast,
        }
    }
}

#[derive(Parser)]
#[command(about = "The Omuzaki design-system gallery")]
struct Args {
    /// The palette.
    #[arg(long, value_enum, default_value = "dark")]
    theme: ThemeArg,
    /// UI scale in percent (75 to 200).
    #[arg(long, default_value_t = 100)]
    scale: u16,
    /// Compact rows.
    #[arg(long)]
    compact: bool,
    /// Speak the pseudo-locale (accented, 40% longer): finds hard-coded text and layouts that cannot take longer text.
    #[arg(long)]
    pseudo: bool,
    /// Render one screenshot of the whole gallery to this PNG and exit.
    #[arg(long)]
    screenshot: Option<PathBuf>,
    /// Render all three themes into this directory and exit.
    #[arg(long)]
    screenshots: Option<PathBuf>,
    /// Render the colour field with its popover open to this PNG and exit.
    #[arg(long)]
    popover: Option<PathBuf>,
    /// Screenshot size in points: WIDTH,HEIGHT. A height of 0 fits the whole gallery.
    #[arg(long, default_value = "1680,0", value_parser = parse_size)]
    size: (f32, f32),
    /// Screenshot pixels per point.
    #[arg(long, default_value_t = 1.0)]
    ppp: f32,
    /// Measure this many frames of the gallery (headless) and print the timings.
    #[arg(long)]
    bench: Option<u32>,
}

fn parse_size(text: &str) -> Result<(f32, f32), String> {
    let (w, h) = text.split_once(',').ok_or("expected WIDTH,HEIGHT")?;
    Ok((w.trim().parse().map_err(|e| format!("{e}"))?, h.trim().parse().map_err(|e| format!("{e}"))?))
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn settings(args: &Args) -> Settings {
    Settings {
        theme: args.theme.into(),
        scale: Scale::nearest(args.scale),
        density: if args.compact { Density::Compact } else { Density::Comfortable },
        reduce_motion: false,
    }
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(frames) = args.bench {
        return bench::run(settings(args), frames);
    }
    #[cfg(feature = "capture")]
    if args.screenshot.is_some() || args.screenshots.is_some() || args.popover.is_some() {
        return screenshots(args);
    }
    #[cfg(not(feature = "capture"))]
    if args.screenshot.is_some() || args.screenshots.is_some() || args.popover.is_some() {
        return Err("built without the `capture` feature: no headless rendering".into());
    }
    window(settings(args), args.pseudo)
}

#[cfg(feature = "capture")]
fn screenshots(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    use omuzaki_ui::ds::gallery::capture::{Shot, render};
    let shot = |settings| Shot {
        settings,
        size: egui::vec2(args.size.0, args.size.1),
        pixels_per_point: args.ppp,
        pseudo: args.pseudo,
    };
    let save = |path: &std::path::Path, image: image::RgbaImage| -> Result<(), Box<dyn std::error::Error>> {
        image.save(path)?;
        println!("wrote {} ({}x{})", path.display(), image.width(), image.height());
        Ok(())
    };
    if let Some(path) = &args.screenshot {
        save(path, render(shot(settings(args)))?)?;
    }
    if let Some(path) = &args.popover {
        save(path, omuzaki_ui::ds::gallery::capture::colour_popover(settings(args), args.ppp)?)?;
    }
    if let Some(dir) = &args.screenshots {
        std::fs::create_dir_all(dir)?;
        for kind in ThemeKind::ALL {
            let prefix = if args.pseudo { "pseudo-" } else { "" };
            save(
                &dir.join(format!("{prefix}{}.png", kind.slug())),
                render(shot(Settings { theme: kind, ..settings(args) }))?,
            )?;
        }
    }
    Ok(())
}

fn window(settings: Settings, pseudo: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut native = eframe::NativeOptions::default();
    native.viewport = native
        .viewport
        .with_inner_size([1680.0, 1050.0])
        .with_min_inner_size([1280.0, 720.0])
        .with_title("Omuzaki design system");
    eframe::run_native(
        "Omuzaki design system",
        native,
        Box::new(move |cc| {
            // Applying before the first frame lets the bundled fonts activate on the second.
            omuzaki_ui::ds::apply_settings(&cc.egui_ctx, settings);
            Ok(Box::new(App { gallery: Gallery::new(settings).with_pseudo(pseudo) }))
        }),
    )?;
    Ok(())
}

struct App {
    gallery: Gallery,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.gallery.ui(ui);
    }
}
