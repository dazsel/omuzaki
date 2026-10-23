//! The boot's plan view: its profile curves, the surface function `plan(y, phi, off)` every boot part is built on, the
//! map from the prototype's boot space to the body, and the skin weights.
//!
//! **Boot space.** The prototype built its boot in metres on one body: sole on the floor (`y = 0`), cuff top at about
//! `0.12`. Every constant of the boot (heights of creases, straps, seams, the cuff) is a height in that space. A boot on
//! another body is the same boot in a [`Space`]: boot space is lifted so the sole sits under the foot and stretched
//! above the sole so the cuff top lands where [`BootHeight`] says. With an explicit
//! [`BootProfile`] the space is the identity and the numbers are the prototype's.
//!
//! **Where the profile comes from.** Either the knots of an explicit [`BootProfile`] (the prototype's `BOOT` curves
//! `cx`, `zb`, `zf`, `rx`), or, by default, the body: `Boot::derive` cuts the fit's foot and lower leg at each knot
//! height and fits the superellipse of `plan` round what it finds, with the garment's ease to spare.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, BodyRegion, JointRef, RegionEnd, Section};
use omuzaki_core::glam::{DVec2, DVec3, Vec3};
use omuzaki_face_api::{Check, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{BootHeight, BootsParams};
use crate::kit::curve::{Pchip, TAU, fraction, lerp, sstep, sup};
use crate::kit::skin::{limit_influences, normalise};
use crate::outfit::common::GarmentError;
use crate::outfit::loft::MIN_RADIUS;
use crate::template::layout::section_basis;

/// The height of the prototype's boot, boot space: the top of its `BOOT` curves (`BOOT.top`), which is where the cuff
/// ends. A derived boot's cuff top is this height in boot space.
pub const BOOT_TOP: f64 = 0.120;
/// The most knots one profile curve may have.
pub const MAX_KNOTS: usize = 32;
/// The weights smaller than this are dropped before the top four are normalised (the prototype's `setWeights`).
const WEIGHT_FLOOR: f32 = 1e-4;

// -------------------------------------------------------------------------------------------------------- profile

/// The boot's outline as four curves over boot-space height, each a list of `[height, value]` knots (metres), the
/// prototype's `BOOT.cx`, `BOOT.zb`, `BOOT.zf` and `BOOT.rx`. Given in full, the boot is built exactly as they say, in
/// the bind frame (sole on the floor); left out, the boot is derived from the body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BootProfile {
    /// The foot centre's `x` (the left boot; the right is mirrored).
    pub cx: Vec<[f64; 2]>,
    /// The back of the boot, `z` (negative: behind the ankle).
    pub zb: Vec<[f64; 2]>,
    /// The front of the boot, `z`.
    pub zf: Vec<[f64; 2]>,
    /// The half width.
    pub rx: Vec<[f64; 2]>,
}

/// The tallest boot space reaches, metres: knots above this are refused.
pub const MAX_PROFILE_HEIGHT: f64 = 0.6;
/// The largest `|x|` or `|z|` of a knot, metres.
const MAX_COORDINATE: f64 = 0.6;

fn check_curve(c: &mut Check, name: &str, knots: &[[f64; 2]], (lo, hi): (f64, f64)) {
    if !(2..=MAX_KNOTS).contains(&knots.len()) {
        c.fail(name, format!("{} knots: a curve has 2 to {MAX_KNOTS}", knots.len()));
        return;
    }
    for (k, [y, v]) in knots.iter().enumerate() {
        c.range(&format!("{name}/{k}/0"), *y, 0.0, MAX_PROFILE_HEIGHT);
        c.range(&format!("{name}/{k}/1"), *v, lo, hi);
    }
    if knots.windows(2).any(|w| w[1][0] <= w[0][0]) {
        c.fail(name, "the knot heights must rise".to_owned());
    }
}

impl Validate for BootProfile {
    fn check(&self, c: &mut Check) {
        check_curve(c, "cx", &self.cx, (-MAX_COORDINATE, MAX_COORDINATE));
        check_curve(c, "zb", &self.zb, (-MAX_COORDINATE, MAX_COORDINATE));
        check_curve(c, "zf", &self.zf, (-MAX_COORDINATE, MAX_COORDINATE));
        check_curve(c, "rx", &self.rx, (MIN_RADIUS, MAX_COORDINATE));
        if let Ok(curves) = Curves::new(self) {
            // Front and back must stay apart at every knot of either curve, or the boot has no depth there.
            let heights = self.zb.iter().chain(&self.zf).map(|k| k[0]);
            for y in heights {
                let depth = curves.zf.eval(y) - curves.zb.eval(y);
                if depth.is_nan() || depth < 2.0 * MIN_RADIUS {
                    c.fail(
                        "zf",
                        format!(
                            "at height {y} the front is {depth} m from the back: at least {} wanted",
                            2.0 * MIN_RADIUS
                        ),
                    );
                    break;
                }
            }
        }
    }
}

/// The four curves of a profile, ready to evaluate.
#[derive(Clone, Debug)]
pub struct Curves {
    cx: Pchip,
    zb: Pchip,
    zf: Pchip,
    rx: Pchip,
}

/// The boot's section at one height: the prototype's `bootSecs`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Secs {
    /// Foot centre `x`.
    pub cx: f64,
    /// Foot centre `z`, halfway between back and front.
    pub cz: f64,
    /// Half width.
    pub rx: f64,
    /// Half depth.
    pub rz: f64,
    /// The front.
    pub zf: f64,
    /// The back.
    pub zb: f64,
}

