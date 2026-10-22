//! The synthetic faces of the likeness design's Appendix A (task T2): three faces drawn in code, in a render-like style
//! and a drawing style, at any size, roll and sub-pixel offset, with their analytic outline as the truth.
//!
//! Geometry is in head-local metres, +y up, front-on; the character's right is the image's left. Each shape is a
//! polygon or a run of round-capped segments, point-sampled at 4× and box-filtered to the shot's size, so the edges are
//! anti-aliased the way a renderer's or a drawing program's are.

#![allow(clippy::many_single_char_names, reason = "geometry: points, radii and colour channels")]

use std::f64::consts::PI;

use ::image::ImageEncoder;
use ::image::codecs::jpeg::JpegEncoder;
use omuzaki_measure::face::outline::{ANGLES, angle};
use omuzaki_measure::image::{Rgba8, decode};
use omuzaki_store_api::ContentHash;

/// The cranium's front ellipse (`Cranium::default()`): half-width, up, down, metres from the head origin.
const CRANIUM: (f64, f64, f64) = (0.186, 0.186, 0.197);

/// The blend from the jaw into the cranium, metres above `top`.
const BLEND: f64 = 0.03;

/// Supersampling per side.
const SS: usize = 4;

/// One synthetic face: the outline and the layout of Appendix A.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SynthFace {
    /// The jaw's half-width at `top`, metres.
    pub half_width: f64,
    /// Where the jaw starts, metres.
    pub top: f64,
    /// The jaw's height from `top` to the chin, metres.
    pub height: f64,
    /// The jaw's superellipse exponent.
    pub exponent: f64,
    /// The eye line.
    pub eye_y: f64,
    /// Half the distance between the eye centres.
    pub eye_spacing: f64,
    /// An eye's width.
    pub eye_width: f64,
    /// The nose tick.
    pub nose_y: f64,
    /// The mouth's top edge.
    pub mouth_y: f64,
    /// The blush.
    pub blush_y: f64,
}

/// Face A: the kit's default head and layout.
pub const FACE_A: SynthFace = SynthFace {
    half_width: 0.15,
    top: -0.10,
    height: 0.10,
    exponent: 1.9,
    eye_y: -0.09,
    eye_spacing: 0.088,
    eye_width: 0.05,
    nose_y: -0.115,
    mouth_y: -0.15,
    blush_y: -0.13,
};

/// Face B: narrower, with a long, square lower face.
pub const FACE_B: SynthFace = SynthFace {
    half_width: 0.142,
    top: -0.095,
    height: 0.135,
    exponent: 2.15,
    eye_y: -0.09,
    eye_spacing: 0.088,
    eye_width: 0.05,
    nose_y: -0.125,
    mouth_y: -0.17,
    blush_y: -0.14,
};

/// Face C: wider, rounder, with wider-set eyes.
pub const FACE_C: SynthFace = SynthFace {
    half_width: 0.155,
    top: -0.105,
    height: 0.115,
    exponent: 1.82,
    eye_y: -0.09,
    eye_spacing: 0.092,
    eye_width: 0.058,
    nose_y: -0.122,
    mouth_y: -0.158,
    blush_y: -0.13,
};

/// The graded edits of the design's 9.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// The lower face 4% longer per step.
    LowerFace,
    /// The jaw 2% wider per step.
    JawWidth,
    /// The jaw's exponent +0.08 per step (squarer).
    JawSquare,
    /// The mouth 0.002 m lower per step.
    Mouth,
    /// The eyes 1.5% further apart per step.
    EyeSpacing,
}

impl SynthFace {
    /// The distance between the eye centres, metres.
    #[must_use]
    pub fn iod(&self) -> f64 {
        2.0 * self.eye_spacing
    }

    /// The chin's height.
    #[must_use]
    pub fn chin_y(&self) -> f64 {
        self.top - self.height
    }

    /// This face after `steps` of `edit`.
    #[must_use]
    pub fn edited(mut self, edit: Edit, steps: f64) -> Self {
        match edit {
            Edit::LowerFace => self.height *= 1.0 + 0.04 * steps,
            Edit::JawWidth => self.half_width *= 1.0 + 0.02 * steps,
            Edit::JawSquare => self.exponent += 0.08 * steps,
            Edit::Mouth => self.mouth_y -= 0.002 * steps,
            Edit::EyeSpacing => self.eye_spacing *= 1.0 + 0.015 * steps,
        }
        self
    }

