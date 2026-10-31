//! The `arm-warmers` garment: detached sleeves, a close tube of cloth on each arm from the upper arm to past the wrist,
//! worn with a sleeveless top (a bodysuit, a halter dress) or over a shirt's sleeve. Opera gloves, gauntlet sleeves
//! and the flared cuffs of idol and mecha outfits are all this one garment at different settings.
//!
//! Each sleeve walks the arm's two bones as the shirt's sleeve does, from `top` (a fraction of the arm from the
//! shoulder) to `past` metres beyond the wrist, the arm's own radius plus the ease and the cloth's thickness at every
//! station, so it follows a slim or a broad arm. Toward its end it bells out by `flare`; a `cuff` band at the end takes
//! its own colour (the white turn-back of a gauntlet), and a darker band marks the top edge as on stockings. Skinned by
//! the capsules of the arm's bones, so a raised arm carries it as it carries the skin. The right sleeve is the left's
//! mirror. Both surfaces are drawn on both sides: the inside of a flared cuff shows.
//!
//! Layer: [`LAYER_ARM_WARMERS`], over a shirt or a bodysuit, under a vest.

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{
    BodyFit, BodyRegion, BuildCtx, BuildError, JointRef, MaterialSpec, PartCategory, PartKind, PartMeta, PartOutput,
};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::{Outline, Rim, Shading, Toon};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, LAYER_SHIRT};
use super::shirt::ARM_BONES;
use crate::kit::curve::{TAU, fraction, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::skin::{CapsuleParams, SegmentSet};

/// The layer arm warmers are worn on: over the shirt and the bodysuit, under the vest.
pub const LAYER_ARM_WARMERS: i32 = LAYER_SHIRT + 2;
/// Segments round each sleeve.
pub const SEGMENTS: usize = 24;
/// The spacing of the stations along the arm, metres.
pub const STATION_STEP: f64 = 0.01;
/// The most a sleeve may run past the wrist, metres: over the back of the hand, short of the knuckles.
pub const MAX_PAST: f64 = 0.05;
/// The most a cuff may bell out, metres.
pub const MAX_FLARE: f64 = 0.08;
/// The most the cuff band may be wide, metres.
pub const MAX_CUFF: f64 = 0.12;
/// The most the cloth may be thick, metres.
pub const MAX_THICKNESS: f64 = 0.008;
/// The top band's width, metres, and how much darker it is.
const BAND_WIDTH: f64 = 0.012;
const BAND_SHADE: f32 = 0.82;
/// How far a sleeve's first ring keeps clear of the chest, metres.
const TORSO_CLEARANCE: f64 = 0.003;
/// The torso is sampled this many times to tell whether a point is inside it.
const TORSO_SAMPLES: usize = 48;
/// The width of the cloth's outline, metres at two metres.
const OUTLINE_WIDTH: f32 = 0.0015;
/// The cloth's shading: shadows pulled in a little, a soft edge and a faint rim (the shirt's).
const CLOTH_SHIFT: f32 = -0.12;
const CLOTH_SOFTNESS: f32 = 0.2;

/// The parameters of the `arm-warmers` kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ArmWarmersParams {
    /// Ease over the arm and the cloth (`common.fabric`'s colour is the sleeve's).
    pub common: GarmentCommon,
    /// Where each sleeve starts, as a fraction of the arm from the shoulder: 0.15 high on the upper arm, 0.5 at the
    /// elbow, 0.75 a gauntlet on the forearm.
    pub top: f64,
    /// How far past the wrist it runs, metres, over the back of the hand.
    pub past: f64,
    /// How much wider its end is than the arm, metres, growing over the forearm: 0 a straight sleeve, more a bell.
    pub flare: f64,
    /// How thick the cloth is, metres.
    pub thickness: f64,
    /// The cuff band at the end, metres along the arm; 0 for none.
    pub cuff: f64,
    /// The cuff band's colour.
    pub cuff_colour: ColourRef,
    /// Whether a darker band marks the top edge.
    pub band: bool,
    /// The shadow colour of the cloth.
    pub shade: ColourRef,
    /// The outline's colour.
    pub line: ColourRef,
}