impl Curves {
    /// The curves through a profile's knots.
    ///
    /// # Errors
    /// [`GarmentError::Kit`] for a curve with no knots, a non-finite knot or two knots at one height.
    pub fn new(profile: &BootProfile) -> Result<Self, GarmentError> {
        let curve = |knots: &[[f64; 2]]| {
            let pairs: Vec<(f64, f64)> = knots.iter().map(|k| (k[0], k[1])).collect();
            Pchip::new(&pairs)
        };
        Ok(Self { cx: curve(&profile.cx)?, zb: curve(&profile.zb)?, zf: curve(&profile.zf)?, rx: curve(&profile.rx)? })
    }

    /// The section at boot-space height `y`.
    pub fn secs(&self, y: f64) -> Secs {
        let (zf, zb) = (self.zf.eval(y), self.zb.eval(y));
        Secs { cx: self.cx.eval(y), cz: f64::midpoint(zf, zb), rx: self.rx.eval(y), rz: (zf - zb) / 2.0, zf, zb }
    }
}

// ---------------------------------------------------------------------------------------------------------- shape

/// The numbers `plan` shapes the outline with: the toe, and where the outline goes from a squared sole to an ellipse.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Shape {
    /// How much wider the toe box is than the heel, as a fraction of the width (`0.075`).
    pub widen: f64,
    /// The superellipse exponent at the sole (`2.65`); the outline is an ellipse (`2`) from the ankle up.
    pub squareness: f64,
}

/// Below this boot-space height the outline has its sole exponent, and above [`EXPO_ANKLE`] the ellipse's.
const EXPO_SOLE: f64 = 0.03;
const EXPO_ANKLE: f64 = 0.095;
/// The toe box's widening fades out between these heights.
const WIDEN_FROM: f64 = 0.04;
const WIDEN_TO: f64 = 0.095;

impl Shape {
    /// The exponent at boot-space height `y`.
    pub fn exponent(&self, y: f64) -> f64 {
        lerp(2.0, self.squareness, 1.0 - sstep(EXPO_SOLE, EXPO_ANKLE, y))
    }

    /// The outline point `(x, z)` relative to the section's centre at angle `phi`, for radii `rx`, `rz` at height `y`.
    /// `phi` is 0 at the toe (+z) and `pi / 2` on the outside (+x).
    fn local(&self, y: f64, phi: f64, rx: f64, rz: f64) -> (f64, f64, f64) {
        let e = self.exponent(y);
        let (c, s) = (phi.cos(), phi.sin());
        let (mut x, z) = (sup(s, e) * rx, sup(c, e) * rz);
        let taper = 1.0 - sstep(WIDEN_FROM, WIDEN_TO, y);
        x *= 1.0 + self.widen * taper * (z / rz);
        (x, z, e)
    }
}

/// A point on the boot's surface with its horizontal outward normal.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Plan {
    /// The point, bind space.
    pub p: DVec3,
    /// The unit outward normal (horizontal: `y` is 0).
    pub n: DVec3,
}

impl Plan {
    /// The point pushed `extra` along the normal (the prototype's `pt`).
    pub fn pt(&self, extra: f64) -> DVec3 {
        self.p + self.n * extra
    }
}

// ----------------------------------------------------------------------------------------------------------- space

/// The map from boot space to the body. Above the sole it is a stretch from the sole's top; below, a shift.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Space {
    /// The boot-space height of the sole's top, the sole's thickness.
    sole_top: f64,
    /// Where the sole's top is on the body, metres.
    sole_top_world: f64,
    /// How much a boot-space height above the sole grows: the cuff top ends at [`BOOT_TOP`] in boot space.
    stretch: f64,
    /// How far the heel block's bottom hangs under the sole's, metres: none in the prototype, whose two bottoms are
    /// coplanar; a hair on a fitted boot, so the two bottoms are two surfaces.
    heel_sag: f64,
}

/// How much of the ease the cuff roll's inner face may sink into the upper.
const CUFF_SINK: f64 = 0.45;
/// The heel's sag on a fitted boot, metres.
const HEEL_SAG: f64 = 0.0005;
/// The ease the prototype's creases (up to 0.75 mm deep) are given for, metres; a thinner ease takes proportionally shallower ones.
const CREASE_EASE: f64 = 0.004;

impl Space {
    /// The prototype's space: the sole's top at its boot-space height, nothing stretched.
    pub fn identity(sole_top: f64) -> Self {
        Self { sole_top, sole_top_world: sole_top, stretch: 1.0, heel_sag: 0.0 }
    }

    /// The space whose sole top is at `sole_top_world` and whose cuff top is at `top_world`.
    ///
    /// # Errors
    /// [`GarmentError::Invalid`] if the cuff would not be above the sole.
    pub fn fitted(sole_top: f64, sole_top_world: f64, top_world: f64) -> Result<Self, GarmentError> {
        let stretch = (top_world - sole_top_world) / (BOOT_TOP - sole_top);
        if !(stretch.is_finite() && stretch > 0.0) {
            return Err(GarmentError::invalid(
                "height",
                format!("the cuff top ({top_world} m) must be above the sole's top ({sole_top_world} m)"),
            ));
        }
        Ok(Self { sole_top, sole_top_world, stretch, heel_sag: HEEL_SAG })
    }