    /// The cranium ellipse's half-width at `y`.
    fn cranium(y: f64) -> f64 {
        let (w, up, down) = CRANIUM;
        let k = if y >= 0.0 { y / up } else { y / down };
        w * (1.0 - k * k).max(0.0).sqrt()
    }

    /// The silhouette's half-width at `y`, or `None` above the head or below the chin.
    #[must_use]
    pub fn half_width_at(&self, y: f64) -> Option<f64> {
        if y < self.chin_y() || y > CRANIUM.1 {
            return None;
        }
        if y <= self.top {
            let t = (self.top - y) / self.height;
            let p = self.exponent;
            return Some(self.half_width * (1.0 - t.powf(p)).max(0.0).powf(1.0 / p));
        }
        if y < self.top + BLEND {
            let s = (y - self.top) / BLEND;
            let s = s * s * (3.0 - 2.0 * s);
            return Some((1.0 - s) * self.half_width + s * Self::cranium(y));
        }
        Some(Self::cranium(y))
    }

    /// The silhouette's right half (x ≥ 0) from the chin up to the crown, dense where it curves.
    fn right_side(&self) -> Vec<[f64; 2]> {
        let mut out = Vec::new();
        let p = self.exponent;
        // The jaw as a superellipse, parametrised by angle so the chin's vertical tangent is sampled finely.
        for i in 0..=240 {
            let theta = PI / 2.0 * (1.0 - f64::from(i) / 240.0);
            let (s, c) = theta.sin_cos();
            let t = s.powf(2.0 / p);
            let x = self.half_width * c.powf(2.0 / p);
            out.push([x, self.top - t * self.height]);
        }
        for i in 1..60 {
            let y = self.top + BLEND * f64::from(i) / 60.0;
            out.push([self.half_width_at(y).unwrap_or(0.0), y]);
        }
        let (w, up, down) = CRANIUM;
        let start = ((self.top + BLEND) / down).asin();
        for i in 0..=200 {
            let a = start + (PI / 2.0 - start) * f64::from(i) / 200.0;
            let y = if a < 0.0 { down * a.sin() } else { up * a.sin() };
            out.push([w * a.cos(), y]);
        }
        out
    }

    /// The whole silhouette as a closed polygon.
    fn silhouette(&self) -> Vec<[f64; 2]> {
        let right = self.right_side();
        let mut out = right.clone();
        out.extend(right.iter().rev().map(|&[x, y]| [-x, y]));
        out
    }

    /// The analytic outline along the measure's 85 rays from the eye midpoint, IOD: the truth a measurement is compared
    /// with.
    #[must_use]
    pub fn truth_profile(&self) -> [f64; ANGLES] {
        std::array::from_fn(|k| {
            let phi = angle(k).to_radians();
            // Image down is head -y; the measure's u is the image's +x.
            let dir = [phi.cos(), -phi.sin()];
            let inside = |r: f64| {
                let (x, y) = (r * dir[0], self.eye_y + r * dir[1]);
                self.half_width_at(y).is_some_and(|w| x.abs() < w)
            };
            let (mut lo, mut hi) = (0.0, 0.5);
            for _ in 0..60 {
                let mid = f64::midpoint(lo, hi);
                if inside(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo / self.iod()
        })
    }
}

/// The two styles of Appendix A.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// A thin outline in a skin-palette colour, a closed mouth, hair behind the face.
    RenderLike,
    /// Dark tapered lines, ears, hatched blush, an open mouth, a side lock over the right cheek, JPEG quality 80.
    Drawing,
}

/// One style's colours and line.
struct Palette {
    skin: [u8; 3],
    shade: [u8; 3],
    line: [u8; 3],
    hair: [u8; 3],
    /// The outline's width, fraction of IOD.
    line_width: f64,
    iris: [u8; 3],
    lash: [u8; 3],
    shirt: [u8; 3],
}

impl Style {
    fn palette(self) -> Palette {
        match self {
            Self::RenderLike => Palette {
                skin: [0xf7, 0xdc, 0xcb],
                shade: [0xe9, 0xbd, 0xa8],
                line: [0xb0, 0x78, 0x68],
                hair: [0x4b, 0x5a, 0x8c],
                line_width: 0.012,
                iris: [0x5b, 0x6e, 0xa8],
                lash: [0x3a, 0x2a, 0x34],
                shirt: [0xe4, 0xe8, 0xf2],
            },
            Self::Drawing => Palette {
                skin: [0xff, 0xe6, 0xd8],
                shade: [0xf0, 0xc4, 0xb2],
                line: [0x3b, 0x2a, 0x2a],
                hair: [0x5c, 0x3b, 0x2e],
                line_width: 0.026,
                iris: [0x6a, 0x46, 0x32],
                lash: [0x2a, 0x1c, 0x1c],
                shirt: [0xfa, 0xfa, 0xfa],
            },
        }
    }
}

/// How a face is shot: its IOD in pixels, its roll about the eye midpoint and a sub-pixel offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shot {
    /// The eye centres' distance, pixels.
    pub iod_px: f64,
    /// Roll, degrees, clockwise on screen.
    pub roll_deg: f64,
    /// Added to the eye midpoint's position, pixels.
    pub offset: [f64; 2],
}

