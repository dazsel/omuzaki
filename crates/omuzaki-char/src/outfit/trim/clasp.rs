//! Medallion clasps: a stud on the surface, a bail hanging from it, a jewelled ring on the bail and a drop or a small
//! pearled cross under the ring (the robe's clasp, as parameters).
//!
//! Every piece is a closed shell laid out in the surface's frame at the clasp: out along the surface normal, down the
//! surface (world down, in the tangent plane), and across. The hanging pieces stand off the surface by enough that the
//! lowest of them stays outside it, so the clasp never sinks into a garment that bulges below it.

use omuzaki_char_api::JointRef;
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::chain::torus;
use super::{Finish, TrimError, checked, feature, position};
use crate::kit::curve::{TAU, fraction};
use crate::kit::grid::{GridOptions, MeshBuilder, Skin};
use crate::kit::loft::lathe_axis;
use crate::outfit::loft::Surface;

/// Segments round a lathed piece.
const ROUND: usize = 16;
/// Points along a pearl's half circle.
const PEARL_ROWS: usize = 6;
/// The gap kept between a hanging piece and the surface, as a share of the clasp's size.
const GAP: f64 = 0.02;
/// Passes that push the hanging pieces out: each moves them by the worst shortfall, which settles in two on a
/// convex garment; the third covers a section that turns.
const STANDOFF_PASSES: usize = 3;

/// What hangs under a clasp's ring.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaspStyle {
    /// A teardrop.
    #[default]
    Medallion,
    /// A small cross of pearls (the pendant cross).
    Cross,
}

/// A medallion clasp at one point of the surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Clasp {
    /// Where its stud sits, `[t, phi]` on the surface.
    pub at: [f64; 2],
    /// What hangs under the ring.
    #[serde(default)]
    pub style: ClaspStyle,
    /// The ring's outer diameter, metres; every other piece is in proportion.
    pub size: f64,
    /// The jewel's colour.
    pub jewel: ColourRef,
    /// What the metal pieces are made of (gold unless set).
    #[serde(default)]
    pub finish: Finish,
}

impl Validate for Clasp {
    fn check(&self, c: &mut Check) {
        position(c, "at", self.at);
        feature(c, "size", self.size);
        c.nested("finish", &self.finish);
    }
}

/// The two materials' pieces of a clasp.
pub struct ClaspPieces {
    /// The stud, bail, ring and drop or cross.
    pub metal: MeshBuilder,
    /// The jewel in the ring.
    pub jewel: MeshBuilder,
}

/// The frame of a clasp: the stud's foot, out, down and across.
#[derive(Copy, Clone, Debug)]
struct Frame {
    foot: DVec3,
    out: DVec3,
    down: DVec3,
    across: DVec3,
}

/// The clasp's pieces' key distances, in clasp sizes, from the stud's foot down.
mod layout {
    /// The stud's radius and height.
    pub const STUD: (f64, f64) = (0.18, 0.12);
    /// How far below the stud the bail's centre hangs, its radius and its wire.
    pub const BAIL: (f64, f64, f64) = (0.26, 0.09, 0.025);
    /// The ring's top below the stud, and its wire.
    pub const RING: (f64, f64) = (0.36, 0.06);
    /// The jewel's radius and height, as shares of the ring's inner radius.
    pub const JEWEL: (f64, f64) = (0.8, 0.45);
    /// The drop's length and widest radius.
    pub const DROP: (f64, f64) = (0.42, 0.12);
    /// A pearl's radius and the step between pearls.
    pub const PEARL: (f64, f64) = (0.045, 0.1);
}

