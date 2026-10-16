//! Glasses (CB-43): `glasses`, four frames (round, square, cat-eye, sunglasses) with see-through lenses, a bridge and
//! temple arms that reach the ears.
//!
//! **Build.** Each lens is placed from the eye layout: the eye line `eye_y` and the half distance between the eyes
//! `eye_spacing`, both head-local as the face layout gives them. The face layout (and the lens outline, which is sized to
//! the face) is in the painter's units, the template's head; the head was scaled to the character's real size, so both
//! are scaled by the same factor ([`Published::scale`](omuzaki_char_api::Published::scale)) to land on the painted
//! eyes. The lens centre stands `gap` metres off the head's surface in front of its eye, and the lens plane is tangent
//! to the surface there, turned a little towards the temple so it wraps round the head. Per side: a rim (a tube round the
//! lens outline), the lens (a thin closed disc in a blended material), and a temple arm, a tube from the rim's outer edge
//! back along the head, `arm_clearance` off its surface, to the ear's root, where the head's `ear_l` or `ear_r` anchor is
//! (a head with no side ear has none: the arm then ends on the side of the head at the eye line). The bridge joins the
//! two rims.
//!
//! **The eye layout** is the character's face profile (`eye_y`, `eye_spacing`, `brow_gap`), read from the build context, so
//! the glasses follow an edited layout. `eye_y` and `eye_spacing` override it. A frame that would reach the brow, or whose
//! rims would touch, is a build error naming the layout.
//!
//! **Both sides.** Each side is generated with its own sign `s` (`+1` the character's left, `-1` the right): the pair is
//! the exact mirror image of one another.
//!
//! Skinned rigidly to the head joint. With an `attach` the whole piece moves as one.

use std::f64::consts::TAU;

use omuzaki_char_api::{
    AnchorsFact, BuildCtx, BuildError, HeadSurface, PartCategory, PartKind, PartMeta, PartOutput, RecolourCtx,
    Recoloured, RigContribution,
};
use omuzaki_core::glam::{DQuat, DVec3};
use omuzaki_face_api::{Check, ColourRef, FaceLayout, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::rigid::{Detail, kit_error};
use super::shells::{Shells, Slot, closed_tube, head_frames, on_head, recolour};
use crate::kit::KitError;
use crate::kit::curve::{fraction, lerp, sstep};
use crate::kit::grid::Grid;

/// The style of the frames.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GlassesStyle {
    /// Round rims.
    #[default]
    Round,
    /// Square rims with rounded corners.
    Square,
    /// Rims lifted at the outer corners.
    CatEye,
    /// Large, dark lenses.
    Sunglasses,
}

/// What a style is when the parameters leave it open: half width, half height, the superellipse exponent of the outline
/// (2 is an ellipse, more is squarer), the lift of the outer corner (as a fraction of the half height) and the lens coverage.
struct Outline {
    half_width: f64,
    half_height: f64,
    exponent: f64,
    lift: f64,
    coverage: f64,
}

impl GlassesStyle {
    fn outline(self) -> Outline {
        match self {
            Self::Round => Outline { half_width: 0.046, half_height: 0.046, exponent: 2.0, lift: 0.0, coverage: 0.25 },
            Self::Square => Outline { half_width: 0.05, half_height: 0.04, exponent: 4.0, lift: 0.0, coverage: 0.25 },
            Self::CatEye => Outline { half_width: 0.052, half_height: 0.036, exponent: 3.0, lift: 0.45, coverage: 0.3 },
            Self::Sunglasses => {
                Outline { half_width: 0.056, half_height: 0.046, exponent: 3.0, lift: 0.0, coverage: 0.85 }
            }
        }
    }
}

/// Glasses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GlassesParams {
    /// The frame style.
    pub style: GlassesStyle,
    /// The eye line, head-local, in the face layout's units (negative is below the head centre). Left out: the
    /// character's face layout's `eye_y`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eye_y: Option<f64>,
    /// Half the distance between the eye centres, in the face layout's units. Left out: the face layout's `eye_spacing`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eye_spacing: Option<f64>,
    /// Scale of the lenses' outline, `1` for the style's own size (sized to the face, so it grows and shrinks with the
    /// head).
    pub size: f64,
    /// How far the lens stands off the head's surface, metres.
    pub gap: f64,
    /// Radius of the rim's wire, metres.
    pub rim: f64,
    /// How far the arms stand off the head on their way to the ears, metres.
    pub arm_clearance: f64,
    /// How much of the light the lens blocks, `0..=1`. Left out: the style's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lens_coverage: Option<f64>,
    /// The frame colour.
    pub frame: ColourRef,
    /// The lens tint.
    pub lens: ColourRef,
}

