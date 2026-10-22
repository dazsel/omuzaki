//! The `trousers` garment (#195): both legs lofted from the waist over the thigh, knee and calf to the hem, with a leg
//! profile (straight, tapered, flared, baggy; shorts are a shorter leg), a waistband at the rise, a hem cuff and optional
//! crease and pocket lines.
//!
//! The prototype has no trousers: this is new design in the garment framework's language. It is layer
//! [`LAYER_TROUSERS`], between the shirt and the vest, and one mesh ([`rows`] says how its rows join: a pelvis tube round
//! both thighs from the waist to the crotch, then a leg each).

pub mod rows;
pub mod shape;

use omuzaki_char_api::{BodyRegion, BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use self::rows::{GUSSET, MIN_FALL, add_rows, crotch, leg_rows, pelvis_rows, tile_uvs};
use self::shape::{BAND_RAMP, Band, CUFF_RAMP, LegCut, LegProfile, Rise};
use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, MAX_EASE};
use super::loft::Surface;
use super::trim::piping::Cord;
use super::trim::{Finish, RailSpec, TrimSpec, add_trim};
use crate::kit::curve::fraction;
use crate::kit::grid::MeshBuilder;

/// The layer of trousers: over the shirt, under the vest and the blazer.
pub const LAYER_TROUSERS: i32 = 25;
/// The cut of every section: an ellipse.
pub const EXPONENT: f64 = 2.0;
/// The default ease, metres.
pub const TROUSERS_EASE: f64 = 0.004;
/// The default hem, as a fraction of the leg region: above the ankle, where the foot's heel begins.
pub const LENGTH: f64 = 0.92;
/// The longest hem: under it the cloth is clear of the foot region on every template.
pub const MAX_LENGTH: f64 = 0.95;
/// The shortest hem, as a fraction of the leg region: the leg parts a little over half way down the thigh on every body,
/// and a hem needs some leg under it ([`MIN_LEG`]).
pub const MIN_LENGTH: f64 = 0.55;
/// The least leg under the gusset, as a fraction of the leg region.
pub const MIN_LEG: f64 = 0.03;
/// Rows down the leg between the gusset and the hem, besides the cuff's.
pub const LEG_ROWS: usize = 24;
/// Rows of the torso part of the pelvis between the band and the bottom of the torso region.
pub const TORSO_ROWS: usize = 12;
/// The waistband's default height, as a fraction of the torso region.
pub const BAND_HEIGHT: f64 = 0.06;
/// The waistband's default thickness, metres.
pub const BAND_THICKNESS: f64 = 0.0015;
/// The tallest the waistband may be.
pub const MAX_BAND_HEIGHT: f64 = 0.12;
/// The thickest.
pub const MAX_BAND_THICKNESS: f64 = 0.01;
/// The default cuff height, as a fraction of the leg region.
pub const CUFF_HEIGHT: f64 = 0.05;
/// The tallest cuff, as a fraction of the leg region.
pub const MAX_CUFF_HEIGHT: f64 = 0.25;
/// The farthest a cuff may roll out, metres.
pub const MAX_CUFF_ROLL: f64 = 0.02;
/// The radius of a crease or a pocket line, metres.
const LINE_RADIUS: f64 = 0.0006;
/// How far the lines stand off the cloth, metres.
const LINE_LIFT: f64 = 0.0004;
/// The lines' default colour: a little darker than the cloth.
const ACCENT: ColourRef = ColourRef::rgb(0x2a, 0x2c, 0x38);
/// The open borders of the cloth: the waist and the two hems.
const BORDERS: usize = 3;

/// The waistband.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct WaistbandSpec {
    /// How tall it is, as a fraction of the torso region.
    pub height: f64,
    /// How far it stands out of the cloth, metres; 0 for none.
    pub thickness: f64,
}

impl Default for WaistbandSpec {
    fn default() -> Self {
        Self { height: BAND_HEIGHT, thickness: BAND_THICKNESS }
    }
}

/// The hem cuff: the last stretch of the leg rolled up, standing out of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct CuffSpec {
    /// How tall it is, as a fraction of the leg region.
    pub height: f64,
    /// How far it stands out of the leg, metres; 0 for no cuff.
    pub roll: f64,
}

impl Default for CuffSpec {
    fn default() -> Self {
        Self { height: CUFF_HEIGHT, roll: 0.0 }
    }
}

