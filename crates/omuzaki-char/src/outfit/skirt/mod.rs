//! The `skirt` garment: a knife-pleated skirt with a rolled hem, thin band lines above it and a plaid cloth.
//!
//! The port of the prototype's skirt (`uniform.js` 313-401), as the Director's reference for #192 gives it, made to fit
//! the body it is worn over:
//! - [`pleats`] is the surface: 12 rows by 145 columns, the base ellipse of the envelope pushed in by the folds, and the
//!   inner skin;
//! - [`hem`] is the rolled bead along the hem and the two band lines above it;
//! - [`plaid`] is the cloth, drawn through the build context's rasteriser;
//! - [`weights`] skins it: the hips, with the pleats over a thigh riding on it;
//! - [`fit`] derives the envelope from the body when the parameters give none, and [`cloth`] is the hook for simulating it.
//!
//! **Where it sits.** `top` is how far up the torso the waist is (a fraction of the torso) and `hem` how far down the
//! thigh the hem is (a fraction from the hip joint to the knee joint, never past [`MAX_HEM`], so it stays above the knee
//! on every template). With no `profile` the envelope is derived from the fit under the skirt, clear of it by the ease.
//! With one the profile is the garment's own and the fit isn't consulted for it: it is the prototype's table, in which
//! case the skirt is the prototype's, vertex for vertex.
//!
//! Layer: [`LAYER_SKIRT`], over the shirt and under the vest's hem.

pub mod cloth;
pub mod fit;
pub mod hem;
pub mod plaid;
pub mod pleats;
pub mod weights;

pub use weights::{PROTOTYPE_BLEND, PROTOTYPE_REACH, ThighPull};

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{
    BodyFit, BodyRegion, BuildCtx, BuildError, JointRef, MaterialName, MaterialSpec, PartCategory, PartColours,
    PartKind, PartMeta, PartOutput, RecolourCtx, Recoloured, TextureName, register_part_kind,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::{Outline, Rim, Shading, Toon};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{
    FabricStyle, Garment, GarmentCommon, GarmentError, LAYER_SHIRT, LAYER_VEST, fabric_tile, publish_envelope,
    recolour_named,
};
use super::loft::MIN_EXPONENT;
use crate::kit::KitError;
use crate::kit::curve::{lerp, sstep};
use crate::kit::grid::{GridOptions, MeshBuilder, Skin};
use hem::ROLL_TONE;
use pleats::{INNER_TONE, Plan, TILE};
use weights::SkirtSkin;

/// The layer a skirt is worn on: over the shirt, under the vest (the contract's 28 to 35).
pub const LAYER_SKIRT: i32 = i32::midpoint(LAYER_SHIRT, LAYER_VEST) + 3;
/// The prototype's pleat count.
pub const PLEATS: usize = 18;
/// The prototype's pleat cross-section: `(p, f)`, `p` the fraction of one pleat period and `f` the fold factor (`0` on a
/// crest, `-1` in the deepest valley). A long gentle slope and a sharp return: the knife-pleat asymmetry.
pub const PLEAT: [[f64; 2]; 8] = [
    [0.00, 0.00],
    [0.035, -0.06],
    [0.30, -0.36],
    [0.62, -0.72],
    [0.84, -0.96],
    [0.905, -1.00],
    [0.95, -0.55],
    [0.985, -0.10],
];
/// The prototype's fold depth at full amplitude, metres (`PLEAT_D`).
pub const PLEAT_DEPTH: f64 = 0.0125;
/// The prototype's roll radius, metres.
pub const HEM_ROLL: f64 = 0.00215;
/// The default waist: how far up the torso, as a fraction of it (the prototype's `0.396` against a torso from `0.325` to
/// `0.545`).
pub const DEFAULT_TOP: f64 = (0.396 - 0.325) / (0.545 - 0.325);
/// The default hem: how far down the thigh from the hip joint (`0.38`) to the knee joint (`0.19`), as a fraction (the
/// prototype's `0.2575`).
pub const DEFAULT_HEM: f64 = (0.38 - 0.2575) / (0.38 - 0.19);
/// The highest the hem may be, as a fraction of the thigh: just above the knee joint.
pub const MAX_HEM: f64 = 0.75;
/// The lowest the hem may be, as a fraction of the thigh.
pub const MIN_HEM: f64 = 0.1;
/// The waist's range, as fractions of the torso.
pub const MIN_TOP: f64 = 0.05;
/// See [`MIN_TOP`].
pub const MAX_TOP: f64 = 0.9;
/// The least a skirt's length may be, metres: less and its rows collapse.
const MIN_LENGTH: f64 = 0.02;
/// The prototype's reach, here for the default (`weights::PROTOTYPE_REACH`, which is not a `const fn` call away).
const PROTOTYPE_REACH_VALUE: f64 = weights::PROTOTYPE_REACH;
/// The default reach of a thigh's pull, as a multiple of its distance from the body's axis.
pub const DEFAULT_THIGH_REACH: f64 = PROTOTYPE_REACH_VALUE;
/// The default hand-over from the thighs to the hips, in fractions of a thigh above the hip joint.
pub const DEFAULT_HIP_BLEND: [f64; 2] = [0.0, 0.5];
/// The default share of a vertex a thigh may carry.
pub const DEFAULT_THIGH_CAP: f64 = 1.0;
/// The default flare of a derived profile at the hem, metres.
pub const DEFAULT_FLARE: f64 = 0.012;
/// The most flare, the deepest folds, the biggest roll, in metres.
pub const MAX_FLARE: f64 = 0.1;
/// See [`MAX_FLARE`].
pub const MAX_DEPTH: f64 = 0.04;
/// See [`MAX_FLARE`].
pub const MAX_ROLL: f64 = 0.01;
/// The most and fewest pleats.
pub const MAX_PLEATS: usize = 64;
/// See [`MAX_PLEATS`].
pub const MIN_PLEATS: usize = 3;
/// The most points in a pleat's cross-section or a profile curve.
pub const MAX_KNOTS: usize = 16;
/// The widest and narrowest half width of a profile, metres, and the farthest its centre may be off the axis.
pub const MAX_RADIUS: f64 = 0.5;
/// See [`MAX_RADIUS`].
pub const MIN_RADIUS: f64 = 0.01;
/// See [`MAX_RADIUS`].
pub const MAX_CENTRE: f64 = 0.1;
/// The skirt cloth's toon terminator shift and softness, and its rim: `skirt` in `uniform.js`.
const SHIFT: f32 = -0.1;
const SOFTNESS: f32 = 0.2;
const RIM_STRENGTH: f32 = 0.12;
const RIM_POWER: f32 = 3.0;
/// The outline's width, metres at the renderer's reference distance.
const OUTLINE_WIDTH: f32 = 0.0016;

/// The skirt's envelope: three curves by the fraction `s` of the way from the hem (`0`) to the waist (`1`), each as knots
/// `[s, value]` in ascending `s`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkirtProfile {
    /// The half width across the body of the base ellipse, metres.
    pub rx: Vec<[f64; 2]>,
    /// The half depth through the body, metres.
    pub rz: Vec<[f64; 2]>,
    /// How far in front of the body's axis the ellipse's centre is, metres.
    pub cz: Vec<[f64; 2]>,
}