    /// The body height of boot-space height `y`: stretched above the sole, as is below it.
    pub fn up(&self, y: f64) -> f64 {
        self.sole_top_world + (y - self.sole_top) * self.slope(y)
    }

    /// The body height of a height that is part of the sole or heel: shifted, never stretched.
    pub fn flat(&self, y: f64) -> f64 {
        self.sole_top_world + (y - self.sole_top)
    }

    /// The body height of a height that is part of the heel block: as [`Space::flat`], hung `HEEL_SAG` lower on a fitted boot.
    pub fn heel(&self, y: f64) -> f64 {
        self.flat(y) - self.heel_sag
    }

    /// How fast the body height rises with boot-space height at `y`.
    pub fn slope(&self, y: f64) -> f64 {
        if y > self.sole_top { self.stretch } else { 1.0 }
    }

    /// The body height of the sole's bottom (boot-space height 0).
    pub fn floor(&self) -> f64 {
        self.flat(0.0)
    }

    /// The boot-space height of body height `world`: the inverse of [`Space::up`].
    pub fn down(&self, world: f64) -> f64 {
        let below = world - self.sole_top_world;
        if below > 0.0 { self.sole_top + below / self.stretch } else { self.sole_top + below }
    }
}

// ------------------------------------------------------------------------------------------------------- weights

/// Where the skin hands over from one bone to the next up the boot: the prototype's `wBoot`, which blends the foot into
/// the shaft at the ankle, the toes in at the toe cap, and a touch of thigh in at the very top.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SkinRamp {
    /// The foot gives way to the lower leg between these heights.
    ankle: (f64, f64),
    /// The foot gives way to the toes between these `z`.
    toes: (f64, f64),
    /// The lower leg gives some of its weight to the upper leg between these heights.
    thigh: (f64, f64),
}

/// How far below and above the ankle joint, and behind and ahead of the toes joint, the hand-overs reach on a body, metres:
/// the prototype's, read off its ankle (`0.045` to `0.098` about an ankle at `0.060`) and toes joint (`0.064` to `0.094` about
/// a toes joint at `0.070`).
const BODY_BLEND: (f64, f64) = (0.015, 0.038);
const TOE_BLEND: (f64, f64) = (0.006, 0.024);
/// The share of the shaft's weight that goes to the upper leg at the top of the ramp.
const THIGH_SHARE: f64 = 0.30;

impl SkinRamp {
    /// The prototype's thresholds, metres, for a boot given as an explicit profile.
    pub const PROTOTYPE: Self = Self { ankle: (0.045, 0.098), toes: (0.064, 0.094), thigh: (0.160, 0.192) };

    /// The ramps placed on a body: offsets from its ankle (`BODY_BLEND`), toes joint (`TOE_BLEND`) and the prototype's from its knee (`0.030` below
    /// to `0.002` above), taken from the joints of `fit`.
    ///
    /// # Errors
    /// [`GarmentError::Invalid`] if the fit lacks one of the joints.
    pub fn of_body(fit: &dyn BodyFit) -> Result<Self, GarmentError> {
        let joint = |bone: HumanoidBone| {
            fit.joint(&JointRef::Humanoid(bone))
                .map(Vec3::as_dvec3)
                .filter(|p| p.is_finite())
                .ok_or_else(|| GarmentError::invalid("joint", format!("the fit has no {bone:?} joint")))
        };
        let (ankle, toes, knee) =
            (joint(HumanoidBone::LeftFoot)?, joint(HumanoidBone::LeftToes)?, joint(HumanoidBone::LeftLowerLeg)?);
        Ok(Self {
            ankle: (ankle.y - BODY_BLEND.0, ankle.y + BODY_BLEND.1),
            toes: (toes.z - TOE_BLEND.0, toes.z + TOE_BLEND.1),
            thigh: (knee.y - 0.030, knee.y + 0.002),
        })
    }

    /// The skin of a left-boot vertex at `p`: at most four weights, normalised.
    #[allow(clippy::cast_possible_truncation)] // weights are in 0..=1; the mesh stores f32
    pub fn at(&self, p: DVec3) -> Vec<(JointRef, f32)> {
        let wl = sstep(self.ankle.0, self.ankle.1, p.y);
        let wt = (1.0 - wl) * sstep(self.toes.0, self.toes.1, p.z);
        let up = THIGH_SHARE * sstep(self.thigh.0, self.thigh.1, p.y);
        let mut blend = vec![
            (JointRef::Humanoid(HumanoidBone::LeftLowerLeg), (wl * (1.0 - up)) as f32),
            (JointRef::Humanoid(HumanoidBone::LeftUpperLeg), (wl * up) as f32),
            (JointRef::Humanoid(HumanoidBone::LeftFoot), ((1.0 - wl) * (1.0 - wt)) as f32),
            (JointRef::Humanoid(HumanoidBone::LeftToes), ((1.0 - wl) * wt) as f32),
        ];
        blend.retain(|(_, w)| *w > WEIGHT_FLOOR);
        limit_influences(&mut blend, crate::kit::skin::MAX_INFLUENCES);
        normalise(&mut blend);
        blend
    }
}

// --------------------------------------------------------------------------------------------------------- the boot

