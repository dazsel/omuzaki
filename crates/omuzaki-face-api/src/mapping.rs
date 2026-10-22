//! What the face does with the channels: the mapping from the ARKit-native bus to **paint parameters** (spec FE-01,
//! FE-06).
//!
//! The bus ([`omuzaki_anim_api::ChannelBuf`]) carries the 52 `ARKit` blendshapes, head and body channels and the anime
//! extras. The painter does not draw "jawOpen"; it draws a mouth of some opening, width and corner lift. The formulas
//! that combine channels into those shapes (how `mouthSmile`, `mouthFrown` and `mouthDimple` become one corner lift;
//! how brow channels become a height and an angle) are a property of the *face*, so they live here, with their
//! coefficients stored in the face profile ([`FaceMapping`]): a character can exaggerate or tame any of them (FE-06).
//!
//! [`FaceMapping::apply`] is pure and allocation-free: the same buffer always gives the same [`PaintParams`], and
//! mirroring the buffer ([`ChannelBuf::mirror`]) mirrors the result ([`PaintParams::mirrored`]); a test holds both.
//! Its default coefficients are the prototype's (`arkit.js`, the half after calibration), so a ported character
//! moves like the prototype's.
//!
//! **Coverage.** Every `ARKit` channel and every anime extra changes the result, except [`UNMAPPED`]: three `ARKit`
//! channels with no effect on a front-facing 2D face. Head, body and `custom` channels are not the face's business.
//! A test enforces the list, so a channel can't silently go dead (spec §32 "expression range").
//!
//! **Left and right** are the character's own: left is +x, on the viewer's right when the character faces the camera.

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::glam::Vec2;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::check::{Check, Validate};
use crate::paint::narrow as coef;

/// The `ARKit` channels the 2D face has no use for: `jawForward` and the two `mouthShrug` shapes move lips in depth.
/// Painters ignore them, and a VRM bake writes empty targets for them (open question in the design doc).
pub const UNMAPPED: [Channel; 3] = [Channel::JAW_FORWARD, Channel::MOUTH_SHRUG_LOWER, Channel::MOUTH_SHRUG_UPPER];

// ------------------------------------------------------------------------------------------------ paint parameters

/// One eye's lids.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EyeParams {
    /// Upper-lid openness relative to the profile's rest aperture: 0 shut, 1 rest, up to 1.9 wide.
    pub open: f32,
    /// Lid closure from a blink, `0..=1`.
    pub close: f32,
    /// How much a closed lid draws as a happy ^ arc instead of a relaxed lash line, `0..=1`. An arc also closes the
    /// lid: the eye is shut when `max(close, arc)` is near 1, and drawn as ^ when `arc >= 0.5` (^^ happy eyes, a wink).
    pub arc: f32,
    /// Lower-lid raise (cheek push), `0..=1`.
    pub lower_lid: f32,
}

/// One brow.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BrowParams {
    /// Height: positive raised, negative lowered. Roughly `-1.5..=1.5`.
    pub height: f32,
    /// Angle: positive angry (inner end down), negative worried (inner end up). Roughly `-2.5..=2.5`.
    pub tilt: f32,
}

/// Eye effects (spec FE-02), each `0..=1`.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct EyeEffects {
    /// Heart catchlights.
    pub heart: f32,
    /// Star catchlights.
    pub star: f32,
    /// Shadowed gloom eyes.
    pub gloom: f32,
    /// White shock eyes.
    pub shock: f32,
    /// Squeezed `>_<` eyes.
    pub squeeze: f32,
    /// Spiral `@_@` eyes.
    pub spiral: f32,
    /// Iris sparkle.
    pub sparkle: f32,
    /// Tear drops.
    pub tears: f32,
}

/// The mouth.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MouthParams {
    /// How far it opens, `0..=1.5`.
    pub open: f32,
    /// Width relative to the profile, `0.3..=2`.
    pub wide: f32,
    /// Lift of the character's left corner (absolute, so a smirk is two different numbers): positive smile, negative
    /// frown. Roughly `-2..=2`.
    pub smile_left: f32,
    /// Lift of the character's right corner.
    pub smile_right: f32,
    /// Rounded "oh" shape, `0..=1`.
    pub round: f32,
    /// Sideways shift: `+1` toward the character's left.
    pub shift: f32,
    /// Lips pressed together, `0..=1`.
    pub press: f32,
    /// Kiss pucker, `0..=1`.
    pub pucker: f32,
    /// Upper teeth show, `0..=1`.
    pub teeth_upper: f32,
    /// Lower teeth show, `0..=1`.
    pub teeth_lower: f32,
    /// Tongue out, `0..=1`.
    pub tongue: f32,
    /// Fangs show, `0..=1`.
    pub fangs: f32,
}

/// Everything a painter needs to draw one face this frame, derived from the channels by a [`FaceMapping`]. `Copy` and
/// fixed-size (164 bytes): nothing allocates between the bus and the painter.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PaintParams {
    /// The character's left eye (+x).
    pub left_eye: EyeParams,
    /// The character's right eye.
    pub right_eye: EyeParams,
    /// Lid tilt of both eyes: positive angry, negative sad. Roughly `-2..=2`.
    pub eye_tilt: f32,
    /// Pupil size relative to the profile: below 1 shrinks it. `0.4..=1.6`.
    pub pupil: f32,
    /// Where both eyes look, in look units: `x` positive toward the character's left, `y` up; 1 is about 40 degrees.
    pub gaze: Vec2,
    /// Convergence: positive turns both irises toward the nose, negative apart. `-1..=1`.
    pub vergence: f32,
    /// Eye effects.
    pub effects: EyeEffects,
    /// The character's left brow.
    pub left_brow: BrowParams,
    /// The character's right brow.
    pub right_brow: BrowParams,
    /// Curvature of both brows: positive more arched. `-1..=1`.
    pub brow_arch: f32,
    /// The mouth.
    pub mouth: MouthParams,
    /// Blush strength, `0..=1.6`.
    pub blush: f32,
    /// Puffed cheeks, `0..=1`.
    pub cheek_puff: f32,
    /// Sweat drop, `0..=1`.
    pub sweat: f32,
}

