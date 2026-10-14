//! A small scanline rasteriser for [`DrawList`]s: exact enough for tests and goldens, not fast.
//!
//! One sample per pixel at the pixel centre (no anti-aliasing), so output is bit-identical on every platform and a
//! test can reason about coverage exactly. Colour is blended in sRGB space and stored premultiplied, as the
//! [`FaceRasterizer`] contract says.

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{
    Cap, Cmd, ColourExt, DrawList, FaceError, FaceLayerSpec, FaceRasterizer, Paint, PathEl, PathRef, Raster, Stops,
};

/// Segments per curve when flattening Béziers.
const CURVE_STEPS: u32 = 16;

/// The mock CPU rasteriser.
#[derive(Debug, Default)]
pub struct MockRasterizer;

impl FaceRasterizer for MockRasterizer {
    fn rasterize(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        px_per_m: f32,
        out: &mut Raster,
    ) -> Result<(), FaceError> {
        let (w, h) = spec.rect.size_px(px_per_m);
        if !(spec.rect.width() > 0.0 && spec.rect.height() > 0.0 && px_per_m.is_finite() && px_per_m > 0.0) {
            return Err(FaceError::Raster("the layer rectangle and density must be positive".to_owned()));
        }
        *out = Raster::new(w, h);
        // Plane (metres, y up) to pixel (y down).
        #[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^24
        let (sx, sy) = (w as f32 / spec.rect.width(), h as f32 / spec.rect.height());
        let to_px = Affine2::from_cols(
            Vec2::new(sx, 0.0),
            Vec2::new(0.0, -sy),
            Vec2::new(-spec.rect.x0 * sx, spec.rect.y1 * sy),
        );
        let mut st = State { to_px, transforms: vec![Affine2::IDENTITY], clips: Vec::new(), w, h };
        for cmd in list.cmds() {
            st.run(cmd, list, out)?;
        }
        Ok(())
    }
}

struct State {
    to_px: Affine2,
    transforms: Vec<Affine2>,
    clips: Vec<Vec<bool>>,
    w: u32,
    h: u32,
}

impl State {
    fn current(&self) -> Affine2 {
        self.transforms.last().copied().unwrap_or(Affine2::IDENTITY)
    }

    fn total(&self) -> Affine2 {
        self.to_px * self.current()
    }

    fn run(&mut self, cmd: &Cmd, list: &DrawList, out: &mut Raster) -> Result<(), FaceError> {
        match cmd {
            Cmd::Fill { path, paint, opacity } => {
                let polys = flatten(list, *path, self.total());
                let mut mask = self.blank();
                self.fill_polys(&mut mask, &polys, true);
                self.composite(out, &mask, paint, *opacity);
            }
            Cmd::Stroke { path, paint, width, cap, opacity, .. } => {
                let mut mask = self.blank();
                for quad in stroke_quads(&flatten_local(list, *path), *width, *cap == Cap::Round) {
                    let poly: Vec<Vec2> = quad.iter().map(|p| self.total().transform_point2(*p)).collect();
                    self.fill_polys(&mut mask, &[poly], false);
                }
                self.composite(out, &mask, paint, *opacity);
            }
            Cmd::PushClip(path) => {
                let polys = flatten(list, *path, self.total());
                let mut mask = self.blank();
                self.fill_polys(&mut mask, &polys, true);
                if let Some(outer) = self.clips.last() {
                    for (m, o) in mask.iter_mut().zip(outer) {
                        *m &= *o;
                    }
                }
                self.clips.push(mask);
            }
            Cmd::PopClip => {
                self.clips.pop().ok_or_else(|| FaceError::Raster("PopClip without a PushClip".to_owned()))?;
            }
            Cmd::PushTransform(t) => {
                let next = self.current() * *t;
                self.transforms.push(next);
            }
            Cmd::PopTransform => {
                if self.transforms.len() < 2 {
                    return Err(FaceError::Raster("PopTransform without a PushTransform".to_owned()));
                }
                self.transforms.pop();
            }
        }
        Ok(())
    }

    fn blank(&self) -> Vec<bool> {
        vec![false; self.w as usize * self.h as usize]
    }

    /// Sets the mask where the polygons cover a pixel centre. `joint`: the polygons are sub-paths of one path and
    /// their windings add; otherwise each is filled on its own and the results are OR-ed.
    fn fill_polys(&self, mask: &mut [bool], polys: &[Vec<Vec2>], joint: bool) {
        if joint {
            scan(mask, self.w, self.h, polys);
        } else {
            for poly in polys {
                scan(mask, self.w, self.h, std::slice::from_ref(poly));
            }
        }
    }

