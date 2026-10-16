//! Horns (CB-43): `horns`, a pair (or one centre horn) swept along a guide curve from the skull, in four curl presets
//! and with an optional ember-crack glow.
//!
//! **Build.** Each horn is a round tube along a guide curve: the root sits on the head's surface in `direction` and
//! starts `sink` metres inside it, so the base is skull-tight with no gap and the hair, which stops short of the scalp,
//! never shows a floating base. The curve is integrated from the root: the start direction is the surface direction
//! tilted by `tilt_deg` (back) and `spread_deg` (outward), then the preset turns it by its own curl, scaled by `curl`.
//! The radius falls from `base_radius` to `taper` times that, with a rounded tip (`tip_round`) and optional ridges.
//! `horn_tip` is a thin sleeve over the last quarter, and `horn_glow` thin crack lines along the lower horn.
//!
//! **Both sides.** The two horns are *generated*, not mirrored: every vector that depends on the side is multiplied by
//! the side's sign `s` (`+1` the character's left, `-1` the right), so the right horn is the exact mirror image of the
//! left one, crack phases included (a mirrored tube frame flips its binormal, hence `s * phase`).
//!
//! Skinned rigidly to the head joint. With an `attach` the whole pair moves as one piece, and the horns then root at
//! `+-spacing / 2` from the attach point instead of on the skull.

use std::f64::consts::PI;

use omuzaki_char_api::{
    BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput, RecolourCtx, Recoloured, RigContribution,
};
use omuzaki_core::glam::{DQuat, DVec3};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::rigid::{Detail, kit_error};
use super::shells::{Shells, Slot, closed_tube, head_frames, recolour};
use crate::kit::KitError;
use crate::kit::curve::{clamp, fraction, lerp};
use crate::kit::sweep::{Sweep, frames};

/// How far into the head the horn's base starts, as a fraction of the base radius: enough that the rim of the base is
/// buried even where the skull curves away.
const SINK_RADII: f64 = 0.5;
/// The fraction of the horn's length over which the base closes (inside the skull) and the tip sleeve starts.
const BASE_CAP: f64 = 0.05;
/// How much wider than the horn the tip sleeve is, metres: enough to cover it without z-fighting.
const SLEEVE: f64 = 2.5e-4;
/// The radius of a crack line, metres.
const CRACK: f64 = 6e-4;

/// A crack line's radius at fraction `t` of its length: it closes to a point at both ends.
fn crack_radius(t: f64) -> (f64, f64) {
    let r = CRACK * (4.0 * t * (1.0 - t)).sqrt().min(1.0);
    (r, r)
}

/// The shape of the horns.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HornShape {
    /// Rising and sweeping back over the head.
    #[default]
    SweptBack,
    /// A ram's spiral: back, round beside the ear and forward again.
    Ram,
    /// Short, thick and upright, leaning a little forward.
    Oni,
    /// A swept-back beam with one fork.
    Antler,
}

/// Horns on the skull.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HornsParams {
    /// The curl preset.
    pub shape: HornShape,
    /// Radius of the base, metres.
    pub base_radius: f64,
    /// Length of the horn along its curve, metres.
    pub length: f64,
    /// The radius at the tip over the radius at the base, `0..=1` (before the tip is rounded off).
    pub taper: f64,
    /// How much of the preset's curl to use: `0` is straight, `1` the preset, `2` twice as much.
    pub curl: f64,
    /// Extra tilt of the start direction, degrees, positive back.
    pub tilt_deg: f64,
    /// Extra spread of the start direction, degrees, positive outward.
    pub spread_deg: f64,
    /// Ridges along the horn, `0` for smooth.
    pub ridges: u32,
    /// How round the tip is, `0` (a point) to `1` (a ball).
    pub tip_round: f64,
    /// A pair, or (off) one horn in the centre.
    pub mirror: bool,
    /// Where on the skull the root sits: a direction from the head centre (x is the character's left, y up, z forward).
    /// With `mirror` off its x is ignored: the single horn sits on the centre line.
    pub direction: [f64; 3],
    /// With an `attach`: the distance between the two roots, metres.
    pub spacing: f64,
    /// How much the cracks glow, `0..=1`; `0` builds no cracks.
    pub glow: f64,
    /// The horn colour.
    pub horn: ColourRef,
    /// The tip colour.
    pub horn_tip: ColourRef,
    /// The crack colour.
    pub horn_glow: ColourRef,
}

