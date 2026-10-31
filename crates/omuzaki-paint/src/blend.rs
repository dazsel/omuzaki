//! Blend modes and compositing, as the W3C "Compositing and Blending Level 1" recommendation defines them (the same
//! formulas Photoshop, Krita and Clip Studio use), on premultiplied pixels.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::colour::Px;

/// How a layer or a mark combines with what is under it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    /// Paint over (source-over).
    #[default]
    Normal,
    /// Darken by multiplying: shadows and line art over colour.
    Multiply,
    /// Lighten by screening: glows and light haze.
    Screen,
    /// Multiply the darks and screen the lights of what is under: contrast and colour grading.
    Overlay,
    /// A gentle overlay: soft light and shading.
    SoftLight,
    /// Overlay keyed on the paint instead of what is under.
    HardLight,
    /// Brighten what is under towards the paint's colour: hot highlights and rim light.
    ColorDodge,
    /// Darken what is under towards the paint's colour: deep shadows.
    ColorBurn,
    /// Add (linear dodge): light, sparkles, emission.
    Add,
    /// Linear burn: what is under plus the paint, minus white.
    LinearBurn,
    /// The darker of the two, per channel.
    Darken,
    /// The lighter of the two, per channel.
    Lighten,
    /// The absolute difference: checking two pictures line up.
    Difference,
    /// A softer difference.
    Exclusion,
    /// The paint's hue with what is under's saturation and luminosity.
    Hue,
    /// The paint's saturation.
    Saturation,
    /// The paint's hue and saturation with what is under's luminosity: tinting a grey painting.
    Color,
    /// The paint's luminosity.
    Luminosity,
}

impl BlendMode {
    /// Every mode, in the order the painter's menu shows them.
    pub const ALL: [Self; 18] = [
        Self::Normal,
        Self::Multiply,
        Self::Screen,
        Self::Overlay,
        Self::SoftLight,
        Self::HardLight,
        Self::ColorDodge,
        Self::ColorBurn,
        Self::Add,
        Self::LinearBurn,
        Self::Darken,
        Self::Lighten,
        Self::Difference,
        Self::Exclusion,
        Self::Hue,
        Self::Saturation,
        Self::Color,
        Self::Luminosity,
    ];

    /// `B(Cb, Cs)` for straight colours: what the paint `s` makes of the backdrop `b` where both are opaque.
    fn mix(self, b: [f32; 3], s: [f32; 3]) -> [f32; 3] {
        let per = |f: fn(f32, f32) -> f32| [f(b[0], s[0]), f(b[1], s[1]), f(b[2], s[2])];
        match self {
            Self::Normal => s,
            Self::Multiply => per(|b, s| b * s),
            Self::Screen => per(screen),
            Self::Overlay => per(|b, s| hard_light(s, b)),
            Self::SoftLight => per(soft_light),
            Self::HardLight => per(hard_light),
            Self::ColorDodge => per(color_dodge),
            Self::ColorBurn => per(color_burn),
            Self::Add => per(|b, s| (b + s).min(1.0)),
            Self::LinearBurn => per(|b, s| (b + s - 1.0).max(0.0)),
            Self::Darken => per(f32::min),
            Self::Lighten => per(f32::max),
            Self::Difference => per(|b, s| (b - s).abs()),
            Self::Exclusion => per(|b, s| b + s - 2.0 * b * s),
            Self::Hue => set_lum(set_sat(s, sat(b)), lum(b)),
            Self::Saturation => set_lum(set_sat(b, sat(s)), lum(b)),
            Self::Color => set_lum(s, lum(b)),
            Self::Luminosity => set_lum(b, lum(s)),
        }
    }
}

fn screen(b: f32, s: f32) -> f32 {
    b + s - b * s
}

fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 { b * 2.0 * s } else { screen(b, 2.0 * s - 1.0) }
}

fn soft_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b - (1.0 - 2.0 * s) * b * (1.0 - b)
    } else {
        let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
        b + (2.0 * s - 1.0) * (d - b)
    }
}

fn color_dodge(b: f32, s: f32) -> f32 {
    if b <= 0.0 {
        0.0
    } else if s >= 1.0 {
        1.0
    } else {
        (b / (1.0 - s)).min(1.0)
    }
}

