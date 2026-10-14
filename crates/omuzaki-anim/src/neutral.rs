//! Neutral capture: the robust form of "relax and look at the camera" (spec PF-20, V0).
//!
//! [`Calibration::rest_at`] takes one frame as the rest face, so a blink or a tracker glitch becomes the performer's
//! neutral. [`NeutralCapture`] takes the median over N tracked frames instead, which a few outliers cannot move.
//!
//! It runs on the perform thread while the performer is live, so [`NeutralCapture::push`] and
//! [`NeutralCapture::finish`] never allocate: every buffer is sized once, in [`NeutralCapture::new`].

use omuzaki_anim_api::{ARKIT_COUNT, Calibration, Channel, ChannelRange, HeadPose, TrackFrame, TrackStatus};
use omuzaki_core::glam::Vec3;

/// The most tracked frames a neutral may be taken over (about 20 s at 30 fps).
pub const MAX_NEUTRAL_FRAMES: usize = 600;
/// The fewest tracked frames a neutral may be taken over. Below this a median is too easily moved by one glitch.
pub const MIN_NEUTRAL_FRAMES: usize = 15;

/// Why a neutral could not be taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NeutralError {
    /// Fewer tracked frames were held than the minimum: the face was not visible for long enough.
    #[error("only {got} tracked frames, need {need}: is the face visible?")]
    TooFewFrames {
        /// Tracked frames actually held.
        got: usize,
        /// The minimum that [`NeutralCapture::finish`] needs.
        need: usize,
    },
}

// Head columns: the three angles, then the three position components.
const YAW: usize = 0;
const PITCH: usize = 1;
const ROLL: usize = 2;
const POS_X: usize = 3;
const POS_Y: usize = 4;
const POS_Z: usize = 5;
const HEAD_COLUMNS: usize = 6;

/// Collects tracked frames, then moves each channel's rest to its median.
///
/// Samples are stored column-major: each channel (and each head component) has its own contiguous column, so a median
/// reads one run of memory. A channel's column holds only the frames where it was present.
pub struct NeutralCapture {
    /// Tracked frames to hold before [`push`](Self::push) reports done.
    target: usize,
    /// Tracked frames held so far.
    held: usize,
    /// Channel `i`'s samples are `values[i * target..][..counts[i]]`.
    values: Vec<f32>,
    /// How many held frames had each channel present.
    counts: [usize; ARKIT_COUNT],
    /// Head samples, one column per [`HEAD_COLUMNS`] entry: angles hold `head_count` samples, position `position_count`.
    head: Vec<f32>,
    head_count: usize,
    position_count: usize,
    /// A copy of one column, reordered in place by the median's selection.
    scratch: Vec<f32>,
}

impl NeutralCapture {
    /// A capture that holds `target` tracked frames, clamped to [`MIN_NEUTRAL_FRAMES`]..=[`MAX_NEUTRAL_FRAMES`]. The
    /// only allocation: every later call reuses these buffers.
    pub fn new(target: usize) -> Self {
        let target = target.clamp(MIN_NEUTRAL_FRAMES, MAX_NEUTRAL_FRAMES);
        Self {
            target,
            held: 0,
            values: vec![0.0; ARKIT_COUNT * target],
            counts: [0; ARKIT_COUNT],
            head: vec![0.0; HEAD_COLUMNS * target],
            head_count: 0,
            position_count: 0,
            scratch: vec![0.0; target],
        }
    }

    /// Adds a frame. [`TrackStatus::Lost`] frames are ignored, and so are frames after the target is reached.
    /// Returns `true` once `target` tracked frames are held.
    pub fn push(&mut self, frame: &TrackFrame) -> bool {
        if self.held >= self.target {
            return true;
        }
        if frame.status == TrackStatus::Tracking {
            self.store(frame);
            self.held += 1;
        }
        self.held >= self.target
    }

    /// `(tracked frames held, target)`.
    pub fn progress(&self) -> (usize, usize) {
        (self.held, self.target)
    }