impl Default for GlassesParams {
    fn default() -> Self {
        Self {
            style: GlassesStyle::Round,
            eye_y: None,
            eye_spacing: None,
            size: 1.0,
            gap: 0.01,
            rim: 0.0035,
            arm_clearance: 0.006,
            lens_coverage: None,
            frame: ColourRef::rgb(0x2a, 0x26, 0x34),
            lens: ColourRef::rgb(0x9c, 0xc8, 0xe8),
        }
    }
}

impl Validate for GlassesParams {
    fn check(&self, c: &mut Check) {
        if let Some(eye_y) = self.eye_y {
            c.range("eye_y", eye_y, -0.3, 0.3);
        }
        if let Some(spacing) = self.eye_spacing {
            c.range("eye_spacing", spacing, 0.03, 0.2);
        }
        c.range("size", self.size, 0.5, 1.6);
        c.range("gap", self.gap, 0.008, 0.05);
        c.range("rim", self.rim, 0.0015, 0.01);
        c.range("arm_clearance", self.arm_clearance, 0.001, 0.02);
        if let Some(coverage) = self.lens_coverage {
            c.range("lens_coverage", coverage, 0.0, 1.0);
        }
    }
}

/// Where the eyes are, in metres from the head centre: the parameters' own, else the character's face layout, scaled
/// from the face layout's units by the head's scale.
#[derive(Copy, Clone)]
struct Eyes {
    /// The eye line, head-local.
    y: f64,
    /// Half the distance between the eyes.
    spacing: f64,
    /// The brow's height over the eye line the glasses use: the frames never reach it.
    brow_gap: f64,
    /// Metres per unit of the face layout: the scale the head was built at.
    scale: f64,
}

impl Eyes {
    fn of(p: &GlassesParams, layout: &FaceLayout, scale: f64) -> Self {
        let y = p.eye_y.unwrap_or(layout.eye_y);
        // The brow stays where the layout puts it, whatever eye line the glasses are given.
        let brow_gap = layout.eye_y + layout.brow_gap - y;
        Self {
            y: y * scale,
            spacing: p.eye_spacing.unwrap_or(layout.eye_spacing) * scale,
            brow_gap: brow_gap * scale,
            scale,
        }
    }

    /// Whether the frames fit this layout: below the brow, and with the two rims apart.
    fn check(&self, p: &GlassesParams) -> Result<(), String> {
        let o = p.style.outline();
        let top = o.half_height * p.size * self.scale * (1.0 + o.lift) + p.rim;
        if top > self.brow_gap - BROW_MARGIN * self.scale {
            return Err(format!(
                "the frames reach {:.1} cm over the eye line, into the brow {:.1} cm up: lower the size",
                top * 100.0,
                self.brow_gap * 100.0
            ));
        }
        if self.spacing - o.half_width * p.size * self.scale - p.rim < RIM_SEPARATION {
            return Err("the two rims would touch each other: lower the size or widen eye_spacing".to_owned());
        }
        Ok(())
    }
}

/// The point `q` moved radially out from the head's centre until it stands at least `offset` metres off the surface.
fn stand_off(head: &dyn HeadSurface, q: DVec3, offset: f64) -> DVec3 {
    let centre = DVec3::from(head.centre());
    let from_centre = q - centre;
    let dir = from_centre.normalize_or_zero();
    centre + dir * from_centre.length().max(f64::from(head.radius(dir.as_vec3())) + offset)
}

/// The `glasses` kind.
pub struct Glasses;

/// A frame's geometry for one side, placed in bind space.
struct Eye {
    /// The side: `+1` the character's left, `-1` the right.
    s: f64,
    /// The lens centre.
    centre: DVec3,
    /// Across the lens, towards the character's left.
    side: DVec3,
    /// Up the lens.
    up: DVec3,
    /// Out of the face.
    front: DVec3,
    /// Metres per unit of the face layout, which the lens outline is sized in.
    scale: f64,
}

