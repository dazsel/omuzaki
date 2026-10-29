//! The strand texture (#2451): what turns a lock from a flat ribbon into hair. Every lock, the cap and the curtain's
//! strips read it as the hair material's base texture, multiplied into the `hair` program's colours, with the material
//! cut at its alpha ([`CUTOFF`]).
//!
//! **Across** (`u`, one lock's width, repeating, so a lock offset by its seed shows its own strands) it is [`STRANDS`]
//! strands of their own shade, a soft dark gap between neighbours and a finer grain inside each. **Along** (`v`, root
//! at 0) the body of the lock is the first `1 - `[`TIP`] of it, the strands drifting gently sideways; the last [`TIP`]
//! is the tip, where each strand ends at its own length, narrowing to a point, so a lock ends in a fringe of fine
//! points instead of one hard edge. A lock maps its last few centimetres onto that band ([`tip_v`]).
//!
//! It is generated, the same every time, from nothing but integer hashes: no image is sampled.

use std::sync::LazyLock;

use omuzaki_char_api::TextureData;
use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{Filter, Sampler, TextureDesc, Wrap};

/// Pixels across one lock.
pub const WIDTH: u32 = 256;
/// Pixels from root to tip.
pub const HEIGHT: u32 = 512;
/// Strands across one lock.
pub const STRANDS: u32 = 28;
/// The share of the texture, at its end, given to the tips.
pub const TIP: f32 = 0.25;
/// The alpha below which the hair is cut away.
pub const CUTOFF: f32 = 0.5;
/// How long a lock's tip is, metres, at most: its last this-much maps onto the texture's tip band.
pub const TIP_LENGTH: f32 = 0.012;
/// The most of a lock's length its tip takes.
pub const TIP_SHARE: f32 = 0.15;
/// The last `v` a lock reaches: two rows short of the end, so the repeating sampler never blends the tip into the root.
#[allow(clippy::cast_precision_loss)] // 512
pub const V_END: f32 = 1.0 - 2.0 / HEIGHT as f32;

/// The strand texture, generated once.
pub fn texture() -> TextureData {
    static TEXTURE: LazyLock<TextureData> = LazyLock::new(generate);
    TEXTURE.clone()
}

/// The description the texture is uploaded with: sRGB, mip-mapped, repeating, sharpened at a slant.
pub fn desc() -> TextureDesc {
    let mut desc = TextureDesc::colour(UVec2::new(WIDTH, HEIGHT));
    desc.sampler = Sampler { filter: Filter::Linear, wrap: Wrap::Repeat, anisotropy: 4 };
    desc
}

/// The texture `v` of a point `along` metres down a lock `length` long: the body of the lock spread over the body of the
/// texture, and its last [`TIP_LENGTH`] (or [`TIP_SHARE`] of a short lock) over the tip band.
pub fn tip_v(along: f32, length: f32) -> f32 {
    if length <= 0.0 {
        return 0.0;
    }
    let tip = (TIP_LENGTH).min(TIP_SHARE * length);
    let body = length - tip;
    let v = if along < body {
        (1.0 - TIP) * along / body.max(f32::EPSILON)
    } else {
        (1.0 - TIP) + TIP * ((along - body) / tip.max(f32::EPSILON)).min(1.0)
    };
    v.clamp(0.0, 1.0) * V_END
}

