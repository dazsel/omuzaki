//! The painted features as flipbook atlases (#173): every state a feature can show, painted on the CPU raster into one
//! atlas per feature, chosen by texture-transform binds.
//!
//! **Binary-coded cells.** A VRM runtime adds the binds of every active expression: a material's UV offset is its base
//! plus the sum of each expression's offset, times its weight. So the baker gives each channel a feature shows one bit
//! of the cell index, and paints a cell for every combination of bits: cell `k` is the feature painted with exactly the
//! channels whose bit is set in `k`. A channel's bind moves the offset by its bit's place in the grid, so any set of
//! active channels lands on the cell painted for that set: a blink with a wide eye, a smile with an open jaw. The
//! expressions are binary (`isBinary`): a channel shows its cell from weight 0.5 on.
//!
//! **Features.** Each eye and each brow is its own feature, so either side flips on its own, and the mouth is one. The
//! static face (marks and nose) is a feature with one cell, and the cheeks are two cross-faded by a colour bind, at rest
//! and at full blush. The Overlay layer's extras (the sweat drop, the gloom's hatch) are an `effects` feature, made only
//! when an export asks for one of them. A feature's rectangle is the union of what its cells paint, and its patch is the head's triangles
//! over that rectangle (`patch`).
//!
//! **Atlas size.** A feature with `n` bits has `2^n` cells, in a grid `2^ceil(n/2)` wide and `2^floor(n/2)` high, each
//! with a gutter of [`GUTTER`] pixels so filtering never reads a neighbour. The density is the live layer's
//! ([`DEFAULT_PX_PER_M`]) unless the grid would pass the atlas limit (2048 pixels a side unless the request says
//! otherwise); then it is lowered to fit.

use image::{ImageFormat, RgbaImage};
use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, FaceDrawing, FaceError, FaceInput, FaceLayerId, FacePainter, FaceProfile, FaceProjection,
    FaceRasterizer, FaceRect, FaceView, Raster,
};

use crate::painter::{RealPainter, Section};
use crate::raster::SkiaRasterizer;

/// Pixels around each cell copied from the painting beyond the cell's rectangle, so bilinear filtering at a cell's edge
/// reads what is really there.
pub const GUTTER: u32 = 2;
/// The longest atlas side unless the request's budget says otherwise (spec FE-09: 2048 x 2048 for a default head).
pub const MAX_ATLAS_PX: u32 = 2048;
/// The most bits a feature takes: 256 cells.
pub const MAX_BITS: usize = 8;
/// The density a feature's bounds are found at: coarse, since only the union of the cells' ink matters.
const BOUNDS_PX_PER_M: f32 = 600.0;
/// Space kept around a feature's ink, in metres, so a patch never cuts a stroke.
const MARGIN_M: f32 = 0.006;

/// Which half of the face a feature keeps.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Side {
    /// The character's left (+x).
    Left,
    /// The character's right (-x).
    Right,
    /// Both halves.
    Both,
}

impl Side {
    fn keeps(self, x: f32) -> bool {
        match self {
            Self::Left => x >= 0.0,
            Self::Right => x < 0.0,
            Self::Both => true,
        }
    }
}

/// One painted feature: the sections it draws, the half it keeps, and the channels that each own a bit of its cells.
#[derive(Clone, Debug, PartialEq)]
pub struct Feature {
    /// Its name, which names its patch, material and image (`face.<name>`).
    pub name: String,
    /// The painter sections it draws.
    pub sections: Vec<Section>,
    /// The half of the face it keeps.
    pub side: Side,
    /// The channels of its cell bits, lowest bit first; at most [`MAX_BITS`].
    pub bits: Vec<Channel>,
    /// Channel values every cell is painted with on top of the bits (the full blush of the blush cheeks).
    pub held: Vec<(Channel, f32)>,
    /// How far its patch lifts off the skin, in metres: features drawn over others lift further.
    pub lift: f32,
}

/// What the eyes, brows and mouth show by default (`ARKit` channels), lowest bit first.
const EYE_BITS: [[&str; 3]; 2] =
    [["eyeBlinkLeft", "eyeWideLeft", "eyeSquintLeft"], ["eyeBlinkRight", "eyeWideRight", "eyeSquintRight"]];
