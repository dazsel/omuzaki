//! Inserts: drawn cards that take over the frame for a moment, as a short cuts away from the character to a reaction
//! (a screaming cartoon mouth, googly eyes) and back.
//!
//! An insert is a flat backdrop and vector shapes, drawn by the face painter's CPU rasteriser at the frame's size, so
//! it is as crisp at 1080x1920 as at a draft's size. A card starts from a preset ([`CardPreset`]) or from nothing, adds
//! its own shapes over it, and moves as one piece with `offset`, `scale` and `roll`; a jumpy cutaway is several inserts
//! in a row, each placed a little differently. Captions still draw over an insert.
//!
//! **Coordinates.** A point is `[x, y]` in shares of the frame, `[0, 0]` the top left and `[1, 1]` the bottom right;
//! an ellipse's radii and a line's width are shares of the frame's width, so a circle stays round on a tall frame.

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{
    Cap, Cmd, DrawList, FaceLayerId, FaceLayerSpec, FaceRasterizer, FaceRect, Join, Paint, PathRef, Raster,
};
use omuzaki_render_api::Colour;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ShortError;

/// The most inserts a short holds.
pub const MAX_INSERTS: usize = 500;
/// The most shapes on one insert.
pub const MAX_SHAPES: usize = 256;
/// The most points on one shape.
pub const MAX_POINTS: usize = 256;
/// The segments an ellipse is drawn with: under half a pixel off a true ellipse filling a 1080-wide frame.
const ELLIPSE_SEGMENTS: usize = 128;

/// A drawn card over the frame from `at` for `duration` seconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InsertDef {
    /// When it cuts in, seconds from the start of the short.
    pub at: f32,
    /// How long it holds, seconds.
    pub duration: f32,
    /// The flat colour behind the shapes.
    #[serde(default = "white")]
    pub backdrop: Colour,
    /// A drawing to start from; the shapes go over it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<CardPreset>,
    /// Moves the whole card, in shares of the frame (+ right and down).
    #[serde(default, skip_serializing_if = "is_zero2")]
    pub offset: [f32; 2],
    /// Scales the whole card about the frame's middle.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub scale: f32,
    /// Turns the whole card about the frame's middle, degrees clockwise.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub roll: f32,
    /// Shapes drawn in order over the card.
    #[serde(rename = "shape", default, skip_serializing_if = "Vec::is_empty")]
    pub shapes: Vec<ShapeDef>,
}

/// A drawing an insert can start from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CardPreset {
    /// Two googly eyes over a huge open mouth: red lips, white teeth, a dark throat and a tongue.
    Scream,
    /// Two googly eyes, looking up and in.
    Googly,
}

/// What a shape is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    /// An ellipse round `centre` with `radii`.
    Ellipse,
    /// A smooth closed shape through `points` (at least 3).
    Blob,
    /// A closed shape with straight sides through `points` (at least 3).
    Polygon,
    /// A smooth open line through `points` (at least 2), drawn with `line`.
    Curve,
}

/// One shape on a card: an outline filled with `fill`, drawn round with `line`, or both.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShapeDef {
    /// What it is.
    pub kind: ShapeKind,
    /// The points of a blob, polygon or curve, in shares of the frame.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<[f32; 2]>,
    /// An ellipse's middle, in shares of the frame.
    #[serde(default, skip_serializing_if = "is_zero2")]
    pub centre: [f32; 2],
    /// An ellipse's half-width and half-height, in shares of the frame's width.
    #[serde(default, skip_serializing_if = "is_zero2")]
    pub radii: [f32; 2],
    /// The colour inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Colour>,
    /// The colour of its outline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<Colour>,
    /// The outline's width, in shares of the frame's width.
    #[serde(default = "default_width")]
    pub width: f32,
}

fn white() -> Colour {
    Colour::rgb(255, 255, 255)
}

const fn one() -> f32 {
    1.0
}