impl Shot {
    /// A level, centred shot.
    #[must_use]
    pub fn new(iod_px: f64) -> Self {
        Self { iod_px, roll_deg: 0.0, offset: [0.0, 0.0] }
    }
}

/// A drawn face and where its landmarks truly are.
#[derive(Clone, Debug)]
pub struct Drawn {
    /// The image.
    pub image: Rgba8,
    /// The image size.
    size: [f64; 2],
    shot: Shot,
    face: SynthFace,
}

impl Drawn {
    /// The image point of the head-local point `[x, y]`.
    #[must_use]
    pub fn to_px(&self, p: [f64; 2]) -> [f64; 2] {
        to_px(&self.face, &self.shot, self.size, p)
    }

    /// The character's right eye's centre, pixels.
    #[must_use]
    pub fn eye_right(&self) -> [f64; 2] {
        self.to_px([-self.face.eye_spacing, self.face.eye_y])
    }

    /// The character's left eye's centre, pixels.
    #[must_use]
    pub fn eye_left(&self) -> [f64; 2] {
        self.to_px([self.face.eye_spacing, self.face.eye_y])
    }

    /// The mouth's centre on its top edge, pixels.
    #[must_use]
    pub fn mouth(&self) -> [f64; 2] {
        self.to_px([0.0, self.face.mouth_y])
    }

    /// The shot's IOD, pixels.
    #[must_use]
    pub fn iod_px(&self) -> f64 {
        self.shot.iod_px
    }

    /// The nose tick, pixels.
    #[must_use]
    pub fn nose(&self) -> [f64; 2] {
        self.to_px([0.0, self.face.nose_y])
    }
}

/// The image size for a shot: the head and hair with room for roll, the eye midpoint at a fixed place.
fn size_of(shot: &Shot) -> [f64; 2] {
    [(3.2 * shot.iod_px).ceil(), (3.6 * shot.iod_px).ceil()]
}

fn to_px(face: &SynthFace, shot: &Shot, size: [f64; 2], [x, y]: [f64; 2]) -> [f64; 2] {
    let s = shot.iod_px / face.iod();
    let (lx, ly) = (x * s, -(y - face.eye_y) * s);
    let (sn, c) = shot.roll_deg.to_radians().sin_cos();
    let centre = [size[0] / 2.0 + shot.offset[0], size[1] * 0.5 + shot.offset[1]];
    [centre[0] + c * lx - sn * ly, centre[1] + sn * lx + c * ly]
}