/// The `u` offset of a lock with strand seed `seed`, so neighbouring locks show different strands.
pub fn offset(seed: f32) -> f32 {
    (seed * 7.31).fract().abs()
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

/// One pixel: the grey the hair's colours are multiplied by, and the coverage.
#[allow(clippy::cast_precision_loss, clippy::many_single_char_names)] // pixel and strand indices; texture maths
fn pixel(x: u32, y: u32) -> (f32, f32) {
    let (u, v) = ((x as f32 + 0.5) / WIDTH as f32, (y as f32 + 0.5) / HEIGHT as f32);
    let n = STRANDS as f32;
    // The strands drift a little sideways down the lock, each band of them its own way.
    let drift = 0.18 / n * (std::f32::consts::TAU * (v * 2.0 + noise(u, 3, 7))).sin();
    let at = (u + drift).rem_euclid(1.0) * n;
    // Each strand's centre wanders off its cell's middle; the pixel belongs to the nearer of the two strands either side.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // in 0..STRANDS
    let cell = (at.floor() as u32) % STRANDS;
    let centre = |k: u32| 0.5 + 0.3 * (hash(k, 1) - 0.5);
    let mut best = (f32::MAX, cell);
    for j in [-1_i64, 0, 1] {
        let k = i64::from(cell) + j;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // in 0..STRANDS
        let strand = k.rem_euclid(i64::from(STRANDS)) as u32;
        let d = (at - (k as f32 + centre(strand))).abs();
        if d < best.0 {
            best = (d, strand);
        }
    }
    let (d, strand) = best;
    // A strand's own shade, a finer grain inside it, and a soft dark gap at its edge.
    let shade = 0.84 + 0.16 * hash(strand, 2);
    let grain = 0.94 + 0.06 * noise(u + drift, STRANDS * 5, 3);
    let gap = 1.0 - 0.28 * smoothstep(0.32, 0.62, d);
    let grey = shade * grain * gap;
    // The tip: each strand ends at its own length within the tip band, narrowing to its point.
    let start = 1.0 - TIP;
    let end = start + TIP * (0.55 + 0.42 * hash(strand, 4));
    let alpha = if v <= start {
        1.0
    } else {
        let left = ((end - v) / (end - start)).clamp(0.0, 1.0);
        let half = 0.62 * left.powf(0.5);
        (1.0 - smoothstep(half - 0.08, half + 0.08, d)) * smoothstep(0.0, 0.1, left)
    };
    (grey, alpha)
}

fn generate() -> TextureData {
    let desc = desc();
    let mut rgba8 = Vec::with_capacity(desc.byte_len());
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let (grey, alpha) = pixel(x, y);
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

    #[allow(clippy::cast_precision_loss)] // pixel counts
    fn coverage(rows: std::ops::Range<u32>) -> f32 {
        let t = texture();
        let mut sum = 0.0;
        for y in rows.clone() {
            for x in 0..WIDTH {
                sum += f32::from(t.rgba8[((y * WIDTH + x) * 4 + 3) as usize]) / 255.0;
            }
        }
        sum / (rows.len() as f32 * WIDTH as f32)
    }

    #[test]
    fn it_is_whole_in_the_body_and_frays_to_nothing_at_the_tip() {
        assert!(texture().validate().is_ok());
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // a row index
        let start = ((1.0 - TIP) * HEIGHT as f32) as u32;
        assert!((coverage(0..start) - 1.0).abs() < 1e-6, "the body is solid");
        let (early, late) = (coverage(start + 8..start + 16), coverage(HEIGHT - 16..HEIGHT - 8));
        assert!(early > 0.6 && late < 0.1, "the tip thins out: {early} then {late}");
        assert!(coverage(HEIGHT - 3..HEIGHT) < 0.01, "and ends before the sampler wraps");
    }

    #[test]
    fn the_strands_show_across_the_lock_and_repeat_seamlessly() {
        let t = texture();
        let grey = |x: u32, y: u32| t.rgba8[((y * WIDTH + x) * 4) as usize];
        let row: Vec<u8> = (0..WIDTH).map(|x| grey(x, 100)).collect();
        let (lo, hi) = (row.iter().min().copied().unwrap_or(0), row.iter().max().copied().unwrap_or(0));
        assert!(hi - lo > 40, "strands and gaps differ: {lo}..{hi}");
        assert!(grey(0, 100).abs_diff(grey(WIDTH - 1, 100)) < 40, "the seam carries on");
    }

    #[test]
    fn the_tip_band_takes_the_last_few_centimetres() {
        assert_eq!(tip_v(0.0, 0.3), 0.0);
        assert!((tip_v(0.3 - TIP_LENGTH, 0.3) - (1.0 - TIP) * V_END).abs() < 1e-5);
        assert!((tip_v(0.3, 0.3) - V_END).abs() < 1e-6);
        // A short lock gives its tip a share of its length.
        assert!((tip_v(0.051, 0.06) - (1.0 - TIP) * V_END).abs() < 1e-5);
    }
}
