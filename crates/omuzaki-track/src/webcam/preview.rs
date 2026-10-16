//! The preview tap: a downscaled copy of the newest frame for the UI, made on the camera thread only while it is on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, TryLockError};

use crate::frame::RgbFrame;
use crate::landmarks::NUM_LANDMARKS;

/// The widest a preview gets, in pixels. Narrower frames are not enlarged.
pub const PREVIEW_MAX_WIDTH: u32 = 640;

/// Fixed-point one for the box filter's division by a pixel count.
const RECIP_BITS: u32 = 24;
const RECIP_ONE: u64 = 1 << RECIP_BITS;

/// 478 landmarks in preview pixels.
type Points = Box<[[f32; 2]; NUM_LANDMARKS]>;

/// A preview frame for the UI: at most [`PREVIEW_MAX_WIDTH`] wide, packed RGB8, with the landmarks in preview pixels.
/// Reuse one value across calls to [`WebcamTracker::preview`](super::WebcamTracker::preview): it keeps its buffers.
#[derive(Clone, Debug, Default)]
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
    /// The landmark box while `landmarks` is `None`, so a face that comes back allocates nothing. Every write to
    /// `landmarks` in this module goes through [`set_landmarks`](Self::set_landmarks).
    spare: Option<Points>,
}

impl PreviewFrame {
    /// Sets `landmarks` to `points` (written by `fill`) or to `None`, keeping the box in either case: the one place
    /// the landmark storage changes hands, whether the camera thread fills a preview or the UI copies one.
    fn set_landmarks(&mut self, fill: Option<impl FnOnce(&mut [[f32; 2]; NUM_LANDMARKS])>) {
        match fill {
            Some(fill) => {
                let spare = &mut self.spare;
                let points = self
                    .landmarks
                    .get_or_insert_with(|| spare.take().unwrap_or_else(|| Box::new([[0.0; 2]; NUM_LANDMARKS])));
                fill(points);
            }
            None => {
                if let Some(points) = self.landmarks.take() {
                    self.spare = Some(points);
                }
            }
        }
    }
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

impl PreviewFrame {
    /// Box-filters `rgb` into this frame, at most [`PREVIEW_MAX_WIDTH`] wide, reusing its buffer: what the preview tap
    /// does with each camera frame. Leaves `seq` and `landmarks` alone. False, and nothing written, if `rgb` is not a
    /// valid frame.
    pub fn fill_from(&mut self, rgb: &RgbFrame<'_>) -> bool {
        downscale(rgb, self)
    }
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
        scratch: &mut PreviewFrame,
        rgb: &RgbFrame<'_>,
        seq: u64,
        landmarks: Option<&[[f32; 3]; NUM_LANDMARKS]>,
    ) {
        if !self.on.load(Ordering::Relaxed) || !downscale(rgb, scratch) {
            return;
        }
        scratch.seq = seq;
        let (w, h) = (scratch.width as f32, scratch.height as f32);
        scratch.set_landmarks(landmarks.map(|src| {
            move |dst: &mut [[f32; 2]; NUM_LANDMARKS]| {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = [s[0] * w, s[1] * h];
                }
            }
        }));
        if let Ok(mut slot) = self.slot.try_lock() {
            std::mem::swap(&mut slot.frame, scratch);
            slot.fresh = true;
        }
    }

    /// UI thread: copies the newest preview into `out`; false if none is new since the last call. Never waits: if the
    /// camera thread holds the slot (to swap a frame in), there is nothing new to report yet and the next call will
    /// find it, as the camera thread in turn skips a frame rather than wait for a reader.
    pub(super) fn take(&self, out: &mut PreviewFrame) -> bool {
        // A panic on the camera thread cannot leave the slot half-written in a way that matters: it is a pixel copy.
        let mut slot = match self.slot.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return false,
        };
        if !slot.fresh {
            return false;
        }
        slot.fresh = false;
        let src = &slot.frame;
        out.rgb.clear();
        out.rgb.extend_from_slice(&src.rgb);
        (out.width, out.height, out.seq) = (src.width, src.height, src.seq);
        out.set_landmarks(src.landmarks.as_deref().map(|points| |dst: &mut [[f32; 2]; NUM_LANDMARKS]| *dst = *points));
        true
    }
}