impl Default for ArmWarmersParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: 0.002,
                fabric: FabricStyle::Plain { colour: ColourRef::rgb(0x2a, 0x2c, 0x3a) },
                ..GarmentCommon::default()
            },
            top: 0.2,
            past: 0.01,
            flare: 0.012,
            thickness: 0.001,
            cuff: 0.0,
            cuff_colour: ColourRef::rgb(0xf2, 0xf4, 0xf8),
            band: true,
            shade: ColourRef::rgb(0x56, 0x58, 0x6c),
            line: ColourRef::rgb(0x14, 0x14, 0x1c),
        }
    }
}

impl Validate for ArmWarmersParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.range("top", self.top, 0.0, 0.85);
        c.range("past", self.past, 0.0, MAX_PAST);
        c.range("flare", self.flare, 0.0, MAX_FLARE);
        c.range("thickness", self.thickness, 0.0, MAX_THICKNESS);
        c.range("cuff", self.cuff, 0.0, MAX_CUFF);
    }
}

/// The `arm-warmers` kind.
pub struct ArmWarmers;

impl PartKind for ArmWarmers {
    type Params = ArmWarmersParams;

    fn layer() -> Option<i32> {
        Some(LAYER_ARM_WARMERS)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "arm-warmers",
            summary: "Build detached sleeves (arm warmers, opera gloves, gauntlets) on both arms: from the upper arm past \
                      the wrist, with a flared end and a cuff band in its own colour.",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &ArmWarmersParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let fit = ctx.fit()?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let left = sleeve(fit.as_ref(), p).map_err(fail)?;
        let mut cloth = left.cloth;
        cloth.merge(&cloth.mirrored()).map_err(|e| fail(e.into()))?;
        out.surface(ctx, "sleeves", cloth)?;
        if let Some(mut cuff) = left.cuff {
            cuff.merge(&cuff.mirrored()).map_err(|e| fail(e.into()))?;
            let material = out.material(ctx, "cuff", cuff_material(p, ctx)?)?;
            out.mesh(ctx, "cuffs", cuff, material)?;
        }
        let mut output = out.finish(ctx, &p.common, &Self::regions(p), super::loft::MIN_EXPONENT)?;
        let fabric = format!("{}-fabric", ctx.part);
        let (shade, line) = (ctx.colour(&p.shade)?, ctx.colour(&p.line)?);
        for (name, spec) in &mut output.materials {
            if name.as_str() == fabric {
                spec.shading = Shading::Toon(Toon {
                    shade,
                    shift: CLOTH_SHIFT,
                    softness: CLOTH_SOFTNESS,
                    rim: Rim { strength: 0.1, ..Rim::default() },
                    ..Toon::default()
                });
                spec.vertex_colours = true;
                spec.double_sided = true;
                spec.outline = Some(Outline { colour: line, width: OUTLINE_WIDTH });
            }
        }
        Ok(output)
    }
}

/// The cuff band: the cuff colour, toon shaded, both sides drawn.
fn cuff_material(p: &ArmWarmersParams, ctx: &BuildCtx<'_>) -> Result<MaterialSpec, BuildError> {
    let mut spec = MaterialSpec::toon(ctx.colour(&p.cuff_colour)?);
    spec.shading = Shading::Toon(Toon {
        shade: ctx.colour(&p.shade)?,
        shift: CLOTH_SHIFT,
        softness: CLOTH_SOFTNESS,
        ..Toon::default()
    });
    spec.double_sided = true;
    spec.outline = Some(Outline { colour: ctx.colour(&p.line)?, width: OUTLINE_WIDTH });
    Ok(spec)
}

impl Garment for ArmWarmers {
    fn common(p: &ArmWarmersParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &ArmWarmersParams) -> Vec<BodyRegion> {
        vec![BodyRegion::ArmL, BodyRegion::ArmR]
    }

