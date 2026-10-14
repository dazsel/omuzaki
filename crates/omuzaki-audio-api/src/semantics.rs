//! The exact arithmetic both engines must reproduce (design doc section 3.6; the mock and the engine are compared to
//! 1e-6, so none of it is left to interpretation).
//!
//! The rules, and the pure code that implements the ones that have a formula, live here so the mock
//! (`omuzaki-audio-mock`) and the engine (`omuzaki-audio`) call the same function instead of re-deriving it.
//!
//! # Gain and the fader ramp
//!
//! - A gain in dB becomes a linear factor with [`db_to_gain`]: `10^(db / 20)`, and exactly `0.0` at minus infinity.
//! - A fader never jumps. [`FaderRamp`] moves its factor **linearly over [`FADER_RAMP_SAMPLES`] = 240 samples** (5 ms at
//!   48 kHz) from the value it has *now* to the new target. A new target **restarts** the ramp from the current value,
//!   so a target changed mid-ramp does not wait for the old one to finish. Sample `k` (1 to 240) after the change is
//!   `start + (target - start) * (k / 240)`, with `k / 240` evaluated in `f32`; sample 240 and every later one is
//!   exactly `target`.
//! - **Mute is a ramp, never a hard cut.** A bus has **two** ramps, `gain` (target `db_to_gain(gain_db)`) and `mute`
//!   (target 0 when muted, 1 when not), and a send has one (target `db_to_gain(send.gain_db)`). Mute is its own ramp, not
//!   the gain ramp aimed at 0, so unmuting returns to the current gain. The engine calls [`FaderRamp::set_target`] on
//!   them; the ramps are applied per sample, so the result does not depend on how the stream is cut into blocks.
//! - Per sample on a bus, with `pre` the sum of the sources run through the chain:
//!   - `post = pre * gain[n] * mute[n] * duck[n]`;
//!   - a **pre-fader** send carries `pre * mute[n] * send[n]`: a mute cuts it, the gain does not;
//!   - a **post-fader** send carries `post * send[n]`;
//!   - outputs and post-fader taps read `post`.
//! - A bus gain of minus infinity cuts what leaves *after* the fader (the post-fader sends and the outputs).
//!
//! # Sidechain
//!
//! A sidechain reads the referenced bus's `post` from the **previous block**; a stereo reference is averaged,
//! `(L + R) / 2`. This is the one signal whose result depends on the block plan: its delay is the length of the
//! previous block (an echo canceller needs its reference before the echo, which a fixed delay on a fast device could
//! miss). Sidechained graphs are therefore compared under the same `BlockPlan`, and the block-plan invariance check
//! leaves sidechained buses out.
//!
//! # Mono to stereo
//!
//! A mono bus that sends to a stereo bus is **centred with equal power**: both sides get the sample times
//! [`MONO_TO_STEREO_GAIN`] (`1/sqrt(2)`, which is -3.01 dB, the "-3 dB per side" of the design). A stereo bus sending
//! to a mono bus sums left and right and scales by [`MONO_TO_STEREO_GAIN`] as well, so a centred signal keeps its level.
//!
//! # Ducking
//!
//! A [`DuckRule`](crate::graph::DuckRule) lowers its target bus while its trigger is active, exactly as follows
//! (design doc section 3.6). All state is `f32`; constants that need `exp` are computed once in `f64` and narrowed.
//!
//! 1. **Trigger level** (bus triggers), per sample on the trigger bus's `post`: [`LevelFollower`], with
//!    `x = max |post_c[n]|` over channels and `e = x if x > e, else e * R`, `R = exp(-1 / (0.050 * 48000))` (instant
//!    attack, 50 ms release), starting at 0.
//! 2. **Trigger signal**: for a bus, `t[n] = e[n] > db_to_gain(threshold_db)`; for a gate, the gate's state at sample
//!    `n`. Each trigger keeps its last [`TRIGGER_DELAY`] samples.
//! 3. **A rule reads `a = t[n - 1024]`**; before sample 1024 of the stream, `a` is false. (This replaces "read the
//!    previous block": the delay is a fixed number of samples, so the result does not depend on the block plan.)
//! 4. **Hold**, per sample and in this order, with `H = round(hold_ms * 48)`:
//!    `if a { hold = H }`, `ducked = a || hold > 0`, `if !a && hold > 0 { hold -= 1 }`. The duck is held for exactly
//!    `H` samples after the trigger ends.
//! 5. **Gain in dB, moved in a whole number of steps**: `g` starts at 0 dB with target `-depth_db` when `ducked`, else 0.
//!    When the target changes the ramp starts from the current `g`; the step count is the integer
//!    `n = max(1, round(attack_ms * 48))` for a fall and `max(1, round(release_ms * 48))` for a rise; step `k` puts
//!    `g` at `start + (target - start) * (k / n)` in `f32` (that is, each step moves it by `(target - start) / n`, without
//!    accumulating rounding), and step `n` snaps `g` to the target. A full fall therefore takes exactly
//!    `n` samples (`attack_ms` to within half a sample) and lands exactly on its target. That is [`DuckEnvelope`].
//! 6. **The bus's duck factor** is `duck[n] = db_to_gain(sum of g_r[n])` over the rules that target it, in the order
//!    `GraphSpec::ducks` lists them ([`duck_factor`]). With every `g_r` at 0 it is exactly 1.0.
//! 7. **Validation**: see [`GraphSpec::validate_structure`](crate::graph::GraphSpec::validate_structure).
//! 8. **Rule state carries across graph swaps**, by `(trigger, target)`. A rule that a swap removes keeps running with
//!    `a` forced false and no hold ([`DuckEnvelope::retire`]) until its `g` is back at 0 ([`DuckEnvelope::is_idle`]);
//!    a rule whose parameters change continues from its current `g` ([`DuckEnvelope::set_params`]). Nothing jumps.

