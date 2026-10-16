//! Shared helpers for the integration tests: a software-rasteriser renderer, so every test is deterministic and none
//! competes with a real GPU.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};

use std::fmt;
use std::path::PathBuf;

/// How a picture differs from its golden, in 8-bit levels per channel. Printed with every golden failure, so a mismatch
/// on a new adapter arrives with the numbers a tolerance has to be chosen from (and says whether the difference is noise
/// or a structured error).
pub struct Diff {
    /// Pixels where any channel differs by more than 3 levels.
    pub beyond_3: usize,
    /// Pixels where any channel differs at all.
    pub any: usize,
    /// The pixel count of the picture.
    pub pixels: usize,
    /// The largest difference in any channel.
    pub max: u8,
    /// The 99th percentile of the per-channel differences.
    pub p99: u8,
    /// The mean per-channel difference.
    pub mean: f64,
}

impl Diff {
    /// Compares two RGBA8 buffers of the same size.
    #[allow(clippy::cast_precision_loss)] // a picture's sample count and level total are far below 2^52
    pub fn of(golden: &[u8], actual: &[u8]) -> Self {
        let mut histogram = [0_usize; 256];
        let (mut beyond_3, mut any) = (0, 0);
        for (a, b) in golden.chunks(4).zip(actual.chunks(4)) {
            let worst = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
            beyond_3 += usize::from(worst > 3);
            any += usize::from(worst > 0);
        }
        let mut total = 0_u64;
        for (x, y) in golden.iter().zip(actual) {
            let d = x.abs_diff(*y);
            histogram[usize::from(d)] += 1;
            total += u64::from(d);
        }
        let samples = golden.len().min(actual.len()).max(1);
        let max = (0..=255_u8).rev().find(|&d| histogram[usize::from(d)] > 0).unwrap_or(0);
        let (mut seen, mut p99) = (0, 0);
        for (level, count) in histogram.iter().enumerate() {
            seen += count;
            if seen * 100 >= samples * 99 {
                p99 = u8::try_from(level).unwrap_or(u8::MAX);
                break;
            }
        }
        Self { beyond_3, any, pixels: samples / 4, max, p99, mean: total as f64 / samples as f64 }
    }
}

impl fmt::Display for Diff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} of {} pixels differ, {} by more than 3 levels; per channel max {}, p99 {}, mean {:.3}",
            self.any, self.pixels, self.beyond_3, self.max, self.p99, self.mean
        )
    }
}

/// Writes `rgba` as `name` into the folder `OMUZAKI_RENDER_OUT` names, if it is set: CI keeps that folder as an artifact,
/// so a failing golden on a platform nobody can run locally can be looked at (and, if right, blessed from).
pub fn keep(name: &str, size: (u32, u32), rgba: &[u8]) {
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        image::save_buffer(PathBuf::from(dir).join(name), rgba, size.0, size.1, image::ColorType::Rgba8).unwrap();
    }
}

/// A fresh headless renderer on the software adapter (or the one `WGPU_ADAPTER_NAME` names).
pub fn renderer() -> WgpuRenderer {
    WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .expect("a software Vulkan adapter (install mesa-vulkan-drivers) or WGPU_ADAPTER_NAME")
}
