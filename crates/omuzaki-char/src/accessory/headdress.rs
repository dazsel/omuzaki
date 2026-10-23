//! The headdress (#394): `headdress`, a frilled headband worn over the hair a little forward of the crown, from above
//! one ear to above the other.
//!
//! **Build.** The band is a flat tube along an arc over the head in a plane tilted forward of the ears by `forward` of
//! the head's radius: its centre line stands `clearance` metres above the head's surface, or further where the hair the
//! hair part published ([`HairReachFact`]) stands higher, so its underside clears the hair by [`HAIR_MARGIN`]. The frill
//! is a thin sheet standing up out of the band, gathered: its middle line runs along the band's top, swung forward and
//! back `gathers` times, so the sheet ruffles. Both are closed tubes cut square to the arc, so the pair of sides is an
//! exact mirror image.
//!
//! Skinned rigidly to the head joint. With an `attach` the whole piece moves as one.

use omuzaki_char_api::{
    BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput, RecolourCtx, Recoloured, RigContribution,
};
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::rigid::{Detail, kit_error};
use super::shells::{Shells, Slot, head_frames, recolour};
use crate::hair::reach::{HairReach, HairReachFact};
use crate::kit::curve::{TAU, fraction, lerp};
use crate::kit::grid::Grid;
use crate::kit::sweep::Sweep;

/// A headdress.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HeaddressParams {
    /// Width of the band along the head's front-back axis, metres.
    pub band_width: f64,
    /// Thickness of the band, metres.
    pub band_thickness: f64,
    /// How far the band's centre line stands above the head's surface at least, metres.
    pub clearance: f64,
    /// How far forward of the ears the band's plane lies, as a fraction of the head's radius.
    pub forward: f64,
    /// How far down the sides the band reaches, radians from the top.
    pub reach: f64,
    /// How tall the frill stands above the band, metres; 0 for none.
    pub frill: f64,
    /// How many gathers the frill has from end to end.
    pub gathers: u32,
    /// How far each gather swings forward and back, metres.
    pub ruffle: f64,
    /// The band.
    pub band: ColourRef,
    /// The frill.
    pub lace: ColourRef,
}

impl Default for HeaddressParams {
    fn default() -> Self {
        Self {
            band_width: 0.012,
            band_thickness: 0.004,
            clearance: 0.006,
            forward: 0.3,
            reach: 1.15,
            frill: 0.026,
            gathers: 22,
            ruffle: 0.004,
            band: ColourRef::rgb(0x26, 0x24, 0x2c),
            lace: ColourRef::rgb(0xf8, 0xf5, 0xee),
        }
    }
}

impl Validate for HeaddressParams {
    fn check(&self, c: &mut Check) {
        c.range("band_width", self.band_width, 0.004, 0.04);
        c.range("band_thickness", self.band_thickness, 0.002, 0.012);
        c.range("clearance", self.clearance, 0.0, 0.12);
        c.range("forward", self.forward, -0.3, 0.7);
        c.range("reach", self.reach, 0.4, 1.4);
        c.range("frill", self.frill, 0.0, 0.06);
        if !(4..=64).contains(&self.gathers) {
            c.fail("gathers", format!("{}: 4 to 64", self.gathers));
        }
        c.range("ruffle", self.ruffle, 0.0, 0.012);
    }
}

/// How far the band's underside stands clear of the hair the hair part publishes ([`HairReachFact`]), metres.
const HAIR_MARGIN: f64 = 0.003;
/// The frill sheet's half thickness, metres.
const FRILL_HALF: f64 = 0.0009;
/// How far into the band the frill's foot reaches, metres, so no gap shows.
const FRILL_FOOT: f64 = 0.001;
/// Samples along the arc per gather.
const PER_GATHER: usize = 12;

/// The `headdress` kind.
pub struct Headdress;

impl Headdress {
    /// The point of the arc at `alpha` (radians from the top, positive to the left) lifted `extra` beyond the band's
    /// centre line, and the outward direction there.
    fn on_arc(
        p: &HeaddressParams,
        head: &dyn omuzaki_char_api::HeadSurface,
        reach: Option<&HairReach>,
        alpha: f64,
        extra: f64,
    ) -> (DVec3, DVec3) {
        let along = DVec3::new(alpha.sin(), alpha.cos(), 0.0);
        let dir = (along + DVec3::Z * p.forward).normalize_or_zero();
        let hair = reach.map_or(0.0, |r| f64::from(r.above(dir.as_vec3())));
        let clearance = p.clearance.max(hair + 0.5 * p.band_thickness + HAIR_MARGIN);
        #[allow(clippy::cast_possible_truncation)] // metres
        let point = DVec3::from(head.point(dir.as_vec3(), (clearance + extra) as f32));
        (point, dir)
    }