    /// Each sleeve's two ends, and the cuff band's two when it has one.
    fn open_borders(p: &ArmWarmersParams) -> usize {
        2 * (2 + if p.cuff > 0.0 { 2 } else { 0 })
    }
}

omuzaki_char_api::register_part_kind!(ArmWarmers);

/// The left sleeve: its cloth and its cuff band.
struct Sleeve {
    cloth: MeshBuilder,
    cuff: Option<MeshBuilder>,
}

/// The shoulder, the elbow and the wrist of the left arm of `fit`.
fn arm(fit: &dyn BodyFit) -> Result<[DVec3; 3], GarmentError> {
    let end = |t: f32| {
        fit.section(BodyRegion::ArmL, t)
            .map(|s| s.centre.as_dvec3())
            .ok_or(GarmentError::NoSection { region: BodyRegion::ArmL, t: f64::from(t) })
    };
    let elbow = fit
        .joint(&JointRef::Humanoid(HumanoidBone::LeftLowerArm))
        .map(Vec3::as_dvec3)
        .ok_or_else(|| GarmentError::invalid("body", "the fit has no left elbow".to_owned()))?;
    Ok([end(0.0)?, elbow, end(1.0)?])
}

/// Builds the left sleeve of `p` over the arm of `fit`.
#[allow(clippy::many_single_char_names)] // the frame's `t`, `n`, `b` and `c`
fn sleeve(fit: &dyn BodyFit, p: &ArmWarmersParams) -> Result<Sleeve, GarmentError> {
    let [shoulder, elbow, wrist] = arm(fit)?;
    let upper = shoulder.distance(elbow);
    let lower = elbow.distance(wrist);
    let length = upper + lower;
    let forearm = (wrist - elbow)
        .try_normalize()
        .ok_or_else(|| GarmentError::invalid("body", "the left arm has no forearm".to_owned()))?;
    let start = p.top * length;
    let end = length + p.past;
    // The arm's own sections, by distance from the shoulder: the sleeve is centred and squared on them, so it follows
    // the arm's line through the elbow as the skin does; past the wrist it runs on along the forearm.
    #[allow(clippy::cast_possible_truncation)] // a fit is sampled in f32
    let section = |a: f64| -> Result<omuzaki_char_api::Section, GarmentError> {
        let t = (a / length).clamp(0.0, 1.0);
        fit.section(BodyRegion::ArmL, t as f32).ok_or(GarmentError::NoSection { region: BodyRegion::ArmL, t })
    };
    let at = |a: f64| -> Result<(DVec3, DVec3), GarmentError> {
        let s = section(a)?;
        let (centre, axis) = (s.centre.as_dvec3(), s.axis.as_dvec3());
        // Down the arm, whichever way the region's axis runs.
        let axis = if axis.dot(wrist - shoulder) < 0.0 { -axis } else { axis };
        Ok(if a > length { (centre + forearm * (a - length), forearm) } else { (centre, axis) })
    };
    let arm_radius = |a: f64| -> Result<f64, GarmentError> { Ok(f64::from(section(a)?.radii.max_element())) };
    // The bell grows from halfway down the forearm (or the sleeve's start, if lower) to the end.
    let bell_from = (upper + lower * 0.4).max(start).min(end - STATION_STEP);
    let radius = |a: f64| -> Result<f64, GarmentError> {
        Ok(arm_radius(a)? + p.common.ease + p.thickness + p.flare * sstep(bell_from, end, a).powf(1.5))
    };
    // The ring of radius `r` round the arm `a` metres from the shoulder, square to the line there. The frame's first
    // axis is the world's +z with the tangent cut out, as the shirt's sleeve's is.
    let ring_at = |a: f64, r: f64| -> Result<Vec<DVec3>, GarmentError> {
        let (c, t) = at(a)?;
        let t = t
            .try_normalize()
            .ok_or_else(|| GarmentError::invalid("arm-warmers", "the arm has no direction".to_owned()))?;
        let n = (DVec3::Z - t * DVec3::Z.dot(t))
            .try_normalize()
            .ok_or_else(|| GarmentError::invalid("arm-warmers", "the arm runs straight along z".to_owned()))?;
        let b = t.cross(n).normalize();
        Ok((0..=SEGMENTS)
            .map(|j| {
                let phi = TAU * fraction(j, SEGMENTS);
                c + (n * phi.cos() + b * phi.sin()) * r
            })
            .collect())
    };
    let torso = Torso::of(fit);
    let clear = |a: f64| -> Result<bool, GarmentError> {
        Ok(ring_at(a, radius(a)? + TORSO_CLEARANCE)?.iter().all(|q| !torso.contains(*q)))
    };
    // A sleeve set high on the arm starts where its ring first clears the chest.
    let mut start = start;
    while !clear(start)? {
        start += STATION_STEP;
        if start > upper {
            return Err(GarmentError::invalid("top", "the sleeve can't clear the chest above the elbow"));
        }
    }
    if end - start < 4.0 * STATION_STEP {
        return Err(GarmentError::invalid("top", "the sleeve would be shorter than 4 cm: start it higher up the arm"));
    }
    let cuff_from = (end - p.cuff).max(start + STATION_STEP);
    let segments = SegmentSet::from_fit(fit, &ARM_BONES)?;
    let skin = |q: DVec3, _: usize, _: usize| segments.weights_at(q, &ARM_BONES, CapsuleParams::PROTOTYPE);
    let tube = |from: f64, to: f64, band: bool| -> Result<MeshBuilder, GarmentError> {
        let mut stations = vec![from];
        let mut s = from + STATION_STEP;
        while s < to - STATION_STEP * 0.4 {
            stations.push(s);
            s += STATION_STEP;
        }
        stations.push(to);
        let points = stations.iter().map(|&a| Ok(at(a)?.0)).collect::<Result<Vec<DVec3>, GarmentError>>()?;
        let rings = stations.iter().map(|&a| ring_at(a, radius(a)?)).collect::<Result<Vec<_>, _>>()?;
        let grid = Grid::from_rows(&rings)?;
        let inside = |i: usize, _: usize| points[i];
        let colour = |i: usize, _: usize, _: DVec3| {
            let k = if band && stations[i] - from <= BAND_WIDTH { BAND_SHADE } else { 1.0 };
            [k, k, k, 1.0]
        };
        let options = GridOptions {
            wrap_j: true,
            inside: Some(&inside),
            colour: Some(&colour),
            skin: Skin::Per(&skin),
            ..GridOptions::default()
        };
        let mut mesh = MeshBuilder::with_colours();
        mesh.add_grid(&grid, &options)?;
        Ok(mesh)
    };
    let has_cuff = p.cuff > 0.0;
    let cloth = tube(start, if has_cuff { cuff_from } else { end }, p.band)?;
    let cuff = if has_cuff { Some(tube(cuff_from, end, false)?) } else { None };
    Ok(Sleeve { cloth, cuff })
}

/// The torso as stacked ellipses, to tell whether a point lies inside it.
struct Torso {
    sections: Vec<omuzaki_char_api::Section>,
}

impl Torso {
    fn of(fit: &dyn BodyFit) -> Self {
        #[allow(clippy::cast_possible_truncation)] // a fit is sampled in f32
        let sections = (0..=TORSO_SAMPLES)
            .filter_map(|k| fit.section(BodyRegion::Torso, fraction(k, TORSO_SAMPLES) as f32))
            .collect();
        Self { sections }
    }

    /// Whether `point` is inside the section nearest it in height (a superellipse a little squarer than an ellipse, so
    /// the test errs toward inside).
    fn contains(&self, point: DVec3) -> bool {
        let point = point.as_vec3();
        let height = |s: &omuzaki_char_api::Section| (s.centre.y - point.y).abs();
        let Some(nearest) = self.sections.iter().min_by(|a, b| height(a).total_cmp(&height(b))) else {
            return false;
        };
        let off = point - nearest.centre;
        let across = (off.x / nearest.radii.x.max(1e-4)).abs();
        let through = (off.z / nearest.radii.y.max(1e-4)).abs();
        across.powf(2.4) + through.powf(2.4) < 1.0
    }
}
