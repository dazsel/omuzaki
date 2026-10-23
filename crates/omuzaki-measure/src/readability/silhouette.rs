//! Readable silhouettes (spec §32 "Readable silhouette"): a character is recognisable at a glance and at thumbnail size
//! when its filled outline alone, in black at 64 px, is unlike other characters'.
//!
//! The character's pixels (by alpha, or by their difference from a plain background) are cropped to their bounds and
//! scaled, keeping the aspect, so the longer side is [`SIDE`] px, centred on a [`SIDE`] px square: two characters are
//! compared by shape, not by size or by where they stand in the frame. Each output pixel is filled when at least half of
//! it is covered (sampled 4 x 4). Two silhouettes' **overlap** is their intersection over their union (1 the same
//! shape, 0 nothing shared); a character's **distinctiveness** is one minus its overlap with the closest other.

use crate::colour::{delta_e, lab};
use crate::image::Rgba8;

/// The silhouette's side, pixels (spec §32: 64 px).
pub const SIDE: u32 = 64;

/// The least distinctiveness that passes: the closest other shares at most 92% of the pair's combined area. On a
/// plain figure, a slightly larger head alone leaves the overlap above 0.95, and a brimmed hat takes it to about 0.91.
/// A starting value: the readability critics (ML-11) tune it against rated characters.
pub const MIN_DISTINCT: f64 = 0.08;

/// The colour difference, `ΔE*ab`, past which a pixel of an opaque image is the character and not the background.
const BACKGROUND_DELTA: f64 = 10.0;

/// The alpha at or above which a pixel is the character.
const ALPHA_ON: u8 = 128;

/// Samples a side per output pixel.
const SAMPLES: u32 = 4;

/// What separates the character from the rest of the image.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Ground {
    /// Pixels with alpha of at least half are the character.
    Alpha,
    /// Pixels that differ from this colour are the character.
    Colour([u8; 3]),
}

impl Ground {
    /// The ground of `image` when none is given: its alpha when any pixel is see-through, else the colour of its top-left
    /// corner.
    #[must_use]
    pub fn of(image: &Rgba8) -> Self {
        let quads = image.pixels.as_chunks::<4>().0;
        if quads.iter().any(|p| p[3] < u8::MAX) {
            Self::Alpha
        } else {
            quads.first().map_or(Self::Alpha, |p| Self::Colour([p[0], p[1], p[2]]))
        }
    }
}

/// A silhouette: [`SIDE`] x [`SIDE`] cells, row by row from the top, `true` where filled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Silhouette {
    /// The cells.
    pub filled: Vec<bool>,
}

impl Silhouette {
    /// The share of the square that is filled.
    #[must_use]
    pub fn fill(&self) -> f64 {
        let count = self.filled.iter().filter(|f| **f).count();
        ratio(count, self.filled.len())
    }

    /// Intersection over union with `other`; 1 for two empty silhouettes.
    #[must_use]
    pub fn overlap(&self, other: &Self) -> f64 {
        let (mut both, mut either) = (0usize, 0usize);
        for (a, b) in self.filled.iter().zip(&other.filled) {
            both += usize::from(*a && *b);
            either += usize::from(*a || *b);
        }
        if either == 0 { 1.0 } else { ratio(both, either) }
    }

    /// The silhouette as greyscale bytes, black on white (for a contact sheet).
    #[must_use]
    pub fn luma(&self) -> Vec<u8> {
        self.filled.iter().map(|f| if *f { 0 } else { u8::MAX }).collect()
    }
}

#[allow(clippy::cast_precision_loss)] // pixel counts, far inside f64's exact range
fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 / whole as f64 }
}

