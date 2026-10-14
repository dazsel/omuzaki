//! The A/V delay for the performance stream (PF-12, ADR 0018 decision 7, `docs/design/audio-api.md` section 5.2).
//!
//! A voice changer makes the heard voice late, so the avatar's performance channels are held back by the same amount.
//! [`PlayoutDelay`] is a fixed-capacity ring of `(performance time, Layer)` that `omuzaki-stage` runs before
//! [`LayerStack::resolve`](crate::LayerStack::resolve), once per layer source. [`DelayRamp`] moves the applied delay
//! when the setting changes, so the face never jumps.
//!
//! The delay is one number for the whole stream; what differs per source is the **stamp** it pushes with:
//!
//! | Source | Stamp |
//! |---|---|
//! | tracker frames | capture time |
//! | hotkeys and triggers on the avatar | when they fired |
//! | audio-derived layers | `perf_time` (capture time of the sound) |
//! | AI-driven layers | `program_time - D`, so they land on time with the AI voice after the one delay |
//!
//! UI feedback and anything outside the avatar (scene switches, OBS actions, overlays) is never delayed and never
//! goes through here.
//!
//! Hot path: nothing here allocates or locks. A `PlayoutDelay<64>` is about 35 KB, so keep it in a field of a
//! long-lived struct (or a `Box`), not on a small stack.

use std::cmp::Ordering;
use std::fmt;

use omuzaki_core::time::Seconds;
use thiserror::Error;

use crate::{BlendMode, Layer};

/// Slots needed to hold at least 500 ms at 120 Hz (`0.5 * 120 = 60`, rounded up to a power of two).
pub const DEFAULT_CAPACITY: usize = 64;

/// The most the playout may run faster or slower than real time while the delay moves (10%).
pub const MAX_SLEW: f64 = 0.1;

/// Why [`PlayoutDelay::push`] refused a sample.
#[derive(Copy, Clone, Debug, PartialEq, Error)]
pub enum PlayoutError {
    /// The stamp was NaN or infinite.
    #[error("playout stamp {0} is not a finite time")]
    NotFinite(f64),
    /// The stamp was not later than the newest one held (stamps must strictly increase).
    #[error("playout stamp {got} is not after the newest stamp {newest}")]
    NotIncreasing {
        /// The newest stamp held.
        newest: f64,
        /// The refused stamp.
        got: f64,
    },
}

/// A ring of the last `N` timestamped layers of one source, read back at `now - delay`.
///
/// Stamps strictly increase. A full ring drops its oldest entry on the next push. `N` must be at least 1 (checked at
/// compile time); `N = 64` ([`DEFAULT_CAPACITY`]) holds 500 ms at 120 Hz.
#[derive(Clone)]
pub struct PlayoutDelay<const N: usize> {
    times: [f64; N],
    layers: [Layer; N],
    /// Index of the oldest entry.
    start: usize,
    len: usize,
}

impl<const N: usize> Default for PlayoutDelay<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Debug for PlayoutDelay<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Printing every 544-byte layer would bury the useful facts.
        f.debug_struct("PlayoutDelay")
            .field("capacity", &N)
            .field("len", &self.len)
            .field("oldest", &self.oldest())
            .field("newest", &self.newest())
            .finish_non_exhaustive()
    }
}

impl<const N: usize> PlayoutDelay<N> {
    /// An empty ring.
    pub const fn new() -> Self {
        const { assert!(N > 0, "a playout delay needs room for at least one sample") };
        Self { times: [0.0; N], layers: [Layer::absent(BlendMode::Override); N], start: 0, len: 0 }
    }

    /// Number of samples held.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// True before the first push.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The oldest stamp held.
    pub fn oldest(&self) -> Option<Seconds> {
        (self.len > 0).then(|| Seconds(self.times[self.start]))
    }

    /// The newest stamp held.
    pub fn newest(&self) -> Option<Seconds> {
        (self.len > 0).then(|| Seconds(self.times[self.slot(self.len - 1)]))
    }

    /// Forgets everything (a source went away, or the clock was reset).
    pub const fn clear(&mut self) {
        self.start = 0;
        self.len = 0;
    }