    /// The band: a flat tube along the arc from `reach` on the left to `reach` on the right.
    fn band(
        p: &HeaddressParams,
        head: &dyn omuzaki_char_api::HeadSurface,
        reach: Option<&HairReach>,
        rows: usize,
        seg: usize,
    ) -> Result<Sweep, crate::kit::KitError> {
        let rings: Vec<Ring> = (0..=rows)
            .map(|k| {
                let (centre, dir) = Self::on_arc(p, head, reach, lerp(p.reach, -p.reach, fraction(k, rows)), 0.0);
                Ring { centre, dir, radii: (0.5 * p.band_width, 0.5 * p.band_thickness) }
            })
            .collect();
        framed_tube(&rings, seg)
    }

    /// The frill: a thin sheet standing out of the band's top, its middle line swung forward and back in gathers.
    fn frill(
        p: &HeaddressParams,
        head: &dyn omuzaki_char_api::HeadSurface,
        reach: Option<&HairReach>,
        seg: usize,
    ) -> Result<Sweep, crate::kit::KitError> {
        let rows = p.gathers as usize * PER_GATHER;
        let middle = 0.5 * p.band_thickness - FRILL_FOOT + 0.5 * p.frill;
        let rings: Vec<Ring> = (0..=rows)
            .map(|k| {
                let t = fraction(k, rows);
                let (point, dir) = Self::on_arc(p, head, reach, lerp(p.reach, -p.reach, t), middle);
                // A cosine, so the two halves mirror each other.
                let swing = p.ruffle * (std::f64::consts::TAU * f64::from(p.gathers) * t).cos();
                Ring { centre: point + across(dir) * swing, dir, radii: (FRILL_HALF, 0.5 * p.frill) }
            })
            .collect();
        framed_tube(&rings, seg)
    }
}

/// One ring of a tube along the arc: its centre, the arc's outward direction there, and its semi-axes across the band
/// (front to back) and out from the head.
struct Ring {
    centre: DVec3,
    dir: DVec3,
    radii: (f64, f64),
}

/// The direction across the band, front to back, square to the arc's outward direction `dir`.
fn across(dir: DVec3) -> DVec3 {
    (DVec3::Z - dir * dir.z).normalize_or_zero()
}

/// A closed tube through `rings`, each cut square to the head's front-back axis and the arc's outward direction, so a
/// mirrored arc gives a mirrored tube (a swept frame would drift); its end rings close to points.
fn framed_tube(rings: &[Ring], seg: usize) -> Result<Sweep, crate::kit::KitError> {
    let last = rings.len().saturating_sub(1);
    let grid = Grid::from_fn(rings.len(), seg + 1, |i, j| {
        let ring = &rings[i];
        if i == 0 || i == last {
            return ring.centre;
        }
        let theta = TAU * fraction(j % seg, seg);
        ring.centre + across(ring.dir) * (ring.radii.0 * theta.cos()) + ring.dir * (ring.radii.1 * theta.sin())
    })?;
    Ok(Sweep { grid, centres: rings.iter().map(|r| r.centre).collect() })
}

impl PartKind for Headdress {
    type Params = HeaddressParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "headdress",
            summary: "Build a headdress: a frilled headband over the hair, a little forward of the crown.",
            category: PartCategory::Accessory,
            requires: &["head"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &HeaddressParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let (attach, default) = head_frames(ctx)?;
        let detail = Detail::of(ctx.quality);
        let err = kit_error(ctx);
        let mut shells = Shells::new(&attach, &default, 2);
        let reach = ctx.published.get::<HairReachFact>().as_ref();
        let inside = |sw: &Sweep| {
            let centres = sw.centres.clone();
            move |i: usize, _: usize| centres[i]
        };
        let band = Self::band(p, head.as_ref(), reach, detail.rows, detail.lathe / 2).map_err(&err)?;
        shells.bind(0, &band.grid, (false, true), &inside(&band)).map_err(&err)?;
        if p.frill > 0.0 {
            let frill = Self::frill(p, head.as_ref(), reach, 6).map_err(&err)?;
            shells.bind(1, &frill.grid, (false, true), &inside(&frill)).map_err(&err)?;
        }
        shells.output(ctx, &Self::slots(p), RigContribution::default())
    }

    fn recolour(p: &HeaddressParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour(out, ctx, &Self::slots(p))
    }
}

impl Headdress {
    fn slots(p: &HeaddressParams) -> [Slot; 2] {
        [Slot::opaque("band", &p.band), Slot::opaque("lace", &p.lace)]
    }
}

omuzaki_char_api::register_part_kind!(Headdress);