/// A boot to build: its profile, its place on the body and its skin.
#[derive(Clone, Debug)]
pub struct Boot {
    curves: Curves,
    shape: Shape,
    space: Space,
    skin: SkinRamp,
    /// How much of the prototype's crease depth the leather has: all of it unless the ease is too thin to hide it.
    crease_scale: f64,
    /// The ease of a fitted boot: the cuff's fold must not sink into the leg by more than half of it.
    fitted_ease: Option<f64>,
    /// On a fitted boot, the body heights of the foot's lowest point and of the ankle joint.
    foot: Option<(f64, f64)>,
}

impl Boot {
    /// The boot of `params` on `fit`: the explicit profile as given, or one derived from the body.
    ///
    /// # Errors
    /// [`GarmentError`] if the profile is refused, or the fit lacks a joint, section or room above the foot.
    pub fn new(params: &BootsParams, fit: &dyn BodyFit) -> Result<Self, GarmentError> {
        let shape = Shape { widen: params.toe.width, squareness: params.toe.squareness() };
        let thickness = params.sole.thickness;
        match &params.profile {
            Some(profile) => Self::explicit(profile, shape, thickness),
            None => Self::derive(params, fit, shape, params.height.fraction()),
        }
    }

    /// The boot fitted to the body as [`Boot::new`] fits one without a profile, its top `top` of the way from the
    /// ankle joint to the knee joint: what the shoes are cut from, their collar somewhere below that top.
    ///
    /// # Errors
    /// As [`Boot::new`].
    pub fn fitted(params: &BootsParams, fit: &dyn BodyFit, top: f64) -> Result<Self, GarmentError> {
        let shape = Shape { widen: params.toe.width, squareness: params.toe.squareness() };
        Self::derive(params, fit, shape, top)
    }

    /// The boot of an explicit profile: exactly its knots, in the bind frame, the sole's top `thickness` above the floor.
    ///
    /// # Errors
    /// [`GarmentError::Kit`] if the profile's knots are refused.
    pub fn explicit(profile: &BootProfile, shape: Shape, thickness: f64) -> Result<Self, GarmentError> {
        Ok(Self {
            curves: Curves::new(profile)?,
            shape,
            space: Space::identity(thickness),
            skin: SkinRamp::PROTOTYPE,
            crease_scale: 1.0,
            fitted_ease: None,
            foot: None,
        })
    }

    /// The boot fitted to the body: the sole and heel `ease` below the lowest point of the foot, the cuff top at the
    /// height of `params.height` up the shin, and a profile that holds the foot and leg, `ease` clear of them.
    fn derive(params: &BootsParams, fit: &dyn BodyFit, shape: Shape, top: f64) -> Result<Self, GarmentError> {
        let ease = params.common.ease;
        let thickness = params.sole.thickness;
        let skin = SkinRamp::of_body(fit)?;
        let feet = Body::of(fit)?;
        let (ankle, knee) = (feet.ankle, feet.knee);
        let top_world = ankle + top * (knee - ankle);
        // The heel block's top (never the sole's) is what clears the foot: the sole and the heel stand on one flat bottom,
        // so a heel taller than the sole drops the whole boot by the difference.
        let drop = (params.heel.height - thickness).max(0.0);
        let sole_top_world = feet.floor - ease - drop;
        let space = Space::fitted(thickness, sole_top_world, top_world)?;
        let curves = derive_curves(&feet, &space, &shape, ease)?;
        let crease_scale = (ease / CREASE_EASE).min(1.0);
        Ok(Self { curves, shape, space, skin, crease_scale, fitted_ease: Some(ease), foot: Some((feet.floor, ankle)) })
    }

    /// On a fitted boot, the body heights of the foot's lowest point and of the ankle joint.
    pub fn foot(&self) -> Option<(f64, f64)> {
        self.foot
    }

    /// The map from boot space to the body.
    pub fn space(&self) -> &Space {
        &self.space
    }

    /// How far the centre of a cuff roll of half thickness `fold` stands off the surface, metres: the prototype's, or on a
    /// fitted boot far enough that the roll's inner face sinks into the upper by at most `CUFF_SINK` of the ease (the
    /// upper itself is an ease from the leg, and the roll may not come within half an ease of it).
    pub fn cuff_stand_off(&self, fold: f64, prototype: f64) -> f64 {
        self.fitted_ease.map_or(prototype, |ease| prototype.max(fold - CUFF_SINK * ease))
    }

    /// The share of the prototype's crease depth to use.
    pub fn crease_scale(&self) -> f64 {
        self.crease_scale
    }

    /// The outline numbers.
    pub fn shape(&self) -> &Shape {
        &self.shape
    }

    /// The profile's section at boot-space height `y`.
    pub fn secs(&self, y: f64) -> Secs {
        self.curves.secs(y)
    }

    /// The prototype's `plan(y, ph, off)`: the point of the boot's surface at boot-space height `y` and angle `phi`,
    /// pushed `off` along the horizontal normal. The point's own height is the body height of `y`.
    pub fn plan(&self, y: f64, phi: f64, off: f64) -> Plan {
        self.plan_at(y, 0.0, phi, off)
    }

