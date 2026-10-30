//! The strand layer of the hair system (#2451, #2481, #2482): what turns a lock from a flat ribbon into hair. Every
//! lock, the cap and the curtain's strips read the strand texture as the hair material's base texture, multiplied into
//! the `hair` program's colours, with the material cut at its alpha ([`CUTOFF`]).
//!
//! **Across** (`u`, one lock's width, repeating, so a lock offset by its seed shows its own strands) the texture is
//! [`Strands::count`] strands, each of its own shade and its own coverage (`0.55..=1`), with a soft dark gap between
//! neighbours ([`Strands::gap`]) and a finer grain inside each ([`Strands::grain`]). **Along** (`v`, root at 0) the body
//! of the lock is the first `1 - `[`TIP`] of it, the strands drifting gently sideways; the last [`TIP`] is the tip,
//! where each strand ends at its own length, narrowing to a point, so a lock ends in a fringe of fine points instead of
//! one hard edge.
//!
//! **The map** ([`StrandMap`]) puts a lock on the texture: its last [`StrandMap::tip_length`] onto the tip band, and
//! its sides' coverage down by [`StrandMap::feather`] through the vertex colour's alpha, so the fainter strands drop
//! out toward a lock's sides and the side reads as strands rather than a cut edge.
//!
//! It is generated, the same every time, from nothing but integer hashes: no image is sampled.

use std::sync::{Arc, Mutex};

use omuzaki_char_api::{HairShading, TextureData};
use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{Filter, Sampler, TextureDesc, Wrap};

/// Pixels across one lock.
pub const WIDTH: u32 = 256;
/// Pixels from root to tip.
pub const HEIGHT: u32 = 512;
/// The share of the texture, at its end, given to the tips.
pub const TIP: f32 = 0.25;
/// The alpha below which the hair is cut away.
pub const CUTOFF: f32 = 0.5;
/// The most of a lock's length its tip takes.
pub const TIP_SHARE: f32 = 0.15;
/// The last `v` a lock reaches: two rows short of the end, so the repeating sampler never blends the tip into the root.
#[allow(clippy::cast_precision_loss)] // 512
pub const V_END: f32 = 1.0 - 2.0 / HEIGHT as f32;
/// The lowest coverage a strand has in the body of the texture: above [`CUTOFF`], so a lock's middle keeps every strand.
const LEAST: f32 = 0.55;
/// Textures kept, by their parameters.
const KEPT: usize = 8;

/// What the strand texture looks like: the texture's own parameters.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Strands {
    /// Strands across one lock.
    pub count: u32,
    /// How much darker the gap between neighbouring strands is, `0..=0.6`.
    pub gap: f32,
    /// The fine grain inside each strand, `0..=0.3`.
    pub grain: f32,
}

impl Default for Strands {
    fn default() -> Self {
        Self::of(&HairShading::default())
    }
}

impl Strands {
    /// The strands `shading` asks for.
    #[allow(clippy::cast_possible_truncation)] // the shading's ranges are small
    pub fn of(shading: &HairShading) -> Self {
        Self {
            count: shading.strands.clamp(1, 256),
            gap: shading.strand_gap as f32,
            grain: shading.strand_grain as f32,
        }
    }

    /// The strand texture of these parameters, generated once and kept.
    pub fn texture(&self) -> TextureData {
        static KEEP: Mutex<Vec<(Strands, Arc<TextureData>)>> = Mutex::new(Vec::new());
        let Ok(mut kept) = KEEP.lock() else { return generate(self) };
        if let Some((_, texture)) = kept.iter().find(|(s, _)| s == self) {
            return texture.as_ref().clone();
        }
        let texture = Arc::new(generate(self));
        if kept.len() >= KEPT {
            kept.remove(0);
        }
        kept.push((*self, texture.clone()));
        texture.as_ref().clone()
    }
}

/// How a lock is laid on the texture.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StrandMap {
    /// How long a lock's frayed tip is, metres: its last this-much maps onto the tip band.
    pub tip_length: f32,
    /// How much a lock's sides lose of their coverage, `0..=0.8`.
    pub feather: f32,
}

impl Default for StrandMap {
    fn default() -> Self {
        Self::of(&HairShading::default())
    }
}

impl StrandMap {
    /// The map `shading` asks for.
    #[allow(clippy::cast_possible_truncation)] // small, checked ranges
    pub fn of(shading: &HairShading) -> Self {
        Self { tip_length: shading.tip_length as f32, feather: shading.edge_feather as f32 }
    }