impl PaintParams {
    /// The left-right mirror image: sides swap, `gaze.x` and `mouth.shift` flip.
    #[must_use]
    pub fn mirrored(&self) -> Self {
        Self {
            left_eye: self.right_eye,
            right_eye: self.left_eye,
            gaze: Vec2::new(-self.gaze.x, self.gaze.y),
            left_brow: self.right_brow,
            right_brow: self.left_brow,
            mouth: MouthParams {
                smile_left: self.mouth.smile_right,
                smile_right: self.mouth.smile_left,
                shift: -self.mouth.shift,
                ..self.mouth
            },
            ..*self
        }
    }
}

// ------------------------------------------------------------------------------------------------ the mapping

/// The character's resting face in paint terms: what every parameter is when no channel moves it. (A sleepy half-lid,
/// a slightly downturned mouth, a usual blush.) Variants override it: a performing face is livelier than a game face.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RestShape {
    /// Upper-lid openness: 1 is the profile's rest aperture, 0.5 sleepy, 1.3 a wide-eyed performing default.
    pub eye_open: f64,
    /// Lower-lid raise.
    pub lower_lid: f64,
    /// Lid tilt: positive angry, negative sad.
    pub eye_tilt: f64,
    /// Pupil size.
    pub pupil: f64,
    /// Brow height.
    pub brow_height: f64,
    /// Brow angle.
    pub brow_tilt: f64,
    /// Brow curvature.
    pub brow_arch: f64,
    /// Mouth-corner lift: negative is an aloof downturn, positive a faint smile.
    pub mouth_smile: f64,
    /// Mouth width.
    pub mouth_wide: f64,
    /// Blush.
    pub blush: f64,
}

impl Default for RestShape {
    fn default() -> Self {
        Self {
            eye_open: 1.0,
            lower_lid: 0.0,
            eye_tilt: 0.0,
            pupil: 1.0,
            brow_height: 0.0,
            brow_tilt: 0.0,
            brow_arch: 0.0,
            mouth_smile: 0.0,
            mouth_wide: 1.0,
            blush: 0.0,
        }
    }
}

impl Validate for RestShape {
    fn check(&self, c: &mut Check) {
        c.range("eye_open", self.eye_open, 0.0, 1.9);
        c.range("lower_lid", self.lower_lid, 0.0, 1.0);
        c.range("eye_tilt", self.eye_tilt, -2.0, 2.0);
        c.range("pupil", self.pupil, 0.4, 1.6);
        c.range("brow_height", self.brow_height, -1.5, 1.5);
        c.range("brow_tilt", self.brow_tilt, -2.5, 2.5);
        c.range("brow_arch", self.brow_arch, -1.0, 1.0);
        c.range("mouth_smile", self.mouth_smile, -2.0, 2.0);
        c.range("mouth_wide", self.mouth_wide, 0.3, 2.0);
        c.range("blush", self.blush, 0.0, 1.6);
    }
}

/// Eye coefficients.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EyeMap {
    /// Openness added by `eyeWide`.
    pub wide: f64,
    /// Openness removed by `eyeSquint`.
    pub squint: f64,
    /// Lower-lid raise from `eyeSquint`.
    pub squint_lid: f64,
    /// Lower-lid raise from `cheekSquint`.
    pub cheek_lid: f64,
    /// How much the mean brow angle also tilts the lids (angry brows slant the eyes, worried ones droop them). 0 turns
    /// it off.
    pub tilt_from_brows: f64,
    /// How far `eyesShock` shrinks the pupil, `0..=1`.
    pub shock_pupil: f64,
}

impl Default for EyeMap {
    fn default() -> Self {
        Self { wide: 0.65, squint: 0.45, squint_lid: 0.35, cheek_lid: 0.4, tilt_from_brows: 0.5, shock_pupil: 0.6 }
    }
}

/// Brow coefficients: how each brow channel moves height, angle and arch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BrowMap {
    /// Height from `browInnerUp`.
    pub inner_up: f64,
    /// Height from `browOuterUp`.
    pub outer_up: f64,
    /// Height lost to `browDown`.
    pub down: f64,
    /// Height lost to `noseSneer`.
    pub sneer: f64,
    /// Angle (worried) from `browInnerUp`.
    pub inner_up_tilt: f64,
    /// Angle from `browOuterUp`.
    pub outer_up_tilt: f64,
    /// Angle (angry) from `browDown`.
    pub down_tilt: f64,
    /// Arch from `browOuterUp`.
    pub outer_up_arch: f64,
    /// Arch lost to `browInnerUp`.
    pub inner_up_arch: f64,
}

impl Default for BrowMap {
    fn default() -> Self {
        Self {
            inner_up: 0.55,
            outer_up: 0.6,
            down: 0.7,
            sneer: 0.25,
            inner_up_tilt: 1.1,
            outer_up_tilt: 0.45,
            down_tilt: 1.0,
            outer_up_arch: 0.4,
            inner_up_arch: 0.3,
        }
    }
}

