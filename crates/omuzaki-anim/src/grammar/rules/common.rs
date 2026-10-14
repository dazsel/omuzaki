//! What the rules that fade a look in and out have in common: thresholds with a release level below them, a hold, eased
//! ramps, and the two readings of the incoming face they all start from (how much it smiles, how far it is raised).

use omuzaki_anim_api::Channel;

use crate::grammar::{RuleCtx, RuleParams};

/// A threshold with its release level below it. A look starts when its value is strictly above [`on`](Band::on) and
/// is kept while it stays strictly above [`off`](Band::off), so a value hovering near one level cannot flicker it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Band {
    pub on: f32,
    pub off: f32,
}

impl Band {
    /// The band named `on_name` / `off_name` in the character's numbers, each `0..=1`.
    ///
    /// A character that sets only the start level keeps the default *gap* below it for the release level (a start of
    /// 0.3 with a default 0.6 / 0.45 band releases at 0.15), so moving one level never collapses the dead zone. Whatever
    /// the character writes, the release level is held at or below the start level, because a band the wrong way round
    /// has no dead zone either.
    pub fn read(params: RuleParams<'_>, on_name: &str, off_name: &str, default: Self) -> Self {
        let on = params.number(on_name, default.on, 0.0, 1.0);
        let gap = default.on - default.off;
        let off = params.number(off_name, (on - gap).max(0.0), 0.0, 1.0).min(on);
        Self { on, off }
    }

    /// Whether a look that is not showing starts at `value`.
    pub fn starts(self, value: f32) -> bool {
        value > self.on
    }

    /// Whether a look that is showing is kept at `value`.
    pub fn keeps(self, value: f32) -> bool {
        value > self.off
    }
}

/// The longest attack hold a character may ask for, seconds: past this a look would feel late, not steady.
pub(super) const MAX_ATTACK: f32 = 0.5;

/// How a look comes and goes, in seconds.
#[derive(Copy, Clone, Debug)]
pub(super) struct Timing {
    /// The look starts only once its start condition has held, unbroken, for longer than this, so a spike of a frame
    /// or two (tracker noise) shows nothing. `0` starts it on the first frame.
    pub attack: f32,
    /// The look ends only once its keep condition has failed for longer than this, so a dip of a few frames is ridden
    /// out.
    pub hold: f32,
    /// Time to fade fully in.
    pub rise: f32,
    /// Time to fade fully out.
    pub fall: f32,
}

/// Whether a look is showing and how far, `0..=1`. It starts on one condition (after an attack hold), is kept on a
/// weaker one (the [`Band`] between them is the hysteresis), ends after a hold, and eases in and out.
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct Presence {
    on: bool,
    /// How long the start condition has held without a break while the look is off.
    armed_for: f32,
    below_for: f32,
    ramp: f32,
}

impl Presence {
    /// Advances by `dt` seconds. `start` is the start condition (read while the look is off), `keep` the keep
    /// condition (read while it is on). Returns the eased weight, `0..=1`.
    pub fn step(&mut self, dt: f32, start: bool, keep: bool, timing: Timing) -> f32 {
        if self.on {
            self.below_for = if keep { 0.0 } else { self.below_for + dt };
            if self.below_for > timing.hold {
                self.on = false;
            }
        } else if start {
            // A paused frame (`dt == 0`) adds nothing, so only an attack of zero starts on it.
            self.armed_for += dt;
            if timing.attack <= 0.0 || self.armed_for > timing.attack {
                self.on = true;
                self.below_for = 0.0;
                self.armed_for = 0.0;
            }
        } else {
            self.armed_for = 0.0;
        }
        let (direction, seconds) = if self.on { (dt, timing.rise) } else { (-dt, timing.fall) };
        self.ramp = (self.ramp + direction / seconds).clamp(0.0, 1.0);
        smoothstep(self.ramp)
    }

    /// Whether the look is on (it may still be fading out).
    pub fn is_on(&self) -> bool {
        self.on
    }
}

/// Hermite smoothstep of `u` clamped to `0..=1`: eases in and out with zero slope at both ends.
fn smoothstep(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

/// How much the face smiles: the mean of the two mouth corners' smile minus frown. This is the life layer's own
/// reading, so the grammar and the life layer agree on what a big smile is.
pub(super) fn net_smile(ctx: &RuleCtx<'_>) -> f32 {
    f32::midpoint(
        ctx.get(Channel::MOUTH_SMILE_LEFT) - ctx.get(Channel::MOUTH_FROWN_LEFT),
        ctx.get(Channel::MOUTH_SMILE_RIGHT) - ctx.get(Channel::MOUTH_FROWN_RIGHT),
    )
}

/// Raises `channel` to at least `level`; what is already there, from the source or an earlier rule, is never lowered.
/// A grammar look decorates and so only adds, which is also why it never doubles up with the life layer's own version
/// of the same look: the result is the larger of the two, not the sum.
pub(super) fn raise(ctx: &mut RuleCtx<'_>, channel: Channel, level: f32) {
    ctx.put(channel, ctx.get(channel).max(level));
}