    fn composite(&self, out: &mut Raster, mask: &[bool], paint: &Paint, opacity: f32) {
        let inv = self.total().inverse();
        let clip = self.clips.last();
        for (i, covered) in mask.iter().enumerate() {
            if !*covered || clip.is_some_and(|c| !c[i]) {
                continue;
            }
            #[allow(clippy::cast_precision_loss)] // pixel coordinates are far below 2^24
            let centre = Vec2::new((i % self.w as usize) as f32 + 0.5, (i / self.w as usize) as f32 + 0.5);
            let [r, g, b, a] = sample(paint, inv.transform_point2(centre));
            let a = (a * opacity).clamp(0.0, 1.0);
            let px = &mut out.pixels[i * 4..i * 4 + 4];
            for (d, s) in px.iter_mut().zip([r * a, g * a, b * a, a]) {
                let dst = f32::from(*d) / 255.0;
                *d = to_u8(s + dst * (1.0 - a));
            }
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255 before the cast
fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn sample(paint: &Paint, p: Vec2) -> [f32; 4] {
    match paint {
        Paint::Solid(c) => c.srgb_f32(),
        Paint::Linear { from, to, stops } => {
            let d = *to - *from;
            let t = if d.length_squared() > 0.0 { (p - *from).dot(d) / d.length_squared() } else { 0.0 };
            ramp(stops, t)
        }
        Paint::Radial { centre, radius, stops } => {
            ramp(stops, if *radius > 0.0 { (p - *centre).length() / radius } else { 0.0 })
        }
    }
}

fn ramp(stops: &Stops, position: f32) -> [f32; 4] {
    let stops = stops.as_slice();
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else { return [0.0; 4] };
    if position <= first.offset {
        return first.colour.srgb_f32();
    }
    for pair in stops.windows(2) {
        if position <= pair[1].offset {
            let span = (pair[1].offset - pair[0].offset).max(f32::EPSILON);
            let along = (position - pair[0].offset) / span;
            let (from, to) = (pair[0].colour.srgb_f32(), pair[1].colour.srgb_f32());
            return [0, 1, 2, 3].map(|k| from[k] + (to[k] - from[k]) * along);
        }
    }
    last.colour.srgb_f32()
}

/// Flattens a path into closed polygons in pixel space.
fn flatten(list: &DrawList, path: PathRef, m: Affine2) -> Vec<Vec<Vec2>> {
    flatten_local(list, path)
        .into_iter()
        .map(|poly| poly.into_iter().map(|p| m.transform_point2(p)).collect())
        .collect()
}

/// Flattens a path into polylines in the path's own coordinates.
fn flatten_local(list: &DrawList, path: PathRef) -> Vec<Vec<Vec2>> {
    let mut out: Vec<Vec<Vec2>> = Vec::new();
    let mut cur: Vec<Vec2> = Vec::new();
    let mut pen = Vec2::ZERO;
    for el in list.elements(path) {
        match *el {
            PathEl::Move(p) => {
                if cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                }
                cur = vec![p];
                pen = p;
            }
            PathEl::Line(p) => {
                cur.push(p);
                pen = p;
            }
            PathEl::Quad(c, p) => {
                cur.extend((1..=CURVE_STEPS).map(|i| bezier2(pen, c, p, step(i))));
                pen = p;
            }
            PathEl::Cubic(c1, c2, p) => {
                cur.extend((1..=CURVE_STEPS).map(|i| bezier3(pen, c1, c2, p, step(i))));
                pen = p;
            }
            PathEl::Close => {
                if cur.len() > 1 {
                    let first = cur[0];
                    cur.push(first);
                    out.push(std::mem::replace(&mut cur, vec![first]));
                }
            }
        }
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

fn step(i: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)] // i <= CURVE_STEPS
    {
        i as f32 / CURVE_STEPS as f32
    }
}

fn bezier2(start: Vec2, ctrl: Vec2, end: Vec2, t: f32) -> Vec2 {
    let rest = 1.0 - t;
    start * (rest * rest) + ctrl * (2.0 * rest * t) + end * (t * t)
}

fn bezier3(start: Vec2, ctrl1: Vec2, ctrl2: Vec2, end: Vec2, t: f32) -> Vec2 {
    let rest = 1.0 - t;
    start * (rest * rest * rest) + ctrl1 * (3.0 * rest * rest * t) + ctrl2 * (3.0 * rest * t * t) + end * (t * t * t)
}

/// One quad per segment (plus a square at each end for round caps), in the path's own coordinates, so the width
/// scales with the transform. The mock approximates round joins and caps with these overlaps.
fn stroke_quads(polylines: &[Vec<Vec2>], width: f32, round_caps: bool) -> Vec<[Vec2; 4]> {
    let half = width * 0.5;
    let mut quads = Vec::new();
    for line in polylines {
        for seg in line.windows(2) {
            let d = seg[1] - seg[0];
            let Some(dir) = d.try_normalize() else { continue };
            let (n, ext) = (Vec2::new(-dir.y, dir.x) * half, if round_caps { dir * half } else { Vec2::ZERO });
            let (a, b) = (seg[0] - ext, seg[1] + ext);
            quads.push([a + n, b + n, b - n, a - n]);
        }
    }
    quads
}

/// Non-zero scanline fill of closed polygons into `mask`, sampling at pixel centres.
fn scan(mask: &mut [bool], w: u32, h: u32, polys: &[Vec<Vec2>]) {
    for row in 0..h {
        #[allow(clippy::cast_precision_loss)] // row < h, far below 2^24
        let y = row as f32 + 0.5;
        let mut hits: Vec<(f32, i32)> = Vec::new();
        for poly in polys {
            for (i, from) in poly.iter().enumerate() {
                let to = poly[(i + 1) % poly.len()];
                if (from.y <= y) != (to.y <= y) {
                    hits.push((
                        from.x + (y - from.y) / (to.y - from.y) * (to.x - from.x),
                        if to.y > from.y { 1 } else { -1 },
                    ));
                }
            }
        }
        hits.sort_by(|p, q| p.0.total_cmp(&q.0));
        let mut winding = 0;
        for pair in hits.windows(2) {
            winding += pair[0].1;
            if winding != 0 {
                fill_span(mask, w, row, pair[0].0, pair[1].0);
            }
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // columns are clamped to 0..w first
fn fill_span(mask: &mut [bool], w: u32, row: u32, x0: f32, x1: f32) {
    // Pixel `c` is covered when its centre `c + 0.5` lies in [x0, x1).
    let lo = (x0 - 0.5).ceil().max(0.0) as u32;
    let hi = ((x1 - 0.5).ceil().max(0.0) as u32).min(w);
    let base = row as usize * w as usize;
    for c in lo..hi {
        mask[base + c as usize] = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_face_api::{Colour, FaceLayerId, FaceRect};

    fn spec() -> FaceLayerSpec {
        // 0.2 m x 0.1 m at 500 px/m: a 100 x 50 image.
        FaceLayerSpec {
            id: FaceLayerId::Base,
            rect: FaceRect { x0: -0.1, x1: 0.1, y0: -0.05, y1: 0.05 },
            px_per_m: 500.0,
        }
    }

    fn raster(list: &DrawList) -> Raster {
        let mut out = Raster::new(1, 1);
        MockRasterizer.rasterize(&spec(), list, 500.0, &mut out).unwrap();
        out
    }

    fn px(r: &Raster, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * r.width + x) * 4) as usize;
        [r.pixels[i], r.pixels[i + 1], r.pixels[i + 2], r.pixels[i + 3]]
    }

    fn covered(r: &Raster) -> usize {
        r.pixels.chunks(4).filter(|p| p[3] > 0).count()
    }

    #[test]
    fn an_ellipse_covers_about_pi_a_b_pixels_and_y_is_up() {
        let mut l = DrawList::new();
        let p = l.path().ellipse(Vec2::new(0.0, 0.02), 0.04, 0.02).finish();
        l.fill(p, Paint::Solid(Colour::rgb(255, 0, 0)));
        let r = raster(&l);
        assert_eq!((r.width, r.height), (100, 50));
        let expected = std::f32::consts::PI * 20.0 * 10.0;
        #[allow(clippy::cast_precision_loss)]
        let got = covered(&r) as f32;
        assert!((got - expected).abs() < 0.03 * expected, "{got} vs {expected}");
        assert_eq!(px(&r, 50, 15), [255, 0, 0, 255], "centre of the ellipse, 0.02 m above the middle row");
        assert_eq!(px(&r, 50, 40), [0, 0, 0, 0], "below it is empty");
    }

    #[test]
    fn alpha_is_premultiplied_and_opacity_multiplies() {
        let mut list = DrawList::new();
        let square = list
            .path()
            .polyline(&[Vec2::new(-0.1, -0.05), Vec2::new(0.1, -0.05), Vec2::new(0.1, 0.05), Vec2::new(-0.1, 0.05)])
            .close()
            .finish();
        list.push(Cmd::Fill { path: square, paint: Paint::Solid(Colour::rgb(200, 100, 0).faded(0.5)), opacity: 0.5 });
        let [red, green, blue, alpha] = px(&raster(&list), 10, 10);
        assert!((i32::from(alpha) - 64).abs() <= 1, "alpha 0.25: {alpha}");
        assert!(
            (i32::from(red) - 50).abs() <= 1 && (i32::from(green) - 25).abs() <= 1 && blue == 0,
            "{red} {green} {blue}"
        );
    }

    #[test]
    fn clips_restrict_and_nest() {
        let mut l = DrawList::new();
        let big = l.path().ellipse(Vec2::ZERO, 0.09, 0.04).finish();
        let small = l.path().ellipse(Vec2::ZERO, 0.02, 0.02).finish();
        l.push(Cmd::PushClip(small));
        l.fill(big, Paint::Solid(Colour::rgb(0, 255, 0)));
        l.push(Cmd::PopClip);
        let clipped = covered(&raster(&l));
        let mut l2 = DrawList::new();
        let small2 = l2.path().ellipse(Vec2::ZERO, 0.02, 0.02).finish();
        l2.fill(small2, Paint::Solid(Colour::rgb(0, 255, 0)));
        assert_eq!(clipped, covered(&raster(&l2)), "fill clipped to the small ellipse equals the small ellipse");
    }

    #[test]
    fn transforms_move_and_scale_everything_inside() {
        let mut l = DrawList::new();
        let p = l.path().ellipse(Vec2::ZERO, 0.02, 0.02).finish();
        l.push(Cmd::PushTransform(
            Affine2::from_translation(Vec2::new(0.05, 0.0)) * Affine2::from_scale(Vec2::new(1.0, 2.0)),
        ));
        l.fill(p, Paint::Solid(Colour::rgb(0, 0, 255)));
        l.push(Cmd::PopTransform);
        let r = raster(&l);
        assert_eq!(px(&r, 75, 25), [0, 0, 255, 255]);
        assert_eq!(px(&r, 75, 8), [0, 0, 255, 255], "stretched to 0.04 m tall: 20 px above the middle");
        assert_eq!(px(&r, 25, 25), [0, 0, 0, 0]);
    }

    #[test]
    fn gradients_interpolate_between_stops() {
        let mut l = DrawList::new();
        let p = l
            .path()
            .polyline(&[Vec2::new(-0.1, -0.05), Vec2::new(0.1, -0.05), Vec2::new(0.1, 0.05), Vec2::new(-0.1, 0.05)])
            .close()
            .finish();
        let stops = Stops::new(&[(0.0, Colour::rgb(0, 0, 0)), (1.0, Colour::rgb(255, 255, 255))]);
        l.fill(p, Paint::Linear { from: Vec2::new(-0.1, 0.0), to: Vec2::new(0.1, 0.0), stops });
        let r = raster(&l);
        let (left, mid, right) = (px(&r, 1, 25)[0], px(&r, 50, 25)[0], px(&r, 98, 25)[0]);
        assert!(left < 10 && (i32::from(mid) - 128).abs() < 8 && right > 245, "{left} {mid} {right}");
    }

    #[test]
    fn strokes_have_the_requested_width() {
        let mut l = DrawList::new();
        let p = l.path().polyline(&[Vec2::new(-0.05, 0.0), Vec2::new(0.05, 0.0)]).finish();
        l.push(Cmd::Stroke {
            path: p,
            paint: Paint::Solid(Colour::rgb(9, 9, 9)),
            width: 0.01,
            cap: Cap::Butt,
            join: omuzaki_face_api::Join::Miter,
            opacity: 1.0,
        });
        let r = raster(&l);
        assert_eq!(covered(&r), 50 * 5, "0.1 m x 0.01 m at 500 px/m");
    }

    #[test]
    fn unbalanced_clips_and_transforms_are_errors() {
        for cmd in [Cmd::PopClip, Cmd::PopTransform] {
            let mut l = DrawList::new();
            l.push(cmd);
            let mut out = Raster::new(1, 1);
            assert!(matches!(MockRasterizer.rasterize(&spec(), &l, 500.0, &mut out), Err(FaceError::Raster(_))));
        }
        let mut out = Raster::new(1, 1);
        assert!(MockRasterizer.rasterize(&spec(), &DrawList::new(), 0.0, &mut out).is_err());
    }
}
