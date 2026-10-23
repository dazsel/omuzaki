//! `laugh-squint`: a big laugh squints the eyes and lifts the cheeks, and a huge one closes the eyes into `^^`.
//!
//! ## Where it comes from
//!
//! The detector is the prototype's laugh (`life.js`, mirrored by `life/laugh.rs`): it starts on a wide smile with an
//! open mouth that is loud (`loud > 0.45`), is kept on weaker values for a short hold so a flicker cannot chatter it,
//! and eases in and out over 0.15 s. The loudness is the life layer's: the speech loudness, raised to the audio
//! laughter probability ([`RuleCtx::loudness`]), so the rule and the life layer start and end the same laughs (#638),
//! and talking through a smile is not a laugh. Without audio the loudness is zero and neither of them laughs.
//!
//! ## Who does what: the life layer and this rule
//!
//! Both see the same laugh, and each does its own part:
//!
//! | | the life layer (`life/laugh.rs`) | this rule |
//! |---|---|---|
//! | sees | the smile, the jaw **and** the speech loudness or [`Cues::laugh`](omuzaki_anim_api::Cues) | the same |
//! | head | **the bob**, back and up at `laugh_bob_hz` | nothing: it never writes a head channel |
//! | eyes | a small natural squint (`laugh_squint`, 0.2) | the anime squint, `eyeSquint*` up to [`SQUINT`] |
//! | cheeks | the cheek push of any smile | `cheekSquint*` up to [`CHEEK`] |
//! | `^^` | on a really loud laugh (`laugh_arcs`) | on a huge smile, `eyeArc*` |
//!
//! The grammar stage runs on the bus after the life layer with the same [`Cues`](omuzaki_anim_api::Cues), so both
//! decide a laugh from the same smile, jaw and loudness. Everything this rule writes is a *raise to at least*
//! ([`raise`]), so where the life layer already squinted the result is the larger of the two and never the sum, and a
//! laugh the life layer closed into `^^` stays closed.
//!
//! ## Numbers a character may set
//!
//! `smile-on` (0.6) and `smile-off` (0.45) bound the smile that starts and keeps a laugh, `jaw-on` (0.3) and `jaw-off`
//! (0.2) the jaw opening, `loud-on` (0.45) and `loud-off` (0.3) the loudness; these are the prototype's (and the life
//! layer's `LaughParams`). `arc-smile` (0.85) is the smile at which a laugh closes into `^^`
//! (kept down to 0.1 below it), `squint` (0.6) and `cheek` (0.6) the levels a full laugh raises the channels to.
//! A release level is never above its start level, and a character that sets only a start level keeps the default gap
//! below it for the release level.
//!
//! ## Ownership
//!
//! Each channel goes through [`RuleCtx::put`], so any a person drives is left alone; a tracker drives `eyeSquint*`
//! and `cheekSquint*` but has no `eyeArc*`, so under a tracker a huge laugh still closes into `^^`. The detector keeps
//! following the mouth whoever drives it, so it is right on the frame the eyes are handed back.

use omuzaki_anim_api::Channel;

use super::common::{Band, Presence, Timing, net_smile, raise};
use crate::grammar::{GrammarRule, RuleCtx, RuleParams};

/// The prototype's laugh timing: a flicker shorter than 0.12 s is ridden out, and a laugh fades over 0.15 s. There is
/// no attack hold: the prototype starts a laugh on the first frame, and this rule keeps its parity.
const LAUGH: Timing = Timing { attack: 0.0, hold: 0.12, rise: 0.15, fall: 0.15 };
/// The `^^` follows the laugh with the same timing, so it cannot chatter at its threshold either.
const ARCS: Timing = LAUGH;
/// The smile that starts and keeps a laugh (the prototype's `0.6` and `0.45`).
const SMILE: Band = Band { on: 0.6, off: 0.45 };
/// The jaw opening that starts and keeps a laugh (the prototype's `0.3` and `0.2`).
const JAW: Band = Band { on: 0.3, off: 0.2 };
/// The loudness that starts and keeps a laugh (the prototype's `0.45` and `0.3`, the life layer's `start_loud` and
/// `keep_loud`).
const LOUD: Band = Band { on: 0.45, off: 0.3 };
/// The smile at which a laugh closes into `^^`, and how far below it the `^^` is kept. The life layer closes a laugh
/// into `^^` on loudness (`0.85`); this rule closes a laugh on a huge smile, so a laugh that shows it does too.
const ARC_SMILE: f32 = 0.85;
const ARC_BAND: f32 = 0.1;
/// Where a full laugh raises `eyeSquint*` and `cheekSquint*`. 0.6 is the life layer's cap on a coupled lid (the
/// prototype's `capLid`), so the anime squint stops where the natural one would.
const SQUINT: f32 = 0.6;
const CHEEK: f32 = 0.6;

