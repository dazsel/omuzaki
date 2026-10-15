//! Buttons and rivets: lathed shapes standing on the surface along its normal, alone or spaced along a rail.

use omuzaki_char_api::JointRef;
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Basis, Finish, Rail, RailSpec, TrimError, checked, cylinder, feature, position, sphere, torus};
use crate::kit::grid::MeshBuilder;
use crate::outfit::loft::Surface;

/// The most buttons in a row.
pub const MAX_BUTTONS: u32 = 64;
/// The prototype's gold button's radius at the foot of its disc, metres: every part below is for this radius.
pub const GOLD_RADIUS: f64 = 0.0070;
/// How far the disc's centre stands off the surface, metres.
pub const STANDOFF: f64 = 0.0010;
/// The disc: its radius at the top and at the foot, and its height (metres).
pub const DISC: (f64, f64, f64) = (0.0066, 0.0070, 0.0020);
/// Segments round the disc.
pub const DISC_SEGMENTS: usize = 24;
/// The rim torus: ring radius and tube radius (metres).
pub const RIM: (f64, f64) = (0.0061, 0.00095);
/// The rim's segments round its tube and round its ring.
pub const RIM_SEGMENTS: (usize, usize) = (8, 28);
/// How far above the disc's centre the rim and the dome sit, metres.
pub const RIM_LIFT: f64 = 0.0009;
/// The dome: a hemisphere of this radius (metres), squashed along the normal by the second number.
pub const DOME: (f64, f64) = (0.0036, 0.38);
/// The dome's segments round and from its rim to its crown.
pub const DOME_SEGMENTS: (usize, usize) = (14, 8);
/// The tip: radius and height (metres).
pub const TIP: (f64, f64) = (0.0009, 0.0012);
/// How far above the disc's centre the tip's centre sits, metres.
pub const TIP_LIFT: f64 = 0.0021;
/// Segments round the tip.
pub const TIP_SEGMENTS: usize = 8;
/// A row's rail is sampled about this often to place its buttons, metres.
const STEP: f64 = super::RAIL_STEP;

/// The shape of a button.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ButtonShape {
    /// The prototype's gold button: a disc, a rim, a low dome and a tip.
    #[default]
    Dome,
    /// A disc and its rim, no dome.
    Flat,
    /// A small disc with a tall dome and no rim.
    Rivet,
}

/// What a button looks like.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ButtonStyle {
    /// Its radius at the foot of its disc, metres (the prototype's gold button is [`GOLD_RADIUS`]); every part is in
    /// proportion.
    pub radius: f64,
    /// How far its disc stands off the surface, metres, beyond the prototype's own [`STANDOFF`] (scaled with it).
    #[serde(default)]
    pub shank: f64,
    /// Its shape.
    #[serde(default)]
    pub shape: ButtonShape,
    /// What it is made of (gold unless set).
    #[serde(default)]
    pub finish: Finish,
}

impl Validate for ButtonStyle {
    fn check(&self, c: &mut Check) {
        feature(c, "radius", self.radius);
        c.range("shank", self.shank, 0.0, super::MAX_FEATURE);
        c.nested("finish", &self.finish);
    }
}

/// One button.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Button {
    /// Where it stands, `[t, phi]` on the surface.
    pub at: [f64; 2],
    /// What it looks like.
    pub style: ButtonStyle,
}

impl Validate for Button {
    fn check(&self, c: &mut Check) {
        position(c, "at", self.at);
        c.nested("style", &self.style);
    }
}

/// Buttons spaced along a rail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ButtonRow {
    /// Where they run (its lift is not used: buttons stand on the surface).
    pub rail: RailSpec,
    /// How many, 1 to [`MAX_BUTTONS`].
    pub count: u32,
    /// From one button to the next along the rail, metres, the row centred on it; `None` spreads them from the rail's
    /// start to its end (one button sits at its middle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<f64>,
    /// What each looks like.
    pub button: ButtonStyle,
}

impl Validate for ButtonRow {
    fn check(&self, c: &mut Check) {
        c.nested("rail", &self.rail);
        if !(1..=MAX_BUTTONS).contains(&self.count) {
            c.fail("count", format!("{}: a row has 1 to {MAX_BUTTONS} buttons", self.count));
        }
        if let Some(spacing) = self.spacing {
            feature(c, "spacing", spacing);
        }
        c.nested("button", &self.button);
    }
}

/// A button of `style` standing on `surface` at `[t, phi]`, skinned like the surface there.
///
/// # Errors
/// The surface's errors for a point it can't place; the kit's for a degenerate shape.
pub fn button_at(surface: &Surface, at: [f64; 2], style: &ButtonStyle) -> Result<MeshBuilder, TrimError> {
    let frame = surface.at(at[0], at[1])?;
    let mut builder = MeshBuilder::new();
    button(&mut builder, frame.point, frame.normal, style, &surface.weights().at(at[0]))?;
    Ok(builder)
}