/// The least the surface at an eye may face forward (the cosine of its turn away from the face's axis): a lens on a
/// surface turned further away than this still stands only `gap / MIN_FACING` in front of it.
const MIN_FACING: f64 = 0.5;
/// How far each lens turns towards its temple, as a share of the angle its half width spans on the head.
const WRAP_SHARE: f64 = 0.5;
/// How far under the brow the frames stop, in the face layout's units.
const BROW_MARGIN: f64 = 0.008;
/// The least gap between the two rims' wires, metres.
const RIM_SEPARATION: f64 = 0.004;
/// How far from the face the rim's wire stays at the nearest, metres (the acceptance is 6 mm).
const MIN_FACE_GAP: f64 = 0.007;
/// The angle step the surface normal is measured over, radians.
const NORMAL_STEP: f64 = 0.02;
/// The radius of the bridge's wire, metres.
const BRIDGE_WIRE: f64 = 0.0028;
/// The radius of a temple arm's wire, metres.
const ARM_WIRE: f64 = 0.0028;
/// How many points the rim's outline has.
const OUTLINE_POINTS: usize = 24;

impl Glasses {
    /// The place and frame of the lens of the character's left eye, in bind space.
    #[allow(clippy::many_single_char_names)] // the geometry formulas use the textbook names
    fn left_eye(head: &dyn HeadSurface, p: &GlassesParams, eyes: Eyes) -> Eye {
        let s = 1.0;
        let centre = DVec3::from(head.centre());
        let radius = f64::from(head.radius(omuzaki_core::glam::Vec3::Z));
        let (x, y) = (s * eyes.spacing, eyes.y);
        // The direction from the head centre to the surface in front of the eye: its depth follows the radius there.
        let mut z = (radius * radius - x * x - y * y).max(0.0).sqrt();
        let mut dir = DVec3::new(x, y, z).normalize_or_zero();
        for _ in 0..4 {
            z = (f64::from(head.radius(dir.as_vec3())).powi(2) - x * x - y * y).max(0.0).sqrt();
            dir = DVec3::new(x, y, z).normalize_or_zero();
        }
        let surface = on_head(head, dir, 0.0);
        // The surface's own normal at the eye (the head is not a sphere: the radial direction can lean off it).
        let across = dir.cross(DVec3::Y).normalize_or_zero();
        let down = across.cross(dir).normalize_or_zero();
        let step = |toward: DVec3| on_head(head, (dir + toward * NORMAL_STEP).normalize_or_zero(), 0.0) - surface;
        let mut front = step(across).cross(step(down)).normalize_or_zero();
        if front.dot(dir) < 0.0 {
            front = -front;
        }
        let up = (DVec3::Y - front * DVec3::Y.dot(front)).normalize_or_zero();
        // The lens stands in front of the eye as the face is painted, straight along the face's forward axis (the
        // painted face is projected from the front), `gap` off the surface along its normal.
        let centre_at = surface + DVec3::Z * (p.gap / front.z.max(MIN_FACING));
        // Wrap: the lens turns about its upright axis towards the temple by half the angle its half width spans on the
        // head, so its outer edge follows the head round instead of standing off where the face curves away (seen from
        // three-quarters, the far lens shows past the head's outline only by about its distance off the face).
        let o = p.style.outline();
        let half_width = o.half_width * p.size * eyes.scale;
        let wrap = DQuat::from_axis_angle(up, WRAP_SHARE * half_width / (surface - centre).length().max(half_width));
        let front = wrap * front;
        let side = up.cross(front);
        let mut eye = Eye { s, centre: centre_at, side, up, front, scale: eyes.scale };
        // The lens plane is near tangent at the eye, but the face curves away unevenly: move the lens out until the nearest point
        // of the rim's wire is at least `MIN_FACE_GAP` from the face.
        let nearest = (0..OUTLINE_POINTS)
            .map(|k| {
                let at = Self::at(&eye, Self::outline(p, &eye, TAU * fraction(k, OUTLINE_POINTS)));
                let from_centre = at - centre;
                from_centre.length() - f64::from(head.radius(from_centre.normalize_or_zero().as_vec3())) - p.rim
            })
            .fold(f64::MAX, f64::min);
        eye.centre += eye.front * (MIN_FACE_GAP - nearest).max(0.0);
        eye
    }

    /// The right eye's frame: the left one's mirror image across the head's centre plane, so the pair is exactly
    /// symmetric whatever the head's own rounding does.
    fn mirrored(left: &Eye, centre_x: f64) -> Eye {
        let flip = |v: DVec3| DVec3::new(-v.x, v.y, v.z);
        let (up, front) = (flip(left.up), flip(left.front));
        let centre = DVec3::new(2.0 * centre_x - left.centre.x, left.centre.y, left.centre.z);
        Eye { s: -1.0, centre, side: up.cross(front), up, front, scale: left.scale }
    }