const BROW_BITS: [[&str; 3]; 2] =
    [["browDownLeft", "browOuterUpLeft", "browInnerUp"], ["browDownRight", "browOuterUpRight", "browInnerUp"]];
const MOUTH_BITS: [&str; 8] = [
    "jawOpen",
    "mouthSmileLeft",
    "mouthSmileRight",
    "mouthFrownLeft",
    "mouthFrownRight",
    "mouthPucker",
    "mouthFunnel",
    "tongueOut",
];
/// The anime extras an eye can carry as more bits, when an export asks for them, per side: as many as fit beside the
/// three base bits, in this order.
const EYE_EXTRAS: [[&str; 7]; 2] = [
    ["eyeArcLeft", "eyesHeart", "eyesStar", "eyesSqueeze", "tears", "eyesGloom", "eyesShock"],
    ["eyeArcRight", "eyesHeart", "eyesStar", "eyesSqueeze", "tears", "eyesGloom", "eyesShock"],
];
/// The extras drawn on the Overlay layer (the sweat drop at the temple, the gloom's band and hatch over the forehead):
/// the bits of the `effects` feature, when an export asks for them.
const EFFECT_EXTRAS: [&str; 2] = ["sweatDrop", "eyesGloom"];

/// Whether `channel` is an eye extra of the feature named `feature` (`eye-left` or `eye-right`).
pub fn is_eye_extra(channel: &str, feature: &str) -> bool {
    let side = usize::from(feature == "eye-right");
    EYE_EXTRAS[side].contains(&channel)
}

/// Whether `channel` is an extra some feature can carry as a bit.
pub fn is_feature_extra(channel: &str) -> bool {
    EYE_EXTRAS.iter().flatten().chain(&EFFECT_EXTRAS).any(|extra| *extra == channel)
}

fn channels(names: &[&str]) -> Vec<Channel> {
    names.iter().filter_map(|n| Channel::from_name(n)).collect()
}

/// The features a bake paints, with the eye extras among `extras` (channel names) added as bits.
pub fn features(extras: &[&str]) -> Vec<Feature> {
    let feature = |name: &str, sections: &[Section], side, bits, lift| Feature {
        name: name.to_owned(),
        sections: sections.to_vec(),
        side,
        bits,
        held: Vec::new(),
        lift,
    };
    let mut out = vec![feature("face", &[Section::Marks, Section::Nose], Side::Both, Vec::new(), 0.000_3)];
    let blush = Channel::from_name("blush").map(|b| vec![(b, 1.0)]).unwrap_or_default();
    out.push(feature("cheeks", &[Section::Cheeks], Side::Both, Vec::new(), 0.000_4));
    out.push(Feature { held: blush, ..feature("blush", &[Section::Cheeks], Side::Both, Vec::new(), 0.000_5) });
    for (side, (name, at)) in [Side::Left, Side::Right].into_iter().zip([("eye-left", 0), ("eye-right", 1)]) {
        let mut bits = channels(&EYE_BITS[at]);
        let wanted: Vec<&str> = EYE_EXTRAS[at].iter().copied().filter(|n| extras.contains(n)).collect();
        bits.extend(channels(&wanted));
        bits.truncate(MAX_BITS);
        out.push(feature(name, &[Section::Eyes], side, bits, 0.000_6));
    }
    for (side, (name, at)) in [Side::Left, Side::Right].into_iter().zip([("brow-left", 0), ("brow-right", 1)]) {
        out.push(feature(name, &[Section::Brows], side, channels(&BROW_BITS[at]), 0.000_9));
    }
    out.push(feature("mouth", &[Section::Mouth], Side::Both, channels(&MOUTH_BITS), 0.000_6));
    let effects: Vec<&str> = EFFECT_EXTRAS.iter().copied().filter(|n| extras.contains(n)).collect();
    if !effects.is_empty() {
        // In front of every other feature, as the Overlay layer is in front of the face.
        out.push(feature("effects", &[Section::Overlay], Side::Both, channels(&effects), 0.001_2));
    }
    out
}

