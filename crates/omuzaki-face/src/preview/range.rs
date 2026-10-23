//! The expression range test (FE-04): paint every expression of a character, see each at the sizes a viewer meets it
//! (an emote, a small stream, a large one) and flag the pairs that read the same.
//!
//! Every expression is painted once, at full weight over the character's resting buffer, with the face window
//! [`PAINT_WIDTH_PX`] wide, then cropped and scaled down so the face window would be `size` pixels wide: a 28 px emote
//! of the head sees the features that small. A viewer reads a face by its eyes and its mouth, so three regions are
//! compared, the eyes with the brows and the cheeks under them, the mouth, and the whole face window (for what sits
//! elsewhere: a sweat drop at the temple, the gloom over the forehead); a pair's **similarity** is the lowest of the
//! regions' structural similarity (SSIM), so a pair that differs in any region reads as different. Comparing
//! the whole face instead would let a small anime mouth vanish in the skin around it. Two expressions are **too
//! similar** at a size when their similarity is above the threshold; an expression is **neutral** when it is too
//! similar to the resting face at the largest size. Visemes are mouth shapes for lip sync and are compared only with
//! each other.
//!
//! **Distinct** at a size is a greedy count over the expressions in name order: one counts when it is not too similar
//! to the rest face or to any expression already counted, so the count is a lower bound on how many a viewer tells
//! apart and does not depend on the order an agent wrote them in.

use image::RgbaImage;
use omuzaki_anim_api::ChannelBuf;
use omuzaki_face_api::{ExpressionKind, FaceError, FaceRect, narrow};
use serde::Serialize;

use super::metrics::{downsample, luma_l2, ssim_worst};
use super::{FaceSubject, Preview};

/// The width the face window is painted at before cropping and scaling down: twice the largest default size, so even
/// the largest is a clean downsample.
pub const PAINT_WIDTH_PX: u32 = 1024;
/// The default sizes: an emote, a small and a large stream face, a close-up.
pub const DEFAULT_SIZES: [u32; 4] = [28, 56, 112, 512];
/// The default similarity above which two expressions read the same, measured on the default face: a pair whose only
/// difference is both outer brows raised by a tenth scores above it at 56 px, the starter moods score well below it.
pub const DEFAULT_SIMILAR: f64 = 0.94;

/// The share of the least similar windows the whole-window region is judged by: something small away from the eyes and
/// the mouth (a sweat drop, a vein) changes only a few windows of the whole face.
const WINDOW_WORST: f64 = 0.03;

/// What to test.
#[derive(Clone, Debug, PartialEq)]
pub struct RangeOptions {
    /// Face-window widths to compare at, pixels.
    pub sizes: Vec<u32>,
    /// The SSIM above which a pair is too similar.
    pub similar: f64,
}

impl Default for RangeOptions {
    fn default() -> Self {
        Self { sizes: DEFAULT_SIZES.to_vec(), similar: DEFAULT_SIMILAR }
    }
}

/// One expression's crops at every size of [`RangeOptions::sizes`], in that order.
pub struct Painted {
    /// The expression, or `rest` for the resting face.
    pub name: String,
    /// Its kind; the rest face counts as an emotion.
    pub kind: ExpressionKind,
    /// The features (brows to below the mouth), for contact sheets.
    pub crops: Vec<RgbaImage>,
    /// The eyes with the brows and the cheeks under them.
    pub eyes: Vec<RgbaImage>,
    /// The mouth.
    pub mouth: Vec<RgbaImage>,
    /// The whole face window.
    pub window: Vec<RgbaImage>,
}

impl Painted {
    /// How alike `self` and `other` read at size index `s`: the lowest of the regions' SSIM, and the largest of their
    /// luma distances.
    fn likeness(&self, other: &Self, s: usize) -> Option<(f64, f64)> {
        let mut score = 1.0f64;
        let mut distance = 0.0f64;
        for (mine, theirs, share) in [
            (&self.eyes, &other.eyes, 1.0),
            (&self.mouth, &other.mouth, 1.0),
            (&self.window, &other.window, WINDOW_WORST),
        ] {
            let (a, b) = (mine.get(s)?, theirs.get(s)?);
            score = score.min(ssim_worst(a, b, share));
            distance = distance.max(luma_l2(a, b));
        }
        Some((score, distance))
    }
}

/// A pair that reads the same.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SimilarPair {
    /// The two expressions, in name order (`rest` for the resting face).
    pub a: String,
    /// See [`a`](Self::a).
    pub b: String,
    /// The face-window width it was measured at.
    pub size: u32,
    /// Their structural similarity there.
    pub ssim: f64,
    /// Their luma distance there.
    pub luma_l2: f64,
    /// How to tell them apart.
    pub hint: &'static str,
}

/// One size's verdict.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SizeReport {
    /// The face-window width, pixels.
    pub size: u32,
    /// How many expressions (not visemes) read as distinct, counted greedily (see the module docs).
    pub distinct: usize,
    /// The darkest luma of the eyes against the skin's (`(skin - eye) / skin`): how well the eyes hold up.
    pub eye_contrast: f64,
    /// The same for the mouth, at rest.
    pub mouth_contrast: f64,
}

