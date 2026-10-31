//! The overlays that show a render against its reference on one pixel grid: onion skin, the exact difference map (the
//! stop rule's map), the colour difference heat map, the signed lightness difference, a wipe, a checkerboard, the two
//! edge sets, the two silhouettes, the regions, and a sheet of the most useful six.
//!
//! An overlay holds reference pixels, so it is evidence only: the ops store it with the reference as an input, and the
//! provenance check refuses any render built from it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::metrics::{delta_e_map, edges, level_map};
use super::raster::LabImage;
use super::regions::Region;

/// Which overlay to draw.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Mode {
    /// Ours under the reference at `opacity` (0: ours only, 1: the reference only).
    Onion {
        /// The reference's opacity (default 0.5).
        #[serde(default = "half")]
        opacity: f32,
    },
    /// The exact difference map: black where every byte matches, red rising with the largest byte difference (any
    /// difference at all shows). Parity is reached when this map is black everywhere.
    Exact,
    /// The per-channel absolute difference, times `gain`.
    Difference {
        /// The gain (default 4).
        #[serde(default = "four")]
        gain: f32,
    },
    /// CIEDE2000 as a heat map: black below 1, blue to 2.3 (just noticeable), green to 5, yellow to 10, red to 20,
    /// white beyond.
    DeltaE,
    /// The lightness difference: red where ours is lighter, blue where darker, brighter with the size of the
    /// difference (`gain` per L*).
    Signed {
        /// Brightness per unit of L* (default 12).
        #[serde(default = "twelve")]
        gain: f32,
    },
    /// Ours on one side of a line, the reference on the other.
    Wipe {
        /// Where the line is, 0 to 1 across (or down) the frame (default 0.5).
        #[serde(default = "half")]
        position: f32,
        /// A horizontal line (ours above) instead of a vertical one (ours left).
        #[serde(default)]
        horizontal: bool,
    },
    /// A checkerboard of ours and the reference, `cell` pixels a side: every seam shows a mismatch.
    Checker {
        /// The cell side in pixels (default 16).
        #[serde(default = "sixteen")]
        cell: u32,
    },
    /// The reference dimmed, with its edges in cyan, ours in magenta and edges both share in white.
    Edges,
    /// The silhouettes: grey where both cover, red where only ours does, blue where only the reference does.
    Silhouette,
    /// Ours and the reference half and half, with every region's border in its own colour.
    Regions,
    /// Six panels: ours, the reference, onion | exact, delta E, edges.
    Sheet,
}

fn half() -> f32 {
    0.5
}
fn four() -> f32 {
    4.0
}
fn twelve() -> f32 {
    12.0
}
fn sixteen() -> u32 {
    16
}

/// An opaque sRGB picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// The width in pixels.
    pub w: usize,
    /// The height in pixels.
    pub h: usize,
    /// `w * h` sRGB triples.
    pub rgb: Vec<[u8; 3]>,
}

impl Picture {
    fn new(w: usize, h: usize) -> Self {
        Self { w, h, rgb: vec![[0, 0, 0]; w * h] }
    }

    /// The picture as straight RGBA8 bytes.
    #[must_use]
    pub fn rgba(&self) -> Vec<u8> {
        self.rgb.iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect()
    }

    /// Draws `other` with its top-left corner at `(x, y)`.
    fn paste(&mut self, other: &Self, x: usize, y: usize) {
        for row in 0..other.h.min(self.h.saturating_sub(y)) {
            for col in 0..other.w.min(self.w.saturating_sub(x)) {
                self.rgb[(y + row) * self.w + x + col] = other.rgb[row * other.w + col];
            }
        }
    }

    /// Outlines a rectangle.
    pub fn frame(&mut self, x: usize, y: usize, w: usize, h: usize, colour: [u8; 3]) {
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        for px in x.min(self.w)..x1 {
            for py in [y, y1.saturating_sub(1)] {
                if py < self.h {
                    self.rgb[py * self.w + px] = colour;
                }
            }
        }
        for py in y.min(self.h)..y1 {
            for px in [x, x1.saturating_sub(1)] {
                if px < self.w {
                    self.rgb[py * self.w + px] = colour;
                }
            }
        }
    }