impl SkirtProfile {
    fn check_curve(c: &mut Check, name: &str, knots: &[[f64; 2]], lo: f64, hi: f64) {
        if !(1..=MAX_KNOTS).contains(&knots.len()) {
            c.fail(name, format!("{} knots: a profile curve has 1 to {MAX_KNOTS}", knots.len()));
        }
        for (k, &[s, v]) in knots.iter().enumerate() {
            c.range(&format!("{name}/{k}/s"), s, 0.0, 1.0);
            c.range(&format!("{name}/{k}/value"), v, lo, hi);
        }
        if knots.windows(2).any(|w| w[1][0] <= w[0][0]) {
            c.fail(name, "the knots must rise in `s`");
        }
    }
}

impl Validate for SkirtProfile {
    fn check(&self, c: &mut Check) {
        Self::check_curve(c, "rx", &self.rx, MIN_RADIUS, MAX_RADIUS);
        Self::check_curve(c, "rz", &self.rz, MIN_RADIUS, MAX_RADIUS);
        Self::check_curve(c, "cz", &self.cz, -MAX_CENTRE, MAX_CENTRE);
    }
}

/// The pleats.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Pleats {
    /// How many pleats go round the skirt.
    pub count: usize,
    /// How deep the folds are at full amplitude, metres. They fade out toward the waist.
    pub depth: f64,
    /// One pleat's cross-section: `[p, f]` pairs, `p` the fraction of the pleat's width (rising from `0`), `f` the fold
    /// from `0` (a crest, on the base ellipse) to `-1` (the deepest valley). Folds only go inwards.
    pub fold: Vec<[f64; 2]>,
}