    /// The texture `v` of a point `along` metres down a lock `length` long: the body of the lock spread over the body
    /// of the texture, and its last [`StrandMap::tip_length`] (or [`TIP_SHARE`] of a short lock) over the tip band.
    pub fn v(&self, along: f32, length: f32) -> f32 {
        if length <= 0.0 {
            return 0.0;
        }
        let tip = self.tip_length.max(1e-4).min(TIP_SHARE * length);
        let body = length - tip;
        let v = if along < body {
            (1.0 - TIP) * along / body.max(f32::EPSILON)
        } else {
            (1.0 - TIP) + TIP * ((along - body) / tip.max(f32::EPSILON)).min(1.0)
        };
        v.clamp(0.0, 1.0) * V_END
    }

    /// The coverage of a lock at `across` (`-1..=1`, its sides at the ends): whole over its middle, falling to
    /// `1 - feather` at its sides. The vertex colour's alpha carries it.
    pub fn coverage(&self, across: f32) -> f32 {
        let side = smoothstep(0.2, 1.0, across.abs());
        1.0 - self.feather.clamp(0.0, 1.0) * side * side
    }
}

/// The `u` offset of a lock with strand seed `seed`, so neighbouring locks show different strands.
pub fn offset(seed: f32) -> f32 {
    (seed * 7.31).fract().abs()
}

/// The description the texture is uploaded with: sRGB, mip-mapped, repeating, sharpened at a slant.
pub fn desc() -> TextureDesc {
    let mut desc = TextureDesc::colour(UVec2::new(WIDTH, HEIGHT));
    desc.sampler = Sampler { filter: Filter::Linear, wrap: Wrap::Repeat, anisotropy: 4 };
    desc
}

/// A hash of `k` and `salt` in `0..1`.
fn hash(k: u32, salt: u32) -> f32 {
    let mut x = u64::from(k) ^ (u64::from(salt) << 32) ^ 0x9e37_79b9_7f4a_7c15;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    #[allow(clippy::cast_precision_loss)] // 24 bits of it
    let unit = (x >> 40) as f32 / (1u64 << 24) as f32;
    unit
}

/// Smooth periodic value noise over `cells` cells per unit of `x`.
fn noise(x: f32, cells: u32, salt: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)] // a few hundred cells
    let at = x.rem_euclid(1.0) * cells as f32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // in 0..cells
    let i = (at.floor() as u32) % cells;
    let f = at.fract();
    let f = f * f * (3.0 - 2.0 * f);
    hash(i, salt) * (1.0 - f) + hash((i + 1) % cells, salt) * f
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Linear light to an sRGB byte.
fn srgb(linear: f32) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to a byte
    let byte = (s * 255.0).round() as u8;
    byte
}

/// One pixel of `strands`: the grey the hair's colours are multiplied by, and the coverage.
#[allow(clippy::cast_precision_loss, clippy::many_single_char_names)] // pixel and strand indices; texture maths
fn pixel(strands: &Strands, x: u32, y: u32) -> (f32, f32) {
    let (u, v) = ((x as f32 + 0.5) / WIDTH as f32, (y as f32 + 0.5) / HEIGHT as f32);
    let count = strands.count.max(1);
    let n = count as f32;
    // The strands drift a little sideways down the lock, each band of them its own way.
    let drift = 0.18 / n * (std::f32::consts::TAU * (v * 2.0 + noise(u, 3, 7))).sin();
    let at = (u + drift).rem_euclid(1.0) * n;
    // Each strand's centre wanders off its cell's middle; the pixel belongs to the nearer of the two strands either side.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // in 0..count
    let cell = (at.floor() as u32) % count;
    let centre = |k: u32| 0.5 + 0.3 * (hash(k, 1) - 0.5);
    let mut best = (f32::MAX, cell);
    for j in [-1_i64, 0, 1] {
        let k = i64::from(cell) + j;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // in 0..count
        let strand = k.rem_euclid(i64::from(count)) as u32;
        let d = (at - (k as f32 + centre(strand))).abs();
        if d < best.0 {
            best = (d, strand);
        }
    }
    let (d, strand) = best;
    // A strand's own shade, a finer grain inside it, and a soft dark gap at its edge.
    let shade = 0.84 + 0.16 * hash(strand, 2);
    let grain = 1.0 - strands.grain * (1.0 - noise(u + drift, count * 5, 3));
    let gap = 1.0 - strands.gap * smoothstep(0.32, 0.62, d);
    let grey = shade * grain * gap;
    // Each strand's coverage: the fainter ones are the first to drop out at a lock's feathered sides.
    let presence = LEAST + (1.0 - LEAST) * hash(strand, 5).sqrt();
    // The tip: each strand ends at its own length within the tip band, narrowing to its point.
    let start = 1.0 - TIP;
    let end = start + TIP * (0.55 + 0.42 * hash(strand, 4));
    let alpha = if v <= start {
        presence
    } else {
        let left = ((end - v) / (end - start)).clamp(0.0, 1.0);
        let half = 0.62 * left.powf(0.5);
        presence * (1.0 - smoothstep(half - 0.08, half + 0.08, d)) * smoothstep(0.0, 0.1, left)
    };
    (grey, alpha)
}