/// Box-filters `src` down to at most [`PREVIEW_MAX_WIDTH`] wide (each output pixel is the mean of the source pixels it
/// covers, rounded, to within one level of the exact mean). False, with `dst` unchanged in size, if `src` is not a valid frame.
fn downscale(src: &RgbFrame<'_>, dst: &mut PreviewFrame) -> bool {
    if src.validate().is_err() {
        return false;
    }
    let (sw, sh) = (src.width as usize, src.height as usize);
    let ow = src.width.min(PREVIEW_MAX_WIDTH);
    let oh = u32::try_from((u64::from(src.height) * u64::from(ow) / u64::from(src.width)).max(1)).unwrap_or(1);
    let (cols, rows) = (ow as usize, oh as usize);
    dst.rgb.resize(cols * rows * 3, 0);
    // Where each output column starts in the source. `cols <= sw`, so every box is at least one pixel wide. Worked
    // out once, so the pixel loop below has no division in it.
    let mut edges = [0_usize; PREVIEW_MAX_WIDTH as usize + 1];
    for (i, edge) in edges[..=cols].iter_mut().enumerate() {
        *edge = i * sw / cols;
    }
    // Most boxes hold the same number of pixels, so the reciprocal is worked out only when that number changes.
    let (mut last_n, mut recip) = (0_u64, 0_u64);
    for (oy, row) in dst.rgb.chunks_exact_mut(cols * 3).enumerate() {
        let y0 = oy * sh / rows;
        let y1 = ((oy + 1) * sh / rows).max(y0 + 1);
        for (ox, px) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let (x0, x1) = (edges[ox], edges[ox + 1]);
            let mut sum = [0_u32; 3];
            for y in y0..y1 {
                let from = y * src.stride;
                for p in src.data[from + x0 * 3..from + x1 * 3].as_chunks::<3>().0 {
                    for (s, &v) in sum.iter_mut().zip(p) {
                        *s += u32::from(v);
                    }
                }
            }
            let n = u64::try_from((y1 - y0) * (x1 - x0)).unwrap_or(u64::MAX);
            if n != last_n {
                (last_n, recip) = (n, RECIP_ONE / n);
            }
            for (o, s) in px.iter_mut().zip(sum) {
                *o = u8::try_from((u64::from(s) * recip + RECIP_ONE / 2) >> RECIP_BITS).unwrap_or(u8::MAX);
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
    fn a_poll_while_the_camera_thread_holds_the_slot_returns_at_once_and_the_next_one_gets_the_frame() {
        let tap = PreviewTap::default();
        tap.set(true);
        let data = vec![9_u8; 4 * 2 * 3];
        tap.offer(&mut PreviewFrame::default(), &frame(&data, 4, 2), 7, None);
        let mut out = PreviewFrame::default();
        {
            // The camera thread is in the middle of its swap. On this thread a blocking `lock` would never return.
            let _swapping = tap.slot.lock().unwrap();
            assert!(!tap.take(&mut out), "nothing is reported while the slot is held");
            assert_eq!(out.width, 0, "and `out` is untouched");
        }
        assert!(tap.take(&mut out));
        assert_eq!(out.seq, 7);
        assert_eq!(out.rgb, data);
        assert!(!tap.take(&mut out), "a frame is reported once");
    }

    #[test]
    fn a_malformed_frame_is_refused() {
        let mut out = PreviewFrame::default();
        assert!(!downscale(&frame(&[0; 5], 4, 2), &mut out));
        assert_eq!(out.width, 0);
    }
}