/// The parameters of a pair of trousers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct TrousersParams {
    /// The common garment settings.
    pub common: GarmentCommon,
    /// How wide the legs are at the thigh, knee and hem, as ratios of the body's section.
    pub leg: LegProfile,
    /// Where the hem is, as a fraction of the leg region from the hip (1 is the ankle). Shorts end above the knee.
    pub length: f64,
    /// Where the waistband's top edge sits.
    pub rise: Rise,
    /// The waistband.
    pub waistband: WaistbandSpec,
    /// The hem cuff.
    pub cuff: CuffSpec,
    /// A crease down the front of each leg.
    pub crease: bool,
    /// The pocket openings, one each side of the front.
    pub pockets: bool,
    /// The colour of the creases and the pocket lines.
    pub accent: ColourRef,
}

impl Default for TrousersParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: TROUSERS_EASE,
                fabric: FabricStyle::Plain { colour: ColourRef::rgb(0x3a, 0x3d, 0x4e) },
                ..GarmentCommon::default()
            },
            leg: LegProfile::default(),
            length: LENGTH,
            rise: Rise::default(),
            waistband: WaistbandSpec::default(),
            cuff: CuffSpec::default(),
            crease: false,
            pockets: false,
            accent: ACCENT,
        }
    }
}

impl Validate for TrousersParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.nested("leg", &self.leg);
        c.range("length", self.length, MIN_LENGTH, MAX_LENGTH);
        c.range("waistband/height", self.waistband.height, 0.01, MAX_BAND_HEIGHT);
        c.range("waistband/thickness", self.waistband.thickness, 0.0, MAX_BAND_THICKNESS);
        c.range("cuff/height", self.cuff.height, 0.01, MAX_CUFF_HEIGHT);
        c.range("cuff/roll", self.cuff.roll, 0.0, MAX_CUFF_ROLL);
        if self.common.ease.is_finite() && self.common.ease + self.waistband.thickness.max(self.cuff.roll) > MAX_EASE {
            c.fail(
                "common/ease",
                format!("the ease and the thickest of the band and the cuff stand out more than {MAX_EASE} m"),
            );
        }
    }
}

/// The trousers part kind.
pub struct Trousers;

impl PartKind for Trousers {
    type Params = TrousersParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "trousers",
            summary: "trousers with a leg profile, waistband, crotch gusset and optional cuffs",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &TrousersParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let fit = ctx.fit()?;
        let ease = p.common.ease;
        let torso = Surface::new(fit.clone(), BodyRegion::Torso, EXPONENT, ease).map_err(fail)?;
        let legs = [
            Surface::new(fit.clone(), BodyRegion::LegL, EXPONENT, ease).map_err(fail)?,
            Surface::new(fit, BodyRegion::LegR, EXPONENT, ease).map_err(fail)?,
        ];
        let laid = Layout::of(p, &torso, &legs).map_err(fail)?;
        let band = laid.band;
        let cut = laid.cut;
        let shaped_torso = torso.clone().with_shape(move |ring, t, phi| band.point(ring, t, phi));
        let shaped_legs = [0, 1].map(|i| legs[i].clone().with_shape(move |ring, t, phi| cut.point(ring, t, phi)));

        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let pelvis = pelvis_rows(&torso, &legs, &laid.torso_ts, laid.crotch, &|t, phi| shaped_torso.point(t, phi))
            .map_err(fail)?;
        let thighs = [legs[0].weights().chain()[0].0.clone(), legs[1].weights().chain()[0].0.clone()];
        let last = pelvis.last().ok_or_else(|| ctx.fail("the pelvis has no rows"))?;
        let mut leg_grids = Vec::new();
        for (side, (leg, shaped)) in [1.0, -1.0].into_iter().zip(legs.iter().zip(&shaped_legs)) {
            leg_grids.push(
                leg_rows(leg, side, last, laid.crotch, &laid.leg_ts, &|t, phi| shaped.point(t, phi), &thighs)
                    .map_err(fail)?,
            );
        }
        let mut builder = MeshBuilder::new();
        for grid in std::iter::once(&pelvis).chain(&leg_grids) {
            let uvs = out.tile().map(|tile| tile_uvs(grid, tile));
            add_rows(&mut builder, grid, uvs.as_deref()).map_err(fail)?;
        }
        out.surface(ctx, "body", builder)?;
        add_lines(p, &laid, &mut out, ctx, &shaped_torso, &shaped_legs)?;
        out.finish(ctx, &p.common, &[BodyRegion::Torso, BodyRegion::LegL, BodyRegion::LegR], EXPONENT)
    }
}

/// What the body decides about the trousers: where the legs part, the rows, the cut of the legs and the band.
struct Layout {
    crotch: f64,
    torso_ts: Vec<f64>,
    leg_ts: Vec<f64>,
    cut: LegCut,
    band: Band,
}