impl Default for Pleats {
    fn default() -> Self {
        Self { count: PLEATS, depth: PLEAT_DEPTH, fold: PLEAT.to_vec() }
    }
}

impl Validate for Pleats {
    fn check(&self, c: &mut Check) {
        if !(MIN_PLEATS..=MAX_PLEATS).contains(&self.count) {
            c.fail("count", format!("{} pleats: a skirt has {MIN_PLEATS} to {MAX_PLEATS}", self.count));
        }
        c.range("depth", self.depth, 0.0, MAX_DEPTH);
        if !(2..=MAX_KNOTS).contains(&self.fold.len()) {
            c.fail("fold", format!("{} points: a pleat has 2 to {MAX_KNOTS}", self.fold.len()));
        }
        for (k, &[p, f]) in self.fold.iter().enumerate() {
            c.range(&format!("fold/{k}/p"), p, 0.0, 1.0);
            c.range(&format!("fold/{k}/f"), f, -1.0, 0.0);
        }
        if self.fold.windows(2).any(|w| w[1][0] <= w[0][0]) {
            c.fail("fold", "the points must rise in `p`");
        }
        if self.fold.last().is_some_and(|&[p, _]| p >= 1.0) {
            c.fail("fold", "the last point must be before the next pleat starts (`p` below 1)");
        }
    }
}

/// The plaid's colours and how finely it is painted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Tartan {
    /// The ground.
    pub base: ColourRef,
    /// The broad, deeper bands.
    pub band: ColourRef,
    /// The thin lines either side of a band.
    pub line: ColourRef,
    /// The faint hairline down a band.
    pub hairline: ColourRef,
    /// The texture's side, pixels: the prototype's is 256; a larger one is the same picture, finer.
    pub resolution: u32,
}

impl Default for Tartan {
    fn default() -> Self {
        Self {
            base: ColourRef::rgb(0x2f, 0x33, 0x46),
            band: ColourRef::rgb(0x20, 0x23, 0x2f),
            line: ColourRef::rgb(0x6e, 0x50, 0x68),
            hairline: ColourRef::rgb(0xb9, 0xb3, 0xc6),
            resolution: plaid::DEFAULT_RESOLUTION,
        }
    }
}

impl Validate for Tartan {
    fn check(&self, c: &mut Check) {
        #[allow(clippy::cast_precision_loss)] // a texture side is tiny
        c.range(
            "resolution",
            f64::from(self.resolution),
            f64::from(plaid::MIN_RESOLUTION),
            f64::from(plaid::MAX_RESOLUTION),
        );
    }
}

/// The parameters of the `skirt` kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct SkirtParams {
    /// Ease and cloth. `common.fabric` is the cloth only when `plaid` is `None`.
    pub common: GarmentCommon,
    /// How far up the torso the waist is, as a fraction of the torso.
    pub top: f64,
    /// How far down the thigh the hem is, as a fraction of the way from the hip joint to the knee joint, up to
    /// [`MAX_HEM`] (the knee joint is `1`).
    pub hem: f64,
    /// The envelope, as the garment's own table; `None` derives it from the body under the skirt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<SkirtProfile>,
    /// How much wider than the body the derived envelope is at the hem, metres. Unused with an explicit `profile`.
    pub flare: f64,
    /// The pleats.
    pub pleats: Pleats,
    /// The radius of the rolled hem, metres; `0` for a plain edge.
    pub hem_roll: f64,
    /// The most of a vertex a thigh may carry, as a share: the pleats over a thigh ride on it. The prototype's is `0.95`;
    /// the default is the whole of it, so a skirt never stays behind a thigh that swings through it.
    pub thigh_cap: f64,
    /// How far from a thigh's centre line its pull fades, as a multiple of the thigh's distance from the body's axis
    /// (the prototype's `1.56`). A wider reach lets the pleats between the thighs ride on both.
    pub thigh_reach: f64,
    /// Whether the thighs hold only the pleats near them (the prototype's) or the whole skirt below the waist.
    pub thigh_pull: ThighPull,
    /// Where the skirt is handed over from the thighs to the hips, as heights above the hip joint in fractions of the
    /// thigh's length: the thighs carry it below the first, the hips above the second. The prototype's is `[-0.24, 0.24]`;
    /// the default is higher, as far up as the body's own thigh goes, so the skirt is never left behind on the hips where
    /// a thigh swings through it.
    pub hip_blend: [f64; 2],
    /// The plaid; `None` for the cloth of `common.fabric`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plaid: Option<Tartan>,
    /// The cloth's shadow side.
    pub shade: ColourRef,
    /// The cloth's rim light.
    pub rim: ColourRef,
    /// The cloth's outline.
    pub line: ColourRef,
    /// The band lines above the hem.
    pub band: ColourRef,
    /// The band lines' shadow side.
    pub band_shade: ColourRef,
}