/// The whole test.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RangeReport {
    /// One entry per size.
    pub sizes: Vec<SizeReport>,
    /// Every pair too similar at some size, smallest size first.
    pub similar: Vec<SimilarPair>,
    /// The expressions that read as the resting face at the largest size.
    pub neutral: Vec<String>,
}

/// The features' crop of the face window, in face-plane metres: the eyes' outer corners with a margin, from above the
/// brows to below the mouth.
fn features(subject: &FaceSubject) -> FaceRect {
    let layout = &subject.profile.layout;
    let (eye_y, mouth_y) = (narrow(layout.eye_y), narrow(layout.mouth_y));
    let half = narrow(layout.eye_spacing + 2.2 * layout.eye_width);
    let span = (eye_y - mouth_y).max(0.02);
    let window = subject.projection.rect;
    FaceRect {
        x0: (-half).max(window.x0),
        x1: half.min(window.x1),
        y0: (mouth_y - 0.8 * span).max(window.y0),
        y1: (eye_y + narrow(layout.brow_gap) + 0.6 * span).min(window.y1),
    }
}

/// The eyes' region: both eyes with a margin, from the brows' top down to the blush under them, in face-plane metres.
pub fn eyes_region(subject: &FaceSubject) -> FaceRect {
    let layout = &subject.profile.layout;
    let (eye_y, eye_w) = (narrow(layout.eye_y), narrow(layout.eye_width));
    let half = narrow(layout.eye_spacing) + 1.7 * eye_w;
    clip(
        FaceRect { x0: -half, x1: half, y0: eye_y - 1.3 * eye_w, y1: eye_y + narrow(layout.brow_gap) + 0.6 * eye_w },
        subject,
    )
}

/// The mouth's region: room for its widest open shape and a tongue below it, in face-plane metres.
pub fn mouth_region(subject: &FaceSubject) -> FaceRect {
    let (layout, mouth) = (&subject.profile.layout, &subject.profile.mouth);
    let mouth_y = narrow(layout.mouth_y);
    let half = (narrow(mouth.half_width * mouth.open_scale.max(mouth.closed_scale)) * 2.6).max(0.02);
    clip(FaceRect { x0: -half, x1: half, y0: mouth_y - 1.6 * half, y1: mouth_y + 0.9 * half }, subject)
}

/// `rect` inside the face window.
fn clip(rect: FaceRect, subject: &FaceSubject) -> FaceRect {
    let window = subject.projection.rect;
    FaceRect {
        x0: rect.x0.max(window.x0),
        x1: rect.x1.min(window.x1),
        y0: rect.y0.max(window.y0),
        y1: rect.y1.min(window.y1),
    }
}

/// `picture` (the whole face window, `PAINT_WIDTH_PX` wide) cropped to `crop`.
fn cropped(picture: &RgbaImage, window: FaceRect, crop: FaceRect) -> RgbaImage {
    #[allow(clippy::cast_precision_loss)] // the width is at most a few thousand
    let px_per_m = picture.width() as f32 / window.width().max(f32::EPSILON);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to the picture below
    let at = |m: f32| (m * px_per_m).round().max(0.0) as u32;
    let x = at(crop.x0 - window.x0).min(picture.width().saturating_sub(1));
    let y = at(window.y1 - crop.y1).min(picture.height().saturating_sub(1));
    let w = at(crop.x1 - crop.x0).clamp(1, picture.width() - x);
    let h = at(crop.y1 - crop.y0).clamp(1, picture.height() - y);
    image::imageops::crop_imm(picture, x, y, w, h).to_image()
}

/// Paints the rest face and every expression of the subject's own set, cropped to the features at each size.
/// `cancelled` is polled between expressions; `None` means it was cancelled.
///
/// # Errors
/// What the painter or the rasteriser returns.
pub fn paint_all(
    subject: &FaceSubject,
    options: &RangeOptions,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Vec<Painted>>, FaceError> {
    let window = subject.projection.rect;
    let (crop, eyes, mouth) = (features(subject), eyes_region(subject), mouth_region(subject));
    let rest = subject.profile.rest_buf();
    let mut preview = Preview::new(subject.clone(), PAINT_WIDTH_PX);
    let scale = |picture: &RgbaImage, size: u32| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small positive width
        let width = (f64::from(picture.width()) * f64::from(size) / f64::from(PAINT_WIDTH_PX)).round().max(1.0) as u32;
        downsample(picture, width)
    };
    let mut paint = |name: String, kind: ExpressionKind, bus: &ChannelBuf| -> Result<Painted, FaceError> {
        preview.paint(bus)?;
        let picture = preview.picture();
        let at_sizes = |rect: FaceRect| -> Vec<RgbaImage> {
            let region = cropped(&picture, window, rect);
            options.sizes.iter().map(|&size| scale(&region, size)).collect()
        };
        Ok(Painted {
            name,
            kind,
            crops: at_sizes(crop),
            eyes: at_sizes(eyes),
            mouth: at_sizes(mouth),
            window: at_sizes(window),
        })
    };
    let mut out = vec![paint("rest".to_owned(), ExpressionKind::Emotion, &rest)?];
    for (id, expression) in &subject.profile.expressions.0 {
        if cancelled() {
            return Ok(None);
        }
        let bus = subject.profile.expressions.blend(&rest, &[(id, 1.0)]).unwrap_or(rest);
        out.push(paint(id.to_string(), expression.kind, &bus)?);
    }
    Ok(Some(out))
}