impl Default for HornsParams {
    fn default() -> Self {
        Self {
            shape: HornShape::SweptBack,
            base_radius: 0.02,
            length: 0.14,
            taper: 0.12,
            curl: 1.0,
            tilt_deg: 0.0,
            spread_deg: 0.0,
            ridges: 0,
            tip_round: 0.3,
            mirror: true,
            direction: [0.5, 0.82, 0.12],
            spacing: 0.12,
            glow: 0.0,
            horn: ColourRef::rgb(0x4a, 0x3a, 0x4c),
            horn_tip: ColourRef::rgb(0xd8, 0xc8, 0xb0),
            horn_glow: ColourRef::rgb(0xff, 0x7a, 0x30),
        }
    }
}

impl Validate for HornsParams {
    fn check(&self, c: &mut Check) {
        c.range("base_radius", self.base_radius, 0.003, 0.04);
        c.range("length", self.length, 0.02, 0.4);
        c.range("taper", self.taper, 0.0, 1.0);
        c.range("curl", self.curl, 0.0, 2.0);
        c.range("tilt_deg", self.tilt_deg, -45.0, 45.0);
        c.range("spread_deg", self.spread_deg, -30.0, 60.0);
        c.range("tip_round", self.tip_round, 0.0, 1.0);
        c.range("spacing", self.spacing, 0.0, 0.4);
        c.range("glow", self.glow, 0.0, 1.0);
        if self.ridges > 24 {
            c.fail("ridges", "at most 24 ridges");
        }
        let finite = self.direction.iter().all(|v| v.is_finite());
        let d = DVec3::from_array(self.direction);
        if !finite || d.y <= 0.0 || d.length() < 1e-6 {
            c.fail("direction", "give a direction from the head centre that points upward: three numbers with y > 0");
        }
    }
}

/// The side's unit axis: `+X` for the left horn, `-X` for the right.
fn side_axis(s: f64) -> DVec3 {
    DVec3::X * s
}

/// What a preset does to a horn: where it starts and how it turns along its length.
struct Turn {
    /// The start direction, before the tilt and spread.
    start: DVec3,
    /// The axis the direction turns about.
    about: About,
    /// The turn over the whole length at `curl` 1, degrees.
    degrees: f64,
    /// How late the turn comes: the turned fraction is `t` to this power.
    ease: f64,
    /// How much the tangent sinks per unit of `t`, so a spiral descends.
    drop: f64,
}

/// The axis of a [`Turn`].
enum About {
    /// Across the start direction and the back of the head: the horn sweeps back.
    TowardBack,
    /// As [`About::TowardBack`] towards the front.
    TowardFront,
    /// The vertical: the horn spirals round beside the head.
    Vertical,
}

/// The integrated guide curve of one horn from `root`, `rows + 1` points evenly spaced along its length. `out` is the
/// unit direction the root points along and `s` the side (`+1`, `-1`, or `0` for the centre horn).
fn guide(p: &HornsParams, root: DVec3, out: DVec3, s: f64, rows: usize) -> Vec<DVec3> {
    let lat = if s == 0.0 { DVec3::X } else { side_axis(s) };
    let back = -DVec3::Z;
    let front = DVec3::Z;
    let sign = if s == 0.0 { 1.0 } else { s };
    let turn = match p.shape {
        HornShape::SweptBack => Turn { start: out, about: About::TowardBack, degrees: 50.0, ease: 1.2, drop: 0.0 },
        HornShape::Oni => Turn {
            start: (out * 0.4 + DVec3::Y * 0.9).normalize_or_zero(),
            about: About::TowardFront,
            degrees: 25.0,
            ease: 1.0,
            drop: 0.0,
        },
        HornShape::Antler => Turn { start: out, about: About::TowardBack, degrees: 25.0, ease: 1.0, drop: 0.0 },
        HornShape::Ram => Turn {
            start: (lat * 0.8 + back * 0.4 + DVec3::Y * 0.35).normalize_or_zero(),
            about: About::Vertical,
            degrees: 260.0,
            ease: 0.9,
            drop: 0.3,
        },
    };
    // Tilt back (about the axis across the head), then spread outward (about the front axis).
    let across = DVec3::X;
    let tilt = DQuat::from_axis_angle(across * sign, -p.tilt_deg.to_radians() * sign);
    let spread = DQuat::from_axis_angle(front, -p.spread_deg.to_radians() * sign);
    let d0 = (spread * tilt * turn.start).normalize_or_zero();
    let axis = match turn.about {
        About::TowardBack => d0.cross(back),
        About::TowardFront => d0.cross(front),
        About::Vertical => DVec3::Y,
    }
    .normalize_or_zero();
    // The ram turns about the vertical, away from the centre line first, so the sense follows the side.
    let handed = if p.shape == HornShape::Ram { -sign } else { 1.0 };
    let ds = p.length / usize_f(rows);
    let mut point = root;
    let mut points = vec![root];
    for k in 0..rows {
        let t = (usize_f(k) + 0.5) / usize_f(rows);
        let angle = handed * (turn.degrees * p.curl).to_radians() * t.powf(turn.ease);
        let mut d = DQuat::from_axis_angle(axis, angle) * d0;
        d.y -= turn.drop * t;
        point += d.normalize_or_zero() * ds;
        points.push(point);
    }
    points
}