    /// Adds `layer`, stamped `perf_time`. When the ring is full the oldest sample is dropped.
    ///
    /// # Errors
    /// [`PlayoutError`] if the stamp is not finite or not after the newest one; the ring is unchanged.
    pub fn push(&mut self, perf_time: Seconds, layer: Layer) -> Result<(), PlayoutError> {
        let t = perf_time.0;
        if !t.is_finite() {
            return Err(PlayoutError::NotFinite(t));
        }
        if let Some(newest) = self.newest().filter(|newest| t <= newest.0) {
            return Err(PlayoutError::NotIncreasing { newest: newest.0, got: t });
        }
        if self.len == N {
            self.start = self.slot(1);
            self.len -= 1;
        }
        let i = self.slot(self.len);
        self.times[i] = t;
        self.layers[i] = layer;
        self.len += 1;
        Ok(())
    }

    /// The layer as it was at `now - delay`, or `None` if that is before the oldest sample held (or nothing is held).
    ///
    /// - Exactly at a stamp, that sample.
    /// - Between two stamps, the channels both samples drive are interpolated linearly, and so is the weight. The
    ///   mask and blend mode are the earlier sample's: a channel the later sample adds appears when its stamp is
    ///   reached, and one it drops holds its earlier value until then.
    /// - After the newest stamp, the newest sample is held (a source that stopped keeps its last word; the stage
    ///   decides when to withdraw it).
    ///
    /// A negative or NaN `delay` counts as zero, so the result is never from the future of `now`. With `delay` larger
    /// than the span the ring covers, the answer is `None` once the target has fallen off the old end.
    pub fn sample(&self, now: Seconds, delay: Seconds) -> Option<Layer> {
        let delay = if delay.0 > 0.0 { delay.0 } else { 0.0 };
        let at = now.0 - delay;
        // `!(at >= oldest)` also rejects NaN.
        if self.len == 0 || at.partial_cmp(&self.times[self.start]).is_none_or(Ordering::is_lt) {
            return None;
        }
        let after = self.count_at_or_before(at);
        let earlier = self.slot(after - 1);
        if after == self.len || at <= self.times[earlier] {
            // The count guarantees `time <= at`, so `at <= time` means a stamp hit exactly.
            return Some(self.layers[earlier]);
        }
        let later = self.slot(after);
        let (t0, t1) = (self.times[earlier], self.times[later]);
        // `t1 > t0` is a push invariant, so the fraction is in (0, 1).
        #[allow(clippy::cast_possible_truncation)] // a fraction in (0, 1) fits f32
        let alpha = ((at - t0) / (t1 - t0)) as f32;
        let (a, b) = (&self.layers[earlier], &self.layers[later]);
        let mut out = *a;
        // Whole-buffer lerp (vectorised), then put back the channels only the earlier sample drives: they hold.
        out.values.lerp_all(&b.values, alpha);
        if a.mask != b.mask {
            out.values.copy_masked(&a.values, a.mask.without(b.mask));
        }
        out.weight = a.weight + (b.weight - a.weight) * alpha;
        Some(out)
    }

    /// Ring slot of the `i`th oldest entry (`i <= N`).
    const fn slot(&self, i: usize) -> usize {
        let s = self.start + i;
        if s >= N { s - N } else { s }
    }

    /// How many entries have a stamp at or before `at`; at least 1 when `at` is not before the oldest.
    fn count_at_or_before(&self, at: f64) -> usize {
        let (mut lo, mut hi) = (0, self.len);
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            if self.times[self.slot(mid)] <= at {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }
}

/// Moves the applied delay to a new target at a constant rate, so the playout position (`now - delay`) never jumps
/// and never reverses.
///
/// The delay changes at [`MAX_SLEW`] (10%) of real time, so the playout runs 10% faster or slower while it ramps and a
/// change of `change` takes `change / 0.1` seconds (20 ms takes 0.2 s, 200 ms takes 2 s), with no minimum.
/// [`DelayRamp::with_slew`] sets a different limit; any limit below 1 keeps the playout moving forward, and a limit of
/// 0 never moves.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DelayRamp {
    applied: f64,
    target: f64,
    /// Seconds of delay change per second; fixed when the target is set.
    rate: f64,
    max_slew: f64,
}