/// Paints a feature's sections for channel buffers at one density: the Base and Overlay layers of each section,
/// composited, kept to the feature's half.
pub(super) struct Painter<'a> {
    profile: &'a FaceProfile,
    face: RealPainter,
    raster: SkiaRasterizer,
    drawing: FaceDrawing,
    layer: Raster,
    /// The face window, which every painting covers.
    window: FaceRect,
    px_per_m: f32,
}

impl<'a> Painter<'a> {
    pub fn new(profile: &'a FaceProfile, projection: FaceProjection, px_per_m: f32) -> Self {
        Self {
            profile,
            face: RealPainter::new(projection, px_per_m),
            raster: SkiaRasterizer::new(),
            drawing: FaceDrawing::new(),
            layer: Raster::new(1, 1),
            window: projection.rect,
            px_per_m,
        }
    }

    /// The feature painted with the channels of `cell`'s bits at their full value: premultiplied sRGB over the face
    /// window, transparent where nothing is drawn and on the half the feature doesn't keep.
    pub fn paint(&mut self, feature: &Feature, cell: usize) -> Result<Raster, FaceError> {
        let mut bus = ChannelBuf::NEUTRAL;
        for &(channel, value) in &feature.held {
            bus.set(channel, value);
        }
        for (bit, &channel) in feature.bits.iter().enumerate() {
            if cell & (1 << bit) != 0 {
                bus.set(channel, channel.range().1);
            }
        }
        let params = self.profile.mapping.apply(&bus);
        let input = FaceInput { profile: self.profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
        let mut out: Option<Raster> = None;
        for &section in &feature.sections {
            self.face.paint_section(section, &input, &mut self.drawing)?;
            for spec in self.face.layers().iter().filter(|s| s.id != FaceLayerId::Brows) {
                self.raster.rasterize(spec, &self.drawing[spec.id], spec.px_per_m, &mut self.layer)?;
                match &mut out {
                    Some(dst) => over(dst, &self.layer),
                    None => out = Some(self.layer.clone()),
                }
            }
        }
        let mut out = out.unwrap_or_else(|| Raster::new(1, 1));
        self.keep_side(&mut out, feature.side);
        Ok(out)
    }

    /// Clears the pixels whose centre is on the half `side` doesn't keep.
    fn keep_side(&self, raster: &mut Raster, side: Side) {
        if side == Side::Both {
            return;
        }
        let width = raster.width as usize;
        for (i, px) in raster.pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            if !side.keeps(self.plane_x(i % width)) {
                *px = [0; 4];
            }
        }
    }

    /// The face-plane `x` of the centre of pixel column `x`.
    fn plane_x(&self, x: usize) -> f32 {
        // A raster is at most a few thousand pixels wide: exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let x = x as f32;
        self.window.x0 + (x + 0.5) / self.px_per_m
    }

    /// The face-plane `y` of the centre of pixel row `y`.
    fn plane_y(&self, y: usize) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let y = y as f32;
        self.window.y1 - (y + 0.5) / self.px_per_m
    }

    /// The face-plane bounds of what `raster` (painted by this painter) inks, or `None` when it is empty.
    pub fn ink_bounds(&self, raster: &Raster) -> Option<FaceRect> {
        let width = raster.width as usize;
        let mut bounds: Option<FaceRect> = None;
        for (i, px) in raster.pixels.as_chunks::<4>().0.iter().enumerate() {
            if px[3] == 0 {
                continue;
            }
            let (x, y) = (self.plane_x(i % width), self.plane_y(i / width));
            let half = 0.5 / self.px_per_m;
            let r = FaceRect { x0: x - half, x1: x + half, y0: y - half, y1: y + half };
            bounds = Some(bounds.map_or(r, |b| union(b, r)));
        }
        bounds
    }

    /// The premultiplied pixel at face-plane point `(x, y)` of `raster`, transparent outside it.
    fn sample(&self, raster: &Raster, x: f32, y: f32) -> [u8; 4] {
        let px = ((x - self.window.x0) * self.px_per_m).floor();
        let py = ((self.window.y1 - y) * self.px_per_m).floor();
        if px < 0.0 || py < 0.0 {
            return [0; 4];
        }
        // Non-negative and bounded by the raster's size below.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (px, py) = (px as usize, py as usize);
        if px >= raster.width as usize || py >= raster.height as usize {
            return [0; 4];
        }
        let at = (py * raster.width as usize + px) * 4;
        raster.pixels.get(at..at + 4).and_then(|s| s.try_into().ok()).unwrap_or([0; 4])
    }
}