#[allow(clippy::cast_precision_loss)] // a row count
fn usize_f(n: usize) -> f64 {
    n as f64
}

/// The radius at fraction `t` of the horn.
fn radius(p: &HornsParams, t: f64) -> f64 {
    let body = lerp(1.0, p.taper, t.powf(0.9));
    let base = {
        let x = clamp(t / BASE_CAP, 0.0, 1.0);
        (1.0 - (1.0 - x) * (1.0 - x)).sqrt()
    };
    let cap = 0.04 + 0.2 * p.tip_round;
    let x = clamp((t - (1.0 - cap)) / cap, 0.0, 1.0);
    let tip = lerp(1.0 - x, (1.0 - x * x).sqrt(), p.tip_round);
    let ridge = if p.ridges == 0 {
        1.0
    } else {
        let envelope = (t / 0.15).min(1.0) * ((1.0 - t) / 0.15).min(1.0);
        1.0 + 0.08 * envelope * (PI * f64::from(p.ridges) * t).sin().powi(2)
    };
    p.base_radius * body * base * tip * ridge
}

/// One horn's shells: the body, the tip sleeve, the cracks and (for an antler) the fork.
struct Horn {
    body: Sweep,
    sleeve: Sweep,
    cracks: Vec<Sweep>,
    /// The fork of an antler, in the horn's own material.
    fork: Option<Sweep>,
}

fn build_horn(p: &HornsParams, detail: Detail, s: f64, path: &[DVec3]) -> Result<Horn, KitError> {
    let rows = path.len() - 1;
    let seg = detail.lathe * 3 / 4;
    let body = closed_tube(path, |t| (radius(p, t), radius(p, t)), seg, DVec3::Z)?;
    // The tip sleeve: the same path from three quarters of the way, a hair wider, closing quickly at its lower end.
    let from = rows * 3 / 4;
    let tail = &path[from..];
    let denominator = fraction(from, rows);
    let sleeve_radius = |u: f64| {
        let t = lerp(denominator, 1.0, u);
        let band = clamp(u / 0.15, 0.0, 1.0);
        (radius(p, t) + SLEEVE) * (1.0 - (1.0 - band) * (1.0 - band)).sqrt()
    };
    let sleeve = closed_tube(tail, |u| (sleeve_radius(u), sleeve_radius(u)), seg, DVec3::Z)?;
    let mut cracks = Vec::new();
    if p.glow > 0.0 {
        let fr = frames(path, DVec3::Z)?;
        // A single centre horn (`s` is 0) turns its cracks the way the left horn does, or both would lie on one line.
        let handed = if s == 0.0 { 1.0 } else { s };
        for c in 0..2 {
            let phase = if c == 0 { 0.7 } else { 0.7 + PI };
            let points: Vec<DVec3> = (2..=rows * 3 / 4)
                .step_by(2)
                .map(|k| {
                    let t = fraction(k, rows);
                    let phi = handed * (phase + 0.55 * (usize_f(k) * 1.9 + usize_f(c)).sin());
                    let frame = fr[k];
                    frame.p + (frame.n * phi.cos() + frame.b * phi.sin()) * radius(p, t)
                })
                .collect();
            if points.len() >= 2 {
                cracks.push(closed_tube(&points, crack_radius, 5, DVec3::Z)?);
            }
        }
    }
    let fork = if p.shape == HornShape::Antler { Some(build_fork(p, detail, path, s)?) } else { None };
    Ok(Horn { body, sleeve, cracks, fork })
}