const fn default_width() -> f32 {
    0.01
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if passes a reference")]
fn is_one(v: &f32) -> bool {
    (*v - 1.0).abs() < f32::EPSILON
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if passes a reference")]
fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if passes a reference")]
fn is_zero2(v: &[f32; 2]) -> bool {
    v[0] == 0.0 && v[1] == 0.0
}

impl ShapeDef {
    /// An ellipse.
    pub fn ellipse(centre: [f32; 2], radii: [f32; 2], fill: Option<Colour>, line: Option<Colour>, width: f32) -> Self {
        Self { kind: ShapeKind::Ellipse, points: Vec::new(), centre, radii, fill, line, width }
    }

    /// A blob, polygon or curve through `points`.
    pub fn through(
        kind: ShapeKind,
        points: &[[f32; 2]],
        fill: Option<Colour>,
        line: Option<Colour>,
        width: f32,
    ) -> Self {
        Self { kind, points: points.to_vec(), centre: [0.0; 2], radii: [0.0; 2], fill, line, width }
    }
}

impl InsertDef {
    /// Whether the insert is on screen at `t`.
    pub fn covers(&self, t: f32) -> bool {
        t >= self.at && t < self.at + self.duration
    }

    /// The card's shapes: its preset's, then its own.
    pub fn all_shapes(&self) -> Vec<ShapeDef> {
        let mut shapes = self.card.map(CardPreset::shapes).unwrap_or_default();
        shapes.extend(self.shapes.iter().cloned());
        shapes
    }
}

/// Checks `insert`, the `i`th of the short.
///
/// # Errors
/// [`ShortError::Invalid`] naming the field that is out of range.
pub fn check(i: usize, insert: &InsertDef, max_seconds: f32) -> Result<(), ShortError> {
    let field = |name: &str| format!("insert[{i}].{name}");
    let ranges = [
        ("at", insert.at, 0.0, max_seconds),
        ("duration", insert.duration, 0.02, 60.0),
        ("offset[0]", insert.offset[0], -2.0, 2.0),
        ("offset[1]", insert.offset[1], -2.0, 2.0),
        ("scale", insert.scale, 0.05, 10.0),
        ("roll", insert.roll, -360.0, 360.0),
    ];
    for (name, value, lo, hi) in ranges {
        if !(lo..=hi).contains(&value) {
            return Err(ShortError::invalid(field(name), format!("{value} is outside {lo} to {hi}")));
        }
    }
    if insert.shapes.len() > MAX_SHAPES {
        return Err(ShortError::invalid(
            field("shape"),
            format!("{} shapes; at most {MAX_SHAPES}", insert.shapes.len()),
        ));
    }
    for (k, shape) in insert.shapes.iter().enumerate() {
        check_shape(&field(&format!("shape[{k}]")), shape)?;
    }
    Ok(())
}

fn check_shape(field: &str, shape: &ShapeDef) -> Result<(), ShortError> {
    let at = |name: &str| format!("{field}.{name}");
    if shape.fill.is_none() && shape.line.is_none() {
        return Err(ShortError::invalid(field.to_owned(), "a shape needs a fill, a line or both"));
    }
    if !(0.0..=0.2).contains(&shape.width) || (shape.line.is_some() && shape.width <= 0.0) {
        return Err(ShortError::invalid(at("width"), format!("{} is outside 0 to 0.2", shape.width)));
    }
    let least = match shape.kind {
        ShapeKind::Ellipse => {
            for (k, r) in shape.radii.iter().enumerate() {
                if !(*r > 0.0 && *r <= 4.0) {
                    return Err(ShortError::invalid(at(&format!("radii[{k}]")), format!("{r} is outside 0 to 4")));
                }
            }
            check_point(&at("centre"), shape.centre)?;
            0
        }
        ShapeKind::Blob | ShapeKind::Polygon => 3,
        ShapeKind::Curve => {
            if shape.fill.is_some() {
                return Err(ShortError::invalid(at("fill"), "a curve is open: give it a line, not a fill"));
            }
            2
        }
    };
    if shape.kind != ShapeKind::Ellipse {
        if !(least..=MAX_POINTS).contains(&shape.points.len()) {
            return Err(ShortError::invalid(
                at("points"),
                format!("{} points; a {:?} takes {least} to {MAX_POINTS}", shape.points.len(), shape.kind),
            ));
        }
        for (k, p) in shape.points.iter().enumerate() {
            check_point(&at(&format!("points[{k}]")), *p)?;
        }
    }
    Ok(())
}

fn check_point(field: &str, p: [f32; 2]) -> Result<(), ShortError> {
    if p.iter().all(|v| (-2.0..=3.0).contains(v)) {
        Ok(())
    } else {
        Err(ShortError::invalid(field.to_owned(), format!("{p:?} is outside -2 to 3")))
    }
}

/// Draws inserts over frames, reusing its rasteriser and draw list.
pub struct InsertPainter {
    rasterizer: omuzaki_face::raster::SkiaRasterizer,
    list: DrawList,
    raster: Raster,
}

impl Default for InsertPainter {
    fn default() -> Self {
        Self {
            rasterizer: omuzaki_face::raster::SkiaRasterizer::new(),
            list: DrawList::new(),
            raster: Raster::new(1, 1),
        }
    }
}

impl std::fmt::Debug for InsertPainter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InsertPainter")
    }
}