/// The clasp `spec` describes on `surface`.
///
/// # Errors
/// [`TrimError::Invalid`] for a spec that fails validation; the surface's and the kit's errors.
pub fn build(surface: &Surface, spec: &Clasp) -> Result<ClaspPieces, TrimError> {
    checked(spec)?;
    let at = surface.at(spec.at[0], spec.at[1])?;
    let down = (-DVec3::Y - at.normal * -at.normal.y).try_normalize().unwrap_or(-at.along);
    let frame = Frame { foot: at.point, out: at.normal, down, across: at.normal.cross(down) };
    let s = spec.size;
    let skin = surface.weights().at(spec.at[0]);
    let mut metal = MeshBuilder::new();
    let mut jewel = MeshBuilder::new();

    let (stud_r, stud_h) = (layout::STUD.0 * s, layout::STUD.1 * s);
    let stud = [(0.0, 0.0), (0.0, stud_r), (0.3 * stud_h, stud_r), (0.8 * stud_h, 0.6 * stud_r), (stud_h, 0.0)];
    lathe(&mut metal, frame.foot, frame.out, &stud, &skin)?;

    // The hanging pieces, laid out first at the stud, then pushed out until each clears the surface by the gap.
    let ring_r = 0.5 * s;
    let ring_wire = layout::RING.1 * s;
    let ring_centre_down = layout::RING.0 * s + ring_r;
    let below = ring_centre_down + ring_r;
    let (bail_down, bail_r, bail_wire) = (layout::BAIL.0 * s, layout::BAIL.1 * s, layout::BAIL.2 * s);
    let (drop_len, drop_wide) = (layout::DROP.0 * s, layout::DROP.1 * s);
    let (pearl_r, pearl_step) = (layout::PEARL.0 * s, layout::PEARL.1 * s);
    // A column of five pearls under the ring, and an arm of two each side across the second.
    let pearls: Vec<(f64, f64)> = (0..5)
        .map(|k| (below + pearl_step * (f64::from(k) + 0.5), 0.0))
        .chain([-2.0, -1.0, 1.0, 2.0].map(|side| (below + pearl_step * 1.5, pearl_step * side)))
        .collect();
    // Points of every hanging piece as (down, across, how far the piece reaches back toward the surface).
    let mut probes = vec![(bail_down, 0.0, bail_r + bail_wire)];
    probes.extend((0..12).map(|k| {
        let a = TAU * fraction(k, 12);
        (ring_centre_down + ring_r * a.cos(), ring_r * a.sin(), ring_wire)
    }));
    match spec.style {
        ClaspStyle::Medallion => {
            probes.extend((0..=4).map(|k| (below - ring_wire + drop_len * fraction(k, 4), 0.0, drop_wide)));
        }
        ClaspStyle::Cross => probes.extend(pearls.iter().map(|&(down, across)| (down, across, pearl_r))),
    }
    let mut standoff = stud_h.max(ring_wire) + GAP * s;
    for _ in 0..STANDOFF_PASSES {
        let mut deficit = 0.0_f64;
        for &(down, across, reach) in &probes {
            let probe = frame.foot + frame.down * down + frame.across * across + frame.out * standoff;
            if let Some(p) = surface.project(probe)? {
                deficit = deficit.max(reach + GAP * s - p.outside);
            }
        }
        standoff += deficit.max(0.0);
    }
    let hang = |down: f64, out: f64| frame.foot + frame.down * down + frame.out * (standoff + out);

    let bail_centre = hang(bail_down, 0.0);
    torus(&mut metal, bail_centre, (frame.down, frame.out), (bail_r, bail_r), bail_wire, skin.clone())?;
    let ring_centre = hang(ring_centre_down, 0.0);
    let inner = ring_r - ring_wire;
    torus(&mut metal, ring_centre, (frame.down, frame.across), (inner, inner), ring_wire, skin.clone())?;
    let (jewel_r, jewel_h) = (layout::JEWEL.0 * inner, layout::JEWEL.1 * inner);
    let gem =
        [(0.0, 0.0), (0.0, jewel_r), (0.35 * jewel_h, 0.95 * jewel_r), (0.8 * jewel_h, 0.5 * jewel_r), (jewel_h, 0.0)];
    lathe(&mut jewel, ring_centre - frame.out * (0.5 * ring_wire), frame.out, &gem, &skin)?;

    match spec.style {
        ClaspStyle::Medallion => {
            let (len, wide) = (drop_len, drop_wide);
            let drop =
                [(0.0, 0.0), (0.12 * len, 0.45 * wide), (0.45 * len, wide), (0.75 * len, 0.8 * wide), (len, 0.0)];
            lathe(&mut metal, hang(below - ring_wire, 0.0), frame.down, &drop, &skin)?;
        }
        ClaspStyle::Cross => {
            for &(down, across) in &pearls {
                pearl(&mut metal, hang(down, 0.0) + frame.across * across, frame.out, pearl_r, &skin)?;
            }
        }
    }
    Ok(ClaspPieces { metal, jewel })
}

/// Adds the closed lathe of `profile` round `axis` from `origin`.
fn lathe(
    builder: &mut MeshBuilder,
    origin: DVec3,
    axis: DVec3,
    profile: &[(f64, f64)],
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    let grid = lathe_axis(origin, axis, profile, ROUND)?;
    let inside = |i: usize, _: usize| origin + axis * profile[i].0;
    let options =
        GridOptions { wrap_j: true, inside: Some(&inside), skin: Skin::Blend(skin.to_vec()), ..GridOptions::default() };
    builder.add_grid(&grid, &options)?;
    Ok(())
}

/// Adds a sphere of radius `r` at `centre`, its poles along `axis`.
fn pearl(
    builder: &mut MeshBuilder,
    centre: DVec3,
    axis: DVec3,
    r: f64,
    skin: &[(JointRef, f32)],
) -> Result<(), TrimError> {
    let profile: Vec<(f64, f64)> = (0..=PEARL_ROWS)
        .map(|k| {
            let a = TAU / 2.0 * fraction(k, PEARL_ROWS);
            (r * (1.0 - a.cos()), if k == 0 || k == PEARL_ROWS { 0.0 } else { r * a.sin() })
        })
        .collect();
    lathe(builder, centre - axis * r, axis, &profile, skin)
}
