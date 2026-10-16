//! The production CPU rasteriser: [`SkiaRasterizer`] draws a [`DrawList`] the way Skia's software pipeline does, so it
//! draws what the 2D canvas the prototype painted the face with draws (`crates/omuzaki-test-face/tests/raster_probes.rs`
//! measures it against Chrome).
//!
//! The GPU painter is the performance path; this one is for everything that needs exact, deterministic pixels: tests and
//! goldens, the WASM fallback, `ExpressionBaker` and print-quality sheets (spec CB-24). It follows the canvas on every
//! point a face drawing can show:
//!
//! - fills use the non-zero rule, anti-aliased by [`scan`](super::scan), which reproduces Skia's analytic coverage;
//! - gradients (linear and radial, up to four stops) pad outside their range and interpolate the *straight* colour
//!   before premultiplying, as `CanvasGradient` does (so a stop that fades to transparent keeps its hue);
//! - strokes scale with the transform stack, take [`Cap`] and [`Join`], and use the canvas's default miter limit of 10;
//! - clips nest by intersection, anti-aliased, and are fixed in pixel space when pushed;
//! - blending is source-over in sRGB on premultiplied RGBA8, never in linear light.
//!
//! Row 0 of the output is the top of `spec.rect` (plane y is up, pixel y is down).

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{
    Cap, Cmd, DrawList, FaceError, FaceLayerSpec, FaceRasterizer, Join, Paint, PathEl, PathRef, Raster,
};

use super::clip::ClipStack;
use super::hairline;
use super::paint::{Shader, blend};
use super::path::Outline;
use super::scan::Scan;
use super::stroke::Stroker;

/// Canvas's default `miterLimit`. A sharper corner than this is bevelled, a blunter one comes to a point.
pub(super) const MITER_LIMIT: f32 = 10.0;

/// The largest side of a raster, matching the GPU's texture limit: a bigger one is a mistake (a density typo), not a request.
const MAX_SIDE: u32 = 16_384;

/// Draws draw lists. Keep one per thread and reuse it: its scratch (the edge buffer, the clip-mask pool, the path and
/// transform stacks) survives between calls and the output is drawn in place, so rasterising a layer of the same size
/// again allocates nothing.
#[derive(Debug, Default)]
pub struct SkiaRasterizer {
    scan: Scan,
    outline: Outline,
    stroker: Stroker,
    transforms: Vec<Affine2>,
    clips: ClipStack,
}

impl SkiaRasterizer {
    /// A rasteriser with empty scratch; the first calls size it.
    pub fn new() -> Self {
        Self::default()
    }
}

impl FaceRasterizer for SkiaRasterizer {
    fn rasterize(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        px_per_m: f32,
        out: &mut Raster,
    ) -> Result<(), FaceError> {
        let rect = spec.rect;
        if !(rect.width() > 0.0 && rect.height() > 0.0 && px_per_m.is_finite() && px_per_m > 0.0) {
            return Err(FaceError::Raster("the layer rectangle and density must be positive".to_owned()));
        }
        let (w, h) = rect.size_px(px_per_m);
        if w > MAX_SIDE || h > MAX_SIDE {
            return Err(FaceError::Raster(format!("a {w} x {h} raster is larger than the {MAX_SIDE} px limit")));
        }
        out.width = w;
        out.height = h;
        out.pixels.clear();
        out.pixels.resize(w as usize * h as usize * 4, 0);

        // Plane (metres, y up) to pixel (y down), scaled so the rect fills the image exactly (`size_px` rounds).
        #[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^24
        let (sx, sy) = (w as f32 / rect.width(), h as f32 / rect.height());
        let to_px = Affine2::from_cols(Vec2::new(sx, 0.0), Vec2::new(0.0, -sy), Vec2::new(-rect.x0 * sx, rect.y1 * sy));

        let (width, height) = (w as usize, h as usize);
        self.scan.reset(width, height);
        self.transforms.clear();
        self.transforms.push(Affine2::IDENTITY);
        self.clips.reset(width, height);
        let mut canvas = Canvas {
            pixels: &mut out.pixels,
            width,
            list,
            to_px,
            scan: &mut self.scan,
            outline: &mut self.outline,
            stroker: &mut self.stroker,
            transforms: &mut self.transforms,
            clips: &mut self.clips,
        };
        list.cmds().iter().enumerate().try_for_each(|(index, cmd)| {
            check(index, cmd, list)?;
            canvas.run(cmd)
        })
    }
}