impl InsertPainter {
    /// Draws the insert of `inserts` on screen at `at` seconds, if any, over the whole of `rgba` (straight alpha, `size`
    /// pixels); the last one listed wins where two overlap. Returns whether one was drawn.
    ///
    /// # Errors
    /// [`ShortError::Render`] if the rasteriser refuses the card.
    pub fn draw(
        &mut self,
        inserts: &[InsertDef],
        at: f32,
        rgba: &mut [u8],
        size: [u32; 2],
    ) -> Result<bool, ShortError> {
        let Some(insert) = inserts.iter().rev().find(|i| i.covers(at)) else { return Ok(false) };
        let [w, h] = size;
        #[allow(clippy::cast_precision_loss, reason = "frame sides are at most 4096")]
        let (wf, hf) = (w as f32, h as f32);
        self.list.clear();
        let place = Placement::new(insert, wf, hf);
        for shape in insert.all_shapes() {
            add_shape(&mut self.list, &shape, &place, wf);
        }
        // One plane metre is one frame width: the plane runs 0..1 across and 0..-h/w down.
        let rect = FaceRect { x0: 0.0, x1: 1.0, y0: -hf / wf, y1: 0.0 };
        let spec = FaceLayerSpec::for_rect(rect, FaceLayerId::Base, wf);
        self.rasterizer
            .rasterize(&spec, &self.list, wf, &mut self.raster)
            .map_err(|e| ShortError::Render(format!("an insert: {e}")))?;
        let back = [insert.backdrop.r, insert.backdrop.g, insert.backdrop.b].map(f32::from);
        let same = self.raster.width == w && self.raster.height == h;
        let clear = [0u8; 4];
        let card = if same { self.raster.pixels.as_chunks::<4>().0 } else { &[] };
        for (k, out) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let src = card.get(k).unwrap_or(&clear);
            let cover = f32::from(src[3]) / 255.0;
            for ((value, drawn), under) in out.iter_mut().zip(src).zip(back) {
                // The raster is premultiplied: its colour plus the backdrop where it doesn't cover.
                let v = f32::from(*drawn) + under * (1.0 - cover);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "rounded into 0..=255")]
                {
                    *value = v.round().clamp(0.0, 255.0) as u8;
                }
            }
            out[3] = 255;
        }
        Ok(true)
    }
}

/// Where a card's points land: shares of the frame to pixels, then the card's scale and roll about the middle and its
/// offset, then pixels to the plane.
struct Placement {
    size: Vec2,
    scale: f32,
    rotation: Vec2,
    offset: Vec2,
}

impl Placement {
    fn new(insert: &InsertDef, w: f32, h: f32) -> Self {
        let roll = insert.roll.to_radians();
        Self {
            size: Vec2::new(w, h),
            scale: insert.scale,
            rotation: Vec2::new(roll.cos(), roll.sin()),
            offset: Vec2::from(insert.offset) * Vec2::new(w, h),
        }
    }

