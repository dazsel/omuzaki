//! Command line flags shared by the `window`, `caps` and `cost` binaries.

use clap::{Args, ValueEnum};

use crate::alpha::Output;
use crate::app::{Config, Mode};
use crate::gpu;
use crate::platform::{self, ClickThrough, HitTestBackend, Level};

/// Default window size, physical pixels: wide enough for the picture's tag rows and stamp column.
pub const DEFAULT_SIZE: &str = "1280x720";

/// The wgpu backend to use.
#[derive(Clone, Copy, ValueEnum)]
pub enum BackendArg {
    /// Every backend this build has; wgpu picks.
    Auto,
    /// Vulkan.
    Vulkan,
    /// Direct3D 12 (Windows).
    Dx12,
    /// Metal (macOS).
    Metal,
    /// OpenGL / GLES.
    Gl,
}

/// The composite alpha mode to configure the surface with.
#[derive(Clone, Copy, ValueEnum)]
pub enum AlphaArg {
    /// The best mode the surface offers.
    Auto,
    /// `PreMultiplied`.
    Premultiplied,
    /// `PostMultiplied`.
    Postmultiplied,
    /// `Inherit`.
    Inherit,
    /// `Opaque`.
    Opaque,
}

/// What the shader writes.
#[derive(Clone, Copy, ValueEnum)]
pub enum OutputArg {
    /// Follow the alpha mode.
    Auto,
    /// Colour multiplied by alpha.
    Premultiplied,
    /// Colour divided by alpha.
    Straight,
}

/// Who toggles click-through on Windows.
#[derive(Clone, Copy, ValueEnum)]
pub enum HitArg {
    /// winit's `set_cursor_hittest`.
    Winit,
    /// Direct window-style calls.
    Native,
}

/// Stacking level.
#[derive(Clone, Copy, ValueEnum)]
pub enum LevelArg {
    /// An ordinary window.
    Normal,
    /// Above other windows.
    Top,
    /// Below other windows.
    Bottom,
}

/// How much of the window lets the mouse through.
#[derive(Clone, Copy, ValueEnum)]
pub enum PassthroughArg {
    /// The whole window.
    Whole,
    /// Pixels the picture leaves transparent.
    Pixel,
}

/// Windows DX12 swap chain kind.
#[derive(Clone, Copy, ValueEnum)]
pub enum Dx12Arg {
    /// A swap chain on the window's HWND: opaque only.
    Hwnd,
    /// A `DirectComposition` visual wgpu attaches to the window: the only kind with alpha.
    Visual,
}

/// Parses `WIDTHxHEIGHT`.
///
/// # Errors
/// A message for clap when the text is not two positive numbers.
pub fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (w, h) = text.split_once('x').ok_or("expected WIDTHxHEIGHT, for example 1920x1080")?;
    let parse = |s: &str| s.parse::<u32>().ok().filter(|n| *n > 0).ok_or(format!("{s:?} is not a positive number"));
    Ok((parse(w)?, parse(h)?))
}

/// GPU flags.
#[derive(Args)]
pub struct GpuArgs {
    /// wgpu backend to use.
    #[arg(long, value_enum, default_value = "auto")]
    backend: BackendArg,
    /// Composite alpha mode to configure the surface with (auto picks the best offered).
    #[arg(long, value_enum, default_value = "auto")]
    alpha: AlphaArg,
    /// What the shader writes: premultiplied or straight colour (auto follows the alpha mode).
    #[arg(long, value_enum, default_value = "auto")]
    output: OutputArg,
    /// Windows DX12 only: which swap chain kind wgpu builds (hwnd is opaque only).
    #[arg(long, value_enum, default_value = "visual")]
    dx12_swapchain: Dx12Arg,
    /// Use the CPU adapter where the platform has one (lavapipe, WARP).
    #[arg(long)]
    software: bool,
}