/// One call's drawing state: the target, the list, and the rasteriser's scratch borrowed for the call.
struct Canvas<'a> {
    pixels: &'a mut [u8],
    width: usize,
    list: &'a DrawList,
    to_px: Affine2,
    scan: &'a mut Scan,
    outline: &'a mut Outline,
    stroker: &'a mut Stroker,
    transforms: &'a mut Vec<Affine2>,
    clips: &'a mut ClipStack,
}

impl Canvas<'_> {
    fn run(&mut self, cmd: &Cmd) -> Result<(), FaceError> {
        match *cmd {
            Cmd::Fill { path, ref paint, opacity } => {
                self.fill(path, paint, opacity);
                Ok(())
            }
            Cmd::Stroke { path, ref paint, width, cap, join, opacity } => {
                self.stroke(path, paint, width, cap, join, opacity);
                Ok(())
            }
            Cmd::PushClip(path) => {
                self.outline.set_from_elements(self.list.elements(path));
                self.clips.push(self.outline, self.total(), self.scan);
                Ok(())
            }
            Cmd::PopClip => self.clips.pop(),
            Cmd::PushTransform(t) => {
                let next = self.current() * t;
                if !next.is_finite() {
                    return Err(FaceError::Raster("PushTransform: the composed transform overflows".to_owned()));
                }
                self.transforms.push(next);
                Ok(())
            }
            Cmd::PopTransform if self.transforms.len() < 2 => {
                Err(FaceError::Raster("PopTransform without a PushTransform".to_owned()))
            }
            Cmd::PopTransform => {
                self.transforms.pop();
                Ok(())
            }
        }
    }

    fn fill(&mut self, path: PathRef, paint: &Paint, opacity: f32) {
        let to_px = self.total();
        let Some(shader) = Shader::new(paint, opacity, to_px) else { return };
        self.outline.set_from_elements(self.list.elements(path));
        self.outline.feed(to_px, self.scan);
        self.blit(&shader);
    }

    fn stroke(&mut self, path: PathRef, paint: &Paint, width: f32, cap: Cap, join: Join, opacity: f32) {
        // A zero or negative width draws nothing on the canvas (a width that is not a number was rejected by `check`).
        if width <= 0.0 {
            return;
        }
        let to_px = self.total();
        if let Some(coverage) = hairline::coverage(width, to_px) {
            self.hairline(path, paint, cap, opacity, coverage, to_px);
            return;
        }
        let Some(shader) = Shader::new(paint, opacity, to_px) else { return };
        self.stroker.stroke(self.list.elements(path), width, cap, join, MITER_LIMIT, Stroker::resolution_scale(to_px));
        self.stroker.outline().feed(to_px, self.scan);
        self.blit(&shader);
    }

    /// A stroke under a pixel wide: one-pixel lines whose alpha Skia scales by the stroke's width (in whole 1/256ths of
    /// the paint's 8-bit alpha).
    fn hairline(&mut self, path: PathRef, paint: &Paint, cap: Cap, opacity: f32, coverage: f32, to_px: Affine2) {
        let alpha = match paint {
            Paint::Solid(c) => f32::from(c.a) * opacity.clamp(0.0, 1.0),
            Paint::Linear { .. } | Paint::Radial { .. } => 255.0 * opacity.clamp(0.0, 1.0),
        };
        #[allow(clippy::cast_possible_truncation)] // 0..=255 and 0..=256
        let (a8, scale) = (alpha.round() as i32, (coverage * 256.0) as i32);
        let scaled = (a8 * scale) >> 8;
        if scaled <= 0 {
            return;
        }
        #[allow(clippy::cast_precision_loss)] // 8-bit values
        let factor = scaled as f32 / a8 as f32;
        let Some(shader) = Shader::new(paint, opacity * factor, to_px) else { return };
        let (width, height) = (self.width, self.pixels.len() / 4 / self.width.max(1));
        let (pixels, clip) = (&mut *self.pixels, self.clips.current());
        hairline::stroke(self.list.elements(path), to_px, cap, (width, height), &mut |x, y, a| {
            if let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y))
                && x < width
                && y < height
            {
                shade(pixels, width, clip, &shader, x, y, a);
            }
        });
    }

    /// Sweeps the edges accumulated in the scan and blends `shader` through the coverage (times the clip's).
    fn blit(&mut self, shader: &Shader) {
        let (width, clip) = (self.width, self.clips.current());
        let opaque = shader.opaque_bytes();
        let pixels = &mut *self.pixels;
        self.scan.sweep(|y, x0, alphas| {
            for (i, &a) in alphas.iter().enumerate() {
                if a == 0 {
                    continue;
                }
                let x = x0 + i;
                if let (255, None, Some(bytes)) = (a, clip, opaque) {
                    // An opaque colour through full coverage replaces the pixel: no blend needed.
                    let at = (y * width + x) * 4;
                    pixels[at..at + 4].copy_from_slice(&bytes);
                    continue;
                }
                shade(pixels, width, clip, shader, x, y, a);
            }
        });
    }

    fn current(&self) -> Affine2 {
        self.transforms.last().copied().unwrap_or(Affine2::IDENTITY)
    }

    /// Plane to pixels under the transform stack.
    fn total(&self) -> Affine2 {
        self.to_px * self.current()
    }
}

