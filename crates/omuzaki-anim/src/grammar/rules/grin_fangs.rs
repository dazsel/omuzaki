//! `grin-fangs`: a big smile with an open mouth shows the fangs.
//!
//! ## Where it comes from
//!
//! The prototype has no fang rule. Its painter draws fangs from a `fang` parameter that a score sets by hand, and only
//! on a smiling mouth (`face.js`: closed, `fang > 0.5 && smile > 0.3`; open, any `fang > 0`); its fang pose
//! (`score.js` `FANG`) is a smile of 0.7 on a stretched mouth, about 0.5 in `ARKit` terms. This rule is the automatic
//! version, a design from that pose and FE-05's "a big smile with an open mouth shows fangs, if the character has them".
//!
//! - A grin starts above a smile of `smile-on` (0.5) and a jaw opening of `jaw-on` (0.15), and is kept above `smile-off`
//!   (0.4) and `jaw-off` (0.08); a release level is never above its start level, and a character that sets only a start
//!   level keeps the default gap below it. The smile is the life layer's reading, the mean of smile minus frown over
//!   the two corners.
//! - It starts only after both have held above their start levels for `attack` (0.08 s; `0` starts at once), so a spike
//!   of a frame or two from a tracker shows no fangs. A grin is a quick expression, so this is shorter than the pout's
//!   0.1 s: the fangs are complete about 0.16 s after the grin begins (this hold, then the 0.08 s rise).
//! - It holds for 0.1 s once the grin drops, and fades over 0.15 s, so the fangs linger a moment after it.
//! - A full grin raises the `fangs` channel to `show` (1).
//!
//! The numbers a character may set are `smile-on`, `smile-off`, `jaw-on`, `jaw-off`, `attack` and `show`.
//!
//! ## A character without fangs
//!
//! `show` is the character's own number for how much fang it has: at `0` the rule never writes anything. It reads that
//! from its configuration, never from the painter. A profile whose fang style is `none` ([`FangKind::None`]) switches
//! the rule off ([`GrammarConfig::from_profile`]), so a character without fangs needs no setting of its own; `show` is
//! for a character that has fangs but wants them dialled down.
//!
//! ## Ownership
//!
//! `fangs` is an extra, not an `ARKit` channel, so a tracker never drives it and the rule decorates a tracked grin
//! too. A score or a person that does own it is left alone ([`RuleCtx::put`]), and a higher value is never lowered.

use omuzaki_anim_api::Channel;
#[cfg(doc)]
use omuzaki_face_api::FangKind;

use super::common::{Band, MAX_ATTACK, Presence, Timing, net_smile, raise};
#[cfg(doc)]
use crate::grammar::GrammarConfig;
use crate::grammar::{GrammarRule, RuleCtx, RuleParams};

/// The fangs are only believed after 0.08 s, then rise in 0.08 s and linger a moment after the grin.
const GRIN: Timing = Timing { attack: 0.08, hold: 0.1, rise: 0.08, fall: 0.15 };
/// The smile that starts and keeps a grin (the prototype's fang pose is about 0.5).
const SMILE: Band = Band { on: 0.5, off: 0.4 };
/// The jaw opening that starts and keeps a grin: a mouth that is open at all.
const JAW: Band = Band { on: 0.15, off: 0.08 };
/// How much fang a character shows at a full grin.
const SHOW: f32 = 1.0;

/// The grin-fangs rule. Its memory is whether a grin is showing and how far it has faded in.
#[derive(Clone, Debug)]
pub struct GrinFangs {
    smile: Band,
    jaw: Band,
    show: f32,
    timing: Timing,
    grin: Presence,
}

impl Default for GrinFangs {
    fn default() -> Self {
        Self { smile: SMILE, jaw: JAW, show: SHOW, timing: GRIN, grin: Presence::default() }
    }
}

impl GrammarRule for GrinFangs {
    fn id(&self) -> &'static str {
        "grin-fangs"
    }

    /// Last of the four: it writes a channel nothing else reads.
    fn priority(&self) -> i32 {
        30
    }

    fn step(&mut self, ctx: &mut RuleCtx<'_>) {
        let (smile, jaw) = (net_smile(ctx), ctx.get(Channel::JAW_OPEN));
        let grin = self.grin.step(
            ctx.dt(),
            self.smile.starts(smile) && self.jaw.starts(jaw),
            self.smile.keeps(smile) && self.jaw.keeps(jaw),
            self.timing,
        );
        // The latch above follows the mouth even for a character without fangs, so it is right if `show` is raised later.
        if grin > 0.0 && self.show > 0.0 {
            raise(ctx, Channel::FANGS, self.show * grin);
        }
    }

    fn reset(&mut self) {
        self.grin = Presence::default();
    }

    fn configure(&mut self, params: RuleParams<'_>) {
        self.smile = Band::read(params, "smile-on", "smile-off", SMILE);
        self.jaw = Band::read(params, "jaw-on", "jaw-off", JAW);
        self.timing.attack = params.number("attack", GRIN.attack, 0.0, MAX_ATTACK);
        self.show = params.number("show", SHOW, 0.0, 1.0);
    }
}

register_grammar_rule!(GrinFangs);