    /// A pixel position (y down) round the card, into the plane.
    fn plane(&self, px: Vec2) -> Vec2 {
        let middle = self.size * 0.5;
        let d = (px - middle) * self.scale;
        // Clockwise on screen (y down) is the usual rotation in pixel space.
        let turned =
            Vec2::new(d.x * self.rotation.x - d.y * self.rotation.y, d.x * self.rotation.y + d.y * self.rotation.x);
        let p = middle + turned + self.offset;
        Vec2::new(p.x / self.size.x, -p.y / self.size.x)
    }

    /// A point in shares of the frame, into the plane.
    fn share(&self, p: [f32; 2]) -> Vec2 {
        self.plane(Vec2::from(p) * self.size)
    }
}

fn add_shape(list: &mut DrawList, shape: &ShapeDef, place: &Placement, w: f32) {
    let path = match shape.kind {
        ShapeKind::Ellipse => {
            let centre = Vec2::from(shape.centre) * place.size;
            let radii = Vec2::from(shape.radii) * w;
            let points: Vec<Vec2> = (0..ELLIPSE_SEGMENTS)
                .map(|k| {
                    #[allow(clippy::cast_precision_loss, reason = "a few hundred segments")]
                    let a = std::f32::consts::TAU * k as f32 / ELLIPSE_SEGMENTS as f32;
                    place.plane(centre + Vec2::new(a.cos(), a.sin()) * radii)
                })
                .collect();
            polygon(list, &points)
        }
        ShapeKind::Polygon => {
            let points: Vec<Vec2> = shape.points.iter().map(|p| place.share(*p)).collect();
            polygon(list, &points)
        }
        ShapeKind::Blob | ShapeKind::Curve => {
            let points: Vec<Vec2> = shape.points.iter().map(|p| place.share(*p)).collect();
            smooth(list, &points, shape.kind == ShapeKind::Blob)
        }
    };
    if let Some(fill) = shape.fill {
        list.push(Cmd::Fill { path, paint: Paint::Solid(fill), opacity: 1.0 });
    }
    if let Some(line) = shape.line {
        let width = shape.width * place.scale;
        list.push(Cmd::Stroke {
            path,
            paint: Paint::Solid(line),
            width,
            cap: Cap::Round,
            join: Join::Round,
            opacity: 1.0,
        });
    }
}

fn polygon(list: &mut DrawList, points: &[Vec2]) -> PathRef {
    list.path().polyline(points).close().finish()
}

/// A Catmull-Rom spline through `points` as cubic Béziers, closed or open (an open one's ends hold their direction).
fn smooth(list: &mut DrawList, points: &[Vec2], closed: bool) -> PathRef {
    let n = points.len();
    let at = |k: isize| -> Vec2 {
        if closed {
            #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss, reason = "a few hundred points")]
            let k = k.rem_euclid(n as isize) as usize;
            points[k]
        } else {
            #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss, reason = "a few hundred points")]
            let k = k.clamp(0, n as isize - 1) as usize;
            points[k]
        }
    };
    let mut path = list.path().move_to(points[0]);
    let segments = if closed { n } else { n - 1 };
    for k in 0..segments {
        #[allow(clippy::cast_possible_wrap, reason = "a few hundred points")]
        let k = k as isize;
        let (p0, p1, p2, p3) = (at(k - 1), at(k), at(k + 1), at(k + 2));
        path = path.cubic_to(p1 + (p2 - p0) / 6.0, p2 - (p3 - p1) / 6.0, p2);
    }
    if closed {
        path = path.close();
    }
    path.finish()
}