    /// [`Boot::plan`] for a point `dy` metres (on the body, not in boot space) above the boot-space height `yc`: the
    /// outline is read where the point is, and the point rises by exactly `dy`. Bands, straps and stitch rows use this
    /// so their size is the same on every body while their place follows the boot.
    pub fn plan_at(&self, yc: f64, dy: f64, phi: f64, off: f64) -> Plan {
        let y = yc + dy / self.space.slope(yc);
        let s = self.curves.secs(y);
        let (x, z, e) = self.shape.local(y, phi, s.rx, s.rz);
        let gx = x.signum() * (x / s.rx).abs().powf(e - 1.0) / s.rx;
        let gz = z.signum() * (z / s.rz).abs().powf(e - 1.0) / s.rz;
        let l = gx.hypot(gz);
        let l = if l == 0.0 || l.is_nan() { 1.0 } else { l };
        let (gx, gz) = if x.abs() + z.abs() > 1e-9 { (gx / l, gz / l) } else { (0.0, 1.0) };
        Plan {
            p: DVec3::new(s.cx + x + gx * off, self.space.up(yc) + dy, s.cz + z + gz * off),
            n: DVec3::new(gx, 0.0, gz),
        }
    }

    /// The skin of a left-boot vertex at `p`.
    pub fn skin(&self, p: DVec3) -> Vec<(JointRef, f32)> {
        self.skin.at(p)
    }
}

// ------------------------------------------------------------------------------------------------------- derivation

/// One elliptical section of the body, ready to be cut by a horizontal plane: its centre and its two half-axes as vectors.
#[derive(Copy, Clone)]
struct Cut {
    centre: DVec3,
    across: DVec3,
    through: DVec3,
}

impl Cut {
    fn of(fit: &dyn BodyFit, region: BodyRegion, t: f64) -> Result<(Self, f64), GarmentError> {
        #[allow(clippy::cast_possible_truncation)] // a fit is sampled in f32
        let s = fit.section(region, t as f32).ok_or(GarmentError::NoSection { region, t })?;
        Ok((Self::from_section(region, &s), f64::from(s.radii.y)))
    }

    fn from_section(region: BodyRegion, s: &Section) -> Self {
        let (across, through) = section_basis(region, s.axis);
        Self {
            centre: s.centre.as_dvec3(),
            across: across.as_dvec3() * f64::from(s.radii.x),
            through: through.as_dvec3() * f64::from(s.radii.y),
        }
    }

    /// The `(x, z)` of the points where the ellipse crosses the horizontal plane at `y`.
    fn at_height(&self, y: f64, out: &mut Vec<DVec2>) {
        let (a, b, d) = (self.across.y, self.through.y, y - self.centre.y);
        let r = a.hypot(b);
        if r < 1e-12 || d.abs() > r {
            return;
        }
        let (base, spread) = (a.atan2(b), (d / r).acos());
        for phi in [base + spread, base - spread] {
            let p = self.centre + self.across * phi.sin() + self.through * phi.cos();
            out.push(DVec2::new(p.x, p.z));
        }
    }

    fn lerp(&self, other: &Self, s: f64) -> Self {
        Self {
            centre: self.centre.lerp(other.centre, s),
            across: self.across.lerp(other.across, s),
            through: self.through.lerp(other.through, s),
        }
    }
}

/// The left foot and lower leg of the body, sampled once, and the heights the boot is placed by.
struct Body {
    foot: Vec<Cut>,
    /// From the hip down, and on past the ankle through the slices of the leg's cap (its end, which the foot's sections
    /// don't cover).
    leg: Vec<Cut>,
    /// The lowest point of the foot, metres.
    floor: f64,
    /// The height of the foot at its widest: where the sole's outline is read.
    equator: f64,
    ankle: f64,
    knee: f64,
}

/// Sections sampled along the foot and the leg, and points round a leg section.
const FOOT_SAMPLES: usize = 64;
const LEG_SAMPLES: usize = 96;
const RING_POINTS: usize = 24;
/// The share of the foot at each end sampled again, and how many sections in it.
const END_SPAN: f64 = 0.15;
const END_SAMPLES: usize = 48;
/// The step between slices of the leg's cap past the ankle, metres, and the most slices taken.
const CAP_STEP: f64 = 0.001;
const CAP_SLICES: usize = 120;

impl Body {
    fn of(fit: &dyn BodyFit) -> Result<Self, GarmentError> {
        let mut foot = Vec::with_capacity(FOOT_SAMPLES + 1);
        let mut halves = Vec::with_capacity(FOOT_SAMPLES + 1);
        let mut floor = f64::INFINITY;
        for k in 0..=FOOT_SAMPLES {
            let (cut, half) = Cut::of(fit, BodyRegion::FootL, fraction(k, FOOT_SAMPLES))?;
            floor = floor.min(cut.centre.y - half);
            halves.push(half);
            foot.push(cut);
        }
        // The heel's and the toe's domes: a slice of the foot meets them only where a section's ellipse just reaches
        // its height, so between even steps their far ends are missed by up to a step. They get more.
        for k in 1..END_SAMPLES {
            let t = END_SPAN * fraction(k, END_SAMPLES);
            for t in [t, 1.0 - t] {
                foot.push(Cut::of(fit, BodyRegion::FootL, t)?.0);
            }
        }
        let mut leg = (0..=LEG_SAMPLES)
            .map(|k| Cut::of(fit, BodyRegion::LegL, fraction(k, LEG_SAMPLES)).map(|(cut, _)| cut))
            .collect::<Result<Vec<_>, _>>()?;
        for k in 1..=CAP_SLICES {
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // a fit is sampled in f32
            let Some(slice) = fit.cap(BodyRegion::LegL, RegionEnd::End, (k as f64 * CAP_STEP) as f32) else { break };
            if slice.centre.is_finite() && slice.radii.is_finite() && slice.axis.is_finite() {
                leg.push(Cut::from_section(BodyRegion::LegL, &slice));
            }
        }

        halves.sort_by(f64::total_cmp);
        // The median half height of the foot: the foot is widest about that far above the sole.
        let equator = floor + halves[halves.len() / 2];
        let joint = |bone: HumanoidBone| {
            fit.joint(&JointRef::Humanoid(bone))
                .map(|p| f64::from(p.y))
                .ok_or_else(|| GarmentError::invalid("joint", format!("the fit has no {bone:?} joint")))
        };
        let (ankle, knee) = (joint(HumanoidBone::LeftFoot)?, joint(HumanoidBone::LeftLowerLeg)?);
        if !(ankle.is_finite() && knee.is_finite() && knee > ankle && floor.is_finite()) {
            return Err(GarmentError::invalid(
                "joint",
                format!("the knee ({knee} m) isn't above the ankle ({ankle} m)"),
            ));
        }
        Ok(Self { foot, leg, floor, equator, ankle, knee })
    }