/// The canvas at 4×: sRGB colours as floats, one per sample.
struct Canvas {
    width: usize,
    height: usize,
    px: Vec<[f32; 3]>,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self { width, height, px: vec![[255.0; 3]; width * height] }
    }

    fn blend(&mut self, i: usize, rgb: [u8; 3], alpha: f32) {
        let p = &mut self.px[i];
        for c in 0..3 {
            p[c] = p[c] * (1.0 - alpha) + f32::from(rgb[c]) * alpha;
        }
    }

    /// The sample rows and columns a box in canvas units covers.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to the canvas
    fn span(&self, lo: [f64; 2], hi: [f64; 2]) -> ((usize, usize), (usize, usize)) {
        let clamp = |v: f64, n: usize| (v.max(0.0) as usize).min(n);
        let x = (clamp(lo[0] - 0.5, self.width), clamp(hi[0] + 1.5, self.width));
        let y = (clamp(lo[1] - 0.5, self.height), clamp(hi[1] + 1.5, self.height));
        (x, y)
    }

    /// Fills a polygon (canvas units) with the non-zero rule.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn fill(&mut self, poly: &[[f64; 2]], rgb: [u8; 3], alpha: f32) {
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        for p in poly {
            for a in 0..2 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        let ((x0, x1), (y0, y1)) = self.span(lo, hi);
        let mut crossings: Vec<(f64, i32)> = Vec::new();
        for row in y0..y1 {
            let sy = row as f64 + 0.5;
            crossings.clear();
            for (i, a) in poly.iter().enumerate() {
                let b = poly[(i + 1) % poly.len()];
                if (a[1] <= sy) != (b[1] <= sy) {
                    let t = (sy - a[1]) / (b[1] - a[1]);
                    crossings.push((a[0] + t * (b[0] - a[0]), if b[1] > a[1] { 1 } else { -1 }));
                }
            }
            crossings.sort_by(|p, q| p.0.total_cmp(&q.0));
            let mut winding = 0;
            for pair in crossings.windows(2) {
                winding += pair[0].1;
                if winding == 0 {
                    continue;
                }
                // Samples whose centre lies in [pair[0].0, pair[1].0).
                let from = ((pair[0].0 - 0.5).ceil().max(x0 as f64)) as usize;
                let to = ((pair[1].0 - 0.5).ceil().min(x1 as f64)).max(0.0) as usize;
                for col in from..to.max(from) {
                    self.blend(row * self.width + col, rgb, alpha);
                }
            }
        }
    }

    /// A round-capped segment from `a` to `b` whose radius goes from `ra` to `rb` (canvas units).
    #[allow(clippy::cast_precision_loss)]
    fn capsule(&mut self, a: [f64; 2], b: [f64; 2], ra: f64, rb: f64, rgb: [u8; 3]) {
        let r = ra.max(rb);
        let lo = [a[0].min(b[0]) - r, a[1].min(b[1]) - r];
        let hi = [a[0].max(b[0]) + r, a[1].max(b[1]) + r];
        let ((x0, x1), (y0, y1)) = self.span(lo, hi);
        let d = [b[0] - a[0], b[1] - a[1]];
        let len2 = (d[0] * d[0] + d[1] * d[1]).max(1e-12);
        for row in y0..y1 {
            for col in x0..x1 {
                let p = [col as f64 + 0.5 - a[0], row as f64 + 0.5 - a[1]];
                let t = ((p[0] * d[0] + p[1] * d[1]) / len2).clamp(0.0, 1.0);
                let q = [p[0] - t * d[0], p[1] - t * d[1]];
                let rad = ra + t * (rb - ra);
                if q[0] * q[0] + q[1] * q[1] <= rad * rad {
                    self.blend(row * self.width + col, rgb, 1.0);
                }
            }
        }
    }

    /// The box filter down to the shot's size.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // rounded and clamped to a byte
    fn finish(&self) -> (u32, u32, Vec<u8>) {
        let (w, h) = (self.width / SS, self.height / SS);
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0_f32; 3];
                for dy in 0..SS {
                    for dx in 0..SS {
                        let p = self.px[(y * SS + dy) * self.width + x * SS + dx];
                        for c in 0..3 {
                            sum[c] += p[c];
                        }
                    }
                }
                #[allow(clippy::cast_precision_loss)]
                let n = (SS * SS) as f32;
                out.extend(sum.map(|v| (v / n).round().clamp(0.0, 255.0) as u8));
                out.push(255);
            }
        }
        (u32::try_from(w).unwrap_or(0), u32::try_from(h).unwrap_or(0), out)
    }
}

/// Draws in head-local metres onto the canvas.
struct Pen<'a> {
    canvas: &'a mut Canvas,
    face: SynthFace,
    shot: Shot,
    size: [f64; 2],
    /// Canvas units per metre.
    scale: f64,
}

