//! The five bangs presets: whole `BangsGen` values, in the order `hair.bangs.list_presets` shows them.
//!
//! The table is `pub` so the preset op and the preset renders read the same values. Heights are head-local, as in
//! `BangLayer::tip_heights`: `0` is the brow line, negative falls toward the eyes.

use omuzaki_char_api::{BangLayer, BangsGen, Wisp};

use super::access::WIDTH;

/// One preset: its id, a line saying what it looks like, and how to make its `BangsGen`.
#[derive(Clone, Copy, Debug)]
pub struct BangsPreset {
    /// The id `hair.bangs.preset` takes.
    pub id: &'static str,
    /// What the fringe looks like, in one line.
    pub summary: &'static str,
    /// Makes the preset's generator parameters.
    pub make: fn() -> BangsGen,
}

/// Every preset, in listing order.
pub const PRESETS: [BangsPreset; 5] = [
    BangsPreset { id: "blunt", summary: "A straight-cut fringe across the brow, two even layers.", make: blunt },
    BangsPreset {
        id: "side-swept",
        summary: "A fringe swept to the character's right, tips shorter toward the left.",
        make: side_swept,
    },
    BangsPreset {
        id: "curtain",
        summary: "A centre-parted fringe falling to both sides, tips hooked outward and shorter at the temples.",
        make: curtain,
    },
    BangsPreset {
        id: "wispy",
        summary: "Three light layers of thin, uneven locks and a few loose wisps between the eyes.",
        make: wispy,
    },
    BangsPreset {
        id: "none",
        summary: "No visible fringe: the one lock the generator needs, tucked above the hairline. Disable the group \
                  with part.set_param to drop it entirely.",
        make: none,
    },
];

/// The preset called `id`.
#[must_use]
pub fn find(id: &str) -> Option<&'static BangsPreset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

/// A layer of `count` clumps over `spread_deg`, with the tips of `tip_heights` and the rest as default.
fn layer(count: u8, spread_deg: f64, tip_heights: &[f64], shade: f64) -> BangLayer {
    BangLayer { count, spread_deg, tip_heights: tip_heights.to_vec(), shade, ..BangLayer::default() }
}

fn blunt() -> BangsGen {
    let under = BangLayer { hook_deg: 0.0, ..layer(11, 46.0, &[0.02], 0.8) };
    let top = BangLayer { hook_deg: 0.0, ..layer(9, 42.0, &[0.035], 1.0) };
    BangsGen { layers: vec![under, top], wisps: Vec::new(), sweep_deg: 0.0 }
}

fn side_swept() -> BangsGen {
    let under = BangLayer { hook_deg: 5.0, ..layer(10, 46.0, &[0.03, 0.02, 0.01, 0.0, -0.01, -0.02], 0.8) };
    let top = BangLayer { hook_deg: 8.0, ..layer(8, 40.0, &[0.045, 0.03, 0.015, 0.0, -0.015, -0.03], 1.0) };
    BangsGen { layers: vec![under, top], wisps: Vec::new(), sweep_deg: 24.0 }
}

fn curtain() -> BangsGen {
    let edges = [0.05, 0.035, 0.02, 0.01, 0.01, 0.02, 0.035, 0.05];
    let under = BangLayer { hook_deg: 10.0, ..layer(10, 48.0, &edges, 0.8) };
    let top = BangLayer { hook_deg: 14.0, ..layer(8, 44.0, &edges, 1.0) };
    BangsGen { layers: vec![under, top], wisps: Vec::new(), sweep_deg: 0.0 }
}

fn wispy() -> BangsGen {
    let thin = |count, spread, heights: &[f64], shade| BangLayer { width: 0.04, ..layer(count, spread, heights, shade) };
    let layers = vec![
        BangLayer { hook_deg: 4.0, ..thin(12, 48.0, &[0.02, 0.0, 0.03, -0.01], 0.7) },
        BangLayer { hook_deg: 6.0, ..thin(10, 44.0, &[0.04, 0.01, 0.035, 0.0, 0.02], 0.85) },
        BangLayer { hook_deg: 8.0, ..thin(8, 38.0, &[0.05, 0.02, 0.045, 0.01], 1.0) },
    ];
    let wisps = vec![
        Wisp { phi_deg: -14.0, tip_height: -0.05, width: 0.02 },
        Wisp { phi_deg: 4.0, tip_height: -0.07, width: 0.016 },
        Wisp { phi_deg: 18.0, tip_height: -0.04, width: 0.02 },
    ];
    BangsGen { layers, wisps, sweep_deg: 8.0 }
}

fn none() -> BangsGen {
    // The generator needs one lock (`layers` is never empty), so "none" is the thinnest one (the lowest width
    // `access::check` accepts), tipped far above the brow.
    let hidden = BangLayer { width: WIDTH.0, hook_deg: 0.0, ..layer(1, 5.0, &[0.4], 1.0) };
    BangsGen { layers: vec![hidden], wisps: Vec::new(), sweep_deg: 0.0 }
}