    /// The `(x, z)` of the points of the foot and the leg at height `y`.
    fn slice(&self, y: f64, out: &mut Vec<DVec2>) {
        for cut in &self.foot {
            cut.at_height(y, out);
        }
        // The leg runs down from the hip: find the pair of sections that bracket `y`.
        if let Some(pair) = self.leg.windows(2).find(|w| w[0].centre.y >= y && w[1].centre.y <= y) {
            let span = pair[0].centre.y - pair[1].centre.y;
            let s = if span > 0.0 { (pair[0].centre.y - y) / span } else { 0.0 };
            let cut = pair[0].lerp(&pair[1], s);
            for i in 0..RING_POINTS {
                let phi = TAU * fraction(i, RING_POINTS);
                let p = cut.centre + cut.across * phi.sin() + cut.through * phi.cos();
                out.push(DVec2::new(p.x, p.z));
            }
        }
    }
}

/// Knots of the derived profile: this many, evenly spread over boot space, so that even a boot stretched tall has a knot
/// every few millimetres where the foot's dome changes fastest.
const KNOT_COUNT: usize = 56;
/// Heights either side of a knot (as a multiple of the clearance) whose slices must also be held, so the boot stays
/// clear of a sloping body between two rows.
const VERTICAL_REACH: [f64; 5] = [-1.0, -0.5, 0.0, 0.5, 1.0];
/// Directions a body point's clearance disc is sampled in.
const DISC_POINTS: usize = 8;
/// How much the clearance disc's polygon is grown so it encloses the disc: `1 / cos(pi / DISC_POINTS)`.
fn disc_growth() -> f64 {
    1.0 / (std::f64::consts::PI * fraction(1, DISC_POINTS)).cos()
}
/// How far above the foot's lowest point the lowest slice read is, metres: the sole's bottom line itself is a point.
const BOTTOM_CLEARANCE: f64 = 0.0005;
/// Heights read between a low row and the foot's widest.
const SILHOUETTE_STEPS: usize = 6;
/// Points the outline's polar table is sampled at.
const POLAR_SAMPLES: usize = 120;
/// Directions the points are binned by, and the most refinements of the fitted size.
const BINS: usize = 120;
const FIT_ITERATIONS: usize = 12;
/// A margin on the fitted size for the body between the sampled points.
const FIT_SAFETY: f64 = 1.02;

fn derive_curves(body: &Body, space: &Space, shape: &Shape, ease: f64) -> Result<Curves, GarmentError> {
    let mut knots = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut points = Vec::new();
    let mut slice = Vec::new();
    for k in 0..KNOT_COUNT {
        let y = BOOT_TOP * fraction(k, KNOT_COUNT - 1);
        let world = space.up(y);
        points.clear();
        for (height, discs) in held_heights(world, body, ease) {
            slice.clear();
            body.slice(height, &mut slice);
            let dy = height - world;
            let radius = if discs { (ease * ease - dy * dy).max(0.0).sqrt() * disc_growth() } else { 0.0 };
            for q in &slice {
                for i in 0..DISC_POINTS {
                    let a = TAU * fraction(i, DISC_POINTS);
                    points.push(*q + DVec2::new(a.sin(), a.cos()) * radius);
                }
            }
        }
        let Some(fitted) = fit_outline(&points, shape, y) else {
            return Err(GarmentError::invalid(
                "profile",
                format!("the body has nothing at height {world} m for the boot to hold"),
            ));
        };
        knots[0].push([y, fitted.cx]);
        knots[1].push([y, fitted.cz - fitted.rz]);
        knots[2].push([y, fitted.cz + fitted.rz]);
        knots[3].push([y, fitted.rx]);
    }
    let [cx, zb, zf, rx] = knots;
    Curves::new(&BootProfile { cx, zb, zf, rx })
}