/// Mouth coefficients.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MouthMap {
    /// Opening per unit of `jawOpen`.
    pub open: f64,
    /// How much `mouthClose` cancels of the jaw.
    pub close_cancel: f64,
    /// Corner lift per unit of `mouthSmile`.
    pub smile: f64,
    /// How much a `mouthFrown` weighs against a smile.
    pub frown: f64,
    /// How much a `mouthDimple` adds to a smile.
    pub dimple: f64,
    /// Width per unit of `mouthStretch`.
    pub stretch: f64,
    /// Width added by a smile.
    pub wide_smile: f64,
    /// Width removed by `mouthPucker`.
    pub wide_pucker: f64,
    /// Roundness per unit of `mouthFunnel`.
    pub round: f64,
    /// `mouthPucker` below this shows no kiss (a dead zone).
    pub pucker_start: f64,
    /// Kiss strength per unit above the dead zone.
    pub pucker_gain: f64,
    /// Sideways shift per unit of `mouthLeft` minus `mouthRight`.
    pub shift_mouth: f64,
    /// Sideways shift per unit of `jawLeft` minus `jawRight`.
    pub shift_jaw: f64,
    /// Press per unit of `mouthPress`.
    pub press: f64,
    /// Press added by `mouthRollLower`.
    pub press_roll_lower: f64,
    /// Press added by `mouthRollUpper`.
    pub press_roll_upper: f64,
    /// Press added by `mouthClose`.
    pub press_close: f64,
    /// `mouthUpperUp` below this shows no teeth.
    pub teeth_start: f64,
    /// Teeth per unit above it.
    pub teeth_gain: f64,
    /// `mouthLowerDown` below this shows no lower teeth.
    pub teeth_lower_start: f64,
    /// Lower teeth per unit above it.
    pub teeth_lower_gain: f64,
    /// A smile weaker than this hides the lower teeth (a toothy grin needs a real smile).
    pub teeth_lower_smile_start: f64,
    /// How fast the lower teeth come in above that smile.
    pub teeth_lower_smile_gain: f64,
    /// How far `mouthFunnel` parts the lips with the jaw closed, as a jaw opening: ARKit's funnel flares the lips into an
    /// O, so an "ooh" with a barely open jaw reads as a small round mouth instead of a closed line.
    pub part_funnel: f64,
    /// How far `mouthUpperUp` parts the lips with the jaw closed (the upper lip lifts off the teeth).
    pub part_upper_up: f64,
    /// How far `mouthLowerDown` parts the lips with the jaw closed (the lower lip drops off the teeth).
    pub part_lower_down: f64,
    /// How far `tongueOut` parts the lips with the jaw closed, so a tongue poked out between closed teeth shows.
    pub part_tongue: f64,
}

impl Default for MouthMap {
    fn default() -> Self {
        Self {
            open: 1.15,
            close_cancel: 0.35,
            smile: 1.35,
            frown: 0.8,
            dimple: 0.25,
            stretch: 0.55,
            wide_smile: 0.2,
            wide_pucker: 0.25,
            round: 1.2,
            pucker_start: 0.3,
            pucker_gain: 1.6,
            shift_mouth: 0.9,
            shift_jaw: 0.5,
            press: 0.8,
            press_roll_lower: 0.5,
            press_roll_upper: 0.3,
            press_close: 0.2,
            teeth_start: 0.25,
            teeth_gain: 2.0,
            teeth_lower_start: 0.35,
            teeth_lower_gain: 2.0,
            teeth_lower_smile_start: 0.25,
            teeth_lower_smile_gain: 4.0,
            part_funnel: 0.18,
            part_upper_up: 0.14,
            part_lower_down: 0.14,
            part_tongue: 0.22,
        }
    }
}

/// Cheek coefficients.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CheekMap {
    /// Blush added by `cheekPuff`.
    pub puff_blush: f64,
    /// Blush added by `cheekSquint`.
    pub squint_blush: f64,
    /// Blush per unit of the `blush` channel.
    pub blush: f64,
}

impl Default for CheekMap {
    fn default() -> Self {
        Self { puff_blush: 0.35, squint_blush: 0.25, blush: 1.0 }
    }
}

/// Gaze coefficients: look units per unit of `eyeLook*` weight.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GazeMap {
    /// Sideways.
    pub x: f64,
    /// Vertical.
    pub y: f64,
}

impl Default for GazeMap {
    fn default() -> Self {
        Self { x: 0.9, y: 0.8 }
    }
}

/// How one character's face reads the channels (spec FE-06 "exaggeration profiles"): a resting face and the
/// coefficients of every formula. Stored in the face profile, so variants and ops edit single numbers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FaceMapping {
    /// The resting face.
    pub rest: RestShape,
    /// Eyes.
    pub eyes: EyeMap,
    /// Brows.
    pub brows: BrowMap,
    /// Mouth.
    pub mouth: MouthMap,
    /// Cheeks.
    pub cheeks: CheekMap,
    /// Gaze.
    pub gaze: GazeMap,
}