impl Pen<'_> {
    fn at(&self, p: [f64; 2]) -> [f64; 2] {
        let q = to_px(&self.face, &self.shot, self.size, p);
        #[allow(clippy::cast_precision_loss)]
        let ss = SS as f64;
        [q[0] * ss, q[1] * ss]
    }

    fn fill(&mut self, poly: &[[f64; 2]], rgb: [u8; 3], alpha: f32) {
        let poly: Vec<[f64; 2]> = poly.iter().map(|&p| self.at(p)).collect();
        self.canvas.fill(&poly, rgb, alpha);
    }

    /// A polyline of width `width` metres; `taper` scales it from 0.65 at the ends to 1.2 in the middle.
    fn stroke(&mut self, points: &[[f64; 2]], width: f64, taper: bool, rgb: [u8; 3]) {
        let n = points.len().saturating_sub(1).max(1);
        #[allow(clippy::cast_precision_loss)]
        let radius = |i: usize| {
            let k = if taper { 0.65 + 0.55 * (PI * i as f64 / n as f64).sin() } else { 1.0 };
            0.5 * width * k * self.scale
        };
        for i in 0..points.len().saturating_sub(1) {
            let (a, b) = (self.at(points[i]), self.at(points[i + 1]));
            self.canvas.capsule(a, b, radius(i), radius(i + 1), rgb);
        }
    }
}

/// An ellipse as a polygon.
fn ellipse(centre: [f64; 2], rx: f64, ry: f64) -> Vec<[f64; 2]> {
    (0..96)
        .map(|i| {
            let a = 2.0 * PI * f64::from(i) / 96.0;
            [centre[0] + rx * a.cos(), centre[1] + ry * a.sin()]
        })
        .collect()
}

/// An arc of an ellipse from `from` to `to` radians.
fn arc(centre: [f64; 2], rx: f64, ry: f64, from: f64, to: f64) -> Vec<[f64; 2]> {
    (0..=32)
        .map(|i| {
            let a = from + (to - from) * f64::from(i) / 32.0;
            [centre[0] + rx * a.cos(), centre[1] + ry * a.sin()]
        })
        .collect()
}