/// The smallest rectangle holding both.
pub(super) fn union(a: FaceRect, b: FaceRect) -> FaceRect {
    FaceRect { x0: a.x0.min(b.x0), x1: a.x1.max(b.x1), y0: a.y0.min(b.y0), y1: a.y1.max(b.y1) }
}

/// Premultiplied `src` over `dst`, both the same size (they come from one painter).
fn over(dst: &mut Raster, src: &Raster) {
    for (d, s) in dst.pixels.as_chunks_mut::<4>().0.iter_mut().zip(src.pixels.as_chunks::<4>().0) {
        let keep = 255 - u32::from(s[3]);
        for (d, s) in d.iter_mut().zip(s) {
            *d = u8::try_from(u32::from(*s) + (u32::from(*d) * keep + 127) / 255).unwrap_or(u8::MAX);
        }
    }
}

/// The rectangle a feature's cells ink, with [`MARGIN_M`] around it; `None` when no cell draws anything (a face
/// without marks).
pub(super) fn bounds(
    profile: &FaceProfile,
    projection: FaceProjection,
    feature: &Feature,
) -> Result<Option<FaceRect>, FaceError> {
    let mut painter = Painter::new(profile, projection, BOUNDS_PX_PER_M);
    let mut found: Option<FaceRect> = None;
    for cell in 0..(1_usize << feature.bits.len()) {
        let raster = painter.paint(feature, cell)?;
        if let Some(b) = painter.ink_bounds(&raster) {
            found = Some(found.map_or(b, |f| union(f, b)));
        }
    }
    Ok(found.map(|b| FaceRect { x0: b.x0 - MARGIN_M, x1: b.x1 + MARGIN_M, y0: b.y0 - MARGIN_M, y1: b.y1 + MARGIN_M }))
}

/// A feature's atlas: the image, its grid, and where cell 0's rectangle sits in UV space.
#[derive(Clone, Debug, PartialEq)]
pub struct Atlas {
    /// The cells, straight-alpha sRGB.
    pub image: RgbaImage,
    /// Cells across and down.
    pub grid: (u32, u32),
    /// One cell's size in pixels, gutters included.
    pub cell_px: (u32, u32),
    /// The face-plane rectangle a cell shows.
    pub rect: FaceRect,
    /// The density the cells were painted at.
    pub px_per_m: f32,
}

impl Atlas {
    /// The atlas UV of face-plane point `q` in cell 0.
    pub fn uv(&self, q: omuzaki_core::glam::Vec2) -> omuzaki_core::glam::Vec2 {
        let gutter = to_f32(GUTTER);
        let across = (gutter + (q.x - self.rect.x0) * self.px_per_m) / to_f32(self.image.width());
        let down = (gutter + (self.rect.y1 - q.y) * self.px_per_m) / to_f32(self.image.height());
        omuzaki_core::glam::Vec2::new(across, down)
    }

    /// How far bit `bit` moves the UV offset: one column or row step of its place in the grid.
    pub fn bit_offset(&self, bit: usize) -> omuzaki_core::glam::Vec2 {
        let across = self.grid.0.trailing_zeros() as usize;
        let (w, h) = (to_f32(self.image.width()), to_f32(self.image.height()));
        if bit < across {
            omuzaki_core::glam::Vec2::new(to_f32(self.cell_px.0 << bit) / w, 0.0)
        } else {
            omuzaki_core::glam::Vec2::new(0.0, to_f32(self.cell_px.1 << (bit - across)) / h)
        }
    }