impl DelayRamp {
    /// A ramp resting at `delay`, with the design's 10% limit.
    pub fn new(delay: Seconds) -> Self {
        Self::with_slew(delay, MAX_SLEW)
    }

    /// A ramp resting at `delay` whose playout may run at most `max_slew` faster or slower than real time
    /// (clamped to `0.0..=0.9`; a non-number counts as the default).
    pub fn with_slew(delay: Seconds, max_slew: f64) -> Self {
        let max_slew = if max_slew.is_nan() { MAX_SLEW } else { max_slew.clamp(0.0, 0.9) };
        let delay = clamp_delay(delay.0);
        Self { applied: delay, target: delay, rate: 0.0, max_slew }
    }

    /// The delay to apply now.
    pub const fn current(&self) -> Seconds {
        Seconds(self.applied)
    }

    /// The delay the ramp is heading for.
    pub const fn target(&self) -> Seconds {
        Seconds(self.target)
    }

    /// True while the applied delay is still moving.
    pub fn is_ramping(&self) -> bool {
        (self.target - self.applied).abs() > 0.0
    }

    /// Heads for `delay` (negative or NaN counts as zero) from wherever the ramp is now, fixing the rate again.
    pub fn set_target(&mut self, delay: Seconds) {
        self.target = clamp_delay(delay.0);
        self.rate = self.max_slew;
    }

    /// Moves by `dt` of real time (a negative or NaN `dt` moves nothing) and returns the delay to apply.
    pub fn advance(&mut self, dt: Seconds) -> Seconds {
        let remaining = self.target - self.applied;
        let step = if dt.0 > 0.0 { self.rate * dt.0 } else { 0.0 };
        self.applied = if step >= remaining.abs() { self.target } else { self.applied + step.copysign(remaining) };
        self.current()
    }
}

/// A delay is never negative, and a NaN counts as none.
fn clamp_delay(delay: f64) -> f64 {
    if delay > 0.0 { delay } else { 0.0 }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::similar_names, clippy::range_plus_one)] // small test counters and stamps
mod tests {
    use super::*;
    use crate::{Channel, ChannelMask};
    use omuzaki_core::rng::Rng;

    const JAW: Channel = Channel::JAW_OPEN;
    const BLINK: Channel = Channel::EYE_BLINK_LEFT;

    /// A layer driving `channels` with `value`.
    fn layer(channels: &[Channel], value: f32, weight: f32) -> Layer {
        let mut l = Layer::absent(BlendMode::Override);
        for &c in channels {
            l.values.set(c, value);
        }
        l.mask = channels.iter().copied().collect::<ChannelMask>();
        l.weight = weight;
        l
    }

    fn ring_of_ramp() -> PlayoutDelay<8> {
        // Stamps 0.0, 0.1, ... with the jaw at 10 x the stamp, so a sample at t reads back 10 t.
        let mut ring = PlayoutDelay::<8>::new();
        for i in 0..5 {
            let t = f64::from(i) * 0.1;
            assert_eq!(ring.push(Seconds(t), layer(&[JAW], i as f32, 1.0)), Ok(()));
        }
        ring
    }

    #[test]
    fn empty_and_before_the_first_stamp_give_none() {
        let ring = PlayoutDelay::<4>::new();
        assert!(ring.sample(Seconds(1.0), Seconds(0.0)).is_none());
        let mut ring = PlayoutDelay::<4>::new();
        assert_eq!(ring.push(Seconds(1.0), layer(&[JAW], 1.0, 1.0)), Ok(()));
        assert!(ring.sample(Seconds(1.2), Seconds(0.3)).is_none());
        assert!(ring.sample(Seconds(1.0), Seconds(0.0)).is_some());
    }

    #[test]
    fn sampling_at_a_stamp_is_exact() {
        let ring = ring_of_ramp();
        for i in 0..5 {
            let t = f64::from(i) * 0.1;
            let got = ring.sample(Seconds(t + 0.25), Seconds(0.25)).map(|l| l.values.get(JAW));
            assert_eq!(got, Some(i as f32), "stamp {i}");
        }
    }