/// The body heights a row at `world` must hold, each with whether the clearance disc is drawn round its points: the
/// heights within `ease` of the row (so the boot stays clear of a sloping body between rows), and, for a row below the
/// foot's widest, every height from the row up to the widest, so the lowest rows and the sole are as wide as the foot's
/// plan view (the toe tip is thin and low: no slice at the equator reaches it).
fn held_heights(world: f64, body: &Body, ease: f64) -> Vec<(f64, bool)> {
    let mut heights: Vec<(f64, bool)> = VERTICAL_REACH.iter().map(|r| (world + r * ease, true)).collect();
    let low = world.max(body.floor + BOTTOM_CLEARANCE);
    if low < body.equator {
        heights.extend((0..=SILHOUETTE_STEPS).map(|k| (lerp(low, body.equator, fraction(k, SILHOUETTE_STEPS)), false)));
    }
    heights
}

/// The boot's outline round `points`, at boot-space height `y`: the bounding box's centre and half extents, grown until
/// the outline `plan` draws holds every point. The growth is shared out between the two axes by where the point that
/// pokes out furthest lies, so a squarish footprint doesn't fatten the boot along both. `None` for no points.
fn fit_outline(points: &[DVec2], shape: &Shape, y: f64) -> Option<Secs> {
    let first = *points.first()?;
    let (lo, hi) = points.iter().fold((first, first), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    let (centre, half) = ((lo + hi) / 2.0, ((hi - lo) / 2.0).max(DVec2::splat(MIN_RADIUS)));
    // Only the farthest point in each direction can poke out of a star-shaped outline.
    let mut farthest = vec![DVec2::ZERO; BINS];
    for p in points {
        let q = *p - centre;
        let bin = angle_bin(q);
        if q.length_squared() > farthest[bin].length_squared() {
            farthest[bin] = q;
        }
    }
    farthest.retain(|q| q.length_squared() > 0.0);
    let mut scale = DVec2::ONE;
    for _ in 0..FIT_ITERATIONS {
        let radii = half * scale;
        let mut polar: Vec<(f64, f64)> = (0..POLAR_SAMPLES)
            .map(|k| {
                let (x, z, _) = shape.local(y, TAU * fraction(k, POLAR_SAMPLES), radii.x, radii.y);
                (x.atan2(z), x.hypot(z))
            })
            .collect();
        polar.sort_by(|a, b| a.0.total_cmp(&b.0));
        let reach = |angle: f64| -> f64 {
            let i = polar.partition_point(|p| p.0 < angle);
            let (a, b) = (polar[(i + polar.len() - 1) % polar.len()], polar[i % polar.len()]);
            let span = (b.0 - a.0).rem_euclid(TAU);
            let t = if span > 0.0 { (angle - a.0).rem_euclid(TAU) / span } else { 0.0 };
            lerp(a.1, b.1, t.clamp(0.0, 1.0))
        };
        let worst =
            farthest.iter().map(|q| (q.length() / reach(q.x.atan2(q.y)), *q)).max_by(|a, b| a.0.total_cmp(&b.0));
        let Some((ratio, q)) = worst.filter(|(r, _)| *r > 1.0) else { break };
        let share = DVec2::new((q.x / radii.x).powi(2), (q.y / radii.y).powi(2));
        let (gx, gz) = (share.x / (share.x + share.y), share.y / (share.x + share.y));
        scale *= DVec2::new(ratio.powf(gx), ratio.powf(gz));
    }
    let radii = half * scale * FIT_SAFETY;
    Some(Secs { cx: centre.x, cz: centre.y, rx: radii.x, rz: radii.y, zf: centre.y + radii.y, zb: centre.y - radii.y })
}

/// The direction bin of `q` (`BINS` of them round a turn), from +z toward +x.
fn angle_bin(q: DVec2) -> usize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a bin index in 0..BINS
    let bin = (q.x.atan2(q.y).rem_euclid(TAU) / TAU * fraction(BINS, 1)) as usize;
    bin.min(BINS - 1)
}