fn generate(strands: &Strands) -> TextureData {
    let desc = desc();
    let mut rgba8 = Vec::with_capacity(desc.byte_len());
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let (grey, alpha) = pixel(strands, x, y);
            let g = srgb(grey);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to a byte
            let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba8.extend([g, g, g, a]);
        }
    }
    TextureData { desc, rgba8 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // a row index
    fn tip_start() -> u32 {
        ((1.0 - TIP) * HEIGHT as f32) as u32
    }

    #[allow(clippy::cast_precision_loss)] // pixel counts
    fn coverage(t: &TextureData, rows: std::ops::Range<u32>, cut: f32) -> f32 {
        let mut kept = 0.0;
        for y in rows.clone() {
            for x in 0..WIDTH {
                let a = f32::from(t.rgba8[((y * WIDTH + x) * 4 + 3) as usize]) / 255.0;
                kept += f32::from(u8::from(a >= cut));
            }
        }
        kept / (rows.len() as f32 * WIDTH as f32)
    }

    #[test]
    fn its_body_is_whole_through_the_cut_and_it_frays_to_nothing_at_the_tip() {
        let t = Strands::default().texture();
        assert!(t.validate().is_ok());
        let start = tip_start();
        assert!((coverage(&t, 0..start, CUTOFF) - 1.0).abs() < 1e-6, "the body survives the cut everywhere");
        let (early, late) =
            (coverage(&t, start + 8..start + 16, CUTOFF), coverage(&t, HEIGHT - 16..HEIGHT - 8, CUTOFF));
        assert!(early > 0.6 && late < 0.1, "the tip thins out: {early} then {late}");
        assert!(coverage(&t, HEIGHT - 3..HEIGHT, 0.01) < 0.01, "and ends before the sampler wraps");
    }

    #[test]
    fn a_feathered_side_drops_the_fainter_strands() {
        let (t, map) = (Strands::default().texture(), StrandMap { feather: 0.45, ..StrandMap::default() });
        // The cut on a side's vertex alpha: the strands whose coverage times the side's stay at or over the cutoff.
        let side = map.coverage(1.0);
        assert!((map.coverage(0.0) - 1.0).abs() < 1e-6 && side < 0.7, "whole in the middle, {side} at the side");
        let kept = coverage(&t, 0..tip_start(), CUTOFF / side);
        assert!(kept > 0.15 && kept < 0.8, "a side keeps some strands, not all: {kept}");
    }

    #[test]
    fn the_strands_show_across_the_lock_and_repeat_seamlessly() {
        let t = Strands::default().texture();
        let grey = |x: u32, y: u32| t.rgba8[((y * WIDTH + x) * 4) as usize];
        let row: Vec<u8> = (0..WIDTH).map(|x| grey(x, 100)).collect();
        let (lo, hi) = (row.iter().min().copied().unwrap_or(0), row.iter().max().copied().unwrap_or(0));
        assert!(hi - lo > 40, "strands and gaps differ: {lo}..{hi}");
        assert!(grey(0, 100).abs_diff(grey(WIDTH - 1, 100)) < 40, "the seam carries on");
    }

    #[test]
    fn every_parameter_changes_the_texture() {
        let base = Strands::default();
        let pixels = |s: Strands| s.texture().rgba8;
        let reference = pixels(base);
        for changed in [Strands { count: 12, ..base }, Strands { gap: 0.55, ..base }, Strands { grain: 0.25, ..base }] {
            assert_ne!(pixels(changed), reference, "{changed:?}");
        }
        assert_eq!(pixels(base), reference, "the same parameters give the same texture");
    }

    #[test]
    fn the_tip_band_takes_the_tip_length() {
        let map = StrandMap::default();
        assert_eq!(map.v(0.0, 0.3), 0.0);
        assert!((map.v(0.3 - map.tip_length, 0.3) - (1.0 - TIP) * V_END).abs() < 1e-5);
        assert!((map.v(0.3, 0.3) - V_END).abs() < 1e-6);
        // A short lock gives its tip a share of its length.
        assert!((map.v(0.051, 0.06) - (1.0 - TIP) * V_END).abs() < 1e-5);
        let long = StrandMap { tip_length: 0.03, ..map };
        assert!((long.v(0.27, 0.3) - (1.0 - TIP) * V_END).abs() < 1e-5);
    }
}