use crate::node::SAMPLE_RATE;

/// Length of every fader, mute and send ramp, in samples: 5 ms at 48 kHz.
pub const FADER_RAMP_SAMPLES: u32 = 240;

/// The factor a mono signal gets on each side when it is centred in a stereo bus: `1/sqrt(2)` (equal power, -3.01 dB).
pub const MONO_TO_STEREO_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;

const _: () = assert!(FADER_RAMP_SAMPLES * 1000 == 5 * SAMPLE_RATE, "the fader ramp is 5 ms");

/// The linear factor of `db` decibels: `10^(db / 20)`, and exactly `0.0` for minus infinity (and for NaN, which is
/// never a valid gain, so it is treated as silence rather than propagated).
///
/// Both engines call this one function, so they agree to the bit.
#[must_use]
pub fn db_to_gain(db: f32) -> f32 {
    if db.is_nan() || db == f32::NEG_INFINITY {
        return 0.0;
    }
    // f64 keeps the power accurate for large negative dB; the narrowing at the end is the only rounding.
    #[allow(clippy::cast_possible_truncation)]
    {
        10f64.powf(f64::from(db) / 20.0) as f32
    }
}

/// Whether a gain in dB is minus infinity, which cuts an edge in the graph.
#[must_use]
pub fn is_silent_gain(db: f32) -> bool {
    db == f32::NEG_INFINITY
}

/// A linear gain that approaches its target over [`FADER_RAMP_SAMPLES`] samples (see the module docs for the exact
/// formula). `Copy`, no allocation: it lives in the audio thread.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FaderRamp {
    /// The value when the current ramp began.
    start: f32,
    /// The value the ramp ends at.
    target: f32,
    /// Samples elapsed in the current ramp, `0..=FADER_RAMP_SAMPLES`.
    step: u32,
}

impl FaderRamp {
    /// A ramp at rest at `gain` (a linear factor).
    #[must_use]
    pub const fn new(gain: f32) -> Self {
        Self { start: gain, target: gain, step: FADER_RAMP_SAMPLES }
    }

    /// The factor reached so far: the value after the samples consumed up to now.
    #[must_use]
    pub fn current(&self) -> f32 {
        self.value_at(self.step)
    }

    /// The value the ramp is heading to.
    #[must_use]
    pub const fn target(&self) -> f32 {
        self.target
    }