impl BootHeight {
    /// How far up the shin, from the ankle joint to the knee joint, the cuff top is.
    pub fn fraction(self) -> f64 {
        match self {
            Self::Ankle => 0.15,
            Self::MidCalf => 0.5,
            Self::Knee => 0.9,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_profile() -> BootProfile {
        BootProfile {
            cx: vec![[0.0, 0.0], [0.12, 0.0]],
            zb: vec![[0.0, -0.04], [0.12, -0.04]],
            zf: vec![[0.0, 0.06], [0.12, 0.06]],
            rx: vec![[0.0, 0.03], [0.12, 0.03]],
        }
    }

    fn boot() -> Boot {
        Boot::explicit(&flat_profile(), Shape { widen: 0.075, squareness: 2.65 }, 0.0115).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    #[test]
    fn a_profile_gives_its_sections() {
        let s = boot().secs(0.05);
        assert!(close(s.cz, 0.01) && close(s.rz, 0.05) && close(s.rx, 0.03) && close(s.zf, 0.06) && close(s.zb, -0.04));
    }

    #[test]
    fn plan_reaches_the_toe_and_the_side_and_pushes_out_along_the_normal() {
        let b = boot();
        // Above the ankle the outline is an ellipse with no widening: phi 0 is the toe, a quarter turn the outside.
        let toe = b.plan(0.12, 0.0, 0.0);
        assert!(close(toe.p.z, 0.06) && close(toe.p.x, 0.0) && close(toe.n.z, 1.0), "{toe:?}");
        let side = b.plan(0.12, std::f64::consts::FRAC_PI_2, 0.01);
        assert!(close(side.p.x, 0.04) && close(side.p.z, 0.01) && close(side.n.x, 1.0), "{side:?}");
        let heel = b.plan(0.12, std::f64::consts::PI, 0.0);
        assert!(close(heel.p.z, -0.04) && close(heel.n.z, -1.0), "{heel:?}");
        assert!(close(side.pt(0.002).x, 0.042), "pt pushes along the normal");
    }

    #[test]
    fn the_toe_box_is_wider_than_the_heel_and_the_outline_squares_up_at_the_sole() {
        let b = boot();
        let phi = 0.6;
        let (front, back) = (b.plan(0.0, phi, 0.0), b.plan(0.0, -phi + std::f64::consts::PI, 0.0));
        assert!(front.p.x.abs() > back.p.x.abs(), "the toe box ({}) is wider than the heel ({})", front.p.x, back.p.x);
        let above = b.plan(0.12, phi, 0.0);
        let (e_low, e_high) = (b.shape().exponent(0.0), b.shape().exponent(0.12));
        assert!(close(e_low, 2.65) && close(e_high, 2.0));
        // A squarer outline reaches further along the diagonal than the ellipse it replaces.
        assert!(front.p.x.hypot(front.p.z - 0.01) > above.p.x.hypot(above.p.z - 0.01));
    }

    #[test]
    fn plan_at_rises_by_exactly_the_height_asked_for() {
        let b = boot();
        let p = b.plan_at(0.05, 0.003, 1.0, 0.0);
        assert!(close(p.p.y, 0.053), "{p:?}");
    }

    #[test]
    fn the_identity_space_changes_nothing_and_a_fitted_one_puts_the_sole_and_the_cuff_where_asked() {
        let identity = Space::identity(0.0115);
        for y in [0.0, 0.0115, 0.05, 0.12] {
            assert!(close(identity.up(y), y) && close(identity.flat(y), y), "{y}");
        }
        let fitted = Space::fitted(0.0115, -0.004, 0.2).unwrap();
        assert!(close(fitted.up(0.0115), -0.004) && close(fitted.up(BOOT_TOP), 0.2));
        assert!(close(fitted.floor(), -0.004 - 0.0115), "the sole is never stretched");
        assert!(close(fitted.flat(0.02), -0.004 + 0.0085), "the heel isn't stretched");
        assert!(fitted.up(0.06) > fitted.up(0.05), "and it rises");
        assert!(Space::fitted(0.0115, 0.3, 0.2).is_err(), "a cuff under the sole is refused");
    }

    #[test]
    fn the_prototypes_skin_goes_from_foot_to_shaft_and_toes() {
        let ramp = SkinRamp::PROTOTYPE;
        let weight = |p: DVec3, bone: HumanoidBone| {
            ramp.at(p).iter().find(|(j, _)| *j == JointRef::Humanoid(bone)).map_or(0.0, |(_, w)| *w)
        };
        let sole = DVec3::new(0.04, 0.01, 0.0);
        assert!(weight(sole, HumanoidBone::LeftFoot) > 0.99);
        let cuff = DVec3::new(0.04, 0.11, 0.0);
        assert!(weight(cuff, HumanoidBone::LeftLowerLeg) > 0.99);
        let toe = DVec3::new(0.04, 0.01, 0.1);
        assert!(weight(toe, HumanoidBone::LeftToes) > 0.99 && weight(toe, HumanoidBone::LeftFoot) < 0.01);
        for p in [sole, cuff, toe, DVec3::new(0.04, 0.06, 0.07), DVec3::new(0.04, 0.2, 0.0)] {
            let w = ramp.at(p);
            assert!(w.len() <= 4 && (w.iter().map(|(_, w)| *w).sum::<f32>() - 1.0).abs() < 1e-5, "{p}: {w:?}");
        }
        // The thigh takes a share only far above the cuff.
        assert!(weight(DVec3::new(0.04, 0.2, 0.0), HumanoidBone::LeftUpperLeg) > 0.2);
    }

    #[test]
    fn a_profile_is_checked_knot_by_knot() {
        assert!(flat_profile().validate().is_ok());
        let mut p = flat_profile();
        p.rx[0][1] = 0.0;
        assert!(p.validate().unwrap_err().to_string().contains("rx/0/1"));
        let mut p = flat_profile();
        p.zb = vec![[0.1, -0.04], [0.05, -0.04]];
        assert!(p.validate().unwrap_err().to_string().contains("zb"), "heights must rise");
        let mut p = flat_profile();
        p.cx = vec![[0.0, 0.0]];
        assert!(p.validate().unwrap_err().to_string().contains("cx"), "one knot is too few");
    }

    #[test]
    fn the_outline_fit_holds_every_point_and_scales_with_them() {
        let shape = Shape { widen: 0.075, squareness: 2.65 };
        let points: Vec<DVec2> = (0..40)
            .map(|k| {
                let a = TAU * fraction(k, 40);
                DVec2::new(0.02 * a.sin(), 0.05 * a.cos() + 0.01)
            })
            .collect();
        let fitted = fit_outline(&points, &shape, 0.12).unwrap();
        let (rx, rz) = (fitted.rx, fitted.rz);
        assert!(rx >= 0.02 && rz >= 0.05, "the ellipse of the points fits inside: {fitted:?}");
        assert!(rx < 0.025 && rz < 0.06, "and not by much: {fitted:?}");
        assert!(fit_outline(&[], &shape, 0.12).is_none());
    }
}