    #[test]
    fn between_stamps_values_and_weight_interpolate() {
        let mut ring = PlayoutDelay::<4>::new();
        assert_eq!(ring.push(Seconds(1.0), layer(&[JAW], 0.0, 0.0)), Ok(()));
        assert_eq!(ring.push(Seconds(2.0), layer(&[JAW], 1.0, 1.0)), Ok(()));
        let Some(l) = ring.sample(Seconds(1.25), Seconds(0.0)) else { panic!("inside the ring") };
        assert!((l.values.get(JAW) - 0.25).abs() < 1e-6);
        assert!((l.weight - 0.25).abs() < 1e-6);
    }

    #[test]
    fn the_mask_is_the_earlier_samples_and_one_sided_channels_hold() {
        let mut ring = PlayoutDelay::<4>::new();
        assert_eq!(ring.push(Seconds(0.0), layer(&[JAW], 0.2, 1.0)), Ok(()));
        let mut next = layer(&[BLINK], 0.9, 1.0);
        next.values.set(JAW, 0.8); // outside the mask: must not leak in
        assert_eq!(ring.push(Seconds(1.0), next), Ok(()));
        let Some(l) = ring.sample(Seconds(0.5), Seconds(0.0)) else { panic!("inside the ring") };
        assert_eq!(l.mask, layer(&[JAW], 0.0, 1.0).mask);
        assert!((l.values.get(JAW) - 0.2).abs() < 1e-6, "held, not blended toward a channel the later sample dropped");
    }

    #[test]
    fn after_the_newest_stamp_the_newest_is_held() {
        let ring = ring_of_ramp();
        let got = ring.sample(Seconds(9.0), Seconds(0.0)).map(|l| l.values.get(JAW));
        assert_eq!(got, Some(4.0));
    }

    #[test]
    fn a_full_ring_drops_the_oldest() {
        let mut ring = PlayoutDelay::<3>::new();
        for i in 0..5 {
            assert_eq!(ring.push(Seconds(f64::from(i)), layer(&[JAW], i as f32, 1.0)), Ok(()));
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.oldest(), Some(Seconds(2.0)));
        assert_eq!(ring.newest(), Some(Seconds(4.0)));
        assert!(ring.sample(Seconds(1.5), Seconds(0.0)).is_none(), "dropped samples are gone");
        let got = ring.sample(Seconds(2.5), Seconds(0.0)).map(|l| l.values.get(JAW));
        assert_eq!(got, Some(2.5));
    }

    #[test]
    fn pushes_must_strictly_increase_and_be_finite() {
        let mut ring = PlayoutDelay::<4>::new();
        let l = layer(&[JAW], 1.0, 1.0);
        assert_eq!(ring.push(Seconds(1.0), l), Ok(()));
        assert_eq!(ring.push(Seconds(1.0), l), Err(PlayoutError::NotIncreasing { newest: 1.0, got: 1.0 }));
        assert_eq!(ring.push(Seconds(0.5), l), Err(PlayoutError::NotIncreasing { newest: 1.0, got: 0.5 }));
        assert!(matches!(ring.push(Seconds(f64::NAN), l), Err(PlayoutError::NotFinite(_))));
        assert!(matches!(ring.push(Seconds(f64::INFINITY), l), Err(PlayoutError::NotFinite(_))));
        assert_eq!(ring.len(), 1, "refused pushes change nothing");
    }

    #[test]
    fn bad_delays_and_times_never_reach_into_the_future() {
        let ring = ring_of_ramp();
        let at_now = ring.sample(Seconds(0.25), Seconds(0.0)).map(|l| l.values.get(JAW));
        assert_eq!(ring.sample(Seconds(0.25), Seconds(-5.0)).map(|l| l.values.get(JAW)), at_now);
        assert_eq!(ring.sample(Seconds(0.25), Seconds(f64::NAN)).map(|l| l.values.get(JAW)), at_now);
        assert!(ring.sample(Seconds(f64::NAN), Seconds(0.0)).is_none());
    }