    /// Whether the ramp has arrived.
    #[must_use]
    pub const fn is_settled(&self) -> bool {
        self.step >= FADER_RAMP_SAMPLES
    }

    /// Aims at `target`, restarting the ramp from the current value. A target equal to the current one does nothing,
    /// so re-sending an unchanged parameter never restarts (and never lengthens) a ramp.
    pub fn set_target(&mut self, target: f32) {
        if target.to_bits() == self.target.to_bits() {
            return;
        }
        self.start = self.current();
        self.target = target;
        self.step = 0;
    }

    /// The factor for the next sample, and advances by one.
    pub fn next_gain(&mut self) -> f32 {
        self.step = (self.step + 1).min(FADER_RAMP_SAMPLES);
        self.value_at(self.step)
    }

    /// Multiplies `samples` by the ramp, one factor per sample.
    pub fn apply(&mut self, samples: &mut [f32]) {
        for s in samples {
            *s *= self.next_gain();
        }
    }

    fn value_at(&self, step: u32) -> f32 {
        if step >= FADER_RAMP_SAMPLES {
            return self.target;
        }
        // Both operands are small integers, exactly representable in f32.
        #[allow(clippy::cast_precision_loss)]
        let fraction = step as f32 / FADER_RAMP_SAMPLES as f32;
        self.start + (self.target - self.start) * fraction
    }
}

impl Default for FaderRamp {
    /// Unity gain, at rest.
    fn default() -> Self {
        Self::new(1.0)
    }
}

/// How many samples a duck rule reads its trigger late: the rule at sample `n` sees the trigger at `n - 1024`.
pub const TRIGGER_DELAY: usize = 1024;

/// The trigger level of a bus: instant attack, 50 ms release (step 1 of the ducking rules).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LevelFollower {
    level: f32,
    release: f32,
}

impl LevelFollower {
    /// A follower at level 0.
    #[must_use]
    pub fn new() -> Self {
        // Computed once in f64 and narrowed: the only rounding of the constant.
        #[allow(clippy::cast_possible_truncation)]
        let release = (-1.0 / (0.050 * f64::from(SAMPLE_RATE))).exp() as f32;
        Self { level: 0.0, release }
    }

    /// Takes the largest absolute channel value of one sample and returns the level `e[n]`.
    pub fn step(&mut self, max_abs: f32) -> f32 {
        self.level = if max_abs > self.level { max_abs } else { self.level * self.release };
        self.level
    }

    /// The current level.
    #[must_use]
    pub const fn level(&self) -> f32 {
        self.level
    }
}

impl Default for LevelFollower {
    fn default() -> Self {
        Self::new()
    }
}

/// One duck rule's state: hold counter and gain in dB (steps 4 and 5 of the ducking rules).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DuckEnvelope {
    depth_db: f32,
    /// Steps of a fall and of a rise: `max(1, round(ms * 48))`.
    attack_steps: u32,
    release_steps: u32,
    hold_samples: u32,
    hold: u32,
    /// The gain in dB, 0 (not ducked) down to `-depth_db`.
    gain_db: f32,
    /// The current ramp: where it started, where it heads, its length in steps, and how many have been taken.
    start_db: f32,
    target_db: f32,
    ramp_steps: u32,
    ramp_done: u32,
}

impl DuckEnvelope {
    /// An envelope at rest (0 dB, no hold) for these rule numbers.
    #[must_use]
    pub fn new(depth_db: f32, attack_ms: f32, hold_ms: f32, release_ms: f32) -> Self {
        let mut envelope = Self {
            depth_db,
            attack_steps: 1,
            release_steps: 1,
            hold_samples: 0,
            hold: 0,
            gain_db: 0.0,
            start_db: 0.0,
            target_db: 0.0,
            ramp_steps: 1,
            ramp_done: 1,
        };
        envelope.set_params(depth_db, attack_ms, hold_ms, release_ms);
        envelope
    }