/// Draws `face` in `style` for `shot` (Appendix A's draw order). The drawing style goes through JPEG at quality 80.
///
/// # Panics
/// When the JPEG round trip fails, which a test reports as a failure.
#[must_use]
#[allow(clippy::too_many_lines, clippy::missing_panics_doc, clippy::expect_used)]
pub fn draw(face: &SynthFace, style: Style, shot: Shot) -> Drawn {
    let pal = style.palette();
    let size = size_of(&shot);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few hundred pixels
    let mut canvas = Canvas::new(size[0] as usize * SS, size[1] as usize * SS);
    #[allow(clippy::cast_precision_loss)]
    let scale = shot.iod_px / face.iod() * SS as f64;
    let line = pal.line_width * face.iod();
    let drawing = style == Style::Drawing;
    let mut pen = Pen { canvas: &mut canvas, face: *face, shot, size, scale };
    let chin = face.chin_y();

    // Back hair: an ellipse 1.12 times the cranium and a box down to -0.2 m.
    let (cw, up, _) = CRANIUM;
    let mut back: Vec<[f64; 2]> = (0..=96)
        .map(|i| {
            let a = PI * f64::from(i) / 96.0;
            [1.12 * cw * a.cos(), 1.12 * up * a.sin()]
        })
        .collect();
    back.extend([[-1.12 * cw, -0.2], [1.12 * cw, -0.2]]);
    pen.fill(&back, pal.hair, 1.0);

    // Shirt and collar, then the neck, then the cast shade under the chin.
    let shirt = [
        [-0.24, chin - 0.10],
        [-0.03, chin - 0.085],
        [0.0, chin - 0.13],
        [0.03, chin - 0.085],
        [0.24, chin - 0.10],
        [0.26, chin - 0.30],
        [-0.26, chin - 0.30],
    ];
    let neck = [[-0.055, chin + 0.04], [0.055, chin + 0.04], [0.06, chin - 0.12], [-0.06, chin - 0.12]];
    pen.fill(&neck, pal.skin, 1.0);
    pen.fill(
        &[[-0.055, chin + 0.04], [0.055, chin + 0.04], [0.057, chin - 0.03], [-0.057, chin - 0.03]],
        pal.shade,
        1.0,
    );
    pen.stroke(&[[-0.055, chin + 0.02], [-0.06, chin - 0.11]], 0.6 * line, false, pal.line);
    pen.stroke(&[[0.055, chin + 0.02], [0.06, chin - 0.11]], 0.6 * line, false, pal.line);
    pen.fill(&shirt, pal.shirt, 1.0);
    pen.stroke(&shirt[..5], 0.8 * line, false, pal.line);

    // Ears (drawing only), under the face fill.
    if drawing {
        let w = face.half_width_at(-0.085).unwrap_or(face.half_width);
        for side in [-1.0, 1.0] {
            let ear = ellipse([side * (w + 0.004), -0.085], 0.02, 0.033);
            pen.fill(&ear, pal.skin, 1.0);
            let mut rim = arc([side * (w + 0.004), -0.085], 0.02, 0.033, -PI / 2.0, PI / 2.0);
            if side < 0.0 {
                rim = arc([side * (w + 0.004), -0.085], 0.02, 0.033, PI / 2.0, 1.5 * PI);
            }
            pen.stroke(&rim, line, true, pal.line);
        }
    }

    // The face fill and its outline from y = 0 round the chin.
    pen.fill(&face.silhouette(), pal.skin, 1.0);
    let right: Vec<[f64; 2]> = face.right_side().into_iter().take_while(|p| p[1] <= 0.0).collect();
    let mut outline: Vec<[f64; 2]> = right.iter().rev().copied().collect();
    outline.extend(right.iter().skip(1).map(|&[x, y]| [-x, y]));
    pen.stroke(&outline, line, drawing, pal.line);

    // Blush.
    for side in [-1.0, 1.0] {
        let c = [side * 0.09, face.blush_y];
        if drawing {
            for k in [-1.0, 0.0, 1.0] {
                let x = c[0] + k * 0.011;
                pen.stroke(
                    &[[x + 0.004, c[1] + 0.006], [x - 0.004, c[1] - 0.006]],
                    0.45 * line,
                    false,
                    [0xe0, 0x7a, 0x7a],
                );
            }
        } else {
            pen.fill(&ellipse(c, 0.025, 0.011), [0xf2, 0xa7, 0xa0], 0.45);
        }
    }

    // The nose tick.
    pen.stroke(&[[0.002, face.nose_y + 0.007], [-0.001, face.nose_y]], 0.7 * line.max(0.002), false, pal.line);

    // The mouth: a closed curve, or an open shape.
    let m = face.mouth_y;
    if drawing {
        let mut open = vec![[-0.017, m], [0.017, m]];
        open.extend(arc([0.0, m], 0.017, 0.013, 0.0, -PI).into_iter().skip(1));
        pen.fill(&open, [0x96, 0x32, 0x3c], 1.0);
        let mut rim = open.clone();
        rim.push(open[0]);
        pen.stroke(&rim, 0.7 * line, false, pal.line);
    } else {
        let curve: Vec<[f64; 2]> = (0..=16)
            .map(|i| {
                let x = -0.016 + 0.032 * f64::from(i) / 16.0;
                [x, m - 0.002 + 0.002 * (x / 0.016).powi(2)]
            })
            .collect();
        pen.stroke(&curve, 0.0025, false, pal.line);
    }

    // The eyes.
    for side in [-1.0, 1.0] {
        let c = [side * face.eye_spacing, face.eye_y];
        let (rx, ry) = (face.eye_width / 2.0, face.eye_width * 0.38);
        pen.fill(&ellipse(c, rx, ry), [0xff, 0xff, 0xff], 1.0);
        pen.fill(&ellipse([c[0], c[1] - 0.002], 0.42 * rx, 0.8 * ry), pal.iris, 1.0);
        pen.fill(&ellipse([c[0], c[1] - 0.002], 0.2 * rx, 0.42 * ry), [0x1d, 0x1a, 0x2a], 1.0);
        pen.fill(&ellipse([c[0] - side * 0.004, c[1] + 0.006], 0.0035, 0.0035), [0xff, 0xff, 0xff], 1.0);
        // The upper lash with an outward flick, and a thin lower line.
        let mut upper = arc(c, rx * 1.05, ry * 1.05, PI * 0.95, PI * 0.05);
        if side < 0.0 {
            upper.reverse();
        }
        let end = upper[upper.len() - 1];
        upper.push([end[0] + side * 0.008, end[1] + 0.006]);
        let lash = if drawing { 0.009 } else { 0.006 };
        pen.stroke(&upper, lash, true, pal.lash);
        pen.stroke(&arc(c, rx * 0.95, ry * 1.02, PI * 1.25, PI * 1.75), 0.0015, false, pal.lash);
        // The brow.
        let brow: Vec<[f64; 2]> = (0..=12)
            .map(|i| {
                let t = f64::from(i) / 12.0 - 0.5;
                [c[0] + 0.044 * t, face.eye_y + 0.058 + 0.006 * (1.0 - 4.0 * t * t)]
            })
            .collect();
        pen.stroke(&brow, 0.003, true, pal.lash);
    }

    // The bangs: a jagged edge, then the cranium 4% oversize over the top.
    let mut bangs = vec![[-0.205, -0.012]];
    for i in 0..=10 {
        let x = -0.17 + 0.034 * f64::from(i);
        bangs.push([x, -0.037 + 0.004 * f64::from(i % 3)]);
        if i < 10 {
            bangs.push([x + 0.017, 0.005]);
        }
    }
    bangs.push([0.205, -0.012]);
    bangs.extend((0..=64).map(|i| {
        let a = PI * f64::from(i) / 64.0;
        [1.1 * cw * a.cos(), 1.04 * up * a.sin()]
    }));
    pen.fill(&bangs, pal.hair, 1.0);
    pen.stroke(&bangs[1..bangs.len() - 66], 0.6 * line, false, pal.line);

    // The side lock over the character's right cheek (drawing only).
    if drawing {
        let mut outer = Vec::new();
        let mut inner = Vec::new();
        for i in 0..=40 {
            let y = 0.03 - 0.19 * f64::from(i) / 40.0;
            let w = face.half_width_at(y).unwrap_or(0.0).max(0.04);
            let mut depth = 0.032 * (PI * f64::from(i) / 40.0).sin().powf(0.25);
            // The lock covers the cheek, never the eye: beside the eye it stays clear of the lash's flick, and where
            // that leaves too little to cover the outline it falls just outside it (a lock behind the face's edge).
            if (y - face.eye_y).abs() < 0.05 {
                depth = depth.min(w - (face.eye_spacing + face.eye_width / 2.0 + 0.03));
                if depth < 0.012 {
                    depth = -0.006;
                }
            }
            outer.push([-(w + 0.012), y]);
            inner.push([-(w - depth), y]);
        }
        let mut lock = outer.clone();
        lock.extend(inner.iter().rev());
        pen.fill(&lock, pal.hair, 1.0);
        pen.stroke(&inner, 0.7 * line, false, pal.line);
    }

    let (width, height, pixels) = canvas.finish();
    let hash = ContentHash::of(&pixels);
    let mut image = Rgba8 { width, height, pixels, hash };
    if drawing {
        let rgb: Vec<u8> = image.pixels.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 80)
            .write_image(&rgb, width, height, ::image::ExtendedColorType::Rgb8)
            .expect("the JPEG encodes");
        image = decode(&jpeg, ContentHash::of(&jpeg), "image").expect("the JPEG decodes");
    }
    Drawn { image, size, shot, face: *face }
}

/// A small deterministic random source (`SplitMix64`) for click noise and offsets.
pub struct Rng(u64);

impl Rng {
    /// A source seeded with `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// A uniform number in [0, 1).
    #[allow(clippy::cast_precision_loss)]
    pub fn uniform(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// A standard normal number (Box-Muller).
    pub fn normal(&mut self) -> f64 {
        let u = self.uniform().max(1e-12);
        let v = self.uniform();
        (-2.0 * u.ln()).sqrt() * (2.0 * PI * v).cos()
    }

    /// `p` moved by Gaussian noise of `sd` pixels on each axis.
    pub fn jitter(&mut self, p: [f64; 2], sd: f64) -> [f64; 2] {
        [p[0] + sd * self.normal(), p[1] + sd * self.normal()]
    }
}
