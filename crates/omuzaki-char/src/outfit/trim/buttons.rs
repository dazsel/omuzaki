//! Buttons and rivets: lathed shapes standing on the surface along its normal, alone or spaced along a rail.

use omuzaki_face_api::{Check, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Finish, Rail, RailSpec, TrimError, checked, feature, position};
use crate::kit::grid::{GridOptions, MeshBuilder, Skin};
use crate::kit::loft::lathe_axis;
use crate::outfit::loft::Surface;

/// The most buttons in a row.
pub const MAX_BUTTONS: u32 = 64;
/// Segments round a button.
const ROUND: usize = 20;
/// The stem under a raised button, as a share of its radius.
const STEM: f64 = 0.35;
/// A row's rail is sampled about this often to place its buttons, metres.
const STEP: f64 = 0.002;

/// The shape of a button.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ButtonShape {
    /// A rounded dome (the prototype's gold buttons).
    #[default]
    Dome,
    /// A flat disc with a rounded rim.
    Flat,
    /// A low rivet with a collar.
    Rivet,
}

/// What a button looks like.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ButtonStyle {
    /// Its radius, metres.
    pub radius: f64,
    /// How far it stands off the surface on its stem, metres (0 sits it on the surface).
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

/// The lathe profile of a button of `style`, `(height along the normal, radius)` from its foot to its crown, both ends
/// on the axis so the shell is closed.
pub fn profile(style: &ButtonStyle) -> Vec<(f64, f64)> {
    let (r, s) = (style.radius, style.shank);
    let mut out = vec![(0.0, 0.0)];
    if s > 0.0 {
        out.extend([(0.0, STEM * r), (s, STEM * r)]);
    }
    let head: &[(f64, f64)] = match style.shape {
        ButtonShape::Dome => &[(0.0, 0.92), (0.08, 1.0), (0.22, 0.97), (0.4, 0.75), (0.5, 0.4), (0.53, 0.0)],
        ButtonShape::Flat => &[(0.0, 0.94), (0.05, 1.0), (0.18, 1.0), (0.24, 0.9), (0.26, 0.0)],
        ButtonShape::Rivet => &[(0.0, 1.0), (0.06, 1.0), (0.1, 0.62), (0.22, 0.6), (0.32, 0.42), (0.36, 0.0)],
    };
    out.extend(head.iter().map(|(u, k)| (s + u * r, k * r)));
    out
}

/// A button of `style` standing on `surface` at `[t, phi]`, skinned like the surface there.
///
/// # Errors
/// The surface's errors for a point it can't place; the kit's for a degenerate shape.
pub fn button_at(surface: &Surface, at: [f64; 2], style: &ButtonStyle) -> Result<MeshBuilder, TrimError> {
    let frame = surface.at(at[0], at[1])?;
    let shape = profile(style);
    let grid = lathe_axis(frame.point, frame.normal, &shape, ROUND)?;
    let inside = |i: usize, _: usize| frame.point + frame.normal * shape[i].0;
    let skin = surface.weights().at(at[0]);
    let options =
        GridOptions { wrap_j: true, inside: Some(&inside), skin: Skin::Blend(skin), ..GridOptions::default() };
    let mut builder = MeshBuilder::new();
    builder.add_grid(&grid, &options)?;
    Ok(builder)
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
    let rail = Rail::with_step(surface, &spec.rail, STEP)?;
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