impl Layout {
    fn of(p: &TrousersParams, torso: &Surface, legs: &[Surface; 2]) -> Result<Self, GarmentError> {
        let bottom = torso.ring(0.0)?;
        let leg_top = legs[0]
            .project(bottom.centre)?
            .map(|projection| projection.t)
            .ok_or_else(|| GarmentError::invalid("legs", "no section of the leg passes through the torso's bottom"))?;
        let crotch = crotch(legs, leg_top + MIN_FALL, p.common.ease)?;
        let starts = crotch + GUSSET;
        let knee_at = legs[0]
            .weights()
            .chain()
            .get(1)
            .map(|(_, at)| *at)
            .ok_or_else(|| GarmentError::invalid("legs", "the fit has no knee"))?;
        if p.length < starts + MIN_LEG {
            return Err(GarmentError::invalid(
                "length",
                format!(
                    "{} ends above the crotch and its gusset ({starts:.3}) on this body: use at least {:.3}",
                    p.length,
                    starts + MIN_LEG
                ),
            ));
        }
        let cuff_height = if p.cuff.roll > 0.0 { p.cuff.height } else { 0.0 };
        let cut = LegCut {
            profile: (p.leg.thigh, p.leg.knee, p.leg.hem),
            knee_at,
            starts,
            hem: p.length,
            cuff_height,
            cuff_roll: p.cuff.roll,
        };
        let band = Band { top: p.rise.height(), height: p.waistband.height, thickness: p.waistband.thickness };
        let edge = band.top - band.height - BAND_RAMP;
        if edge < MIN_LEG {
            return Err(GarmentError::invalid(
                "waistband",
                format!(
                    "a band {} tall at the {:?} rise reaches the bottom of the torso: make it under {:.3}",
                    band.height,
                    p.rise,
                    band.top - BAND_RAMP - MIN_LEG
                ),
            ));
        }
        let mut torso_ts = vec![band.top, band.top - band.height, edge];
        torso_ts.extend((1..=TORSO_ROWS).map(|k| edge * (1.0 - fraction(k, TORSO_ROWS))));
        let mut leg_ts: Vec<f64> =
            (1..=LEG_ROWS).map(|k| starts + (p.length - starts) * fraction(k, LEG_ROWS)).collect();
        if cuff_height > 0.0 {
            let top = p.length - cuff_height;
            leg_ts.extend([top - CUFF_RAMP, top].into_iter().filter(|t| *t > starts));
        }
        leg_ts.sort_by(f64::total_cmp);
        leg_ts.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
        Ok(Self { crotch, torso_ts, leg_ts, cut, band })
    }
}

/// The crease down the front of each leg and the pocket openings, as thin cords in the accent colour.
fn add_lines(
    p: &TrousersParams,
    laid: &Layout,
    out: &mut GarmentOutput,
    ctx: &BuildCtx<'_>,
    torso: &Surface,
    legs: &[Surface; 2],
) -> Result<(), BuildError> {
    let cord = |points: &[[f64; 2]]| {
        TrimSpec::Cord(Cord {
            rail: RailSpec { points: points.to_vec(), closed: false, lift: LINE_LIFT, step: super::trim::RAIL_STEP },
            radius: LINE_RADIUS,
            segments: 4,
            finish: Finish::cloth(p.accent.clone()),
        })
    };
    if p.crease {
        let (from, to) = (laid.cut.starts + 0.10, p.length - laid.cut.cuff_height - 0.04);
        for (name, leg) in ["crease-l", "crease-r"].into_iter().zip(legs) {
            add_trim(out, ctx, leg, name, &cord(&[[from, 0.0], [to, 0.0]]))?;
        }
    }
    if p.pockets {
        let first = laid.band.top - laid.band.height - 0.03;
        let (second, third) = (first - 0.07, (first - 0.17).max(0.02));
        for (name, side) in ["pocket-l", "pocket-r"].into_iter().zip([1.0, -1.0]) {
            let points = [[first, side * 1.05], [second, side * 0.80], [third, side * 0.62]];
            add_trim(out, ctx, torso, name, &cord(&points))?;
        }
    }
    Ok(())
}

impl Garment for Trousers {
    fn common(params: &TrousersParams) -> &GarmentCommon {
        &params.common
    }

    fn regions(_: &TrousersParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso, BodyRegion::LegL, BodyRegion::LegR]
    }

    fn open_borders(_: &TrousersParams) -> usize {
        BORDERS
    }
}

omuzaki_char_api::register_part_kind!(Trousers);