    /// The picture shrunk by a whole factor (block means).
    #[allow(clippy::cast_possible_truncation)] // a mean of bytes is a byte
    fn shrink(&self, factor: usize) -> Self {
        if factor <= 1 {
            return self.clone();
        }
        let (w, h) = ((self.w / factor).max(1), (self.h / factor).max(1));
        let mut out = Self::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0u32; 3];
                let mut n = 0u32;
                for dy in 0..factor {
                    for dx in 0..factor {
                        let (sx, sy) = (x * factor + dx, y * factor + dy);
                        if sx < self.w && sy < self.h {
                            let c = self.rgb[sy * self.w + sx];
                            (0..3).for_each(|k| acc[k] += u32::from(c[k]));
                            n += 1;
                        }
                    }
                }
                out.rgb[y * w + x] = acc.map(|a| (a / n.max(1)) as u8);
            }
        }
        out
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to a byte
fn byte(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    [0, 1, 2].map(|k| byte(f32::from(a[k]) * (1.0 - t) + f32::from(b[k]) * t))
}

/// The heat colour of a CIEDE2000 value (see [`Mode::DeltaE`]).
#[must_use]
pub fn heat(de: f32) -> [u8; 3] {
    const STOPS: [(f32, [u8; 3]); 6] = [
        (1.0, [0, 0, 0]),
        (2.3, [30, 60, 200]),
        (5.0, [20, 190, 60]),
        (10.0, [240, 220, 30]),
        (20.0, [230, 40, 30]),
        (40.0, [255, 255, 255]),
    ];
    if de <= STOPS[0].0 {
        return STOPS[0].1;
    }
    for pair in STOPS.windows(2) {
        let ((a, ca), (b, cb)) = (pair[0], pair[1]);
        if de <= b {
            return mix(ca, cb, (de - a) / (b - a));
        }
    }
    STOPS[5].1
}

/// A distinct colour for the `k`th region.
fn region_colour(k: usize) -> [u8; 3] {
    const COLOURS: [[u8; 3]; 10] = [
        [255, 80, 80],
        [80, 200, 255],
        [255, 210, 60],
        [120, 255, 120],
        [255, 120, 255],
        [255, 160, 60],
        [160, 120, 255],
        [60, 255, 220],
        [255, 255, 255],
        [200, 200, 80],
    ];
    COLOURS[k % COLOURS.len()]
}