    /// The outline of a lens of `p` in the plane of `eye`, metres: `(across, up)` at angle `theta`.
    #[allow(clippy::many_single_char_names)] // the geometry formulas use the textbook names
    fn outline(p: &GlassesParams, eye: &Eye, theta: f64) -> (f64, f64) {
        let (s, scale) = (eye.s, eye.scale);
        let o = p.style.outline();
        let power = |t: f64| t.signum() * t.abs().powf(2.0 / o.exponent);
        let (a, b) = (o.half_width * p.size * scale, o.half_height * p.size * scale);
        let (u, v) = (a * power(theta.cos()), b * power(theta.sin()));
        // The outer upper corner lifts: the outer side is the one away from the nose.
        let outer = (s * u / a).max(0.0);
        let v = if v > 0.0 { v * (1.0 + o.lift * outer) } else { v };
        (u, v)
    }

    fn at(eye: &Eye, (u, v): (f64, f64)) -> DVec3 {
        eye.centre + eye.side * u + eye.up * v
    }

    /// The rim: a tube round the outline.
    fn rim(p: &GlassesParams, eye: &Eye, seg: usize) -> Result<Grid, KitError> {
        let curve: Vec<DVec3> = (0..OUTLINE_POINTS)
            .map(|k| Self::at(eye, Self::outline(p, eye, TAU * fraction(k, OUTLINE_POINTS))))
            .collect();
        let n = curve.len();
        Grid::from_fn(n + 1, seg + 1, |i, j| {
            let k = i % n;
            let tangent = (curve[(k + 1) % n] - curve[(k + n - 1) % n]).normalize_or_zero();
            let outward = tangent.cross(eye.front).normalize_or_zero();
            let phi = TAU * fraction(j, seg);
            curve[k] + (outward * phi.cos() + eye.front * phi.sin()) * p.rim
        })
    }

    /// The lens: a thin closed disc filling the outline, a little domed.
    fn lens(p: &GlassesParams, eye: &Eye) -> Result<(Grid, Vec<DVec3>), KitError> {
        const RINGS: usize = 2;
        const HALF_THICK: f64 = 0.0007;
        let inner = |theta: f64, t: f64, side: f64| {
            let (u, v) = Self::outline(p, eye, theta);
            let dome = 0.0015 * (1.0 - t * t);
            Self::at(eye, (u * t, v * t)) + eye.front * (side * HALF_THICK + dome)
        };
        let rows = 2 * (RINGS + 1);
        let at = |i: usize, j: usize| {
            let (front, k) = if i <= RINGS { (1.0, i) } else { (-1.0, 2 * RINGS + 1 - i) };
            inner(TAU * fraction(j, OUTLINE_POINTS), fraction(k, RINGS), front)
        };
        let grid = Grid::from_fn(rows, OUTLINE_POINTS + 1, at)?;
        let mids = (0..rows)
            .flat_map(|i| (0..=OUTLINE_POINTS).map(move |j| (i, j)))
            .map(|(i, j)| {
                let (_, k) = if i <= RINGS { (1.0, i) } else { (-1.0, 2 * RINGS + 1 - i) };
                inner(TAU * fraction(j, OUTLINE_POINTS), fraction(k, RINGS), 0.0)
            })
            .collect();
        Ok((grid, mids))
    }

    /// The ear root of side `s`: the anchor the head published, else the side of the head at the eye line.
    fn ear(ctx: &BuildCtx<'_>, eyes: Eyes, head: &dyn HeadSurface, s: f64) -> DVec3 {
        let name = if s > 0.0 { "ear_l" } else { "ear_r" };
        if let Some((_, anchor)) = ctx.published.get::<AnchorsFact>().iter().find(|((_, a), _)| a.as_str() == name) {
            return DVec3::from(anchor.position);
        }
        let centre = DVec3::from(head.centre());
        let side = on_head(head, DVec3::X * s, 0.0);
        DVec3::new(side.x, centre.y + eyes.y, centre.z)
    }