    /// `base` with each qualifying channel's `lo` moved to its median, and `hi` moved with it so the span is kept
    /// (as [`Calibration::rest_at`] does). `head_rest` takes the median head pose, component by component.
    ///
    /// A channel qualifies if it is present in at least half the held frames and in at least
    /// [`MIN_NEUTRAL_FRAMES`] of them; a channel that does not keeps `base`'s range. The head works the same way, and
    /// its position is taken only when present in enough frames. Samples are kept, so calling this twice gives the
    /// same result.
    ///
    /// # Errors
    ///
    /// [`NeutralError::TooFewFrames`] if fewer than [`MIN_NEUTRAL_FRAMES`] tracked frames are held.
    pub fn finish(&mut self, base: &Calibration) -> Result<Calibration, NeutralError> {
        if self.held < MIN_NEUTRAL_FRAMES {
            return Err(NeutralError::TooFewFrames { got: self.held, need: MIN_NEUTRAL_FRAMES });
        }
        let mut out = *base;
        for info in Channel::arkit() {
            let i = info.channel.index();
            let n = self.counts[i];
            if !qualifies(n, self.held) {
                continue;
            }
            let m = median_of(&mut self.scratch, column(&self.values, self.target, i, n));
            let range = out.range(info.channel);
            // Same arithmetic as `Calibration::rest_at`, so the two agree bit for bit.
            let span = range.hi - range.lo;
            out.set_range(info.channel, ChannelRange { lo: m, hi: m + span });
        }
        out.head_rest = self.head_rest(base.head_rest);
        Ok(out)
    }

    /// Forgets every held frame, so the capture can be started again. The buffers are kept.
    pub fn reset(&mut self) {
        self.held = 0;
        self.counts = [0; ARKIT_COUNT];
        self.head_count = 0;
        self.position_count = 0;
    }

    /// Copies a tracked frame's present channels and head pose into the columns.
    fn store(&mut self, frame: &TrackFrame) {
        let t = self.target;
        for channel in frame.present.iter().filter(|c| c.is_arkit()) {
            let i = channel.index();
            self.values[i * t + self.counts[i]] = frame.blend[i];
            self.counts[i] += 1;
        }
        if let Some(head) = frame.head {
            let n = self.head_count;
            self.head[YAW * t + n] = head.yaw;
            self.head[PITCH * t + n] = head.pitch;
            self.head[ROLL * t + n] = head.roll;
            self.head_count += 1;
            if let Some(p) = head.position {
                let m = self.position_count;
                self.head[POS_X * t + m] = p.x;
                self.head[POS_Y * t + m] = p.y;
                self.head[POS_Z * t + m] = p.z;
                self.position_count += 1;
            }
        }
    }

    /// The head rest: each angle and, if enough frames had one, the position replaced by its median; the rest keeps
    /// `base`'s value.
    fn head_rest(&mut self, base: HeadPose) -> HeadPose {
        let t = self.target;
        let mut rest = base;
        if qualifies(self.head_count, self.held) {
            let n = self.head_count;
            rest.yaw = median_of(&mut self.scratch, column(&self.head, t, YAW, n));
            rest.pitch = median_of(&mut self.scratch, column(&self.head, t, PITCH, n));
            rest.roll = median_of(&mut self.scratch, column(&self.head, t, ROLL, n));
        }
        if qualifies(self.position_count, self.held) {
            let n = self.position_count;
            rest.position = Some(Vec3::new(
                median_of(&mut self.scratch, column(&self.head, t, POS_X, n)),
                median_of(&mut self.scratch, column(&self.head, t, POS_Y, n)),
                median_of(&mut self.scratch, column(&self.head, t, POS_Z, n)),
            ));
        }
        rest
    }
}

/// Whether a value seen in `present` of `held` frames is trustworthy enough to fit: at least half the frames, and at
/// least [`MIN_NEUTRAL_FRAMES`] of them, so one glitch cannot set a rest.
fn qualifies(present: usize, held: usize) -> bool {
    present >= MIN_NEUTRAL_FRAMES && present * 2 >= held
}

/// The first `len` samples of column `index`, where each column is `stride` long.
fn column(buf: &[f32], stride: usize, index: usize, len: usize) -> &[f32] {
    &buf[index * stride..][..len]
}

/// The median of `column`, computed in `scratch` so the column keeps its order.
fn median_of(scratch: &mut [f32], column: &[f32]) -> f32 {
    let scratch = &mut scratch[..column.len()];
    scratch.copy_from_slice(column);
    median(scratch)
}

/// The median of `samples`, which it reorders. For an even count, the mean of the two middle values.
fn median(samples: &mut [f32]) -> f32 {
    let len = samples.len();
    // `total_cmp` gives a total order, so a NaN sample cannot break the selection.
    let (below, upper, _) = samples.select_nth_unstable_by(len / 2, f32::total_cmp);
    let upper = *upper;
    if len % 2 == 1 {
        return upper;
    }
    // After selection, everything before the middle index is at most the upper middle value.
    below.iter().copied().max_by(f32::total_cmp).map_or(upper, |lower| f32::midpoint(lower, upper))
}
