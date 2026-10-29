//! Textures for the CPU renderer: premultiplied RGBA8 with a mip chain, sampled trilinearly so art stays clean when the
//! camera pulls back and smooth when it pushes in.
#![allow(clippy::many_single_char_names, reason = "geometry and sampling read best in their formulas' own letters")]

use crate::blend::Px;

/// One mip level.
#[derive(Clone, Debug)]
struct Level {
    w: u32,
    h: u32,
    px: Vec<[u8; 4]>,
}

/// A texture: premultiplied, sRGB-encoded RGBA8, with its mips.
#[derive(Clone, Debug)]
pub struct Texture {
    levels: Vec<Level>,
}

const INV255: f32 = 1.0 / 255.0;

impl Texture {
    /// A texture from straight (not premultiplied) RGBA8 pixels, row-major from the top. `None` if the sizes disagree.
    #[must_use]
    pub fn from_straight(w: u32, h: u32, rgba: &[u8]) -> Option<Self> {
        if w == 0 || h == 0 || rgba.len() != (w as usize) * (h as usize) * 4 {
            return None;
        }
        let px = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let a = u16::from(p[3]);
                #[allow(clippy::cast_possible_truncation, reason = "at most 255")]
                let pm = |c: u8| ((u16::from(c) * a + 127) / 255) as u8;
                [pm(p[0]), pm(p[1]), pm(p[2]), p[3]]
            })
            .collect();
        Some(Self::from_levels(Level { w, h, px }))
    }

    /// A texture from premultiplied RGBA8 pixels. `None` if the sizes disagree.
    #[must_use]
    pub fn from_premultiplied(w: u32, h: u32, rgba: &[u8]) -> Option<Self> {
        if w == 0 || h == 0 || rgba.len() != (w as usize) * (h as usize) * 4 {
            return None;
        }
        let px = rgba.as_chunks::<4>().0.to_vec();
        Some(Self::from_levels(Level { w, h, px }))
    }

    fn from_levels(base: Level) -> Self {
        let mut levels = vec![base];
        while let Some(last) = levels.last() {
            if last.w == 1 && last.h == 1 {
                break;
            }
            let (w, h) = ((last.w / 2).max(1), (last.h / 2).max(1));
            let mut px = Vec::with_capacity((w * h) as usize);
            for y in 0..h {
                for x in 0..w {
                    let mut acc = [0u32; 4];
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(last.w - 1);
                        let sy = (y * 2 + dy).min(last.h - 1);
                        let p = last.px[(sy * last.w + sx) as usize];
                        for c in 0..4 {
                            acc[c] += u32::from(p[c]);
                        }
                    }
                    #[allow(clippy::cast_possible_truncation, reason = "an average of bytes")]
                    px.push(acc.map(|v| ((v + 2) / 4) as u8));
                }
            }
            levels.push(Level { w, h, px });
        }
        Self { levels }
    }

    /// The full-size dimensions.
    #[must_use]
    pub fn size(&self) -> [u32; 2] {
        self.levels.first().map_or([0, 0], |l| [l.w, l.h])
    }

    /// The premultiplied texel at `(x, y)` of the full-size level (clamped to the edge), as `0..1` values.
    #[must_use]
    pub fn texel(&self, x: i64, y: i64) -> Px {
        self.levels.first().map_or([0.0; 4], |l| fetch(l, x, y))
    }

    /// Samples at `uv` (`0..1`, y down) with level of detail `lod` (0 is full size; each step halves).
    #[must_use]
    pub fn sample(&self, uv: [f32; 2], lod: f32) -> Px {
        #[allow(clippy::cast_precision_loss, reason = "a handful of levels")]
        let top = (self.levels.len() - 1) as f32;
        let lod = lod.clamp(0.0, top);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped above")]
        let l0 = lod.floor() as usize;
        #[allow(clippy::cast_precision_loss, reason = "a handful of levels")]
        let f = lod - l0 as f32;
        let a = bilinear(&self.levels[l0], uv);
        if f <= 1e-3 || l0 + 1 >= self.levels.len() {
            return a;
        }
        let b = bilinear(&self.levels[l0 + 1], uv);
        [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f, a[3] + (b[3] - a[3]) * f]
    }
}

fn fetch(l: &Level, x: i64, y: i64) -> Px {
    let x = x.clamp(0, i64::from(l.w) - 1);
    let y = y.clamp(0, i64::from(l.h) - 1);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation, reason = "clamped into the level")]
    let p = l.px[(y as usize) * (l.w as usize) + x as usize];
    [f32::from(p[0]) * INV255, f32::from(p[1]) * INV255, f32::from(p[2]) * INV255, f32::from(p[3]) * INV255]
}

fn bilinear(l: &Level, uv: [f32; 2]) -> Px {
    #[allow(clippy::cast_precision_loss, reason = "texture sides far below 2^24")]
    let (fx, fy) = (uv[0] * l.w as f32 - 0.5, uv[1] * l.h as f32 - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    #[allow(clippy::cast_possible_truncation, reason = "texture coordinates stay small")]
    let (x0, y0) = (x0 as i64, y0 as i64);
    let a = fetch(l, x0, y0);
    let b = fetch(l, x0 + 1, y0);
    let c = fetch(l, x0, y0 + 1);
    let d = fetch(l, x0 + 1, y0 + 1);
    let mut out = [0.0; 4];
    for k in 0..4 {
        let top = a[k] + (b[k] - a[k]) * tx;
        let bottom = c[k] + (d[k] - c[k]) * tx;
        out[k] = top + (bottom - top) * ty;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_pixels_are_premultiplied_and_mips_average() {
        let t = Texture::from_straight(2, 2, &[255, 0, 0, 255, 255, 0, 0, 0, 0, 0, 255, 255, 0, 0, 255, 0]);
        let Some(t) = t else { panic!("a 2x2 texture") };
        assert_eq!(t.texel(1, 0), [0.0; 4], "a transparent texel premultiplies to zero");
        let top = t.sample([0.5, 0.5], 1.0);
        assert!((top[3] - 0.5).abs() < 0.01 && (top[0] - 0.25).abs() < 0.01 && (top[2] - 0.25).abs() < 0.01);
        assert!(Texture::from_straight(2, 2, &[0; 3]).is_none());
    }

    #[test]
    fn bilinear_sampling_blends_neighbours_and_clamps_at_edges() {
        let Some(t) = Texture::from_premultiplied(2, 1, &[0, 0, 0, 255, 255, 255, 255, 255]) else {
            panic!("a 2x1 texture")
        };
        let mid = t.sample([0.5, 0.5], 0.0);
        assert!((mid[0] - 0.5).abs() < 1e-3, "{mid:?}");
        let edge = t.sample([0.0, 0.5], 0.0);
        assert!(edge[0].abs() < 1e-3, "clamped to the first texel: {edge:?}");
    }
}
