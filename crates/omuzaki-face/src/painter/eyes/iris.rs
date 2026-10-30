//! The iris, its lit crescent, the pupil, the ring and the lid's shadow (the `eye()` body of `face.js`, lines 141-161).
//!
//! Everything here is drawn inside the aperture clip that [`super::paint`] brackets, so an iris that travels past the
//! lids (an extreme gaze) is cut by them with no work of its own. The catchlights and the shimmer on top of the iris
//! are [`super::highlight`]; they share [`Iris`], the iris ellipse of one eye.
//!
//! # From the JS constants to the profile
//!
//! | Profile | Replaces (JS) |
//! |---|---|
//! | `iris.width`, `iris.height` | `0.64`, `0.84` (radii in eye half-widths) |
//! | `iris.inset`, `iris.drop` | `0.10` toward the nose, `0.07` below the eye's centre |
//! | `iris.gaze_travel` | `0.026`, `0.013` per look unit (times the eye scale) |
//! | `iris.top`, `mid`, `low` | `C.irisTop`, `irisMid`, `irisLow`; the extra `#b4cfa2` stop is `low` lightened by [`LIGHTEST`] |
//! | `iris.ring` | the ring's colour (`rgba(30,60,48,0.55)`; its alpha is the colour's own) |
//! | `iris.pupil` | the pupil's colour (`rgba(24,52,42,0.55)`), shape and `size` |
//! | `eyes.lid_shadow` | the `0.55` opacity of the lid's shadow band |
//!
//! The prototype's camera-follow gaze and driven gaze are already one vector in [`PaintParams::gaze`] (the mapping adds
//! them), so the iris moves by `gaze * gaze_travel * eye scale`. Convergence, [`PaintParams::vergence`], moves each iris
//! toward its own nose by `vergence * gaze_travel[0] * eye scale`: the same look units as gaze x, mirrored per side. The
//! crescent's colour is `low` lightened by [`CRESCENT_LIGHTEN`], so a character with another iris colour gets a
//! crescent of its own hue.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{
    Cmd, DrawList, FaceLayerId, FaceProfile, IrisPaint, Paint, PaintParams, PathRef, PupilShape, Stops,
};

use super::FALLBACK_COLOUR;
use super::Side;
use super::aperture::Eye;
use super::detail;
use super::highlight;
use crate::painter::PaintCtx;
use crate::painter::fixed_colour;

// ---- the prototype's constants that the profile does not parametrise

/// A full stare (`eyesStare`) shrinks the iris to this share of its size: a small pinned iris in a wide white.
pub(super) const STARE_IRIS: f32 = 0.52;

/// The colours and pupil the iris of the eye on `side` paints with: the right eye's own when the profile gives the
/// right eye a different iris (heterochromia).
pub(super) fn colours(profile: &FaceProfile, side: Side) -> IrisPaint<'_> {
    profile.eyes.iris_paint(side == Side::Right)
}

/// How far `low` is lightened toward white for the gradient's last stop (`#b4cfa2` against `#a8c99c`).
const LIGHTEST: f32 = 0.1;
/// How far `low` is lightened toward white for the crescent (`rgba(206,228,184,0.35)` against `#a8c99c`).
const CRESCENT_LIGHTEN: f32 = 0.4;
/// The crescent's opacity.
const CRESCENT_OPACITY: f32 = 0.35;
/// The crescent's centre below the iris centre, in iris half-heights.
const CRESCENT_DROP: f32 = 0.62;
/// The crescent's radii, as fractions of the iris radii.
const CRESCENT_RADII: Vec2 = Vec2::new(0.72, 0.32);
/// The gradient's third stop (`0.85`): where `low` is reached.
const LOW_STOP: f32 = 0.85;
/// The pupil sits this far above the iris centre, in iris half-heights (`0.15`).
const PUPIL_RISE: f32 = 0.15;
/// The round pupil's radii as fractions of the iris radii (`0.24`, `0.34`).
const PUPIL_RADII: Vec2 = Vec2::new(0.24, 0.34);
/// The smallest pupil scale the `pupil` channel can reach (`Math.max(0.6, P.pupil)`).
const PUPIL_MIN_SCALE: f32 = 0.6;
/// A slit pupil's half-width and half-height as fractions of the iris radii (at scale 1).
const SLIT_RADII: Vec2 = Vec2::new(0.10, 0.55);
/// A heart pupil's radius as a fraction of the iris half-width (at scale 1).
const HEART_PUPIL_RADIUS: f32 = 0.30;
/// The ring's width in metres (`0.0007`).
const RING_WIDTH: f32 = 0.0007;
/// The lid shadow: colour, how far below the lid it fades out, and how far the band reaches past the eye each side
/// (in eye half-widths), and how far above the lid it starts, in prototype pixels.
const SHADOW_COLOUR: (u8, u8, u8) = (40, 30, 44);
const SHADOW_FADE: f32 = 0.016;
const SHADOW_HALF_WIDTH: f32 = 1.3;
const SHADOW_BAND: f32 = 0.02;
const SHADOW_ABOVE_PX: f32 = 2.0;

