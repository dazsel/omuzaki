//! One image graph of the body tracker: a session, its input tensor and output buffers, and the warp from a region of
//! the frame into the input and back.

use omuzaki_ai_api::{DType, Dim, Session, Signature, TensorBuf, TensorRef, TensorSpec};

use crate::error::{FaceTrackError, Result};
use crate::frame::{Border, ImageSpec, RgbFrame, warp_to_tensor};
use crate::roi::{Letterbox, NormRect, RotatedRect, pad_to_aspect, roi_to_frame};

/// A slot signature: one square NHWC image in, the named outputs of the given shapes out.
pub(super) fn image_signature(side: usize, outputs: &[(&str, &[usize])]) -> Signature {
    Signature {
        inputs: vec![TensorSpec::new("image", DType::F32, [1, side, side, 3].map(Dim::Fixed))],
        outputs: outputs
            .iter()
            .map(|(name, shape)| TensorSpec::new(*name, DType::F32, shape.iter().map(|&d| Dim::Fixed(d)).collect::<Vec<_>>()))
            .collect(),
    }
}

/// Where the last run's input came from: the region, padded to the input's square, and the padding.
#[derive(Copy, Clone, Debug)]
pub(super) struct Placement {
    /// The padded region in frame pixels.
    pub roi: RotatedRect,
    /// The padding as fractions of the input.
    pub letterbox: Letterbox,
    /// Frame width in pixels.
    pub fw: f32,
    /// Frame height in pixels.
    pub fh: f32,
}

impl Placement {
    /// A point given in input pixels, in normalised frame coordinates.
    #[allow(clippy::cast_precision_loss)] // input sides are small constants
    pub fn to_frame(&self, side: usize, x: f32, y: f32) -> (f32, f32) {
        let lb = &self.letterbox;
        let (horizontal, vertical) = (1.0 - lb.left - lb.right, 1.0 - lb.top - lb.bottom);
        let u = (x / side as f32 - lb.left) / horizontal;
        let v = (y / side as f32 - lb.top) / vertical;
        // The padded ROI covers the input; the unpadded one is what `u`, `v` are relative to.
        let unpadded = RotatedRect { width: self.roi.width * horizontal, height: self.roi.height * vertical, ..self.roi };
        roi_to_frame(&unpadded, self.fw, self.fh, u, v)
    }
}

/// A graph over a square image input.
pub(super) struct ImageGraph {
    name: &'static str,
    session: Box<dyn Session>,
    side: usize,
    spec: ImageSpec,
    input: Vec<f32>,
    outputs: Vec<TensorBuf>,
}

impl ImageGraph {
    /// Wraps `session`, which must present `signature` exactly; `range` and `border` are how the graph reads pixels.
    pub fn new(
        name: &'static str,
        session: Box<dyn Session>,
        signature: &Signature,
        range: (f32, f32),
        border: Border,
    ) -> Result<Self> {
        crate::error::check_signature(name, signature, session.signature())?;
        let side = match signature.inputs.first().map(|s| s.shape.as_slice()) {
            Some([_, Dim::Fixed(side), _, _]) => *side,
            _ => return Err(FaceTrackError::Shape { graph: name }),
        };
        let outputs = TensorBuf::for_outputs(session.signature());
        Ok(Self {
            name,
            session,
            side,
            spec: ImageSpec { width: side, height: side, range, border },
            input: vec![0.0; side * side * 3],
            outputs,
        })
    }

    /// The input side in pixels.
    pub fn side(&self) -> usize {
        self.side
    }

    /// Forgets the session's carried state.
    pub fn reset(&mut self) {
        self.session.reset();
    }

    /// Warps `rect` of `frame` into the input, letterboxed to its square, and runs the graph. `None` when the region
    /// has no area.
    #[allow(clippy::cast_precision_loss)] // frame sizes are far below f32's 24 bits
    pub fn run(&mut self, frame: &RgbFrame<'_>, rect: &NormRect) -> Result<Option<Placement>> {
        let (fw, fh) = (frame.width as f32, frame.height as f32);
        let mut roi = rect.to_pixels(fw, fh);
        let Some(letterbox) = pad_to_aspect(&mut roi, self.side, self.side) else { return Ok(None) };
        warp_to_tensor(frame, &roi, self.spec, &mut self.input)?;
        self.session.run(&[TensorRef::f32(&self.input, &[1, self.side, self.side, 3])], &mut self.outputs)?;
        Ok(Some(Placement { roi, letterbox, fw, fh }))
    }

    /// Output `index` of the last run, which must hold `len` values.
    pub fn output(&self, index: usize, len: usize) -> Result<&[f32]> {
        match self.outputs.get(index).and_then(TensorBuf::as_f32) {
            Some(values) if values.len() == len => Ok(values),
            _ => Err(FaceTrackError::Shape { graph: self.name }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roi::Letterbox;

    #[test]
    fn input_pixels_map_back_through_padding_and_rotation() {
        // A 100x50 region at (300, 200), padded to a 100x100 square: a quarter of the input is padding top and bottom.
        let placement = Placement {
            roi: RotatedRect { cx: 300.0, cy: 200.0, width: 100.0, height: 100.0, rotation: 0.0 },
            letterbox: Letterbox { left: 0.0, top: 0.25, right: 0.0, bottom: 0.25 },
            fw: 600.0,
            fh: 400.0,
        };
        let (x, y) = placement.to_frame(256, 128.0, 128.0);
        assert!((x - 0.5).abs() < 1e-6 && (y - 0.5).abs() < 1e-6);
        // The input's top edge is in the padding, a quarter of the square above the region's top: y = 150 px.
        let (_, top) = placement.to_frame(256, 128.0, 0.0);
        assert!((top - 150.0 / 400.0).abs() < 1e-6, "{top}");
    }
}