impl Default for SkirtParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon::default(),
            top: DEFAULT_TOP,
            hem: DEFAULT_HEM,
            profile: None,
            flare: DEFAULT_FLARE,
            pleats: Pleats::default(),
            hem_roll: HEM_ROLL,
            thigh_cap: DEFAULT_THIGH_CAP,
            thigh_reach: DEFAULT_THIGH_REACH,
            thigh_pull: ThighPull::default(),
            hip_blend: DEFAULT_HIP_BLEND,
            plaid: Some(Tartan::default()),
            shade: ColourRef::rgb(0x7d, 0x8a, 0x90),
            rim: ColourRef::rgb(0x6f, 0x9a, 0x86),
            line: ColourRef::rgb(0x0d, 0x1a, 0x14),
            band: ColourRef::rgb(0x7a, 0x73, 0x52),
            band_shade: ColourRef::rgb(0x6a, 0x70, 0x60),
        }
    }
}

impl Validate for SkirtParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.range("top", self.top, MIN_TOP, MAX_TOP);
        c.range("hem", self.hem, MIN_HEM, MAX_HEM);
        if let Some(profile) = &self.profile {
            c.nested("profile", profile);
        }
        c.range("flare", self.flare, 0.0, MAX_FLARE);
        c.nested("pleats", &self.pleats);
        c.range("hem_roll", self.hem_roll, 0.0, MAX_ROLL);
        c.range("thigh_cap", self.thigh_cap, weights::MIN_THIGH_CAP, 1.0);
        for (k, edge) in self.hip_blend.iter().enumerate() {
            c.range(&format!("hip_blend/{k}"), *edge, -weights::MAX_BLEND, weights::MAX_BLEND);
        }
        if self.hip_blend[1] <= self.hip_blend[0] {
            c.fail("hip_blend", "the hand-over must end above where it starts");
        }
        c.range("thigh_reach", self.thigh_reach, weights::MIN_REACH, weights::MAX_REACH);
        if let Some(plaid) = &self.plaid {
            c.nested("plaid", plaid);
        }
    }
}

/// The `skirt` kind.
pub struct Skirt;

impl PartKind for Skirt {
    type Params = SkirtParams;