impl CardPreset {
    /// The preset's shapes, on a 9:16 frame's shares.
    pub fn shapes(self) -> Vec<ShapeDef> {
        let ink = Some(Colour::rgb(0x0c, 0x0a, 0x0c));
        let white = Some(Colour::rgb(0xfc, 0xfc, 0xfa));
        let mut eyes = vec![
            ShapeDef::ellipse([0.22, 0.15], [0.135, 0.13], white, ink, 0.012),
            ShapeDef::ellipse([0.255, 0.14], [0.042, 0.042], ink, None, 0.0),
            ShapeDef::ellipse([0.68, 0.16], [0.125, 0.12], white, ink, 0.012),
            ShapeDef::ellipse([0.645, 0.15], [0.04, 0.04], ink, None, 0.0),
        ];
        match self {
            Self::Googly => eyes,
            Self::Scream => {
                let lips = Some(Colour::rgb(0xe8, 0x16, 0x1a));
                let throat = Some(Colour::rgb(0x14, 0x0b, 0x12));
                let tongue = Some(Colour::rgb(0x5e, 0x1c, 0x3a));
                eyes.extend([
                    ShapeDef::through(
                        ShapeKind::Blob,
                        &[
                            [0.03, 0.27],
                            [0.5, 0.235],
                            [0.97, 0.3],
                            [0.95, 0.47],
                            [0.86, 0.62],
                            [0.5, 0.66],
                            [0.1, 0.6],
                            [0.05, 0.45],
                        ],
                        lips,
                        ink,
                        0.008,
                    ),
                    ShapeDef::through(
                        ShapeKind::Blob,
                        &[
                            [0.1, 0.3],
                            [0.5, 0.275],
                            [0.9, 0.33],
                            [0.88, 0.47],
                            [0.8, 0.585],
                            [0.5, 0.62],
                            [0.16, 0.565],
                            [0.11, 0.45],
                        ],
                        throat,
                        None,
                        0.0,
                    ),
                    ShapeDef::through(
                        ShapeKind::Blob,
                        &[[0.11, 0.305], [0.5, 0.28], [0.89, 0.335], [0.85, 0.37], [0.5, 0.335], [0.15, 0.355]],
                        white,
                        ink,
                        0.004,
                    ),
                    ShapeDef::through(
                        ShapeKind::Blob,
                        &[[0.2, 0.55], [0.5, 0.57], [0.76, 0.565], [0.72, 0.6], [0.5, 0.615], [0.24, 0.58]],
                        white,
                        ink,
                        0.004,
                    ),
                    ShapeDef::through(
                        ShapeKind::Blob,
                        &[[0.3, 0.55], [0.42, 0.49], [0.6, 0.48], [0.7, 0.54], [0.5, 0.565]],
                        tongue,
                        None,
                        0.0,
                    ),
                ]);
                eyes
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32) -> Vec<u8> {
        vec![40; (w * h * 4) as usize]
    }

    fn pixel(rgba: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    fn insert(at: f32, shapes: Vec<ShapeDef>) -> InsertDef {
        InsertDef {
            at,
            duration: 1.0,
            backdrop: Colour::rgb(0x10, 0xf0, 0x20),
            card: None,
            offset: [0.0; 2],
            scale: 1.0,
            roll: 0.0,
            shapes,
        }
    }

    #[test]
    fn an_insert_covers_the_frame_with_its_backdrop_and_shapes_only_while_it_holds() {
        let (w, h) = (90u32, 160u32);
        let red = Some(Colour::rgb(255, 0, 0));
        let inserts = vec![insert(1.0, vec![ShapeDef::ellipse([0.5, 0.5], [0.2, 0.2], red, None, 0.0)])];
        let mut painter = InsertPainter::default();
        let mut rgba = frame(w, h);
        assert!(!painter.draw(&inserts, 0.5, &mut rgba, [w, h]).unwrap());
        assert!(rgba.iter().all(|v| *v == 40), "before it, the frame is untouched");
        assert!(painter.draw(&inserts, 1.5, &mut rgba, [w, h]).unwrap());
        assert_eq!(pixel(&rgba, w, 2, 2), [0x10, 0xf0, 0x20, 255], "the backdrop");
        assert_eq!(pixel(&rgba, w, 45, 80), [255, 0, 0, 255], "the shape, at the middle");
        // The circle is round: 0.2 of the width, 18 px, either way from the middle.
        assert_eq!(pixel(&rgba, w, 45 + 14, 80)[0], 255);
        assert_eq!(pixel(&rgba, w, 45, 80 + 14)[0], 255);
        assert_eq!(pixel(&rgba, w, 45, 80 + 22), [0x10, 0xf0, 0x20, 255]);
    }

    #[test]
    fn offset_scale_and_roll_move_the_whole_card() {
        let (w, h) = (90u32, 160u32);
        let blue = Some(Colour::rgb(0, 0, 255));
        let bar =
            ShapeDef::through(ShapeKind::Polygon, &[[0.4, 0.1], [0.6, 0.1], [0.6, 0.2], [0.4, 0.2]], blue, None, 0.0);
        let mut moved = insert(0.0, vec![bar.clone()]);
        moved.offset = [0.0, 0.5];
        let mut painter = InsertPainter::default();
        let mut rgba = frame(w, h);
        painter.draw(&[moved], 0.1, &mut rgba, [w, h]).unwrap();
        assert_eq!(pixel(&rgba, w, 45, 24)[2], 0x20, "it left its place");
        assert_eq!(pixel(&rgba, w, 45, 104)[2], 255, "half a frame down");
        // A half turn about the middle puts the bar near the bottom.
        let mut turned = insert(0.0, vec![bar]);
        turned.roll = 180.0;
        painter.draw(&[turned], 0.1, &mut rgba, [w, h]).unwrap();
        assert_eq!(pixel(&rgba, w, 45, 160 - 24)[2], 255);
    }

    #[test]
    fn a_preset_card_draws_and_shapes_draw_over_it() {
        let (w, h) = (270u32, 480u32);
        let mut scream = insert(0.0, Vec::new());
        scream.card = Some(CardPreset::Scream);
        let mut painter = InsertPainter::default();
        let mut rgba = frame(w, h);
        painter.draw(&[scream.clone()], 0.0, &mut rgba, [w, h]).unwrap();
        let count = |rgba: &[u8], f: &dyn Fn(&[u8]) -> bool| rgba.chunks(4).filter(|p| f(p)).count();
        let red = count(&rgba, &|p| p[0] > 200 && p[1] < 60 && p[2] < 60);
        let white = count(&rgba, &|p| p[0] > 240 && p[1] > 240 && p[2] > 240);
        assert!(red > 2000 && white > 4000, "lips {red}, eyes and teeth {white}");
        scream.shapes.push(ShapeDef::ellipse([0.5, 0.5], [2.0, 2.0], Some(Colour::rgb(0, 0, 0)), None, 0.0));
        painter.draw(&[scream], 0.0, &mut rgba, [w, h]).unwrap();
        assert_eq!(count(&rgba, &|p| p[0] > 200), 0, "a shape covers the preset under it");
    }

    #[test]
    fn shapes_are_checked() {
        let fine = insert(1.0, vec![ShapeDef::ellipse([0.5, 0.5], [0.2, 0.1], Some(white()), None, 0.0)]);
        assert!(check(0, &fine, 600.0).is_ok());
        let invisible = insert(1.0, vec![ShapeDef::ellipse([0.5, 0.5], [0.2, 0.1], None, None, 0.0)]);
        assert!(check(0, &invisible, 600.0).unwrap_err().to_string().contains("insert[0].shape[0]"));
        let flat =
            insert(1.0, vec![ShapeDef::through(ShapeKind::Blob, &[[0.0, 0.0], [1.0, 1.0]], Some(white()), None, 0.0)]);
        assert!(check(2, &flat, 600.0).unwrap_err().to_string().contains("insert[2].shape[0].points"));
        let filled_curve =
            insert(1.0, vec![ShapeDef::through(ShapeKind::Curve, &[[0.0, 0.0], [1.0, 1.0]], Some(white()), None, 0.0)]);
        assert!(check(0, &filled_curve, 600.0).unwrap_err().to_string().contains("fill"));
        let late = InsertDef { at: 700.0, ..fine };
        assert!(check(0, &late, 600.0).unwrap_err().to_string().contains("insert[0].at"));
    }
}
