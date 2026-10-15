//! The GPU rasteriser: draws a `DrawList` through Vello. This module is the CPU half, [`SceneEncoder`], which turns a
//! list into a `vello::Scene`; it does no GPU work. Strokes beyond the basic mapping (hairline parity) are [`stroke`]'s.

pub mod clip;
pub mod fill;
pub mod gradient;
pub mod stroke;

use omuzaki_core::glam::Affine2;
use omuzaki_face_api::{Cap, Cmd, DrawList, FaceError, FaceLayerSpec, Join};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap as KCap, Join as KJoin, Rect, Stroke};
use vello::peniko::{ColorStop, Fill};

use self::clip::{OpenStack, Transforms};
use self::fill::{Bounds, fill_bez};
use self::gradient::brush;

/// The most a layer may be per side, as in the CPU raster.
const MAX_SIDE: u32 = 16_384;
/// The miter limit the CPU raster and the canvas use.
const MITER_LIMIT: f64 = 10.0;

/// Encodes draw lists into Vello scenes. Keep one per layer: it owns the scratch buffers, so a warm encode only
/// allocates what Vello itself does.
#[derive(Default)]
pub struct SceneEncoder {
    path: BezPath,
    stops: Vec<ColorStop>,
    transforms: Transforms,
    clips: OpenStack<()>,
    bounds: Bounds,
}

/// A face-plane affine as a kurbo one.
fn affine(a: Affine2) -> Affine {
    Affine::new(a.to_cols_array().map(f64::from))
}

impl SceneEncoder {
    /// An encoder with empty buffers.
    pub fn new() -> Self {
        Self::default()
    }

    /// The pixel rectangle the last [`encode`](Self::encode) drew into, or `None` if it drew nothing. It is a
    /// conservative hull (control points, stroke reach, clips ignored), so a damage rectangle built from it is never
    /// too small.
    pub fn last_bounds(&self) -> Option<Rect> {
        self.bounds.get()
    }

    /// Appends the encoding of `list` to `scene`, which the caller owns and resets. The layer-to-pixel transform is the
    /// CPU raster's: the plane's y axis points up, the rectangle fills the `size_px` image exactly.
    ///
    /// # Errors
    /// [`FaceError::Raster`] if the rectangle or density is not positive, the image is too large, a transform overflows,
    /// or a pop has no push or a push is never popped (the message names the command's index). `scene` is then
    /// unspecified: reset it.
    pub fn encode(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        px_per_m: f32,
        scene: &mut Scene,
    ) -> Result<(), FaceError> {
        let rect = spec.rect;
        if !(rect.width() > 0.0 && rect.height() > 0.0 && px_per_m.is_finite() && px_per_m > 0.0) {
            return Err(FaceError::Raster("the layer rectangle and density must be positive".to_owned()));
        }
        let (w, h) = rect.size_px(px_per_m);
        if w > MAX_SIDE || h > MAX_SIDE {
            return Err(FaceError::Raster(format!("a {w} x {h} raster is larger than the {MAX_SIDE} px limit")));
        }
        #[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^24
        let (sx, sy) = (f64::from(w as f32 / rect.width()), f64::from(h as f32 / rect.height()));
        let to_px = Affine::new([sx, 0.0, 0.0, -sy, -f64::from(rect.x0) * sx, f64::from(rect.y1) * sy]);

        self.bounds.clear();
        self.transforms.clear();
        self.clips.clear();
        for (index, cmd) in list.cmds().iter().enumerate() {
            let current = self.transforms.top().map_or(to_px, |t| *t);
            self.command(index, cmd, current, list, scene)?;
        }
        self.finish(scene)
    }

    /// Encodes command `index`, drawing under `to_px`, the plane-to-pixel transform in force.
    fn command(
        &mut self,
        index: usize,
        cmd: &Cmd,
        to_px: Affine,
        list: &DrawList,
        scene: &mut Scene,
    ) -> Result<(), FaceError> {
        match *cmd {
            Cmd::PushTransform(t) => {
                let next = to_px * affine(t);
                if !next.as_coeffs().iter().all(|c| c.is_finite()) {
                    return Err(FaceError::Raster(format!("command {index}: PushTransform overflows")));
                }
                self.transforms.push(index, next);
            }
            Cmd::PopTransform => {
                self.transforms.pop(index, "Transform")?;
            }
            Cmd::PushClip(path) => {
                fill_bez(list.elements(path), &mut self.path);
                scene.push_clip_layer(Fill::NonZero, to_px, &self.path);
                self.clips.push(index, ());
            }
            Cmd::PopClip => {
                self.clips.pop(index, "Clip")?;
                scene.pop_layer();
            }
            Cmd::Fill { path, ref paint, opacity } => {
                fill_bez(list.elements(path), &mut self.path);
                self.bounds.add(&self.path, to_px, 0.0);
                scene.fill(Fill::NonZero, to_px, &brush(paint, opacity, &mut self.stops), None, &self.path);
            }
            Cmd::Stroke { path, ref paint, width, cap, join, opacity } => {
                fill_bez(list.elements(path), &mut self.path);
                let mut style = Stroke::new(f64::from(width));
                style.start_cap = if cap == Cap::Round { KCap::Round } else { KCap::Butt };
                style.end_cap = style.start_cap;
                style.join = if join == Join::Round { KJoin::Round } else { KJoin::Miter };
                style.miter_limit = MITER_LIMIT;
                let reach = if join == Join::Miter { MITER_LIMIT } else { 1.0 };
                let [a, b, c, d, ..] = to_px.as_coeffs();
                let scale = a.hypot(b).max(c.hypot(d));
                self.bounds.add(&self.path, to_px, 0.5 * f64::from(width) * scale * reach);
                scene.stroke(&style, to_px, &brush(paint, opacity, &mut self.stops), None, &self.path);
            }
        }
        Ok(())
    }

    /// Checks nothing is left open; closes clips so the scene stays valid for Vello either way.
    fn finish(&mut self, scene: &mut Scene) -> Result<(), FaceError> {
        let open_clip = self.clips.oldest_open();
        for _ in 0..self.clips.depth() {
            scene.pop_layer();
        }
        if let Some(index) = open_clip {
            return Err(FaceError::Raster(format!("command {index}: PushClip is never popped")));
        }
        match self.transforms.oldest_open() {
            Some(index) => Err(FaceError::Raster(format!("command {index}: PushTransform is never popped"))),
            None => Ok(()),
        }
    }
}