/// The laugh-squint rule. Its memory is whether a laugh and a `^^` are showing, and how far each has faded in.
#[derive(Clone, Debug)]
pub struct LaughSquint {
    smile: Band,
    jaw: Band,
    loud: Band,
    arc_smile: Band,
    squint: f32,
    cheek: f32,
    laugh: Presence,
    arcs: Presence,
}

impl Default for LaughSquint {
    fn default() -> Self {
        Self {
            smile: SMILE,
            jaw: JAW,
            loud: LOUD,
            arc_smile: Band { on: ARC_SMILE, off: ARC_SMILE - ARC_BAND },
            squint: SQUINT,
            cheek: CHEEK,
            laugh: Presence::default(),
            arcs: Presence::default(),
        }
    }
}

impl GrammarRule for LaughSquint {
    fn id(&self) -> &'static str {
        "laugh-squint"
    }

    /// After `smile-blink-arc` (0), so a `^` it drew is already on the bus when the laugh decides about `^^`.
    fn priority(&self) -> i32 {
        10
    }

    fn step(&mut self, ctx: &mut RuleCtx<'_>) {
        let (smile, jaw, loud) = (net_smile(ctx), ctx.get(Channel::JAW_OPEN), ctx.loudness());
        let dt = ctx.dt();
        let laugh = self.laugh.step(
            dt,
            self.smile.starts(smile) && self.jaw.starts(jaw) && self.loud.starts(loud),
            self.smile.keeps(smile) && self.jaw.keeps(jaw) && self.loud.keeps(loud),
            LAUGH,
        );
        // The `^^` needs a laugh and a huge smile. It reads the laugh after this frame's update, as the prototype does.
        let laughing = self.laugh.is_on();
        let arcs =
            self.arcs.step(dt, laughing && self.arc_smile.starts(smile), laughing && self.arc_smile.keeps(smile), ARCS);

        if laugh > 0.0 {
            for (squint, cheek) in [
                (Channel::EYE_SQUINT_LEFT, Channel::CHEEK_SQUINT_LEFT),
                (Channel::EYE_SQUINT_RIGHT, Channel::CHEEK_SQUINT_RIGHT),
            ] {
                raise(ctx, squint, self.squint * laugh);
                raise(ctx, cheek, self.cheek * laugh);
            }
        }
        if arcs > 0.0 {
            raise(ctx, Channel::EYE_ARC_LEFT, arcs);
            raise(ctx, Channel::EYE_ARC_RIGHT, arcs);
        }
    }

    fn reset(&mut self) {
        self.laugh = Presence::default();
        self.arcs = Presence::default();
    }

    fn configure(&mut self, params: RuleParams<'_>) {
        let default = Self::default();
        self.smile = Band::read(params, "smile-on", "smile-off", default.smile);
        self.jaw = Band::read(params, "jaw-on", "jaw-off", default.jaw);
        self.loud = Band::read(params, "loud-on", "loud-off", default.loud);
        let arc_on = params.number("arc-smile", ARC_SMILE, 0.0, 1.0);
        self.arc_smile = Band { on: arc_on, off: (arc_on - ARC_BAND).max(0.0) };
        self.squint = params.number("squint", SQUINT, 0.0, 1.0);
        self.cheek = params.number("cheek", CHEEK, 0.0, 1.0);
    }
}

register_grammar_rule!(LaughSquint);