/// `colour` mixed toward white by `t`, alpha kept.
pub(super) fn lighten(colour: Colour, t: f32) -> Colour {
    let mix = |c: u8| {
        let c = f32::from(c);
        // Rounded and within 0..=255, so the cast keeps the value.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let out = (c + (255.0 - c) * t).round() as u8;
        out
    };
    Colour::rgba(mix(colour.r), mix(colour.g), mix(colour.b), colour.a)
}

/// One eye's iris ellipse on the face plane.
#[derive(Copy, Clone, Debug)]
pub(super) struct Iris {
    /// Centre.
    pub centre: Vec2,
    /// Half-width in metres.
    pub rx: f32,
    /// Half-height in metres.
    pub ry: f32,
}

impl Iris {
    /// The iris of `eye`: set toward the nose and below the eye's centre, then moved by the gaze and by the
    /// convergence, and shrunk by a stare.
    #[allow(clippy::cast_possible_truncation)] // the profile is f64 for the file's sake; the painter works in f32
    pub(super) fn new(profile: &FaceProfile, params: &PaintParams, eye: &Eye) -> Self {
        let style = &profile.eyes;
        let iris = &style.iris;
        let rest = eye.point(-(iris.inset as f32), -(iris.drop as f32));
        let travel = Vec2::new(iris.gaze_travel[0] as f32, iris.gaze_travel[1] as f32) * style.scale as f32;
        // Toward the nose on this side: the eye's own mirror, so the right eye converges by +x.
        let nose = (eye.point(-1.0, 0.0) - eye.point(0.0, 0.0)).normalize_or_zero();
        let stare = 1.0 - (1.0 - STARE_IRIS) * params.effects.stare.clamp(0.0, 1.0);
        Self {
            centre: rest + params.gaze * travel + nose * params.vergence * travel.x,
            rx: iris.width as f32 * eye.width() * stare,
            ry: iris.height as f32 * eye.width() * stare,
        }
    }

    /// A point `(x, y)` iris radii from the centre, `y` up.
    pub(super) fn at(&self, x: f32, y: f32) -> Vec2 {
        self.centre + Vec2::new(x * self.rx, y * self.ry)
    }

    /// The outline as a path.
    pub(super) fn outline(&self, list: &mut DrawList) -> PathRef {
        list.path().ellipse(self.centre, self.rx, self.ry).finish()
    }
}

/// Paints the iris, crescent, pupil, ring and lid shadow of the eye on `side`. Draws nothing for a shut eye.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let (profile, params) = (ctx.profile, ctx.params);
    let eye = Eye::new(profile, params, side);
    if !eye.is_open() {
        return;
    }
    let iris = Iris::new(profile, params, &eye);
    let style = colours(profile, side);
    let outline = iris.outline(ctx.list(FaceLayerId::Base));

    let (top, mid, low) = (
        fixed_colour(style.top, FALLBACK_COLOUR),
        fixed_colour(style.mid, FALLBACK_COLOUR),
        fixed_colour(style.low, FALLBACK_COLOUR),
    );
    let stops = Stops::new(&[(0.0, top), (0.5, mid), (LOW_STOP, low), (1.0, lighten(low, LIGHTEST))]);
    let gradient = Paint::Linear { from: iris.at(0.0, 1.0), to: iris.at(0.0, -1.0), stops };
    ctx.list(FaceLayerId::Base).fill(outline, gradient);

    // The detail, the crescent and the pupil are cut by the iris.
    ctx.list(FaceLayerId::Base).push(Cmd::PushClip(outline));
    let extent = pupil_extent(ctx, &iris, side);
    detail::paint(ctx, &iris, side, low, extent);
    crescent(ctx.list(FaceLayerId::Base), &iris, lighten(low, CRESCENT_LIGHTEN));
    pupil(ctx, &iris, side);
    ctx.list(FaceLayerId::Base).push(Cmd::PopClip);

    let ring = fixed_colour(style.ring, FALLBACK_COLOUR);
    ctx.list(FaceLayerId::Base).stroke(outline, Paint::Solid(ring), RING_WIDTH);
    lid_shadow(ctx, &eye);
}