    fn layer() -> Option<i32> {
        Some(LAYER_SKIRT)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "skirt",
            summary: "Build a knife-pleated skirt with a rolled hem and a plaid cloth over the hips.",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &SkirtParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fit = ctx.fit()?;
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let fail_kit = |e: KitError| ctx.fail(e.to_string());
        let (hem, top) = heights(fit.as_ref(), p).map_err(fail)?;
        let columns = pleats::columns(p.pleats.count, &p.pleats.fold);
        let plan = plan(fit.as_ref(), p, hem, top).map_err(fail)?;
        let skins = pleats::skins(&plan, &columns).map_err(fail_kit)?;
        let skin = SkirtSkin::new(fit.as_ref(), p.thigh_cap, p.thigh_reach, p.hip_blend, p.thigh_pull).map_err(fail)?;
        let cloth = cloth(p, ctx)?;

        let arcs = pleats::arc_lengths(&skins.outer);
        let v_reference = plan.height(pleats::V_REFERENCE);
        let tile = cloth.tile;
        let reference = |i: usize, j: usize| pleats::inside(skins.outer.at(i, j).y);
        let uv = |i: usize, j: usize| {
            omuzaki_core::glam::DVec2::new(arcs[j] / tile, 1.0 - (v_reference - skins.outer.at(i, j).y) / tile)
        };
        let weights = |v: DVec3, _: usize, _: usize| skin.at(v);
        let outer_colour = |_: usize, j: usize, v: DVec3| {
            let valley = (-columns[j].fold).max(0.0).powf(pleats::VALLEY_EXPONENT);
            let light = sstep(pleats::SHADE_TOP, pleats::SHADE_BOTTOM, plan.fraction(v.y));
            #[allow(clippy::cast_possible_truncation)] // a grey in 0..=1; the mesh stores f32
            let k = ((1.0 - pleats::VALLEY_DARKEN * valley) * lerp(0.88, 1.0, light)) as f32;
            [k, k, k, 1.0]
        };
        let inner_colour = |_: usize, _: usize, _: DVec3| [INNER_TONE[0], INNER_TONE[1], INNER_TONE[2], 1.0];

        let mut cloth_mesh = MeshBuilder::with_colours();
        cloth_mesh
            .add_grid(
                &skins.outer,
                &GridOptions {
                    wrap_j: true,
                    inside: Some(&reference),
                    uv: Some(&uv),
                    colour: Some(&outer_colour),
                    skin: Skin::Per(&weights),
                    ..GridOptions::default()
                },
            )
            .map_err(fail_kit)?;
        cloth_mesh
            .add_grid(
                &skins.inner,
                &GridOptions {
                    wrap_j: true,
                    invert: true,
                    inside: Some(&reference),
                    uv: Some(&uv),
                    colour: Some(&inner_colour),
                    skin: Skin::Per(&weights),
                    ..GridOptions::default()
                },
            )
            .map_err(fail_kit)?;
        if p.hem_roll > 0.0 {
            let tone = |_: usize, _: usize, _: DVec3| [ROLL_TONE[0], ROLL_TONE[1], ROLL_TONE[2], 1.0];
            hem::add_roll(&mut cloth_mesh, &skins, p.hem_roll, (&arcs, tile), &tone, &weights).map_err(fail_kit)?;
        }
        let mut band_mesh = MeshBuilder::new();
        hem::add_bands(&mut band_mesh, &plan, &columns, &weights).map_err(fail_kit)?;

        let cloth_id = name::<MaterialName>(ctx, "cloth")?;
        let band_id = name::<MaterialName>(ctx, "band")?;
        let cloth_mesh = cloth_mesh.finish(format!("{}-pleats", ctx.part), cloth_id.clone()).map_err(fail_kit)?;
        let mut band_mesh = band_mesh.finish(format!("{}-band", ctx.part), band_id.clone()).map_err(fail_kit)?;
        // The band lines are thin and sit on the cloth: they cast no shadow of their own.
        band_mesh.casts_shadow = false;
        let envelope = publish_envelope(ctx, &Self::regions(p), p.common.ease, MIN_EXPONENT)?;
        let mut output = PartOutput::new()
            .with_meshes(vec![cloth_mesh, band_mesh])
            .with_materials(vec![(cloth_id, cloth.material), (band_id, band_material(p, ctx)?)])
            .with_publishes(envelope);
        if let Some(texture) = cloth.texture {
            output.textures.push(texture);
        }
        let drape = self::cloth::drape(skins.outer.rows(), skins.outer.cols());
        super::drape::simulate(&p.common.cloth, &drape, ctx, &mut output)?;
        Ok(output)
    }

    /// The cloth (with its plaid or fabric tile) and the band, made by the build's own functions; the pleats' vertex
    /// colours are tones and the drape is geometry, so both are kept.
    fn recolour(p: &SkirtParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        let cloth = cloth(p, ctx)?;
        let textures = cloth.texture.map(|(_, data)| ("plaid", data)).into_iter().collect();
        let materials = vec![("cloth", cloth.material), ("band", band_material(p, ctx)?)];
        Ok(recolour_named(output, ctx.part, materials, textures))
    }
}

impl Garment for Skirt {
    fn common(p: &SkirtParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &SkirtParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso]
    }

    /// The outer and inner skins each have their waist edge and their hem edge open; the roll, the band lines and the
    /// seams of the closed turn are closed.
    fn open_borders(_: &SkirtParams) -> usize {
        4
    }
}

register_part_kind!(Skirt);

/// The waist and hem heights over `fit`: `(hem, top)`, metres.
fn heights(fit: &dyn BodyFit, p: &SkirtParams) -> Result<(f64, f64), GarmentError> {
    #[allow(clippy::cast_possible_truncation)] // a fit is sampled in f32
    let top = fit
        .section(BodyRegion::Torso, p.top as f32)
        .ok_or(GarmentError::NoSection { region: BodyRegion::Torso, t: p.top })?
        .centre
        .y;
    let joint = |bone| {
        fit.joint(&JointRef::Humanoid(bone))
            .map(|j| f64::from(j.y))
            .ok_or_else(|| GarmentError::invalid("joint", format!("the fit has no {bone:?} joint")))
    };
    let (hip, knee) = (joint(HumanoidBone::LeftUpperLeg)?, joint(HumanoidBone::LeftLowerLeg)?);
    let (top, hem) = (f64::from(top), hip - p.hem * (hip - knee));
    if top - hem < MIN_LENGTH || top.is_nan() || hem.is_nan() {
        return Err(GarmentError::invalid(
            "hem",
            format!("the waist at {top:.3} m is not at least {MIN_LENGTH} m above the hem at {hem:.3} m"),
        ));
    }
    Ok((hem, top))
}