/// A button of `style` on the surface point `foot` with outward normal `normal`: the prototype's gold button (#188's
/// reference, D) for [`ButtonShape::Dome`] at [`GOLD_RADIUS`], every part scaled with the radius.
///
/// # Errors
/// The kit's errors for a degenerate shape.
pub fn button(
    builder: &mut MeshBuilder,
    foot: DVec3,
    normal: DVec3,
    style: &ButtonStyle,
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    let k = style.radius / GOLD_RADIUS;
    let c = foot + normal * (STANDOFF * k + style.shank);
    let at = |lift: f64| Basis::standing(c + normal * (lift * k), normal);
    let (top, bottom, height) = DISC;
    let (rim_big, rim_r) = RIM;
    let (dome_r, dome_squash) = DOME;
    let (tip_r, tip_h) = TIP;
    match style.shape {
        ButtonShape::Dome | ButtonShape::Flat => {
            cylinder(builder, &at(0.0), (top * k, bottom * k), height * k, DISC_SEGMENTS, skin)?;
            // The rim's axis along the normal: a torus lies in its local xy plane, round local z.
            let b = at(RIM_LIFT);
            let rim = Basis { x: b.x, y: b.z, z: b.y, ..b };
            torus(builder, &rim, (rim_big * k, rim_r * k), RIM_SEGMENTS, DVec3::ONE, skin)?;
        }
        ButtonShape::Rivet => {
            cylinder(builder, &at(0.0), (0.6 * top * k, 0.6 * bottom * k), height * k, DISC_SEGMENTS, skin)?;
        }
    }
    match style.shape {
        ButtonShape::Dome => {
            let squash = DVec3::new(1.0, dome_squash, 1.0);
            sphere(builder, &at(RIM_LIFT), dome_r * k, squash, DOME_SEGMENTS, std::f64::consts::FRAC_PI_2, skin)?;
            cylinder(builder, &at(TIP_LIFT), (tip_r * k, tip_r * k), tip_h * k, TIP_SEGMENTS, skin)?;
        }
        ButtonShape::Rivet => {
            let squash = DVec3::new(1.0, 2.0 * dome_squash, 1.0);
            sphere(builder, &at(RIM_LIFT), dome_r * k, squash, DOME_SEGMENTS, std::f64::consts::FRAC_PI_2, skin)?;
        }
        ButtonShape::Flat => {}
    }
    Ok(())
}

/// The button `spec` describes on `surface`.
///
/// # Errors
/// [`TrimError::Invalid`] for a spec that fails validation; as [`button_at`].
pub fn build(surface: &Surface, spec: &Button) -> Result<MeshBuilder, TrimError> {
    checked(spec)?;
    button_at(surface, spec.at, &spec.style)
}

/// Where the buttons of a row of `count` sit along a rail of `length`, metres from its start.
///
/// # Errors
/// [`TrimError::Invalid`] if the spaced buttons don't fit on the rail.
pub fn row_distances(length: f64, count: u32, spacing: Option<f64>) -> Result<Vec<f64>, TrimError> {
    let gaps = f64::from(count.saturating_sub(1));
    let (start, step) = match spacing {
        Some(spacing) => {
            let span = gaps * spacing;
            if span > length {
                return Err(TrimError::invalid(
                    "spacing",
                    format!("{count} buttons {spacing} m apart need {span} m; the rail is {length} m"),
                ));
            }
            ((length - span) / 2.0, spacing)
        }
        None if count == 1 => (length / 2.0, 0.0),
        None => (0.0, length / gaps),
    };
    Ok((0..count).map(|k| start + step * f64::from(k)).collect())
}

/// The row `spec` describes on `surface`.
///
/// # Errors
/// [`TrimError::ZeroLengthRail`] for a rail without length; [`TrimError::Invalid`] for a spec that fails validation or
/// buttons that don't fit.
pub fn build_row(surface: &Surface, spec: &ButtonRow) -> Result<MeshBuilder, TrimError> {
    checked(spec)?;
    let rail = Rail::from_polyline_on_surface(surface, &spec.rail, STEP.min(spec.rail.step))?;
    let mut builder = MeshBuilder::new();
    for distance in row_distances(rail.length(), spec.count, spec.spacing)? {
        builder.merge(&button_at(surface, rail_position(&rail, distance), &spec.button)?)?;
    }
    Ok(builder)
}

/// The surface position `[t, phi]` `distance` metres along `rail`, between its two nearest samples.
pub fn rail_position(rail: &Rail, distance: f64) -> [f64; 2] {
    let points = rail.points();
    let k = points.partition_point(|p| p.distance < distance).clamp(1, points.len() - 1);
    let (a, b) = (&points[k - 1], &points[k]);
    let s = if b.distance > a.distance {
        ((distance - a.distance) / (b.distance - a.distance)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // A closed rail's last sample repeats the first with its angle a turn round: interpolate the unwrapped angle.
    let dphi = (b.phi - a.phi + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
    [(a.t + (b.t - a.t) * s).clamp(0.0, 1.0), a.phi + dphi * s]
}