impl GpuArgs {
    /// The GPU options. `animate` moves the picture every frame; `timestamps` asks for GPU timing.
    pub fn options(&self, animate: bool, timestamps: bool) -> gpu::Options {
        gpu::Options {
            backends: match self.backend {
                BackendArg::Auto => wgpu::Backends::all(),
                BackendArg::Vulkan => wgpu::Backends::VULKAN,
                BackendArg::Dx12 => wgpu::Backends::DX12,
                BackendArg::Metal => wgpu::Backends::METAL,
                BackendArg::Gl => wgpu::Backends::GL,
            },
            alpha: match self.alpha {
                AlphaArg::Auto => None,
                AlphaArg::Premultiplied => Some(wgpu::CompositeAlphaMode::PreMultiplied),
                AlphaArg::Postmultiplied => Some(wgpu::CompositeAlphaMode::PostMultiplied),
                AlphaArg::Inherit => Some(wgpu::CompositeAlphaMode::Inherit),
                AlphaArg::Opaque => Some(wgpu::CompositeAlphaMode::Opaque),
            },
            output: match self.output {
                OutputArg::Auto => None,
                OutputArg::Premultiplied => Some(Output::Premultiplied),
                OutputArg::Straight => Some(Output::Straight),
            },
            fallback_adapter: self.software,
            dx12_visual: matches!(self.dx12_swapchain, Dx12Arg::Visual),
            animate,
            timestamps,
        }
    }

    /// Whether the DX12 `DirectComposition` visual swap chain was asked for (eframe needs the same choice).
    pub fn dx12_visual(&self) -> bool {
        matches!(self.dx12_swapchain, Dx12Arg::Visual)
    }
}

/// Flags of the `window` binary.
#[derive(Args)]
pub struct WindowArgs {
    /// GPU flags.
    #[command(flatten)]
    pub gpu: GpuArgs,
    /// Stacking level of the window.
    #[arg(long, value_enum, default_value = "top")]
    level: LevelArg,
    /// Hide from the taskbar or Dock and from the window switcher.
    #[arg(long)]
    skip_taskbar: bool,
    /// Click-through: omit for none, `whole` for the whole window, `pixel` where the picture is transparent.
    #[arg(long, value_enum)]
    passthrough: Option<PassthroughArg>,
    /// Windows only: who toggles click-through.
    #[arg(long, value_enum, default_value = "native")]
    hit_test: HitArg,
    /// Window size in physical pixels, `WIDTHxHEIGHT` (at least 1024x256).
    #[arg(long, default_value = DEFAULT_SIZE, value_parser = parse_size)]
    pub size: (u32, u32),
    /// Target frames per second.
    #[arg(long, default_value_t = 30)]
    fps: u32,
    /// Stop after this many seconds (default: run until closed or Esc).
    #[arg(long)]
    seconds: Option<f64>,
    /// Starting opacity, 0 to 1 (Up and Down arrows change it while running).
    #[arg(long, default_value_t = 1.0)]
    opacity: f32,
}

impl WindowArgs {
    /// The run configuration for `mode`.
    pub fn config(&self, mode: Mode) -> Config {
        Config {
            mode,
            gpu: self.gpu.options(false, false),
            platform: platform::Options {
                level: match self.level {
                    LevelArg::Normal => Level::Normal,
                    LevelArg::Top => Level::Top,
                    LevelArg::Bottom => Level::Bottom,
                },
                hide_from_switcher: self.skip_taskbar,
                click_through: match self.passthrough {
                    None => ClickThrough::Off,
                    Some(PassthroughArg::Whole) => ClickThrough::Window,
                    Some(PassthroughArg::Pixel) => ClickThrough::Pixel,
                },
                hit_test: match self.hit_test {
                    HitArg::Winit => HitTestBackend::Winit,
                    HitArg::Native => HitTestBackend::Native,
                },
            },
            size: self.size,
            fps: self.fps,
            seconds: self.seconds,
            opacity: self.opacity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse_and_refuse_nonsense() {
        assert_eq!(parse_size("1920x1080"), Ok((1920, 1080)));
        assert!(parse_size("1920").is_err());
        assert!(parse_size("0x10").is_err());
        assert!(parse_size("axb").is_err());
    }
}