    /// New rule numbers; the envelope continues from its current gain and hold, so nothing jumps.
    pub fn set_params(&mut self, depth_db: f32, attack_ms: f32, hold_ms: f32, release_ms: f32) {
        let samples_per_ms = f32::from(u16::try_from(SAMPLE_RATE / 1000).unwrap_or(48));
        self.depth_db = depth_db;
        // Non-negative and bounded by validation (at most 5,000 ms = 240,000 samples), so the narrowing is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            self.attack_steps = ((attack_ms * samples_per_ms).round() as u32).max(1);
            self.release_steps = ((release_ms * samples_per_ms).round() as u32).max(1);
            self.hold_samples = (hold_ms * samples_per_ms).round() as u32;
        }
    }

    /// One sample: `active` is the rule's `a = t[n - 1024]`. Returns the gain in dB, 0 or below.
    pub fn step(&mut self, active: bool) -> f32 {
        if active {
            self.hold = self.hold_samples;
        }
        let ducked = active || self.hold > 0;
        if !active && self.hold > 0 {
            self.hold -= 1;
        }
        let target = if ducked { -self.depth_db } else { 0.0 };
        if target.to_bits() != self.target_db.to_bits() {
            self.retarget(target);
        }
        if self.ramp_done < self.ramp_steps {
            self.ramp_done += 1;
            self.gain_db = if self.ramp_done == self.ramp_steps {
                self.target_db
            } else {
                // Step k moves g to start + (target - start) * (k / n): the k-th multiple of the step, without the
                // rounding that adding the step k times would accumulate.
                #[allow(clippy::cast_precision_loss)] // at most 240,000, exact in f32
                let fraction = self.ramp_done as f32 / self.ramp_steps as f32;
                self.start_db + (self.target_db - self.start_db) * fraction
            };
        }
        self.gain_db
    }

    /// Starts a ramp from the current gain to `target`: `n` steps, the last one snapping onto the target.
    fn retarget(&mut self, target: f32) {
        self.target_db = target;
        self.start_db = self.gain_db;
        self.ramp_done = 0;
        self.ramp_steps = if target < self.gain_db { self.attack_steps } else { self.release_steps };
        if target.to_bits() == self.gain_db.to_bits() {
            self.ramp_done = self.ramp_steps;
        }
    }

    /// The gain in dB after the last step.
    #[must_use]
    pub const fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// Cancels the hold: a rule that a graph swap removed is stepped with `active = false` from now on, and this makes it
    /// release at once.
    pub fn retire(&mut self) {
        self.hold = 0;
    }

    /// Whether the gain is back at exactly 0 dB with no hold left, so a retired rule can be dropped.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.gain_db == 0.0 && self.hold == 0 && self.ramp_done >= self.ramp_steps
    }
}

