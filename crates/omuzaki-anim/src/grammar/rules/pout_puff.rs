//! `pout-cheek-puff`: a pout puffs the cheeks and lowers the brows a little; it lets go gradually.
//!
//! ## Where it comes from
//!
//! The prototype has no pout rule. It only maps `cheekPuff` to a touch of blush and `mouthPucker` to its kiss pose
//! (`arkit.js`: `pucker = clamp01((mouthPucker - 0.3) * 1.6) * (1 - clamp01(jaw * 2))`). This rule is a design from
//! those two lines and FE-05's "a pout puffs cheeks": a pout is a pucker **together with** a frown, on a shut mouth.
//!
//! - The pout level is `min(mouthPucker, mean mouthFrown)`, so a kiss (all pucker, no frown) and a sulky frown (all
//!   frown, no pucker) are not pouts, only both together are.
//! - It is scaled by how shut the mouth is, `1 - clamp01(jaw / jaw-max)`: the prototype's own factor, with `jaw-max`
//!   0.5. A pout is made with the jaw closed.
//! - It starts above `pout-on` (0.3, the prototype's pucker dead zone) and is kept above `pout-off` (0.2). Once
//!   the level drops it holds for 0.15 s and fades out over 0.25 s, slower than it came in (0.12 s), so a pout dies away
//!   the way a held breath is let out instead of snapping flat.
//! - It starts only after the level has held above `pout-on` for `attack` (0.1 s; `0` starts at once). A tracker's
//!   pucker and frown can spike for a frame or two (up to about 0.1 s at 30 fps) without anyone pouting. A deliberate
//!   pout lasts far longer, so the hold costs it nothing: the look is complete about 0.22 s after the pout begins (this
//!   hold, then the 0.12 s rise).
//! - At full strength a pout raises `cheekPuff` to `puff` (0.8) and `browDown*` to `brow-down` (0.25, "slightly").
//!
//! All of these are numbers a character may set under the names given (`pout-on`, `pout-off`, `jaw-max`, `attack`,
//! `puff`, `brow-down`), a release level is never above its start level, and a character that sets only `pout-on` keeps
//! the default gap below it for the release level. Writes are raises to at least ([`raise`]): a cheek puff a person
//! makes themselves is never lowered.
//!
//! ## Ownership
//!
//! `cheekPuff` and `browDown*` are `ARKit` channels, so while a tracker drives them the rule writes nothing (a tracked
//! person's own puff and brow stay theirs); its latch keeps following the mouth regardless.

use omuzaki_anim_api::Channel;

use super::common::{Band, MAX_ATTACK, Presence, Timing, raise};
use crate::grammar::{GrammarRule, RuleCtx, RuleParams};
use crate::register_grammar_rule;

/// A pout is only believed after 0.1 s, then builds in 0.12 s, rides out a 0.15 s dip, and fades over 0.25 s.
const POUT: Timing = Timing { attack: 0.1, hold: 0.15, rise: 0.12, fall: 0.25 };
/// The pout level that starts a pout and the one that keeps it.
const LEVEL: Band = Band { on: 0.3, off: 0.2 };
/// The jaw opening at which a mouth is too open to pout (the prototype's `1 - clamp01(jaw * 2)`).
const JAW_MAX: f32 = 0.5;
/// Where a full pout raises `cheekPuff` and `browDown*`.
const PUFF: f32 = 0.8;
const BROW_DOWN: f32 = 0.25;

/// The pout-cheek-puff rule. Its memory is whether a pout is showing and how far it has faded in.
#[derive(Clone, Debug)]
pub struct PoutCheekPuff {
    level: Band,
    jaw_max: f32,
    puff: f32,
    brow_down: f32,
    timing: Timing,
    pout: Presence,
}

impl Default for PoutCheekPuff {
    fn default() -> Self {
        Self {
            level: LEVEL,
            jaw_max: JAW_MAX,
            puff: PUFF,
            brow_down: BROW_DOWN,
            timing: POUT,
            pout: Presence::default(),
        }
    }
}

impl PoutCheekPuff {
    /// How much of a pout the incoming face shows, `0..=1`.
    fn level(&self, ctx: &RuleCtx<'_>) -> f32 {
        let frown = f32::midpoint(ctx.get(Channel::MOUTH_FROWN_LEFT), ctx.get(Channel::MOUTH_FROWN_RIGHT));
        let shut = 1.0 - (ctx.get(Channel::JAW_OPEN) / self.jaw_max).clamp(0.0, 1.0);
        ctx.get(Channel::MOUTH_PUCKER).min(frown) * shut
    }
}

impl GrammarRule for PoutCheekPuff {
    fn id(&self) -> &'static str {
        "pout-cheek-puff"
    }

    /// After the laugh (10): a face that is laughing is not pouting, and neither rule reads what the other writes.
    fn priority(&self) -> i32 {
        20
    }

    fn step(&mut self, ctx: &mut RuleCtx<'_>) {
        let level = self.level(ctx);
        let pout = self.pout.step(ctx.dt(), self.level.starts(level), self.level.keeps(level), self.timing);
        if pout > 0.0 {
            raise(ctx, Channel::CHEEK_PUFF, self.puff * pout);
            raise(ctx, Channel::BROW_DOWN_LEFT, self.brow_down * pout);
            raise(ctx, Channel::BROW_DOWN_RIGHT, self.brow_down * pout);
        }
    }

    fn reset(&mut self) {
        self.pout = Presence::default();
    }

    fn configure(&mut self, params: RuleParams<'_>) {
        self.level = Band::read(params, "pout-on", "pout-off", LEVEL);
        // Never zero: it divides the jaw opening.
        self.jaw_max = params.number("jaw-max", JAW_MAX, 0.05, 1.0);
        self.timing.attack = params.number("attack", POUT.attack, 0.0, MAX_ATTACK);
        self.puff = params.number("puff", PUFF, 0.0, 1.0);
        self.brow_down = params.number("brow-down", BROW_DOWN, 0.0, 1.0);
    }
}

register_grammar_rule!(PoutCheekPuff);