fn color_burn(b: f32, s: f32) -> f32 {
    if b >= 1.0 {
        1.0
    } else if s <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - b) / s).min(1.0)
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_colour(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 && l - n > 0.0 {
        out = out.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 && x - l > 0.0 {
        out = out.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    out
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_colour(c.map(|v| v + d))
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let (lo, hi) = (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
    if hi - lo <= 0.0 {
        return [0.0; 3];
    }
    c.map(|v| (v - lo) * s / (hi - lo))
}

/// Straight colour of a premultiplied pixel (black where it is clear).
fn straight(p: Px) -> [f32; 3] {
    if p[3] <= 0.0 { [0.0; 3] } else { [p[0] / p[3], p[1] / p[3], p[2] / p[3]] }
}

/// How a source's alpha is kept when it lands.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Keep {
    /// Source-over: the source adds coverage where the backdrop is clear.
    Over,
    /// Source-atop: the backdrop's alpha is kept (clipping, alpha lock).
    Atop,
}

/// Composites premultiplied `src`, scaled by `weight` (opacity times coverage, 0..1), onto `dst` with `mode`.
#[inline]
pub fn composite(dst: &mut Px, src: Px, weight: f32, mode: BlendMode, keep: Keep) {
    let a_s = src[3] * weight;
    if a_s <= 0.0 {
        return;
    }
    let a_b = dst[3];
    let cs = [src[0] * weight, src[1] * weight, src[2] * weight];
    let mixed =
        if mode == BlendMode::Normal || a_b <= 0.0 { straight(src) } else { mode.mix(straight(*dst), straight(src)) };
    let outside = match keep {
        Keep::Over => 1.0 - a_b,
        Keep::Atop => 0.0,
    };
    for i in 0..3 {
        dst[i] = cs[i] * outside + dst[i] * (1.0 - a_s) + a_s * a_b * mixed[i];
    }
    dst[3] = match keep {
        Keep::Over => a_s + a_b * (1.0 - a_s),
        Keep::Atop => a_b,
    };
}

/// Erases `dst` by `weight` (0..1): destination-out.
#[inline]
pub fn erase(dst: &mut Px, weight: f32) {
    let k = 1.0 - weight.clamp(0.0, 1.0);
    for v in dst.iter_mut() {
        *v *= k;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Px, b: Px) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn normal_over_transparent_is_the_source() {
        let mut d = [0.0; 4];
        composite(&mut d, [0.5, 0.25, 0.0, 0.5], 1.0, BlendMode::Normal, Keep::Over);
        assert!(close(d, [0.5, 0.25, 0.0, 0.5]));
    }

    #[test]
    fn multiply_white_is_identity_and_black_is_black() {
        let base = [0.3, 0.6, 0.9, 1.0];
        let mut d = base;
        composite(&mut d, [1.0, 1.0, 1.0, 1.0], 1.0, BlendMode::Multiply, Keep::Over);
        assert!(close(d, base));
        composite(&mut d, [0.0, 0.0, 0.0, 1.0], 1.0, BlendMode::Multiply, Keep::Over);
        assert!(close(d, [0.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn atop_keeps_the_backdrop_alpha() {
        let mut clear = [0.0; 4];
        composite(&mut clear, [1.0, 0.0, 0.0, 1.0], 1.0, BlendMode::Normal, Keep::Atop);
        assert!(close(clear, [0.0; 4]));
        let mut half = [0.0, 0.0, 0.5, 0.5];
        composite(&mut half, [1.0, 0.0, 0.0, 1.0], 1.0, BlendMode::Normal, Keep::Atop);
        assert!(close(half, [0.5, 0.0, 0.0, 0.5]));
    }

    #[test]
    fn every_mode_stays_in_range() {
        let samples = [0.0, 0.1, 0.5, 0.9, 1.0];
        for mode in BlendMode::ALL {
            for &b in &samples {
                for &s in &samples {
                    let mut d = [b * 0.8, (1.0 - b) * 0.8, 0.4, 0.8];
                    composite(&mut d, [s * 0.6, 0.3, (1.0 - s) * 0.6, 0.6], 0.7, mode, Keep::Over);
                    assert!(d.iter().all(|v| (-1e-5..=1.0 + 1e-5).contains(v)), "{mode:?} {d:?}");
                    assert!(d[..3].iter().all(|&v| v <= d[3] + 1e-5), "{mode:?} not premultiplied {d:?}");
                }
            }
        }
    }

    #[test]
    fn colour_keeps_the_backdrop_luminosity() {
        let mut d = [0.5, 0.5, 0.5, 1.0];
        composite(&mut d, [1.0, 0.0, 0.0, 1.0], 1.0, BlendMode::Color, Keep::Over);
        assert!((lum([d[0], d[1], d[2]]) - 0.5).abs() < 1e-4);
        assert!(d[0] > d[1]);
    }
}
