//! `smile-blink-arc`: a blink while smiling closes into the happy `^` arc; any other blink stays a plain lash line.
//!
//! Ported from the prototype: the smile level and its hysteresis are `arkit.js` (the `winkArc` line), and the per-eye
//! decision is `face.js` `eye()` (a wink closes into the arc when `wk > 0.55 && winkArc > 0.5`).
//!
//! The prototype only chose the arc shape; here the rule also writes it. When an eye blinks past [`BLINK_AT`] while
//! the face is smiling it raises that eye's `eyeArc` channel (the painter draws `^` from an arc of `0.5` or more, and
//! an arc closes the lid itself) and relaxes the same eye's `eyeBlink`, so the lash line is not drawn under the arc.
//! Strength dials how far the eye follows: at `0.5` the arc is half there, at `1` or more it is fully shut as `^`.
//!
//! ## Ownership
//!
//! Each channel is written only when no person drives it, through [`RuleCtx::put`]. A tracker normally drives
//! `eyeBlink*` but has no `eyeArc*` channel, so a tracked blink while smiling still raises the arc and the painter
//! draws `^`, as the prototype did. The smile latch is updated every frame from whatever the mouth shows, whoever drives
//! it, so it is already right on the frame the eyes are handed back.

use omuzaki_anim_api::Channel;

use crate::grammar::{GrammarRule, RuleCtx};

/// The smile level above which the face counts as smiling (the prototype's `0.42`).
const SMILE_ON: f32 = 0.42;
/// The smile level below which it stops (the prototype's `0.32`). Between the two the previous answer stands, so a
/// smile hovering near one threshold cannot flicker the arcs.
const SMILE_OFF: f32 = 0.32;
/// An eye blinking past this much turns into an arc while smiling (the prototype's `0.55`).
const BLINK_AT: f32 = 0.55;
/// How much a frown takes off a smile, and a dimple adds to it (the prototype's `0.8` and `0.25`).
const FROWN_WEIGHT: f32 = 0.8;
const DIMPLE_WEIGHT: f32 = 0.25;

/// The smile-blink-arc rule. Its only memory is whether the face is smiling.
#[derive(Clone, Debug, Default)]
pub struct SmileBlinkArc {
    smiling: bool,
}

impl SmileBlinkArc {
    /// The smile level of the incoming face: the two mouth corners' `smile - 0.8 frown + 0.25 dimple`, averaged.
    fn smile_level(ctx: &RuleCtx<'_>) -> f32 {
        let corner =
            |smile, frown, dimple| ctx.get(smile) - FROWN_WEIGHT * ctx.get(frown) + DIMPLE_WEIGHT * ctx.get(dimple);
        let left = corner(Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_FROWN_LEFT, Channel::MOUTH_DIMPLE_LEFT);
        let right = corner(Channel::MOUTH_SMILE_RIGHT, Channel::MOUTH_FROWN_RIGHT, Channel::MOUTH_DIMPLE_RIGHT);
        f32::midpoint(left, right)
    }
}

impl GrammarRule for SmileBlinkArc {
    fn id(&self) -> &'static str {
        "smile-blink-arc"
    }

    /// Runs early: the arc is a closure, and later rules that squint or squeeze the eyes should see it.
    fn priority(&self) -> i32 {
        0
    }

    fn step(&mut self, ctx: &mut RuleCtx<'_>) {
        // The prototype's `s > 0.42 ? 1 : s < 0.32 ? 0 : arc`: strict comparisons, and between them the answer stands.
        // A corrupt (non-finite) mouth channel reads as 0, which is no smile.
        let smile = Self::smile_level(ctx);
        if smile > SMILE_ON {
            self.smiling = true;
        } else if smile < SMILE_OFF {
            self.smiling = false;
        }
        if !self.smiling {
            return;
        }
        for (blink, arc) in
            [(Channel::EYE_BLINK_LEFT, Channel::EYE_ARC_LEFT), (Channel::EYE_BLINK_RIGHT, Channel::EYE_ARC_RIGHT)]
        {
            if ctx.get(blink) > BLINK_AT {
                ctx.put(arc, 1.0);
                ctx.put(blink, 0.0);
            }
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

register_grammar_rule!(SmileBlinkArc);