/// A bus's duck factor from the dB gains of the rules that target it, in `GraphSpec::ducks` order: `db_to_gain(sum)`.
/// Exactly 1.0 when every gain is 0.
#[must_use]
pub fn duck_factor(gains_db: impl IntoIterator<Item = f32>) -> f32 {
    db_to_gain(gains_db.into_iter().fold(0.0, |sum, g| sum + g))
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // exact landings; small positive test values
mod tests {
    use super::*;

    #[test]
    fn db_conversion_is_exact_at_the_landmarks() {
        assert_eq!(db_to_gain(0.0), 1.0);
        assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0);
        assert_eq!(db_to_gain(f32::NAN), 0.0);
        assert!((db_to_gain(-6.0) - 0.501_187_2).abs() < 1e-7);
        assert!((db_to_gain(12.0) - 3.981_071_7).abs() < 1e-6);
        assert!((MONO_TO_STEREO_GAIN - db_to_gain(-3.010_3)).abs() < 1e-5);
        assert!(is_silent_gain(f32::NEG_INFINITY) && !is_silent_gain(-200.0));
    }

    #[test]
    fn a_ramp_takes_exactly_240_samples_and_lands_on_the_target() {
        let mut r = FaderRamp::new(0.0);
        r.set_target(1.0);
        let v: Vec<f32> = (0..300).map(|_| r.next_gain()).collect();
        assert!((v[0] - 1.0 / 240.0).abs() < 1e-9);
        assert!((v[119] - 0.5).abs() < 1e-6);
        assert_eq!(v[239], 1.0);
        assert!(v[240..].iter().all(|&x| x == 1.0));
        assert!(v[..239].iter().all(|&x| x < 1.0));
        assert!(r.is_settled());
    }

    #[test]
    fn a_new_target_restarts_from_the_current_value() {
        let mut r = FaderRamp::new(1.0);
        r.set_target(0.0);
        for _ in 0..120 {
            r.next_gain();
        }
        let mid = r.current();
        assert!((mid - 0.5).abs() < 1e-6);
        r.set_target(1.0);
        assert_eq!(r.current(), mid);
        let first = r.next_gain();
        assert!((first - (mid + (1.0 - mid) / 240.0)).abs() < 1e-7);
        for _ in 0..239 {
            r.next_gain();
        }
        assert_eq!(r.current(), 1.0);
    }

    #[test]
    fn repeating_the_same_target_does_not_restart_the_ramp() {
        let mut r = FaderRamp::new(0.0);
        r.set_target(1.0);
        for _ in 0..100 {
            r.next_gain();
        }
        let before = r;
        r.set_target(1.0);
        assert_eq!(r, before);
    }

    #[test]
    fn mute_is_a_ramp_to_zero_and_not_a_cut() {
        let mut r = FaderRamp::new(db_to_gain(-6.0));
        r.set_target(0.0);
        let mut block = [1.0f32; 480];
        r.apply(&mut block);
        assert!(block[0] > 0.49, "the first sample is still nearly at full gain");
        assert_eq!(block[239], 0.0);
        assert!(block[240..].iter().all(|&x| x == 0.0));
        assert!(block.windows(2).all(|w| w[1] <= w[0]));
    }

    #[test]
    fn the_result_does_not_depend_on_the_block_split() {
        let input: Vec<f32> = (0..1000u16).map(|i| (f32::from(i) * 0.37).sin()).collect();
        let mut whole = input.clone();
        let mut a = FaderRamp::new(0.25);
        a.set_target(0.9);
        a.apply(&mut whole);
        for split in [1usize, 7, 64, 240, 333] {
            let mut parts = input.clone();
            let mut b = FaderRamp::new(0.25);
            b.set_target(0.9);
            for chunk in parts.chunks_mut(split) {
                b.apply(chunk);
            }
            assert_eq!(parts, whole, "split {split}");
        }
    }

    fn run_envelope(mut env: DuckEnvelope, pattern: impl Iterator<Item = bool>) -> Vec<f32> {
        pattern.map(|a| env.step(a)).collect()
    }

    #[test]
    fn the_level_follower_attacks_instantly_and_releases_over_50_ms() {
        let mut f = LevelFollower::new();
        assert_eq!(f.step(0.8), 0.8);
        assert_eq!(f.step(0.9), 0.9);
        let after_50ms: f32 = (0..2400).map(|_| f.step(0.0)).last().unwrap();
        assert!((after_50ms / 0.9 - (-1.0f32).exp()).abs() < 1e-3, "one time constant: {after_50ms}");
    }

    #[test]
    fn a_duck_holds_for_exactly_the_hold_length_after_the_trigger_ends() {
        // depth 6 dB, instant-ish attack and release (1 ms), hold 10 ms = 480 samples.
        let env = DuckEnvelope::new(6.0, 1.0, 10.0, 1.0);
        let pattern = std::iter::repeat_n(true, 100).chain(std::iter::repeat_n(false, 2000));
        let g = run_envelope(env, pattern);
        // The trigger ends after sample 99; the hold keeps `ducked` for 480 more samples, so the rise starts at 100 + 480.
        assert_eq!(g[100 + 479], -6.0, "still held on the last held sample");
        assert!(g[100 + 480] > -6.0, "released on the first sample after the hold");
        assert!(g[100 + 480 + 100] > g[100 + 480], "and keeps rising");
    }

    #[test]
    fn a_full_fall_takes_exactly_the_attack_steps_and_lands_exactly_on_the_depth() {
        for (depth, attack_ms) in [(12.0f32, 10.0f32), (9.0, 20.0), (60.0, 1000.0), (6.0, 1.0), (30.0, 0.01)] {
            let steps = ((attack_ms * 48.0).round() as usize).max(1);
            let mut env = DuckEnvelope::new(depth, attack_ms, 0.0, 100.0);
            let g: Vec<f32> = (0..steps + 5).map(|_| env.step(true)).collect();
            assert_eq!(g[steps - 1], -depth, "depth {depth} attack {attack_ms}: lands on step {steps}");
            assert!(steps == 1 || g[steps - 2] > -depth, "depth {depth} attack {attack_ms}: not before step {steps}");
            assert!(g[steps - 1..].iter().all(|&x| x == -depth));
            assert!(g.windows(2).all(|w| w[1] <= w[0]), "monotone fall");
        }
    }

    #[test]
    fn a_full_rise_takes_exactly_the_release_steps_and_lands_exactly_on_zero() {
        for (depth, release_ms) in [(12.0f32, 300.0f32), (9.0, 500.0), (60.0, 5000.0), (6.0, 1.0)] {
            let steps = ((release_ms * 48.0).round() as usize).max(1);
            let mut env = DuckEnvelope::new(depth, 1.0, 0.0, release_ms);
            (0..100_000).for_each(|_| {
                env.step(true);
            });
            assert_eq!(env.gain_db(), -depth);
            let g: Vec<f32> = (0..steps + 5).map(|_| env.step(false)).collect();
            assert_eq!(g[steps - 1], 0.0, "depth {depth} release {release_ms}: lands on step {steps}");
            assert!(steps == 1 || g[steps - 2] < 0.0, "depth {depth} release {release_ms}: not before step {steps}");
            assert!(env.is_idle());
        }
    }

    #[test]
    fn a_change_of_target_mid_ramp_restarts_from_the_current_gain_with_the_new_step_count() {
        let mut env = DuckEnvelope::new(12.0, 10.0, 0.0, 20.0); // 480 steps down, 960 up
        (0..240).for_each(|_| {
            env.step(true);
        });
        let mid = env.gain_db();
        assert!((mid - -6.0).abs() < 1e-4);
        let g: Vec<f32> = (0..965).map(|_| env.step(false)).collect();
        assert_eq!(g[959], 0.0, "960 steps up, whatever the distance");
        assert!(g[958] < 0.0);
        assert!((g[0] - (mid + 6.0 / 960.0)).abs() < 1e-5);
    }

    #[test]
    fn the_envelope_does_not_depend_on_how_samples_are_grouped() {
        let pattern: Vec<bool> = (0..5000).map(|i| (i / 700) % 2 == 0).collect();
        let whole = run_envelope(DuckEnvelope::new(12.0, 10.0, 50.0, 300.0), pattern.iter().copied());
        let mut env = DuckEnvelope::new(12.0, 10.0, 50.0, 300.0);
        let mut chunked = Vec::new();
        for chunk in pattern.chunks(333) {
            chunked.extend(chunk.iter().map(|&a| env.step(a)));
        }
        assert_eq!(whole, chunked);
    }

    #[test]
    fn changing_the_numbers_continues_from_the_current_gain() {
        let mut env = DuckEnvelope::new(12.0, 10.0, 0.0, 300.0);
        (0..240).for_each(|_| {
            env.step(true);
        });
        let before = env.gain_db();
        env.set_params(6.0, 10.0, 0.0, 300.0);
        assert_eq!(env.gain_db(), before, "no jump");
        env.retire();
        assert!(!env.is_idle());
    }

    #[test]
    fn duck_factors_multiply_by_adding_decibels() {
        assert_eq!(duck_factor([0.0, 0.0]), 1.0);
        assert_eq!(duck_factor(std::iter::empty()), 1.0);
        assert!((duck_factor([-6.0, -6.0]) - db_to_gain(-12.0)).abs() < 1e-7);
    }

    #[test]
    fn the_trigger_delay_is_1024_samples() {
        assert_eq!(TRIGGER_DELAY, 1024);
    }
}