/// Whether two expressions are compared: visemes only with visemes, everything else (the rest face included) with
/// everything else.
fn comparable(a: ExpressionKind, b: ExpressionKind) -> bool {
    (a == ExpressionKind::Viseme) == (b == ExpressionKind::Viseme)
}

/// The fix hint for a look-alike pair.
fn hint(a: &Painted, b: &Painted) -> &'static str {
    if a.name == "rest" || b.name == "rest" {
        "it reads as the resting face: push its channels further or add an extra (blush, sparkle, tears)"
    } else if a.kind == ExpressionKind::Viseme {
        "widen the mouth shapes: give them different openings, widths or roundness"
    } else {
        "make the eyes or the mouth differ, add an extra, or tune the per-channel gains"
    }
}

/// Compares every comparable pair at every size and counts what reads as distinct.
pub fn compare(painted: &[Painted], options: &RangeOptions, subject: &FaceSubject) -> RangeReport {
    let mut similar = Vec::new();
    let mut sizes = Vec::with_capacity(options.sizes.len());
    for (s, &size) in options.sizes.iter().enumerate() {
        let mut too_close = vec![vec![false; painted.len()]; painted.len()];
        for (i, a) in painted.iter().enumerate() {
            for (j, b) in painted.iter().enumerate().skip(i + 1) {
                if !comparable(a.kind, b.kind) {
                    continue;
                }
                let Some((score, distance)) = a.likeness(b, s) else { continue };
                if score > options.similar {
                    too_close[i][j] = true;
                    too_close[j][i] = true;
                    let (first, second) = if a.name <= b.name { (a, b) } else { (b, a) };
                    similar.push(SimilarPair {
                        a: first.name.clone(),
                        b: second.name.clone(),
                        size,
                        ssim: round4(score),
                        luma_l2: round4(distance),
                        hint: hint(a, b),
                    });
                }
            }
        }
        // Greedy: the rest face is always kept, then each non-viseme in name order if it is apart from all kept.
        let mut kept = vec![0usize];
        for (i, p) in painted.iter().enumerate().skip(1) {
            if p.kind != ExpressionKind::Viseme && kept.iter().all(|&k| !too_close[i][k]) {
                kept.push(i);
            }
        }
        let rest = painted.first().and_then(|p| p.crops.get(s));
        sizes.push(SizeReport {
            size,
            distinct: kept.len() - 1,
            eye_contrast: rest.map_or(0.0, |r| round4(band_contrast(r, subject, Band::Eyes))),
            mouth_contrast: rest.map_or(0.0, |r| round4(band_contrast(r, subject, Band::Mouth))),
        });
    }
    let last = options.sizes.len().saturating_sub(1);
    let largest = options.sizes.get(last).copied();
    let neutral = similar
        .iter()
        .filter(|p| Some(p.size) == largest && (p.a == "rest" || p.b == "rest"))
        .map(|p| if p.a == "rest" { p.b.clone() } else { p.a.clone() })
        .collect();
    RangeReport { sizes, similar, neutral }
}

/// Four decimals: enough to read, stable to print.
fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// A horizontal band of the features crop.
#[derive(Copy, Clone)]
enum Band {
    Eyes,
    Mouth,
}

/// `(skin - darkest) / skin` in luma over a band of `crop`: how dark the band's darkest pixel reads against the skin.
fn band_contrast(crop: &RgbaImage, subject: &FaceSubject, band: Band) -> f64 {
    let rect = features(subject);
    let layout = &subject.profile.layout;
    let centre = match band {
        Band::Eyes => narrow(layout.eye_y),
        Band::Mouth => narrow(layout.mouth_y),
    };
    let half = 0.35 * narrow(layout.eye_width) * 2.0;
    let (h, height_m) = (crop.height(), rect.y1 - rect.y0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // clamped rows
    let row = |m: f32| (((rect.y1 - m) / height_m) * h as f32).round().clamp(0.0, h as f32) as u32;
    let (top, bottom) = (row(centre + half), row(centre - half).max(row(centre + half) + 1).min(h));
    let luma = |p: &image::Rgba<u8>| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
    let skin = subject.skin;
    let skin_luma = luma(&image::Rgba([skin.r, skin.g, skin.b, 255])).max(1.0);
    let darkest = (top..bottom)
        .flat_map(|y| (0..crop.width()).map(move |x| (x, y)))
        .map(|(x, y)| luma(crop.get_pixel(x, y)))
        .fold(skin_luma, f64::min);
    (skin_luma - darkest) / skin_luma
}