    #[test]
    fn clear_empties_the_ring() {
        let mut ring = ring_of_ramp();
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.push(Seconds(0.0), layer(&[JAW], 1.0, 1.0)), Ok(()));
    }

    /// Property (seeded): whatever the stamps, wrap-around and delays, a sample is never from after `now - delay`.
    /// The jaw value carries the sample's own stamp, so the result says when it is from.
    #[test]
    fn sample_never_returns_a_time_in_the_future() {
        let mut rng = Rng::new(0x5EED_0983);
        for case in 0..200 {
            let mut ring = PlayoutDelay::<16>::new();
            let mut t = f64::from(rng.range_f32(0.0, 5.0));
            for _ in 0..rng.next_u64() % 60 + 1 {
                t += f64::from(rng.range_f32(0.001, 0.05));
                let stamp = layer(&[JAW], t as f32, 1.0);
                assert_eq!(ring.push(Seconds(t), stamp), Ok(()));
            }
            for _ in 0..50 {
                let now = t + f64::from(rng.range_f32(-1.0, 1.0));
                let delay = f64::from(rng.range_f32(-0.1, 0.8));
                let limit = now - delay.max(0.0);
                if let Some(l) = ring.sample(Seconds(now), Seconds(delay)) {
                    // Held values (past the newest stamp) read the newest stamp, which is itself <= limit then.
                    let from = f64::from(l.values.get(JAW));
                    assert!(from <= limit + 1e-4, "case {case}: sampled {from} for a target of {limit}");
                    assert!(from >= f64::from(ring.oldest().map_or(0.0, |s| s.0 as f32)) - 1e-4);
                } else {
                    let oldest = ring.oldest().map_or(f64::MAX, |s| s.0);
                    assert!(limit < oldest, "case {case}: None for a target inside the ring");
                }
            }
        }
    }

    #[test]
    fn a_20_ms_ramp_takes_0_2_s_and_is_monotonic() {
        let mut ramp = DelayRamp::new(Seconds(0.0));
        ramp.set_target(Seconds(0.02));
        let (dt, mut last, mut elapsed) = (1.0 / 120.0, 0.0, 0.0);
        while ramp.is_ramping() {
            let d = ramp.advance(Seconds(dt)).0;
            assert!(d >= last, "never steps back");
            assert!(d - last <= 0.1 * dt + 1e-12, "playout speed within 10%");
            (last, elapsed) = (d, elapsed + dt);
        }
        assert!((elapsed - 0.2).abs() <= 1.5 * dt, "took {elapsed} s");
        assert_eq!(ramp.current(), Seconds(0.02), "lands exactly");
    }

    #[test]
    fn a_ramp_from_0_to_200_ms_is_limited_to_ten_percent_unless_the_slew_is_raised() {
        let take = |mut ramp: DelayRamp| {
            ramp.set_target(Seconds(0.2));
            let (dt, mut last, mut elapsed) = (1.0 / 120.0, 0.0, 0.0);
            while ramp.is_ramping() {
                let d = ramp.advance(Seconds(dt)).0;
                assert!(d >= last && d - last < dt, "monotonic and the playout never reverses");
                (last, elapsed) = (d, elapsed + dt);
            }
            elapsed
        };
        assert!((take(DelayRamp::new(Seconds(0.0))) - 2.0).abs() < 0.02, "10% slew: 200 ms takes 2 s");
        assert!((take(DelayRamp::with_slew(Seconds(0.0), 0.4)) - 0.5).abs() < 0.02, "40% slew: 0.5 s");
    }

    #[test]
    fn a_ramp_down_and_a_retarget_mid_ramp_stay_continuous() {
        let mut ramp = DelayRamp::new(Seconds(0.1));
        ramp.set_target(Seconds(0.0));
        let a = ramp.advance(Seconds(0.1)).0;
        assert!(a < 0.1 && a > 0.0);
        ramp.set_target(Seconds(0.3));
        let b = ramp.advance(Seconds(0.0)).0;
        assert_eq!(a, b, "retargeting does not move the applied delay");
        assert!(ramp.advance(Seconds(0.1)).0 > b);
        assert_eq!(ramp.advance(Seconds(f64::NAN)), ramp.current());
        ramp.set_target(Seconds(-1.0));
        assert_eq!(ramp.target(), Seconds(0.0));
    }
}