/// Blends `shader` onto pixel (`x`, `y`) with coverage `a`, limited by the clip mask if there is one.
#[allow(clippy::cast_possible_truncation)] // the clipped alpha is at most 255
fn shade(pixels: &mut [u8], width: usize, clip: Option<&[u8]>, shader: &Shader, x: usize, y: usize, a: u8) {
    let a = match clip {
        Some(mask) => {
            let m = mask[y * width + x];
            if m == 0 {
                return;
            }
            // Skia's anti-aliased clip blitter: alpha * (clip + 1) / 256.
            ((u32::from(a) * (u32::from(m) + 1)) >> 8) as u8
        }
        None => a,
    };
    let at = (y * width + x) * 4;
    blend(&mut pixels[at..at + 4], shader.colour_at(x, y), a);
}

/// The error for a command whose `field` is not a finite number: the command's place in the list says which.
fn not_finite(index: usize, command: &str, field: &str) -> FaceError {
    FaceError::Raster(format!("command {index} ({command}): {field} is not a finite number"))
}

/// Rejects a command with a number that is not finite (NaN or infinite), naming it. Nothing is skipped silently: a NaN
/// opacity would otherwise erase what is under it, and a NaN coordinate would lose a shape without a trace. Values that
/// are finite but draw nothing (a zero width, a gradient with no span) are not errors, as on the canvas.
pub(crate) fn check(index: usize, cmd: &Cmd, list: &DrawList) -> Result<(), FaceError> {
    let check_path = |name: &str, path: PathRef| -> Result<(), FaceError> {
        let ok = list.elements(path).iter().all(|el| match *el {
            PathEl::Move(p) | PathEl::Line(p) => p.is_finite(),
            PathEl::Quad(c, p) => c.is_finite() && p.is_finite(),
            PathEl::Cubic(a, b, p) => a.is_finite() && b.is_finite() && p.is_finite(),
            PathEl::Close => true,
        });
        if ok { Ok(()) } else { Err(not_finite(index, name, "a path coordinate")) }
    };
    let check_paint = |name: &str, paint: &Paint| -> Result<(), FaceError> {
        let (stops, field) = match *paint {
            Paint::Solid(_) => return Ok(()),
            Paint::Linear { from, to, ref stops } => {
                if !(from.is_finite() && to.is_finite()) {
                    return Err(not_finite(index, name, "a gradient point"));
                }
                (stops, "a gradient stop offset")
            }
            Paint::Radial { centre, radius, ref stops } => {
                if !(centre.is_finite() && radius.is_finite()) {
                    return Err(not_finite(index, name, "the gradient centre or radius"));
                }
                (stops, "a gradient stop offset")
            }
        };
        if stops.as_slice().iter().all(|s| s.offset.is_finite()) { Ok(()) } else { Err(not_finite(index, name, field)) }
    };
    match *cmd {
        Cmd::Fill { path, ref paint, opacity } => {
            if !opacity.is_finite() {
                return Err(not_finite(index, "Fill", "opacity"));
            }
            check_paint("Fill", paint)?;
            check_path("Fill", path)
        }
        Cmd::Stroke { path, ref paint, width, opacity, .. } => {
            if !width.is_finite() {
                return Err(not_finite(index, "Stroke", "width"));
            }
            if !opacity.is_finite() {
                return Err(not_finite(index, "Stroke", "opacity"));
            }
            check_paint("Stroke", paint)?;
            check_path("Stroke", path)
        }
        Cmd::PushClip(path) => check_path("PushClip", path),
        Cmd::PushTransform(t) if !t.is_finite() => Err(not_finite(index, "PushTransform", "the matrix")),
        Cmd::PushTransform(_) | Cmd::PopClip | Cmd::PopTransform => Ok(()),
    }
}