    /// The image as PNG bytes. Deterministic: the same pixels give the same bytes.
    ///
    /// # Errors
    /// The encoder's error, as text.
    pub fn png(&self) -> Result<Vec<u8>, String> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        self.image.write_to(&mut bytes, ImageFormat::Png).map_err(|e| e.to_string())?;
        Ok(bytes.into_inner())
    }
}

/// Pixel counts and sizes are at most a few thousand: exact in f32.
#[allow(clippy::cast_precision_loss)]
fn to_f32(n: u32) -> f32 {
    n as f32
}

/// Paints every cell of `feature` over `rect` into one atlas no larger than `max_px` a side.
///
/// # Errors
/// What the painter or the rasteriser returns.
pub(super) fn atlas(
    profile: &FaceProfile,
    projection: FaceProjection,
    feature: &Feature,
    rect: FaceRect,
    max_px: u32,
) -> Result<Atlas, FaceError> {
    let n = feature.bits.len();
    let grid = (1_u32 << n.div_ceil(2), 1_u32 << (n / 2));
    let fit = |cells: u32, metres: f32| (to_f32(max_px / cells) - to_f32(2 * GUTTER) - 1.0) / metres.max(1e-4);
    let px_per_m = DEFAULT_PX_PER_M.min(fit(grid.0, rect.width())).min(fit(grid.1, rect.height())).max(1.0);
    // At most `max_px` by construction.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let inner = |metres: f32| (metres * px_per_m).ceil().max(1.0) as u32;
    let cell_px = (inner(rect.width()) + 2 * GUTTER, inner(rect.height()) + 2 * GUTTER);
    let mut image = RgbaImage::new(grid.0 * cell_px.0, grid.1 * cell_px.1);
    let mut painter = Painter::new(profile, projection, px_per_m);
    let gutter = to_f32(GUTTER);
    for cell in 0..(1_usize << n) {
        let raster = painter.paint(feature, cell)?;
        let index = u32::try_from(cell).unwrap_or(u32::MAX);
        let mask = grid.0 - 1;
        let (cx, cy) = (index & mask, index >> grid.0.trailing_zeros());
        for j in 0..cell_px.1 {
            for i in 0..cell_px.0 {
                let x = rect.x0 + (to_f32(i) - gutter + 0.5) / px_per_m;
                let y = rect.y1 - (to_f32(j) - gutter + 0.5) / px_per_m;
                let px = unpremultiply(painter.sample(&raster, x, y));
                image.put_pixel(cx * cell_px.0 + i, cy * cell_px.1 + j, image::Rgba(px));
            }
        }
    }
    Ok(Atlas { image, grid, cell_px, rect, px_per_m })
}

/// Straight alpha from premultiplied, as PNG stores it.
fn unpremultiply(p: [u8; 4]) -> [u8; 4] {
    let a = u32::from(p[3]);
    if a == 0 {
        return [0; 4];
    }
    let c = |v: u8| u8::try_from(((u32::from(v) * 255 + a / 2) / a).min(255)).unwrap_or(u8::MAX);
    [c(p[0]), c(p[1]), c(p[2]), p[3]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_feature_fits_its_bits_and_the_default_ones_are_all_channels() {
        let all = features(&[]);
        let names: Vec<&str> = all.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["face", "cheeks", "blush", "eye-left", "eye-right", "brow-left", "brow-right", "mouth"],
            "the patch order"
        );
        let bits: usize = all.iter().map(|f| f.bits.len()).sum();
        assert_eq!(bits, 3 + 3 + 3 + 3 + 8);
        let extras = features(&["eyesHeart", "eyeArcLeft"]);
        let left = extras.iter().find(|f| f.name == "eye-left").map(|f| f.bits.len());
        let right = extras.iter().find(|f| f.name == "eye-right").map(|f| f.bits.len());
        assert_eq!((left, right), (Some(5), Some(4)));
    }

    #[test]
    fn unpremultiply_restores_straight_colour() {
        assert_eq!(unpremultiply([64, 32, 0, 128]), [128, 64, 0, 128]);
        assert_eq!(unpremultiply([0, 0, 0, 0]), [0, 0, 0, 0]);
    }
}