/// The silhouette of `image` against `ground`; `None` when no pixel is the character.
#[must_use]
pub fn silhouette(image: &Rgba8, ground: Ground) -> Option<Silhouette> {
    let (w, h) = (image.width as usize, image.height as usize);
    let quads = image.pixels.as_chunks::<4>().0;
    let ground_lab = match ground {
        Ground::Colour(rgb) => Some(lab(rgb)),
        Ground::Alpha => None,
    };
    let on: Vec<bool> = quads
        .iter()
        .map(|p| p[3] >= ALPHA_ON && ground_lab.is_none_or(|g| delta_e(lab([p[0], p[1], p[2]]), g) > BACKGROUND_DELTA))
        .collect();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (i, _) in on.iter().enumerate().filter(|(_, f)| **f) {
        let (x, y) = (i % w, i / w);
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
    }
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    // Source pixels per output pixel, the same both ways, so the aspect is kept.
    #[allow(clippy::cast_precision_loss)] // image sides are at most 8192
    let (bw, bh) = ((x1 - x0) as f64, (y1 - y0) as f64);
    let scale = bw.max(bh) / f64::from(SIDE);
    // Where the bounds start on the square, so the shape is centred.
    let (left, top) = ((f64::from(SIDE) - bw / scale) / 2.0, (f64::from(SIDE) - bh / scale) / 2.0);
    let side = SIDE as usize;
    let mut filled = vec![false; side * side];
    for (cell, slot) in (0..SIDE * SIDE).zip(filled.iter_mut()) {
        let (cx, cy) = (f64::from(cell % SIDE), f64::from(cell / SIDE));
        let mut hits = 0;
        for sy in 0..SAMPLES {
            for sx in 0..SAMPLES {
                let (u, v) =
                    (cx + (f64::from(sx) + 0.5) / f64::from(SAMPLES), cy + (f64::from(sy) + 0.5) / f64::from(SAMPLES));
                let (px, py) = ((u - left) * scale, (v - top) * scale);
                if px < 0.0 || py < 0.0 || px >= bw || py >= bh {
                    continue;
                }
                // Inside the bounds, so the sums stay inside the image.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let (column, row) = (x0 + px as usize, y0 + py as usize);
                hits += u32::from(on.get(row * w + column).copied().unwrap_or(false));
            }
        }
        *slot = 2 * hits >= SAMPLES * SAMPLES;
    }
    Some(Silhouette { filled })
}

#[cfg(test)]
mod tests {
    use omuzaki_store_api::ContentHash;

    use super::*;

    /// A `w` x `h` picture, transparent except where `inside` holds.
    fn picture(w: u32, h: u32, inside: impl Fn(f64, f64) -> bool) -> Rgba8 {
        let mut pixels = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let on = inside(f64::from(x) + 0.5, f64::from(y) + 0.5);
                pixels.extend_from_slice(&if on { [20, 20, 30, 255] } else { [0, 0, 0, 0] });
            }
        }
        Rgba8 { width: w, height: h, pixels, hash: ContentHash::of(b"") }
    }

    /// A stick figure: a round head on a rectangle body, `head` the head's radius.
    fn figure(head: f64, extra: impl Fn(f64, f64) -> bool) -> Rgba8 {
        picture(200, 400, |x, y| {
            let in_head = (x - 100.0).hypot(y - 80.0) <= head;
            let in_body = (60.0..140.0).contains(&x) && (80.0 + head..390.0).contains(&y);
            in_head || in_body || extra(x, y)
        })
    }

    #[test]
    fn the_same_shape_at_another_size_and_place_is_the_same_silhouette() {
        let small = picture(300, 300, |x, y| (x - 60.0).hypot(y - 200.0) <= 40.0);
        let large = picture(500, 400, |x, y| (x - 300.0).hypot(y - 150.0) <= 120.0);
        let (a, b) = (silhouette(&small, Ground::Alpha).unwrap(), silhouette(&large, Ground::Alpha).unwrap());
        assert!(a.overlap(&b) > 0.95, "{}", a.overlap(&b));
        // A disc fills pi/4 of its square.
        assert!((a.fill() - std::f64::consts::FRAC_PI_4).abs() < 0.03, "{}", a.fill());
    }

    #[test]
    fn a_signature_shape_makes_a_character_distinct() {
        let plain = silhouette(&figure(50.0, |_, _| false), Ground::Alpha).unwrap();
        let bigger_head = silhouette(&figure(54.0, |_, _| false), Ground::Alpha).unwrap();
        // A brimmed hat on the same figure: a crown over the head and a brim wider than the shoulders.
        let hat = figure(50.0, |x, y| {
            let crown = (75.0..125.0).contains(&x) && (0.0..30.0).contains(&y);
            let brim = (30.0..170.0).contains(&x) && (22.0..34.0).contains(&y);
            crown || brim
        });
        let hat = silhouette(&hat, Ground::Alpha).unwrap();
        assert!(plain.overlap(&bigger_head) > 0.93, "{}", plain.overlap(&bigger_head));
        assert!(1.0 - plain.overlap(&hat) >= MIN_DISTINCT, "{}", plain.overlap(&hat));
    }

    #[test]
    fn an_opaque_image_is_cut_from_its_background() {
        let mut image = picture(100, 100, |x, y| (x - 50.0).hypot(y - 50.0) <= 30.0);
        for p in image.pixels.as_chunks_mut::<4>().0 {
            if p[3] == 0 {
                *p = [240, 240, 240, 255];
            }
        }
        let ground = Ground::of(&image);
        assert_eq!(ground, Ground::Colour([240, 240, 240]));
        let cut = silhouette(&image, ground).unwrap();
        let disc = silhouette(&picture(100, 100, |x, y| (x - 50.0).hypot(y - 50.0) <= 30.0), Ground::Alpha).unwrap();
        assert_eq!(cut, disc);
        let blank = picture(10, 10, |_, _| false);
        assert!(silhouette(&blank, Ground::Alpha).is_none());
    }
}