/// Draws `mode` for ours against the reference (one size); `silhouettes` are ours and the reference's, `regions`
/// the scored regions.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::too_many_lines)] // pixel grids; one arm a mode
pub fn draw(
    mode: &Mode,
    ours: &LabImage,
    reference: &LabImage,
    silhouettes: (&[bool], &[bool]),
    regions: &[Region],
) -> Picture {
    let (w, h) = (ours.w, ours.h);
    let mut out = Picture::new(w, h);
    match mode {
        Mode::Onion { opacity } => {
            let t = opacity.clamp(0.0, 1.0);
            for i in 0..w * h {
                out.rgb[i] = mix(ours.rgb[i], reference.rgb[i], t);
            }
        }
        Mode::Exact => {
            let levels = level_map(ours, reference);
            for (i, d) in levels.v.iter().enumerate() {
                // Any difference shows: one level is already a visible dark red.
                out.rgb[i] = if *d <= 0.0 {
                    [0, 0, 0]
                } else {
                    [byte(90.0 + d * 4.0), byte((d - 40.0) * 2.0), byte((d - 100.0) * 2.0)]
                };
            }
        }
        Mode::Difference { gain } => {
            for i in 0..w * h {
                let (p, q) = (ours.rgb[i], reference.rgb[i]);
                out.rgb[i] = [0, 1, 2].map(|k| byte(f32::from(p[k].abs_diff(q[k])) * gain));
            }
        }
        Mode::DeltaE => {
            let de = delta_e_map(ours, reference);
            for (i, d) in de.v.iter().enumerate() {
                out.rgb[i] = heat(*d);
            }
        }
        Mode::Signed { gain } => {
            for i in 0..w * h {
                let d = ours.l.v[i] - reference.l.v[i];
                out.rgb[i] = if d >= 0.0 {
                    [byte(d * gain), byte(d * gain * 0.25), 0]
                } else {
                    [0, byte(-d * gain * 0.4), byte(-d * gain)]
                };
            }
        }
        Mode::Wipe { position, horizontal } => {
            let at = position.clamp(0.0, 1.0);
            for i in 0..w * h {
                let (x, y) = (i % w, i / w);
                let (k, n) = if *horizontal { (y, h) } else { (x, w) };
                let line = (at * n as f32).round() as usize;
                out.rgb[i] = match k.cmp(&line) {
                    std::cmp::Ordering::Equal => [255, 230, 0],
                    std::cmp::Ordering::Less => ours.rgb[i],
                    std::cmp::Ordering::Greater => reference.rgb[i],
                };
            }
        }
        Mode::Checker { cell } => {
            let c = (*cell as usize).max(1);
            for i in 0..w * h {
                let (x, y) = (i % w, i / w);
                out.rgb[i] = if (x / c + y / c).is_multiple_of(2) { ours.rgb[i] } else { reference.rgb[i] };
            }
        }
        Mode::Edges => {
            let (eo, er) = (edges(&ours.l), edges(&reference.l));
            for i in 0..w * h {
                out.rgb[i] = match (eo[i], er[i]) {
                    (true, true) => [255, 255, 255],
                    (true, false) => [255, 40, 220],
                    (false, true) => [0, 230, 255],
                    (false, false) => {
                        let g = byte(reference.l.v[i] * 2.55 * 0.3);
                        [g, g, g]
                    }
                };
            }
        }
        Mode::Silhouette => {
            let (so, sr) = silhouettes;
            for i in 0..w * h {
                out.rgb[i] = match (so[i], sr[i]) {
                    (true, true) => [128, 128, 128],
                    (true, false) => [230, 50, 40],
                    (false, true) => [40, 110, 240],
                    (false, false) => [0, 0, 0],
                };
            }
        }
        Mode::Regions => {
            for i in 0..w * h {
                out.rgb[i] = mix(ours.rgb[i], reference.rgb[i], 0.5);
            }
            for (k, r) in regions.iter().enumerate() {
                if r.name == "background" {
                    continue;
                }
                let colour = region_colour(k);
                for i in 0..w * h {
                    let (x, y) = (i % w, i / w);
                    let edge = r.mask[i]
                        && (x == 0
                            || y == 0
                            || x + 1 == w
                            || y + 1 == h
                            || !r.mask[i - 1]
                            || !r.mask[i + 1]
                            || !r.mask[i - w]
                            || !r.mask[i + w]);
                    if edge {
                        out.rgb[i] = colour;
                    }
                }
            }
        }
        Mode::Sheet => {
            let panels = [
                Mode::Onion { opacity: 0.0 },
                Mode::Onion { opacity: 1.0 },
                Mode::Onion { opacity: 0.5 },
                Mode::Exact,
                Mode::DeltaE,
                Mode::Edges,
            ];
            // Keep the sheet within the decoder's 8192 px a side, so a measure can read it back.
            let factor = (w * 3).div_ceil(8192).max((h * 2).div_ceil(8192)).max(1);
            let (pw, ph) = (w / factor, h / factor);
            let mut sheet = Picture::new(pw * 3 + 2 * 4, ph * 2 + 4);
            sheet.rgb.fill([40, 40, 40]);
            for (k, m) in panels.iter().enumerate() {
                let panel = draw(m, ours, reference, silhouettes, regions).shrink(factor);
                sheet.paste(&panel, (k % 3) * (pw + 4), (k / 3) * (ph + 4));
            }
            return sheet;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Rgba8;
    use omuzaki_store_api::ContentHash;

    fn flat(w: u32, h: u32, c: [u8; 3]) -> LabImage {
        let pixels = (0..w * h).flat_map(|_| [c[0], c[1], c[2], 255]).collect();
        LabImage::from_rgba(&Rgba8 { width: w, height: h, pixels, hash: ContentHash::of(b"x") }, [0, 0, 0])
    }

    #[test]
    fn the_exact_map_is_black_only_when_every_byte_matches() {
        let (a, b) = (flat(4, 4, [10, 20, 30]), flat(4, 4, [10, 20, 31]));
        let same = draw(&Mode::Exact, &a, &a, (&[true; 16], &[true; 16]), &[]);
        assert!(same.rgb.iter().all(|c| *c == [0, 0, 0]));
        let off = draw(&Mode::Exact, &a, &b, (&[true; 16], &[true; 16]), &[]);
        assert!(off.rgb.iter().all(|c| c[0] >= 90), "one level shows");
    }

    #[test]
    fn the_sheet_holds_six_panels() {
        let a = flat(10, 8, [200, 100, 50]);
        let sheet = draw(&Mode::Sheet, &a, &a, (&[true; 80], &[true; 80]), &[]);
        assert_eq!((sheet.w, sheet.h), (38, 20));
    }

    #[test]
    fn the_heat_scale_is_black_below_one_and_white_far_off() {
        assert_eq!(heat(0.5), [0, 0, 0]);
        assert_eq!(heat(100.0), [255, 255, 255]);
        assert_eq!(heat(2.3), [30, 60, 200]);
    }
}