fn unit(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

impl FaceMapping {
    /// The parameters of the resting face: [`FaceMapping::apply`] on a neutral buffer.
    pub fn rest_params(&self) -> PaintParams {
        self.apply(&ChannelBuf::NEUTRAL)
    }

    /// Turns the bus into paint parameters. Pure and allocation-free. It first sanitises a stack copy of the bus
    /// ([`ChannelBuf::sanitize`]: NaN and infinities become neutral, each value is clamped to its channel's range), so a
    /// stray value, even a NaN, only moves the result within the documented ranges, never out of them, and the caller's
    /// buffer is untouched.
    pub fn apply(&self, bus: &ChannelBuf) -> PaintParams {
        let mut sanitised = *bus;
        sanitised.sanitize();
        let ch = &sanitised;
        let rest = &self.rest;
        let (left_eye, right_eye) = (self.eye(ch, Side::Left), self.eye(ch, Side::Right));
        let (left_brow, right_brow) = (self.brow(ch, Side::Left), self.brow(ch, Side::Right));
        let (look_x, look_y) = ch.gaze();
        let converge = mid(ch, Channel::EYE_LOOK_IN_LEFT, Channel::EYE_LOOK_IN_RIGHT)
            - mid(ch, Channel::EYE_LOOK_OUT_LEFT, Channel::EYE_LOOK_OUT_RIGHT);
        let shock = ch.get(Channel::EYES_SHOCK);
        let g = |c: Channel| ch.get(c);
        PaintParams {
            left_eye,
            right_eye,
            eye_tilt: (coef(rest.eye_tilt)
                + coef(self.eyes.tilt_from_brows) * f32::midpoint(left_brow.tilt, right_brow.tilt))
            .clamp(-2.0, 2.0),
            pupil: (coef(rest.pupil) * (1.0 - coef(self.eyes.shock_pupil) * shock)).clamp(0.4, 1.6),
            gaze: Vec2::new(
                (coef(self.gaze.x) * look_x).clamp(-1.0, 1.0),
                (coef(self.gaze.y) * look_y).clamp(-1.0, 1.0),
            ),
            vergence: converge.clamp(-1.0, 1.0),
            effects: EyeEffects {
                heart: g(Channel::EYES_HEART),
                star: g(Channel::EYES_STAR),
                gloom: g(Channel::EYES_GLOOM),
                shock,
                squeeze: g(Channel::EYES_SQUEEZE),
                spiral: g(Channel::EYES_SPIRAL),
                sparkle: g(Channel::SPARKLE),
                tears: g(Channel::TEARS),
            },
            left_brow,
            right_brow,
            brow_arch: (coef(rest.brow_arch)
                + coef(self.brows.outer_up_arch) * mid(ch, Channel::BROW_OUTER_UP_LEFT, Channel::BROW_OUTER_UP_RIGHT)
                - coef(self.brows.inner_up_arch) * g(Channel::BROW_INNER_UP))
            .clamp(-1.0, 1.0),
            mouth: self.mouth_params(ch),
            blush: (coef(rest.blush)
                + coef(self.cheeks.puff_blush) * g(Channel::CHEEK_PUFF)
                + coef(self.cheeks.squint_blush) * mid(ch, Channel::CHEEK_SQUINT_LEFT, Channel::CHEEK_SQUINT_RIGHT)
                + coef(self.cheeks.blush) * g(Channel::BLUSH))
            .clamp(0.0, 1.6),
            cheek_puff: g(Channel::CHEEK_PUFF),
            sweat: g(Channel::SWEAT_DROP),
        }
    }

    /// One eye: openness from wide and squint, lower lid from squint and cheek push, closure from blink and arc.
    fn eye(&self, ch: &ChannelBuf, side: Side) -> EyeParams {
        let (blink, arc, wide, squint, cheek) = match side {
            Side::Left => (
                Channel::EYE_BLINK_LEFT,
                Channel::EYE_ARC_LEFT,
                Channel::EYE_WIDE_LEFT,
                Channel::EYE_SQUINT_LEFT,
                Channel::CHEEK_SQUINT_LEFT,
            ),
            Side::Right => (
                Channel::EYE_BLINK_RIGHT,
                Channel::EYE_ARC_RIGHT,
                Channel::EYE_WIDE_RIGHT,
                Channel::EYE_SQUINT_RIGHT,
                Channel::CHEEK_SQUINT_RIGHT,
            ),
        };
        let (rest, map) = (&self.rest, &self.eyes);
        EyeParams {
            open: (coef(rest.eye_open) + coef(map.wide) * ch.get(wide) - coef(map.squint) * ch.get(squint))
                .clamp(0.0, 1.9),
            close: ch.get(blink),
            arc: ch.get(arc),
            lower_lid: unit(
                coef(rest.lower_lid) + coef(map.squint_lid) * ch.get(squint) + coef(map.cheek_lid) * ch.get(cheek),
            ),
        }
    }

    /// One brow: inner-up worries (up and slanted), down crosses, outer-up raises; a sneer pulls the brow down a little.
    fn brow(&self, ch: &ChannelBuf, side: Side) -> BrowParams {
        let (outer, down, sneer) = match side {
            Side::Left => (Channel::BROW_OUTER_UP_LEFT, Channel::BROW_DOWN_LEFT, Channel::NOSE_SNEER_LEFT),
            Side::Right => (Channel::BROW_OUTER_UP_RIGHT, Channel::BROW_DOWN_RIGHT, Channel::NOSE_SNEER_RIGHT),
        };
        let (rest, map) = (&self.rest, &self.brows);
        let (inner, outer, down, sneer) = (ch.get(Channel::BROW_INNER_UP), ch.get(outer), ch.get(down), ch.get(sneer));
        BrowParams {
            height: (coef(rest.brow_height) + coef(map.inner_up) * inner + coef(map.outer_up) * outer
                - coef(map.down) * down
                - coef(map.sneer) * sneer)
                .clamp(-1.5, 1.5),
            tilt: (coef(rest.brow_tilt) - coef(map.inner_up_tilt) * inner
                + coef(map.outer_up_tilt) * outer
                + coef(map.down_tilt) * down)
                .clamp(-2.5, 2.5),
        }
    }

    /// The mouth: jaw, corners (smile against frown, plus dimple), width, shapes and teeth.
    fn mouth_params(&self, ch: &ChannelBuf) -> MouthParams {
        let g = |c: Channel| ch.get(c);
        let (rest, map) = (&self.rest, &self.mouth);
        let corner = |smile, frown, dimple| g(smile) - coef(map.frown) * g(frown) + coef(map.dimple) * g(dimple);
        let s_left = corner(Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_FROWN_LEFT, Channel::MOUTH_DIMPLE_LEFT);
        let s_right = corner(Channel::MOUTH_SMILE_RIGHT, Channel::MOUTH_FROWN_RIGHT, Channel::MOUTH_DIMPLE_RIGHT);
        let s_avg = f32::midpoint(s_left, s_right);
        let jaw = (g(Channel::JAW_OPEN) - coef(map.close_cancel) * g(Channel::MOUTH_CLOSE)).max(0.0);
        // The lips can part without the jaw (an "ooh", bared teeth, a tongue poked out): the strongest of those partings
        // opens the mouth when the jaw opens it less. The kiss below still fades with the jaw alone.
        let parted = (coef(map.part_funnel) * g(Channel::MOUTH_FUNNEL))
            .max(coef(map.part_upper_up) * mid(ch, Channel::MOUTH_UPPER_UP_LEFT, Channel::MOUTH_UPPER_UP_RIGHT))
            .max(coef(map.part_lower_down) * mid(ch, Channel::MOUTH_LOWER_DOWN_LEFT, Channel::MOUTH_LOWER_DOWN_RIGHT))
            .max(coef(map.part_tongue) * g(Channel::TONGUE_OUT));
        let press = (coef(map.press) * g(Channel::MOUTH_PRESS_LEFT).max(g(Channel::MOUTH_PRESS_RIGHT))
            + coef(map.press_roll_lower) * g(Channel::MOUTH_ROLL_LOWER)
            + coef(map.press_roll_upper) * g(Channel::MOUTH_ROLL_UPPER)
            + coef(map.press_close) * g(Channel::MOUTH_CLOSE))
        .clamp(0.0, 1.0);
        let pucker = g(Channel::MOUTH_PUCKER);
        let lower_down = mid(ch, Channel::MOUTH_LOWER_DOWN_LEFT, Channel::MOUTH_LOWER_DOWN_RIGHT);
        MouthParams {
            open: (coef(map.open) * jaw.max(parted)).clamp(0.0, 1.5),
            wide: (coef(rest.mouth_wide)
                + coef(map.stretch) * mid(ch, Channel::MOUTH_STRETCH_LEFT, Channel::MOUTH_STRETCH_RIGHT)
                + coef(map.wide_smile) * s_avg.max(0.0)
                - coef(map.wide_pucker) * pucker)
                .clamp(0.3, 2.0),
            smile_left: (coef(rest.mouth_smile) + coef(map.smile) * s_left).clamp(-2.0, 2.0),
            smile_right: (coef(rest.mouth_smile) + coef(map.smile) * s_right).clamp(-2.0, 2.0),
            round: unit(coef(map.round) * g(Channel::MOUTH_FUNNEL)),
            shift: (coef(map.shift_mouth) * (g(Channel::MOUTH_LEFT) - g(Channel::MOUTH_RIGHT))
                + coef(map.shift_jaw) * (g(Channel::JAW_LEFT) - g(Channel::JAW_RIGHT)))
            .clamp(-1.0, 1.0),
            press,
            // A kiss needs a closed-ish jaw: it fades as the mouth opens.
            pucker: unit((pucker - coef(map.pucker_start)) * coef(map.pucker_gain)) * (1.0 - unit(jaw * 2.0)),
            teeth_upper: unit(
                (mid(ch, Channel::MOUTH_UPPER_UP_LEFT, Channel::MOUTH_UPPER_UP_RIGHT) - coef(map.teeth_start))
                    * coef(map.teeth_gain),
            ),
            teeth_lower: unit((lower_down - coef(map.teeth_lower_start)) * coef(map.teeth_lower_gain))
                * unit((s_avg - coef(map.teeth_lower_smile_start)) * coef(map.teeth_lower_smile_gain)),
            tongue: g(Channel::TONGUE_OUT),
            fangs: g(Channel::FANGS),
        }
    }
}

/// The mean of two channels (the two sides of one shape).
fn mid(ch: &ChannelBuf, left: Channel, right: Channel) -> f32 {
    f32::midpoint(ch.get(left), ch.get(right))
}

/// Which side of the face.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

impl Validate for FaceMapping {
    fn check(&self, c: &mut Check) {
        c.nested("rest", &self.rest);
        let (eyes, brows, mouth, cheeks, gaze) = (&self.eyes, &self.brows, &self.mouth, &self.cheeks, &self.gaze);
        // Gains: non-negative numbers, up to a generous ceiling (a gain of 6 is already a cartoon).
        let gains = [
            ("eyes/wide", eyes.wide),
            ("eyes/squint", eyes.squint),
            ("eyes/squint_lid", eyes.squint_lid),
            ("eyes/cheek_lid", eyes.cheek_lid),
            ("eyes/tilt_from_brows", eyes.tilt_from_brows),
            ("brows/inner_up", brows.inner_up),
            ("brows/outer_up", brows.outer_up),
            ("brows/down", brows.down),
            ("brows/sneer", brows.sneer),
            ("brows/inner_up_tilt", brows.inner_up_tilt),
            ("brows/outer_up_tilt", brows.outer_up_tilt),
            ("brows/down_tilt", brows.down_tilt),
            ("brows/outer_up_arch", brows.outer_up_arch),
            ("brows/inner_up_arch", brows.inner_up_arch),
            ("mouth/open", mouth.open),
            ("mouth/smile", mouth.smile),
            ("mouth/frown", mouth.frown),
            ("mouth/dimple", mouth.dimple),
            ("mouth/stretch", mouth.stretch),
            ("mouth/wide_smile", mouth.wide_smile),
            ("mouth/wide_pucker", mouth.wide_pucker),
            ("mouth/round", mouth.round),
            ("mouth/pucker_gain", mouth.pucker_gain),
            ("mouth/shift_mouth", mouth.shift_mouth),
            ("mouth/shift_jaw", mouth.shift_jaw),
            ("mouth/press", mouth.press),
            ("mouth/press_roll_lower", mouth.press_roll_lower),
            ("mouth/press_roll_upper", mouth.press_roll_upper),
            ("mouth/press_close", mouth.press_close),
            ("mouth/teeth_gain", mouth.teeth_gain),
            ("mouth/teeth_lower_gain", mouth.teeth_lower_gain),
            ("mouth/teeth_lower_smile_gain", mouth.teeth_lower_smile_gain),
            ("cheeks/puff_blush", cheeks.puff_blush),
            ("cheeks/squint_blush", cheeks.squint_blush),
            ("cheeks/blush", cheeks.blush),
            ("gaze/x", gaze.x),
            ("gaze/y", gaze.y),
        ];
        for (name, value) in gains {
            c.range(name, value, 0.0, 6.0);
        }
        // Fractions: dead zones and cancellations are weights on a 0..1 channel.
        let fractions = [
            ("eyes/shock_pupil", eyes.shock_pupil),
            ("mouth/close_cancel", mouth.close_cancel),
            ("mouth/pucker_start", mouth.pucker_start),
            ("mouth/teeth_start", mouth.teeth_start),
            ("mouth/teeth_lower_start", mouth.teeth_lower_start),
            ("mouth/teeth_lower_smile_start", mouth.teeth_lower_smile_start),
            ("mouth/part_funnel", mouth.part_funnel),
            ("mouth/part_upper_up", mouth.part_upper_up),
            ("mouth/part_lower_down", mouth.part_lower_down),
            ("mouth/part_tongue", mouth.part_tongue),
        ];
        for (name, value) in fractions {
            c.range(name, value, 0.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_anim_api::{CHANNELS, Region};
    use proptest::prelude::*;

    fn with(pairs: &[(Channel, f32)]) -> ChannelBuf {
        let mut b = ChannelBuf::NEUTRAL;
        for (c, v) in pairs {
            b.set(*c, *v);
        }
        b
    }

    #[test]
    fn the_paint_parameters_are_small_and_copy() {
        assert_eq!(std::mem::size_of::<PaintParams>(), 164, "update the design doc if this changes");
    }

    #[test]
    fn lips_part_without_the_jaw_and_add_nothing_once_the_jaw_opens_further() {
        let m = FaceMapping::default();
        let parted = [
            &[(Channel::MOUTH_FUNNEL, 1.0)][..],
            &[(Channel::MOUTH_UPPER_UP_LEFT, 1.0), (Channel::MOUTH_UPPER_UP_RIGHT, 1.0)],
            &[(Channel::MOUTH_LOWER_DOWN_LEFT, 1.0), (Channel::MOUTH_LOWER_DOWN_RIGHT, 1.0)],
            &[(Channel::TONGUE_OUT, 1.0)],
        ];
        for pairs in parted {
            let open = m.apply(&with(pairs)).mouth.open;
            // Past the closed line's switch (0.06), so the interior shows, and far short of a talking mouth.
            assert!(open > 0.12 && open < 0.3, "{pairs:?}: {open}");
            let mut jaw = pairs.to_vec();
            jaw.push((Channel::JAW_OPEN, 0.5));
            assert_eq!(m.apply(&with(&jaw)).mouth.open, m.apply(&with(&[(Channel::JAW_OPEN, 0.5)])).mouth.open);
        }
    }

    #[test]
    fn a_neutral_buffer_gives_the_rest_face() {
        let m = FaceMapping::default();
        let p = m.rest_params();
        assert_eq!((p.left_eye.open, p.right_eye.open, p.mouth.open, p.mouth.wide), (1.0, 1.0, 0.0, 1.0));
        assert_eq!((p.left_eye.close, p.mouth.smile_left, p.blush, p.pupil), (0.0, 0.0, 0.0, 1.0));
        let sleepy = FaceMapping {
            rest: RestShape { eye_open: 0.5, mouth_smile: -0.45, blush: 0.75, ..RestShape::default() },
            ..FaceMapping::default()
        };
        let p = sleepy.rest_params();
        assert_eq!((p.left_eye.open, p.mouth.smile_right, p.blush), (0.5, -0.45, 0.75));
    }

    #[test]
    fn every_arkit_channel_and_anime_extra_moves_the_face_except_the_three_unmapped() {
        let m = FaceMapping::default();
        // A few shapes need company to show: the prototype draws lower teeth only on a toothy grin. So a channel counts
        // as alive if it changes the face either alone or on top of a grin.
        let grin = [(Channel::MOUTH_SMILE_LEFT, 0.8), (Channel::MOUTH_SMILE_RIGHT, 0.8), (Channel::JAW_OPEN, 0.6)];
        let (rest, grinning) = (m.rest_params(), m.apply(&with(&grin)));
        let mut dead = Vec::new();
        for info in
            CHANNELS.iter().filter(|c| c.channel.is_arkit() || matches!(c.region, Region::Eyes | Region::Extras))
        {
            let alone = m.apply(&with(&[(info.channel, 1.0)])) != rest;
            let mut on_grin = with(&grin);
            on_grin.set(info.channel, 1.0);
            if !alone && m.apply(&on_grin) == grinning {
                dead.push(info.channel);
            }
        }
        assert_eq!(dead, UNMAPPED, "a channel went dead, or one of the three started to matter");
    }

    #[test]
    fn head_body_and_custom_channels_are_not_the_faces_business() {
        let m = FaceMapping::default();
        let rest = m.rest_params();
        for info in CHANNELS.iter().filter(|c| matches!(c.region, Region::Head | Region::Body | Region::Custom)) {
            assert_eq!(m.apply(&with(&[(info.channel, info.max.min(1.0))])), rest, "{}", info.name);
        }
    }

    #[test]
    fn mirroring_the_bus_mirrors_the_face() {
        let m = FaceMapping::default();
        // A deterministic spread of values over every face channel, different on each side.
        let mut buf = ChannelBuf::NEUTRAL;
        for (i, info) in CHANNELS.iter().enumerate() {
            buf.set(info.channel, f32::from(u8::try_from((i * 37) % 100).unwrap_or(0)) / 140.0);
        }
        let mut mirrored = buf;
        mirrored.mirror();
        let (a, b) = (m.apply(&mirrored), m.apply(&buf).mirrored());
        // The sums that form each parameter run in a different order after mirroring, so allow rounding.
        let close = |x: f32, y: f32| (x - y).abs() < 1e-5;
        assert!(
            close(a.mouth.smile_left, b.mouth.smile_left)
                && close(a.mouth.shift, b.mouth.shift)
                && close(a.gaze.x, b.gaze.x),
            "{a:?} vs {b:?}"
        );
        assert!(
            close(a.left_brow.height, b.left_brow.height)
                && close(a.right_eye.open, b.right_eye.open)
                && close(a.eye_tilt, b.eye_tilt),
            "{a:?} vs {b:?}"
        );
        assert_ne!(m.apply(&buf), m.apply(&buf).mirrored(), "the spread isn't symmetric to begin with");
    }

    #[test]
    fn the_prototypes_smile_and_brow_formulas_hold() {
        let m = FaceMapping::default();
        // A one-sided smirk: the corner lifts 1.35 * 0.8 = 1.08 on its own side only.
        let p = m.apply(&with(&[(Channel::MOUTH_SMILE_LEFT, 0.8)]));
        assert!((p.mouth.smile_left - 1.08).abs() < 1e-5 && p.mouth.smile_right == 0.0);
        // Frown fights smile at 0.8; dimple adds 0.25.
        let p = m.apply(&with(&[
            (Channel::MOUTH_SMILE_RIGHT, 0.5),
            (Channel::MOUTH_FROWN_RIGHT, 0.5),
            (Channel::MOUTH_DIMPLE_RIGHT, 0.4),
        ]));
        assert!((p.mouth.smile_right - 1.35 * (0.5 - 0.4 + 0.1)).abs() < 1e-5);
        // Jaw: 1.15 per unit, `mouthClose` cancels 35% of itself.
        let p = m.apply(&with(&[(Channel::JAW_OPEN, 0.6), (Channel::MOUTH_CLOSE, 0.4)]));
        assert!((p.mouth.open - 1.15 * (0.6 - 0.14)).abs() < 1e-5);
        // Brows: inner-up worries (up and slanted), down crosses.
        let p = m.apply(&with(&[(Channel::BROW_INNER_UP, 1.0)]));
        assert!(p.left_brow.height > 0.5 && p.left_brow.tilt < -1.0 && p.eye_tilt < 0.0 && p.brow_arch < 0.0);
        let p = m.apply(&with(&[(Channel::BROW_DOWN_LEFT, 1.0)]));
        assert!(p.left_brow.height < 0.0 && p.left_brow.tilt > 0.9 && p.right_brow.height == 0.0 && p.eye_tilt > 0.0);
        // A kiss needs a closed jaw.
        let kiss = m.apply(&with(&[(Channel::MOUTH_PUCKER, 1.0)]));
        let open = m.apply(&with(&[(Channel::MOUTH_PUCKER, 1.0), (Channel::JAW_OPEN, 0.8)]));
        assert!(kiss.mouth.pucker > 0.99 && open.mouth.pucker < 0.01, "{} {}", kiss.mouth.pucker, open.mouth.pucker);
    }

    #[test]
    fn gaze_points_toward_the_characters_left_and_convergence_is_separate() {
        let m = FaceMapping::default();
        let mut b = ChannelBuf::NEUTRAL;
        b.set_gaze(1.0, 0.0);
        let p = m.apply(&b);
        assert!(
            (p.gaze.x - 0.9).abs() < 1e-6 && p.gaze.y == 0.0 && p.vergence == 0.0,
            "parallel gaze doesn't converge: {p:?}"
        );
        b.set_gaze(0.0, -1.0);
        assert!((m.apply(&b).gaze.y - -0.8).abs() < 1e-6);
        let cross = m.apply(&with(&[(Channel::EYE_LOOK_IN_LEFT, 0.6), (Channel::EYE_LOOK_IN_RIGHT, 0.6)]));
        assert!((cross.vergence - 0.6).abs() < 1e-6 && cross.gaze.x == 0.0, "both eyes in: converge, no sideways look");
    }

    #[test]
    fn closed_eyes_arcs_and_wide_eyes() {
        let m = FaceMapping::default();
        let p = m.apply(&with(&[
            (Channel::EYE_ARC_LEFT, 1.0),
            (Channel::EYE_BLINK_RIGHT, 1.0),
            (Channel::EYE_WIDE_LEFT, 1.0),
        ]));
        assert_eq!((p.left_eye.arc, p.left_eye.close, p.right_eye.arc, p.right_eye.close), (1.0, 0.0, 0.0, 1.0));
        assert!((p.left_eye.open - 1.65).abs() < 1e-6, "wide adds 0.65");
        let squint = m.apply(&with(&[(Channel::EYE_SQUINT_RIGHT, 1.0)]));
        assert!((squint.right_eye.open - 0.55).abs() < 1e-6 && (squint.right_eye.lower_lid - 0.35).abs() < 1e-6);
    }

    #[test]
    fn outputs_stay_inside_their_ranges_even_for_wild_input() {
        let m = FaceMapping::default();
        let mut b = ChannelBuf::NEUTRAL;
        for info in CHANNELS {
            b.set(info.channel, 9.0);
        }
        let p = m.apply(&b);
        assert!(p.left_eye.open <= 1.9 && p.mouth.open <= 1.5 && p.mouth.wide <= 2.0 && p.blush <= 1.6);
        assert!(p.mouth.smile_left.abs() <= 2.0 && p.left_brow.height.abs() <= 1.5 && p.gaze.x.abs() <= 1.0);
        assert!((0.0..=1.0).contains(&p.mouth.press) && (0.0..=1.0).contains(&p.mouth.teeth_upper));
    }

    /// Asserts every output range the field docs promise, for one frame of paint parameters.
    fn assert_documented_ranges(p: &PaintParams) {
        let within = |x: f32, lo: f32, hi: f32| (lo..=hi).contains(&x);
        for eye in [p.left_eye, p.right_eye] {
            assert!(within(eye.open, 0.0, 1.9) && within(eye.close, 0.0, 1.0), "{eye:?}");
            assert!(within(eye.arc, 0.0, 1.0) && within(eye.lower_lid, 0.0, 1.0), "{eye:?}");
        }
        assert!(
            within(p.eye_tilt, -2.0, 2.0) && within(p.pupil, 0.4, 1.6),
            "eye_tilt {} pupil {}",
            p.eye_tilt,
            p.pupil
        );
        assert!(within(p.gaze.x, -1.0, 1.0) && within(p.gaze.y, -1.0, 1.0) && within(p.vergence, -1.0, 1.0));
        let e = p.effects;
        for v in [e.heart, e.star, e.gloom, e.shock, e.squeeze, e.spiral, e.sparkle, e.tears] {
            assert!(within(v, 0.0, 1.0), "effect {v}");
        }
        for brow in [p.left_brow, p.right_brow] {
            assert!(within(brow.height, -1.5, 1.5) && within(brow.tilt, -2.5, 2.5), "{brow:?}");
        }
        assert!(within(p.brow_arch, -1.0, 1.0), "brow_arch {}", p.brow_arch);
        let m = p.mouth;
        assert!(within(m.open, 0.0, 1.5) && within(m.wide, 0.3, 2.0), "{m:?}");
        assert!(within(m.smile_left, -2.0, 2.0) && within(m.smile_right, -2.0, 2.0), "{m:?}");
        assert!(within(m.shift, -1.0, 1.0), "{m:?}");
        for v in [m.round, m.press, m.pucker, m.teeth_upper, m.teeth_lower, m.tongue, m.fangs] {
            assert!(within(v, 0.0, 1.0), "mouth {m:?}");
        }
        assert!(within(p.blush, 0.0, 1.6) && within(p.cheek_puff, 0.0, 1.0) && within(p.sweat, 0.0, 1.0), "{p:?}");
    }

    proptest! {
        /// Every channel at once, each drawn from -10..10 or NaN: whatever the bus holds, every output stays in its
        /// documented range. The channel reads sanitise each value to its own declared range.
        #[test]
        fn every_output_stays_in_its_documented_range_for_any_bus(
            values in proptest::collection::vec(prop_oneof![-10.0f32..10.0, Just(f32::NAN)], CHANNELS.len()),
        ) {
            let mut b = ChannelBuf::NEUTRAL;
            for (info, v) in CHANNELS.iter().zip(values) {
                b.set(info.channel, v);
            }
            assert_documented_ranges(&FaceMapping::default().apply(&b));
        }
    }

    #[test]
    fn a_nan_channel_reads_as_neutral() {
        let m = FaceMapping::default();
        let mut nan = ChannelBuf::NEUTRAL;
        nan.set(Channel::JAW_OPEN, f32::NAN);
        nan.set(Channel::EYE_LOOK_IN_LEFT, f32::NAN);
        assert_eq!(m.apply(&nan), m.rest_params());
    }

    #[test]
    fn press_stays_in_its_range_when_the_closing_channels_go_negative() {
        let m = FaceMapping::default();
        for value in [-1.7, -2.0, 3.0] {
            for channel in [Channel::MOUTH_CLOSE, Channel::MOUTH_ROLL_LOWER] {
                let p = m.apply(&with(&[(channel, value)])).mouth;
                assert!((0.0..=1.0).contains(&p.press), "press {} for {} at {value}", p.press, channel.name());
                assert!((0.0..=1.0).contains(&p.teeth_upper), "teeth_upper for {} at {value}", channel.name());
                assert!((0.0..=1.0).contains(&p.teeth_lower), "teeth_lower for {} at {value}", channel.name());
                assert!((0.0..=1.0).contains(&p.pucker) && (0.0..=1.0).contains(&p.round), "{}", channel.name());
                assert!((0.0..=1.5).contains(&p.open) && (0.3..=2.0).contains(&p.wide), "{}", channel.name());
            }
        }
    }

    #[test]
    fn parameters_are_f64_so_files_and_agents_see_the_numbers_they_wrote() {
        let mut mapping = FaceMapping::default();
        mapping.mouth.smile = 0.6;
        assert!(
            serde_json::to_string(&mapping).unwrap().contains(r#""smile":0.6,"#),
            "no f32 widening noise like 0.6000000238418579"
        );
        let values = crate::expression::ChannelValues::default().with(Channel::JAW_OPEN, 0.6);
        assert_eq!(serde_json::to_string(&values).unwrap(), r#"{"jawOpen":0.6}"#);
    }

    #[test]
    fn the_mapping_is_data_a_profile_can_edit() {
        let m = FaceMapping::default();
        m.validate().unwrap();
        let mut animated = m.clone();
        animated.mouth.smile = 2.0;
        let b = with(&[(Channel::MOUTH_SMILE_LEFT, 0.5)]);
        assert!(animated.apply(&b).mouth.smile_left > m.apply(&b).mouth.smile_left, "exaggeration is a coefficient");
        let back: FaceMapping = serde_json::from_str(&serde_json::to_string(&animated).unwrap()).unwrap();
        assert_eq!(back, animated);
        assert!(serde_json::from_str::<FaceMapping>(r#"{"mouth":{"smyle":1}}"#).is_err());
        let mut bad = m;
        bad.eyes.shock_pupil = 3.0;
        assert_eq!(bad.validate().unwrap_err().0[0].path, "eyes/shock_pupil");
    }
}
