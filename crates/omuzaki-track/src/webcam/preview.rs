//! The preview tap: a downscaled copy of the newest frame for the UI, made on the camera thread only while it is on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use crate::frame::RgbFrame;
use crate::landmarks::NUM_LANDMARKS;

/// The widest a preview gets, in pixels. Narrower frames are not enlarged.
pub const PREVIEW_MAX_WIDTH: u32 = 640;

/// 478 landmarks in preview pixels.
type Points = Box<[[f32; 2]; NUM_LANDMARKS]>;

/// A preview frame for the UI: at most [`PREVIEW_MAX_WIDTH`] wide, packed RGB8, with the landmarks in preview pixels.
/// Reuse one value across calls to [`WebcamTracker::preview`](super::WebcamTracker::preview): it keeps its buffers.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct PreviewFrame {
    /// `width * height` pixels of R, G, B.
    pub rgb: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The `TrackFrame::seq` this preview was taken with.
    pub seq: u64,
    /// The face's 478 landmarks as `[x, y]` in preview pixels, or `None` when no face was found in this frame.
    pub landmarks: Option<Points>,
}

/// The camera thread's private preview buffer: filled without any lock, then swapped into the shared slot.
#[derive(Default)]
pub(super) struct Scratch {
    frame: PreviewFrame,
    /// The landmark box while the face is absent, so a face that comes back allocates nothing.
    spare: Option<Points>,
}

#[derive(Default)]
struct Slot {
    frame: PreviewFrame,
    fresh: bool,
}

/// Shared between the camera thread (writer) and the UI (reader).
#[derive(Default)]
pub(super) struct PreviewTap {
    on: AtomicBool,
    slot: Mutex<Slot>,
}

impl PreviewTap {
    pub(super) fn set(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
    }

    /// Camera thread: downscales `rgb` into `scratch` and swaps it into the slot, unless the tap is off or the reader
    /// holds the slot right now (that frame is skipped, never waited for).
    // Preview sizes are at most 640 wide and a few thousand high: exact in an `f32`.
    #[allow(clippy::cast_precision_loss)]
    pub(super) fn offer(
        &self,
        scratch: &mut Scratch,
        rgb: &RgbFrame<'_>,
        seq: u64,
        landmarks: Option<&[[f32; 3]; NUM_LANDMARKS]>,
    ) {
        if !self.on.load(Ordering::Relaxed) || !downscale(rgb, &mut scratch.frame) {
            return;
        }
        scratch.frame.seq = seq;
        let (w, h) = (scratch.frame.width as f32, scratch.frame.height as f32);
        let mut points = scratch.frame.landmarks.take().or_else(|| scratch.spare.take());
        if let Some(src) = landmarks {
            let dst = points.get_or_insert_with(|| Box::new([[0.0; 2]; NUM_LANDMARKS]));
            for (d, s) in dst.iter_mut().zip(src) {
                *d = [s[0] * w, s[1] * h];
            }
            scratch.frame.landmarks = points;
        } else {
            scratch.spare = points;
        }
        if let Ok(mut slot) = self.slot.try_lock() {
            std::mem::swap(&mut slot.frame, &mut scratch.frame);
            slot.fresh = true;
        }
    }

    /// UI thread: copies the newest preview into `out`; false if none is new since the last call.
    pub(super) fn take(&self, out: &mut PreviewFrame) -> bool {
        // A panic on the camera thread cannot leave the slot half-written in a way that matters: it is a pixel copy.
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if !slot.fresh {
            return false;
        }
        slot.fresh = false;
        let src = &slot.frame;
        out.rgb.clear();
        out.rgb.extend_from_slice(&src.rgb);
        (out.width, out.height, out.seq) = (src.width, src.height, src.seq);
        match (&mut out.landmarks, &src.landmarks) {
            (Some(dst), Some(points)) => **dst = **points,
            (dst, points) => dst.clone_from(points),
        }
        true
    }
}

/// Box-filters `src` down to at most [`PREVIEW_MAX_WIDTH`] wide (each output pixel is the mean of the source pixels it
/// covers). False, with `dst` unchanged in size, if `src` is not a valid frame.
fn downscale(src: &RgbFrame<'_>, dst: &mut PreviewFrame) -> bool {
    if src.validate().is_err() {
        return false;
    }
    let (sw, sh) = (src.width as usize, src.height as usize);
    let ow = src.width.min(PREVIEW_MAX_WIDTH);
    let oh = u32::try_from((u64::from(src.height) * u64::from(ow) / u64::from(src.width)).max(1)).unwrap_or(1);
    let (cols, rows) = (ow as usize, oh as usize);
    dst.rgb.resize(cols * rows * 3, 0);
    for (oy, row) in dst.rgb.chunks_exact_mut(cols * 3).enumerate() {
        let y0 = oy * sh / rows;
        let y1 = ((oy + 1) * sh / rows).max(y0 + 1);
        for (ox, px) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let x0 = ox * sw / cols;
            let x1 = ((ox + 1) * sw / cols).max(x0 + 1);
            let mut sum = [0_u32; 3];
            for y in y0..y1 {
                let from = y * src.stride;
                for p in src.data[from + x0 * 3..from + x1 * 3].as_chunks::<3>().0 {
                    for (s, &v) in sum.iter_mut().zip(p) {
                        *s += u32::from(v);
                    }
                }
            }
            let n = u32::try_from((y1 - y0) * (x1 - x0)).unwrap_or(u32::MAX);
            for (o, s) in px.iter_mut().zip(sum) {
                *o = u8::try_from((s + n / 2) / n).unwrap_or(u8::MAX);
            }
        }
    }
    (dst.width, dst.height) = (ow, oh);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(data: &[u8], width: u32, height: u32) -> RgbFrame<'_> {
        RgbFrame { data, width, height, stride: width as usize * 3 }
    }

    #[test]
    fn a_narrow_frame_is_copied_not_enlarged() {
        let data: Vec<u8> = (0..4 * 2 * 3).collect();
        let mut out = PreviewFrame::default();
        assert!(downscale(&frame(&data, 4, 2), &mut out));
        assert_eq!((out.width, out.height, out.rgb), (4, 2, data));
    }

    #[test]
    fn a_wide_frame_is_box_filtered_to_640() {
        // 1280x4: columns alternate 0 and 100, rows alternate 0 and 50; every 2x2 box averages to 75.
        let data: Vec<u8> = (0..4_usize)
            .flat_map(|y| {
                (0..1280_usize)
                    .flat_map(move |x| [(if x % 2 == 0 { 0 } else { 100 }) + (if y % 2 == 0 { 0 } else { 50 }); 3])
            })
            .collect();
        let mut out = PreviewFrame::default();
        assert!(downscale(&frame(&data, 1280, 4), &mut out));
        assert_eq!((out.width, out.height), (640, 2));
        assert!(out.rgb.iter().all(|&v| v == 75), "{:?}", &out.rgb[..6]);
    }

    #[test]
    fn a_malformed_frame_is_refused() {
        let mut out = PreviewFrame::default();
        assert!(!downscale(&frame(&[0; 5], 4, 2), &mut out));
        assert_eq!(out.width, 0);
    }
}