    /// The temple arm of `eye`: from the rim's outer edge back along the head to `ear`.
    fn arm(
        head: &dyn HeadSurface,
        p: &GlassesParams,
        eye: &Eye,
        ear: DVec3,
        rows: usize,
        seg: usize,
    ) -> Result<crate::kit::sweep::Sweep, KitError> {
        let hinge = Self::at(eye, Self::outline(p, eye, if eye.s > 0.0 { 0.35 } else { std::f64::consts::PI - 0.35 }));
        let centre = DVec3::from(head.centre());
        // The arm's wire thins towards the ear, so its end meets the ear's root without sinking into the head.
        let wire = |t: f64| ARM_WIRE * ((1.0 - t) / 0.15).min(1.0).sqrt();
        let path: Vec<DVec3> = (0..=rows)
            .map(|k| {
                let t = fraction(k, rows);
                let q = hinge.lerp(ear, t);
                // Stand off the head's surface by the wire and the clearance, which fades to nothing over the last fifth of the arm, at the ear.
                let from_centre = q - centre;
                let dir = from_centre.normalize_or_zero();
                let want = f64::from(head.radius(dir.as_vec3()))
                    + 1.5 * wire(t)
                    + p.arm_clearance * (1.0 - sstep(0.8, 1.0, t));
                let r = from_centre.length().max(want);
                if k == rows { ear } else { centre + dir * r }
            })
            .collect();
        closed_tube(&path, |t| (wire(t), wire(t)), seg, DVec3::Y)
    }
}

impl PartKind for Glasses {
    type Params = GlassesParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "glasses",
            summary: "Build glasses (round, square, cat-eye or sunglasses) with temple arms to the ears.",
            category: PartCategory::Accessory,
            requires: &["head"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    #[allow(clippy::many_single_char_names)] // the geometry formulas use the textbook names
    fn build(p: &GlassesParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let (attach, default) = head_frames(ctx)?;
        let detail = Detail::of(ctx.quality);
        let err = kit_error(ctx);
        let mut shells = Shells::new(&attach, &default, 2);
        let seg = detail.cross.min(8);
        let placing = Eyes::of(p, &ctx.face.layout, f64::from(ctx.published.scale()));
        placing.check(p).map_err(|m| ctx.fail(m))?;
        let left = Self::left_eye(head.as_ref(), p, placing);
        let right = Self::mirrored(&left, f64::from(head.centre().x));
        let eyes = [left, right];
        for eye in &eyes {
            let rim = Self::rim(p, eye, seg).map_err(&err)?;
            let curve = |i: usize, _: usize| {
                let k = i % OUTLINE_POINTS;
                Self::at(eye, Self::outline(p, eye, TAU * fraction(k, OUTLINE_POINTS)))
            };
            shells.bind(0, &rim, (true, true), &curve).map_err(&err)?;
            let (lens, mids) = Self::lens(p, eye).map_err(&err)?;
            shells.bind(1, &lens, (false, true), &|i, j| mids[i * (OUTLINE_POINTS + 1) + j]).map_err(&err)?;
            let ear = Self::ear(ctx, placing, head.as_ref(), eye.s);
            let arm = Self::arm(head.as_ref(), p, eye, ear, detail.rows / 2, seg).map_err(&err)?;
            let centres = arm.centres.clone();
            shells.bind(0, &arm.grid, (false, true), &|i, _| centres[i]).map_err(&err)?;
        }
        // The bridge: an arch over the nose between the inner edges of the two rims, standing off the face.
        let [left, right] = &eyes;
        let inner =
            |eye: &Eye| Self::at(eye, Self::outline(p, eye, if eye.s > 0.0 { std::f64::consts::PI } else { 0.0 }));
        let (a, b) = (inner(left), inner(right));
        let path: Vec<DVec3> = (0..=8)
            .map(|k| {
                let t = fraction(k, 8);
                let arch = (std::f64::consts::PI * t).sin();
                let q = a.lerp(b, t) + DVec3::Y * (0.006 * arch);
                stand_off(head.as_ref(), q, MIN_FACE_GAP + 1.5 * BRIDGE_WIRE)
            })
            .collect();
        let bridge = closed_tube(&path, |_| (BRIDGE_WIRE, BRIDGE_WIRE), seg, DVec3::Z).map_err(&err)?;
        let centres = bridge.centres.clone();
        shells.bind(0, &bridge.grid, (false, true), &|i, _| centres[i]).map_err(&err)?;
        shells.output(ctx, &Self::slots(p), RigContribution::default())
    }

    fn recolour(p: &GlassesParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour(out, ctx, &Self::slots(p))
    }
}

impl Glasses {
    fn slots(p: &GlassesParams) -> [Slot; 2] {
        let coverage = p.lens_coverage.unwrap_or_else(|| p.style.outline().coverage);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a coverage in 0..=1 as a byte
        let alpha = (lerp(0.0, 255.0, coverage.clamp(0.0, 1.0))).round() as u8;
        [Slot::opaque("frame", &p.frame), Slot { alpha: Some(alpha), ..Slot::opaque("lens", &p.lens) }]
    }
}

omuzaki_char_api::register_part_kind!(Glasses);
