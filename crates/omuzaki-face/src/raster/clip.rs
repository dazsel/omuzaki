//! The clip stack: one coverage mask per nesting level, each already intersected with the levels below.

use omuzaki_core::glam::Affine2;
use omuzaki_face_api::FaceError;

use super::path::Outline;
use super::scan::Scan;

/// Drawing reads only the top mask. Masks are pooled and reused, so a deep stack costs memory once, not per call.
#[derive(Debug, Default)]
pub(super) struct ClipStack {
    masks: Vec<Vec<u8>>,
    /// How many of `masks` are in use.
    depth: usize,
    size: (usize, usize),
}

impl ClipStack {
    /// Empties the stack for a raster of `width` x `height`; the pool is dropped only when the size changes.
    pub fn reset(&mut self, width: usize, height: usize) {
        if self.size != (width, height) {
            self.masks.clear();
            self.size = (width, height);
        }
        self.depth = 0;
    }

    /// The coverage (0..=255 per pixel, row by row) everything drawn now is limited to, or `None` outside any clip.
    pub fn current(&self) -> Option<&[u8]> {
        self.depth.checked_sub(1).and_then(|top| self.masks.get(top)).map(Vec::as_slice)
    }

    /// Clips to `outline` mapped through `map`: the new mask is the outline's coverage times the mask below.
    pub fn push(&mut self, outline: &Outline, map: Affine2, scan: &mut Scan) {
        let (width, height) = self.size;
        if self.masks.len() == self.depth {
            self.masks.push(vec![0; width * height]);
        }
        let (below, rest) = self.masks.split_at_mut(self.depth);
        let top = &mut rest[0];
        top.fill(0);
        outline.feed(map, scan);
        let parent = below.last();
        scan.sweep(|y, x0, alphas| {
            let row = y * width + x0;
            let cells = &mut top[row..row + alphas.len()];
            match parent {
                Some(parent) => {
                    for ((cell, &own), &up) in cells.iter_mut().zip(alphas).zip(&parent[row..]) {
                        *cell = mul_255(own, up);
                    }
                }
                None => cells.copy_from_slice(alphas),
            }
        });
        self.depth += 1;
    }

    pub fn pop(&mut self) -> Result<(), FaceError> {
        self.depth =
            self.depth.checked_sub(1).ok_or_else(|| FaceError::Raster("PopClip without a PushClip".to_owned()))?;
        Ok(())
    }
}

/// `a * b / 255`, rounded: how Skia's anti-aliased clip intersects two coverages.
#[allow(clippy::cast_possible_truncation)] // the result is at most 255
fn mul_255(a: u8, b: u8) -> u8 {
    let p = u32::from(a) * u32::from(b) + 128;
    ((p + (p >> 8)) >> 8) as u8
}