/// The plan of the pleats: the explicit profile, or the one derived from `fit`.
fn plan(fit: &dyn BodyFit, p: &SkirtParams, hem: f64, top: f64) -> Result<Plan, GarmentError> {
    let profile = if let Some(profile) = &p.profile {
        profile.clone()
    } else {
        let [rx, rz, cz] = fit::derive(fit, hem, top, p.common.ease, p.pleats.depth, p.flare)?;
        SkirtProfile { rx, rz, cz }
    };
    Ok(Plan::new(hem, top, [&profile.rx, &profile.rz, &profile.cz], p.pleats.depth)?)
}

/// The cloth: its material, its texture, and the length of one repeat of the texture, metres.
struct Cloth {
    material: MaterialSpec,
    texture: Option<(TextureName, omuzaki_char_api::TextureData)>,
    tile: f64,
}

/// The id `<part>-<suffix>` as a `T`.
fn name<T: std::str::FromStr>(ctx: &impl PartColours, suffix: &str) -> Result<T, BuildError>
where
    T::Err: std::fmt::Display,
{
    let id = format!("{}-{suffix}", ctx.part());
    id.parse::<T>().map_err(|e| ctx.fail(format!("`{id}` isn't a valid name: {e}")))
}

/// The cloth's toon: shaded, with the rim and the outline, in vertex colours.
fn cloth<C: PartColours>(p: &SkirtParams, ctx: &C) -> Result<Cloth, BuildError> {
    let (base, texture, tile) = match &p.plaid {
        Some(tartan) => {
            let colours = plaid::Colours {
                base: ctx.colour(&tartan.base)?,
                band: ctx.colour(&tartan.band)?,
                line: ctx.colour(&tartan.line)?,
                hairline: ctx.colour(&tartan.hairline)?,
            };
            let list = plaid::draw(&colours);
            let size = omuzaki_core::glam::UVec2::splat(tartan.resolution);
            let data = ctx.paint_texture(size, plaid::plane(), &list, plaid::desc(tartan.resolution))?;
            (Colour::rgb(255, 255, 255), Some((name::<TextureName>(ctx, "plaid")?, data)), TILE)
        }
        None => match fabric_tile(&p.common.fabric, 1, |c| ctx.colour(c), |m| ctx.fail(m))? {
            Some(tile) => {
                let data = tile.paint(|size, rect, list, desc| ctx.paint_texture(size, rect, list, desc))?;
                (Colour::rgb(255, 255, 255), Some((name::<TextureName>(ctx, "plaid")?, data)), tile.size)
            }
            None => match &p.common.fabric {
                FabricStyle::Plain { colour } => (ctx.colour(colour)?, None, TILE),
                _ => return Err(ctx.fail("a patterned fabric painted no tile")),
            },
        },
    };
    let mut material = MaterialSpec::toon(base);
    material.base_texture = texture.as_ref().map(|(id, _)| id.clone());
    material.shading = Shading::Toon(Toon {
        shade: ctx.colour(&p.shade)?,
        shift: SHIFT,
        softness: SOFTNESS,
        rim: Rim { colour: ctx.colour(&p.rim)?, power: RIM_POWER, strength: RIM_STRENGTH },
        emissive: Colour::BLACK,
        skin: None,
    });
    material.vertex_colours = true;
    material.outline = Some(Outline { colour: ctx.colour(&p.line)?, width: OUTLINE_WIDTH });
    Ok(Cloth { material, texture, tile })
}

/// The band lines: a plain toon without a rim or an outline.
fn band_material<C: PartColours>(p: &SkirtParams, ctx: &C) -> Result<MaterialSpec, BuildError> {
    let mut material = MaterialSpec::toon(ctx.colour(&p.band)?);
    material.shading = Shading::Toon(Toon {
        shade: ctx.colour(&p.band_shade)?,
        shift: SHIFT,
        softness: SOFTNESS,
        rim: Rim { colour: Colour::BLACK, power: RIM_POWER, strength: 0.0 },
        emissive: Colour::BLACK,
        skin: None,
    });
    material.outline = None;
    Ok(material)
}