/// The fork of an antler: a short, thin tube leaving the beam at its middle, forward and outward.
fn build_fork(p: &HornsParams, detail: Detail, path: &[DVec3], s: f64) -> Result<Sweep, KitError> {
    let rows = path.len() - 1;
    let at = rows / 2;
    let beam = (path[at + 1] - path[at - 1]).normalize_or_zero();
    let lat = side_axis(if s == 0.0 { 1.0 } else { s });
    let mut d = (beam * 0.5 + lat * 0.35 + DVec3::Z * 0.55 + DVec3::Y * 0.3).normalize_or_zero();
    let steps = (rows / 2).max(3);
    let ds = 0.55 * p.length / usize_f(steps);
    let mut points = vec![path[at]];
    for _ in 0..steps {
        d = (d + DVec3::Y * 0.12).normalize_or_zero();
        points.push(points[points.len() - 1] + d * ds);
    }
    let thin = |t: f64| {
        let r = 0.7 * radius(p, 0.5) * lerp(1.0, 0.15, t) * (1.0 - clamp((t - 0.8) / 0.2, 0.0, 1.0).powi(2)).sqrt();
        (r, r)
    };
    closed_tube(&points, thin, detail.lathe * 3 / 4, DVec3::Z)
}

/// The `horns` kind.
pub struct Horns;

impl PartKind for Horns {
    type Params = HornsParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "horns",
            summary: "Build a pair of horns (swept back, ram, oni or antler) rooted on the skull, optionally with glowing cracks.",
            category: PartCategory::Accessory,
            requires: &["head"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &HornsParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let (attach, default) = head_frames(ctx)?;
        let detail = Detail::of(ctx.quality);
        let rows = detail.rows;
        let sides: &[f64] = if p.mirror { &[1.0, -1.0] } else { &[0.0] };
        let mut shells = Shells::new(&attach, &default, 3);
        let err = kit_error(ctx);
        let sink = SINK_RADII * p.base_radius;
        for &s in sides {
            let mut dir = DVec3::from_array(p.direction);
            dir.x = s * dir.x.abs();
            let out = dir.normalize_or_zero();
            let root = if ctx.attach.is_some() {
                // Rooted `spacing / 2` either side of the attach point, which is the head joint's frame origin.
                let origin = default.origin();
                origin + DVec3::X * (0.5 * s * p.spacing)
            } else {
                #[allow(clippy::cast_possible_truncation)] // metres
                let surface = head.point(out.as_vec3(), -(sink as f32));
                DVec3::from(surface)
            };
            let path = guide(p, root, out, s, rows);
            let horn = build_horn(p, detail, s, &path).map_err(&err)?;
            let inside = |sw: &Sweep| {
                let centres = sw.centres.clone();
                move |i: usize, _: usize| centres[i]
            };
            for (slot, sw) in [(0, Some(&horn.body)), (1, Some(&horn.sleeve)), (0, horn.fork.as_ref())] {
                if let Some(sw) = sw {
                    shells.bind(slot, &sw.grid, (false, true), &inside(sw)).map_err(&err)?;
                }
            }
            for crack in &horn.cracks {
                shells.bind(2, &crack.grid, (false, true), &inside(crack)).map_err(&err)?;
            }
        }
        shells.output(ctx, &Self::slots(p), RigContribution::default())
    }

    fn recolour(p: &HornsParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour(out, ctx, &Self::slots(p))
    }
}

impl Horns {
    fn slots(p: &HornsParams) -> [Slot; 3] {
        #[allow(clippy::cast_possible_truncation)] // a fraction
        let glow = p.glow as f32;
        [
            Slot::opaque("horn", &p.horn),
            Slot::opaque("horn_tip", &p.horn_tip),
            Slot { glow, ..Slot::opaque("horn_glow", &p.horn_glow) },
        ]
    }
}

omuzaki_char_api::register_part_kind!(Horns);