/// The light crescent at the bottom of the iris.
fn crescent(list: &mut DrawList, iris: &Iris, colour: Colour) {
    let path = list
        .path()
        .ellipse(iris.at(0.0, -CRESCENT_DROP), iris.rx * CRESCENT_RADII.x, iris.ry * CRESCENT_RADII.y)
        .finish();
    list.push(Cmd::Fill { path, paint: Paint::Solid(colour), opacity: CRESCENT_OPACITY });
}

/// The pupil's centre and radii, whatever its shape (the round pupil's; a slit or a heart is about as wide): what
/// the glow round it fits to.
#[allow(clippy::cast_possible_truncation)] // see `Iris::new`
fn pupil_extent(ctx: &PaintCtx<'_>, iris: &Iris, side: Side) -> (Vec2, Vec2) {
    let style = colours(ctx.profile, side).pupil;
    let scale = ctx.params.pupil.max(PUPIL_MIN_SCALE) * style.size as f32;
    let radii = match style.shape {
        PupilShape::None => Vec2::ZERO,
        _ => Vec2::new(iris.rx * PUPIL_RADII.x, iris.ry * PUPIL_RADII.y) * scale,
    };
    (iris.at(0.0, PUPIL_RISE), radii)
}

/// The pupil: the profile's shape and colour, scaled by its `size` and the `pupil` channel (never below
/// [`PUPIL_MIN_SCALE`] for the channel, as in the prototype).
#[allow(clippy::cast_possible_truncation)] // see `Iris::new`
fn pupil(ctx: &mut PaintCtx<'_>, iris: &Iris, side: Side) {
    let style = colours(ctx.profile, side).pupil;
    let scale = ctx.params.pupil.max(PUPIL_MIN_SCALE) * style.size as f32;
    let paint = Paint::Solid(fixed_colour(&style.colour, FALLBACK_COLOUR));
    let centre = iris.at(0.0, PUPIL_RISE);
    let list = ctx.list(FaceLayerId::Base);
    let path = match style.shape {
        PupilShape::None => return,
        PupilShape::Round => {
            list.path().ellipse(centre, iris.rx * PUPIL_RADII.x * scale, iris.ry * PUPIL_RADII.y * scale).finish()
        }
        PupilShape::Slit => {
            // A lens: two quadratics whose control points sit at twice the half-width.
            let (w, h) = (iris.rx * SLIT_RADII.x * scale, iris.ry * SLIT_RADII.y * scale);
            let (top, bottom) = (centre + Vec2::new(0.0, h), centre - Vec2::new(0.0, h));
            list.path()
                .move_to(top)
                .quad_to(centre + Vec2::new(2.0 * w, 0.0), bottom)
                .quad_to(centre - Vec2::new(2.0 * w, 0.0), top)
                .close()
                .finish()
        }
        PupilShape::Heart => highlight::heart(list, centre, iris.rx * HEART_PUPIL_RADIUS * scale),
    };
    list.fill(path, paint);
}

/// The band of shadow the upper lid casts over the top of the iris and the white: from just above the lid at the
/// eye's middle, fading out over [`SHADOW_FADE`] metres. Level whatever the lid's tilt, as in the prototype.
fn lid_shadow(ctx: &mut PaintCtx<'_>, eye: &Eye) {
    #[allow(clippy::cast_possible_truncation)] // clamped to 0..=1, far inside f32 range
    let opacity = ctx.profile.eyes.lid_shadow.clamp(0.0, 1.0) as f32;
    let above = ctx.metres_from_px(SHADOW_ABOVE_PX);
    let lid = eye.point(0.0, eye.top(0.0));
    let half = SHADOW_HALF_WIDTH * eye.width();
    let (r, g, b) = SHADOW_COLOUR;
    let stops = Stops::new(&[(0.0, Colour::rgb(r, g, b)), (1.0, Colour::rgba(r, g, b, 0))]);
    let paint = Paint::Linear { from: lid, to: lid - Vec2::new(0.0, SHADOW_FADE), stops };
    let list = ctx.list(FaceLayerId::Base);
    let (y0, y1) = (lid.y + above, lid.y + above - SHADOW_BAND);
    let band = list
        .path()
        .polyline(&[
            Vec2::new(lid.x - half, y0),
            Vec2::new(lid.x + half, y0),
            Vec2::new(lid.x + half, y1),
            Vec2::new(lid.x - half, y1),
        ])
        .close()
        .finish();
    list.push(Cmd::Fill { path: band, paint, opacity });
}
