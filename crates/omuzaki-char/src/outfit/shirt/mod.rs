//! The `shirt` garment: a body, two sleeves and two cuffs over the torso and arms of whatever lies beneath it (CB-40;
//! the prototype's `uniform.js` 58-147).
//!
//! The shirt is the first layer of clothing ([`LAYER_SHIRT`]) and needs a body. It lofts its body from the hem to the
//! neck opening over the fit beneath it, runs a sleeve along each arm to a rolled cuff with a fold seam and a pearl
//! button, and publishes two things for the parts after it: its [`Envelope`](crate::outfit::common::Envelope), so a
//! vest fits over the shirt, and the `collar_ring` anchor, the centre of its neck opening, which the collar and the tie
//! hang on ([`neckwear`](crate::outfit::neckwear)).
//!
//! | Piece | Mesh | Built by |
//! |---|---|---|
//! | body, sleeves, cuffs, pearl buttons | `<part>-shirt` | [`torso`], [`sleeve`], [`cuff`] |
//! | the dark lining under a tucked hem | `<part>-lining` | [`torso`] |
//! | the cuffs' fold seams | `<part>-lines` | [`cuff`] |
//! | the buttons' thread holes | `<part>-holes` | [`cuff`] |
//!
//! Everything is fitted to the [`BodyFit`](omuzaki_char_api::BodyFit) the shirt is given: heights are fractions of the
//! torso's length, the sleeve follows the arm's joints and sections, and the cuff closes round the sleeve's end. Where
//! the prototype's numbers were fitted to its own body they appear here as the fractions and offsets of the same shirt
//! on the soft-sd template, named once.
//!
//! **Where the sleeve meets the body.** At the shoulder the arm and torso of a body overlap, and a garment must stay
//! outside both. The sleeve's shoulder end is open (the prototype closes it in a dome inside its arm), and the places
//! where the sleeve and the shirt's body overlap are pushed onto the outside of one another, so the shirt is the surface
//! of their union.

use omuzaki_anim_api::HumanoidBone;
use std::sync::Arc;

use omuzaki_char_api::{
    Anchor, AnchorId, AnchorsFact, BodyFit, BodyRegion, BuildCtx, BuildError, FitFact, JointRef, MaterialSpec,
    PartCategory, PartKind, PartMeta, PartOutput,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DVec2, DVec3, Quat};
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::{Outline, Rim, Shading, Toon};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use self::frame::Column;
use super::common::{DEFAULT_EASE, Envelope, Garment, GarmentCommon, GarmentError, GarmentOutput};
use super::loft::{RegionWeights, Ring};
use crate::kit::grid::{Grid, MeshBuilder};

pub mod cuff;
pub mod frame;
pub mod sleeve;
pub mod torso;
pub mod union;

/// The superellipse exponent of the shirt's rings: a little squarer than an ellipse (`uniform.js` `TE`).
pub const SHIRT_EXPONENT: f64 = 2.15;
/// The regions a shirt covers: the torso and arms, and the neck, whose lowest part the neck opening rises over (so the
/// collar and tie, which read the shirt's surface from its envelope, find the neck with the shirt's ease on it).
pub const SHIRT_REGIONS: [BodyRegion; 4] = [BodyRegion::Torso, BodyRegion::Neck, BodyRegion::ArmL, BodyRegion::ArmR];
/// The name of the anchor at the centre of the neck opening.
pub const COLLAR_RING: &str = "collar_ring";
/// The bones that compete for a sleeve's vertices, as for the body's arm: the chest, the shoulder, the upper arm, the
/// forearm and the hand.
pub(crate) const ARM_BONES: [JointRef; 5] = [
    JointRef::CHEST,
    JointRef::Humanoid(HumanoidBone::LeftShoulder),
    JointRef::Humanoid(HumanoidBone::LeftUpperArm),
    JointRef::Humanoid(HumanoidBone::LeftLowerArm),
    JointRef::Humanoid(HumanoidBone::LeftHand),
];

/// The shirt cloth's shading: shadows pulled in a little, a soft edge and a faint rim (`uniform.js` 440-441).
const CLOTH_SHIFT: f32 = -0.12;
const CLOTH_SOFTNESS: f32 = 0.2;
const CLOTH_RIM: (u32, f32) = (0x00df_e6ff, 0.14);
/// The lining's shading: a plain edge and no rim, so nothing bright shows under a hem.
const LINING_SOFTNESS: f32 = 0.2;
/// The width of the cloth's outline, metres at two metres.
const OUTLINE_WIDTH: f32 = 0.0015;

/// The most and least a hem may be raised, as a fraction of the torso's length: from just above the hips to below the
/// waist, where the shirt still reaches the sleeves.
const HEM_RANGE: (f64, f64) = (0.02, 0.55);
/// The most a hips end may be pulled in, as a fraction of its radius.
const MAX_TUCK: f64 = 0.12;
/// How far below the hem, as a fraction of the torso's length, the layers over the shirt have come back in to the body.
const HEM_FADE: f64 = 0.08;
/// The range of a sleeve's length, as a fraction of the arm.
const SLEEVE_LENGTH_RANGE: (f64, f64) = (0.3, 1.0);
/// The most a sleeve may stand off the arm beyond the shirt's ease, metres.
const MAX_SLEEVE_EXTRA: f64 = 0.02;
/// The range of a cuff's height, metres past the wrist.
const CUFF_HEIGHT_RANGE: (f64, f64) = (0.012, 0.06);
/// The range of the neck opening's rise above the torso, metres.
const NECK_RISE_RANGE: (f64, f64) = (0.004, 0.04);

/// The sleeves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct SleeveParams {
    /// How much of the arm the sleeve covers, from `0.3` to `1`. At `1` it runs to the wrist and into the cuff; a shorter
    /// sleeve ends open and has no cuff.
    pub length: f64,
    /// How far the sleeve stands off the arm beyond the shirt's ease, metres.
    pub width: f64,
    /// Extra ease at the elbow, metres, so a bent arm has room.
    pub ease_at_elbow: f64,
}

impl Default for SleeveParams {
    fn default() -> Self {
        Self { length: 1.0, width: 0.0035, ease_at_elbow: 0.0 }
    }
}

impl Validate for SleeveParams {
    fn check(&self, c: &mut Check) {
        c.range("length", self.length, SLEEVE_LENGTH_RANGE.0, SLEEVE_LENGTH_RANGE.1);
        c.range("width", self.width, 0.0, MAX_SLEEVE_EXTRA);
        c.range("ease_at_elbow", self.ease_at_elbow, 0.0, MAX_SLEEVE_EXTRA);
    }
}

/// The cuffs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct CuffParams {
    /// How far past the wrist the cuff reaches, metres.
    pub height: f64,
    /// Whether the cuff has a pearl button.
    pub button: bool,
}

impl Default for CuffParams {
    fn default() -> Self {
        Self { height: cuff::REFERENCE_HEIGHT, button: true }
    }
}

impl Validate for CuffParams {
    fn check(&self, c: &mut Check) {
        c.range("height", self.height, CUFF_HEIGHT_RANGE.0, CUFF_HEIGHT_RANGE.1);
    }
}

/// The neck opening.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct NeckParams {
    /// How far the opening stands above the top of the torso, metres: the shirt's collar stand. The opening follows the
    /// neck's own radius, so a collar hung on it fits the neck.
    pub rise: f64,
}

impl Default for NeckParams {
    fn default() -> Self {
        Self { rise: 0.018 }
    }
}

impl Validate for NeckParams {
    fn check(&self, c: &mut Check) {
        c.range("rise", self.rise, NECK_RISE_RANGE.0, NECK_RISE_RANGE.1);
    }
}

/// The shirt's colours beyond its fabric, each a palette slot or a fixed colour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ShirtColours {
    /// The tint of the cloth in shadow.
    pub shade: ColourRef,
    /// The outline of the cloth, the cuffs' fold seams and the buttons' thread holes.
    pub line: ColourRef,
    /// The dark lining under a tucked hem.
    pub lining: ColourRef,
    /// The lining's tint in shadow.
    pub lining_shade: ColourRef,
}

impl Default for ShirtColours {
    fn default() -> Self {
        Self {
            shade: ColourRef::rgb(0xc6, 0xc2, 0xd6),
            line: ColourRef::rgb(0x8f, 0x8a, 0x9c),
            lining: ColourRef::rgb(0x1d, 0x1c, 0x24),
            lining_shade: ColourRef::rgb(0x88, 0x80, 0x90),
        }
    }
}

impl Validate for ShirtColours {
    fn check(&self, _: &mut Check) {}
}

/// The parameters of a shirt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ShirtParams {
    /// The ease and the cloth (white by default).
    pub common: GarmentCommon,
    /// Where the hem sits, as a fraction of the torso's length from the hips (0) to the neck base (1).
    pub length: f64,
    /// How far the hips end is pulled in, as a fraction of its radius, so the pleats of a skirt over it hide it. A tucked
    /// shirt gets a dark lining under its hem. Zero leaves the shirt standing off the body everywhere.
    pub tuck: f64,
    /// The sleeves.
    pub sleeve: SleeveParams,
    /// The cuffs, on sleeves that reach the wrist.
    pub cuff: CuffParams,
    /// The neck opening.
    pub neck: NeckParams,
    /// The colours beyond the fabric.
    pub colours: ShirtColours,
}

impl Default for ShirtParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: DEFAULT_EASE,
                fabric: super::common::FabricStyle::Plain { colour: ColourRef::rgb(0xf5, 0xf2, 0xed) },
                ..GarmentCommon::default()
            },
            length: torso::DEFAULT_HEM,
            tuck: 0.0,
            sleeve: SleeveParams::default(),
            cuff: CuffParams::default(),
            neck: NeckParams::default(),
            colours: ShirtColours::default(),
        }
    }
}

impl Validate for ShirtParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.range("length", self.length, HEM_RANGE.0, HEM_RANGE.1);
        c.range("tuck", self.tuck, 0.0, MAX_TUCK);
        c.nested("sleeve", &self.sleeve);
        c.nested("cuff", &self.cuff);
        c.nested("neck", &self.neck);
        c.nested("colours", &self.colours);
    }
}

impl ShirtParams {
    /// Whether each sleeve ends in a cuff: only a sleeve that reaches the wrist has one.
    pub fn has_cuffs(&self) -> bool {
        self.sleeve.length >= 1.0
    }
}

/// The `shirt` part kind.
pub struct Shirt;

impl PartKind for Shirt {
    type Params = ShirtParams;

    fn layer() -> Option<i32> {
        Some(super::common::LAYER_SHIRT)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "shirt",
            summary: "Build a shirt: body, sleeves and cuffs fitted over the torso and arms.",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &ShirtParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let fit = ctx.fit()?;
        let column = Column::new(fit.clone(), SHIRT_EXPONENT).map_err(fail)?;
        let weights = RegionWeights::of(fit.as_ref(), BodyRegion::Torso).map_err(fail)?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let tile = out.tile();

        let body = torso::build(
            &column,
            &weights,
            &torso::TorsoSpec { ease: p.common.ease, hem: p.length, rise: p.neck.rise, tuck: p.tuck },
            tile,
        )
        .map_err(fail)?;
        let left = sleeve::build(
            fit.as_ref(),
            &sleeve::SleeveSpec {
                ease: p.common.ease,
                width: p.sleeve.width,
                ease_at_elbow: p.sleeve.ease_at_elbow,
                length: p.sleeve.length,
            },
            tile,
        )
        .map_err(fail)?;

        let mut shirt = body.shirt;
        let torso_vertices = shirt.vertex_count();
        let mut lines = MeshBuilder::new();
        let mut holes = MeshBuilder::new();
        let mut side = left.mesh.clone();
        if p.has_cuffs() {
            let height = cuff::clear_of_hand(fit.as_ref(), &left, p.cuff.height);
            let cuff = cuff::build(&left, &cuff::CuffSpec { height, button: p.cuff.button }).map_err(fail)?;
            side.merge(&cuff.cloth).map_err(|e| fail(e.into()))?;
            lines.merge(&cuff.lines).map_err(|e| fail(e.into()))?;
            holes.merge(&cuff.holes).map_err(|e| fail(e.into()))?;
        }
        let side_vertices = side.vertex_count();
        shirt.merge(&side).map_err(|e| fail(e.into()))?;
        shirt.merge(&side.mirrored()).map_err(|e| fail(e.into()))?;
        let lines = both_sides(lines).map_err(|e| fail(e.into()))?;
        let holes = both_sides(holes).map_err(|e| fail(e.into()))?;

        out.surface(ctx, "shirt", shirt)?;
        if let Some(lining) = body.lining {
            let colour = ctx.colour(&p.colours.lining)?;
            let shade = ctx.colour(&p.colours.lining_shade)?;
            let mut spec = MaterialSpec::toon(colour);
            spec.shading = Shading::Toon(Toon {
                shade,
                shift: 0.0,
                softness: LINING_SOFTNESS,
                rim: Rim { strength: 0.0, ..Rim::default() },
                ..Toon::default()
            });
            spec.vertex_colours = true;
            let material = out.material(ctx, "lining", spec)?;
            out.mesh(ctx, "lining", lining, material)?;
        }
        let line = ctx.colour(&p.colours.line)?;
        for (name, mesh) in [("lines", lines), ("holes", holes)] {
            if mesh.triangle_count() > 0 {
                let mut spec = MaterialSpec::toon(line);
                spec.shading = Shading::Unlit;
                let material = out.material(ctx, name, spec)?;
                out.mesh(ctx, name, mesh, material)?;
            }
        }
        let mut output = out.finish(ctx, &p.common, &SHIRT_REGIONS, SHIRT_EXPONENT)?;
        // The layers over the shirt (trousers on the hips) come back in to the body below its hem.
        let envelope = Envelope::new(ctx.fit()?, &SHIRT_REGIONS, p.common.ease, SHIRT_EXPONENT)
            .map_err(|e| ctx.fail(e.to_string()))?
            .ending(BodyRegion::Torso, p.length, HEM_FADE);
        let envelope: Arc<dyn BodyFit> = Arc::new(envelope);
        for fit in output.publishes.values_mut::<FitFact>() {
            *fit = Arc::clone(&envelope);
        }
        tune_cloth(&mut output, ctx, p)?;
        let sleeves = left.rows * (sleeve::SEGMENTS + 1);
        weld_shoulders(&mut output, ctx, &column, &left.arm, p.common.ease, [torso_vertices, side_vertices, sleeves])?;
        let anchor: AnchorId = COLLAR_RING.parse().map_err(|e| ctx.fail(format!("anchor `{COLLAR_RING}`: {e}")))?;
        let centre = body.opening.centre.as_vec3();
        output
            .publishes
            .put::<AnchorsFact>((anchor, Anchor { bone: JointRef::NECK, position: centre, rotation: Quat::IDENTITY }));
        Ok(output)
    }
}

impl Garment for Shirt {
    fn common(p: &ShirtParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &ShirtParams) -> Vec<BodyRegion> {
        SHIRT_REGIONS.to_vec()
    }

    /// The hem and the neck opening; the armhole in the body on each side; a tucked hem's lining; and on each sleeve its
    /// two ends and, with a cuff, the band's two edges.
    fn open_borders(p: &ShirtParams) -> usize {
        4 + usize::from(p.tuck > 0.0) + 2 * (2 + if p.has_cuffs() { 2 } else { 0 })
    }
}

omuzaki_char_api::register_part_kind!(Shirt);

/// Joins the body and the sleeves of the finished `<part>-shirt` mesh into one surface ([`union`]). `vertices` counts the
/// body's vertices, a whole side's (sleeve, cuff and button) and a sleeve's, which is the first piece of its side.
fn weld_shoulders(
    output: &mut PartOutput,
    ctx: &BuildCtx<'_>,
    column: &Column,
    arm: &union::Tube,
    ease: f64,
    [torso, side, sleeve]: [usize; 3],
) -> Result<(), BuildError> {
    let name = format!("{}-shirt", ctx.part);
    let mesh = output
        .meshes
        .iter_mut()
        .find(|m| m.name == name)
        .ok_or_else(|| ctx.fail(format!("the garment has no mesh `{name}`")))?;
    let right = arm.mirrored();
    let in_arm = |p: DVec3| arm.inside(p).max(right.inside(p));
    // The body's surface a sleeve is trimmed against: the shirt's own, a little over.
    let shell = |p: DVec3| -> Option<(Ring, DVec2)> {
        if !(column.bottom()..=column.neck_top()).contains(&p.y) {
            return None;
        }
        let ring = column.ring(p.y, ease + union::LIFT).ok()?;
        let d = p - ring.centre;
        Some((ring, DVec2::new(d.dot(ring.across), d.dot(ring.through))))
    };
    let in_body = |p: DVec3| shell(p).map_or(f64::NEG_INFINITY, |(ring, q)| -ring.along_ray(q).1);
    union::weld(
        &mut mesh.geometry,
        &[
            union::Cut { vertices: 0..torso, depth: &in_arm },
            union::Cut { vertices: torso..torso + sleeve, depth: &in_body },
            union::Cut { vertices: torso + side..torso + side + sleeve, depth: &in_body },
        ],
    )
    .map_err(|e| ctx.fail(e.to_string()))?;
    mesh.validate().map_err(|e| ctx.fail(format!("the welded shirt: {e}")))
}

/// `mesh` with its mirror image added: the right side of a left-side piece.
fn both_sides(mut mesh: MeshBuilder) -> Result<MeshBuilder, crate::kit::KitError> {
    let mirror = mesh.mirrored();
    mesh.merge(&mirror)?;
    Ok(mesh)
}

/// Gives the fabric material the shirt's look and the thin meshes their shadowless draw.
fn tune_cloth(output: &mut PartOutput, ctx: &BuildCtx<'_>, p: &ShirtParams) -> Result<(), BuildError> {
    let name = format!("{}-fabric", ctx.part);
    let shade = ctx.colour(&p.colours.shade)?;
    let line = ctx.colour(&p.colours.line)?;
    let spec = output
        .materials
        .iter_mut()
        .find(|(n, _)| n.as_str() == name)
        .map(|(_, spec)| spec)
        .ok_or_else(|| ctx.fail(format!("the garment has no material `{name}`")))?;
    cloth_look(spec, shade, line);
    // The fold seams and thread holes are ink: they cast no shadow.
    for mesh in &mut output.meshes {
        if mesh.name.ends_with("-lines") || mesh.name.ends_with("-holes") {
            mesh.casts_shadow = false;
        }
    }
    Ok(())
}

/// Gives `spec` the look of the prototype's shirt cloth: the `shade` tint, a soft edge, a faint rim, an outline in `line`
/// and the vertex colours that shade the sleeves and cuffs. The collar uses it too, so the two read as one cloth.
pub(crate) fn cloth_look(spec: &mut MaterialSpec, shade: Colour, line: Colour) {
    spec.shading = Shading::Toon(Toon {
        shade,
        shift: CLOTH_SHIFT,
        softness: CLOTH_SOFTNESS,
        rim: Rim { colour: Colour::hex(CLOTH_RIM.0), strength: CLOTH_RIM.1, ..Rim::default() },
        ..Toon::default()
    });
    spec.vertex_colours = true;
    spec.outline = Some(Outline { colour: line, width: OUTLINE_WIDTH });
}

/// Texture coordinates that count fabric tiles of `tile` metres over `grid`: `u` along each row (rounded to whole tiles
/// on a full turn, so the seam matches) and `v` along the centre line of the rows. `None` for a plain fabric.
pub(crate) fn tile_uvs(grid: &Grid, tile: Option<f64>, full_turn: bool) -> Option<Vec<DVec2>> {
    let tile = tile.filter(|t| t.is_finite() && *t > 0.0)?;
    let (rows, cols) = (grid.rows(), grid.cols());
    #[allow(clippy::cast_precision_loss)] // grid sizes are tiny
    let centre = |i: usize| (0..cols).map(|j| grid.at(i, j)).sum::<DVec3>() / cols as f64;
    let ring_length = |i: usize| (1..cols).map(|j| grid.at(i, j).distance(grid.at(i, j - 1))).sum::<f64>();
    #[allow(clippy::cast_precision_loss)]
    let width = (0..rows).map(ring_length).sum::<f64>() / rows as f64;
    let across = if full_turn { (width / tile).round().max(1.0) } else { width / tile };
    let mut out = Vec::with_capacity(rows * cols);
    let mut v = 0.0;
    for i in 0..rows {
        if i > 0 {
            v += centre(i).distance(centre(i - 1)) / tile;
        }
        out.extend((0..cols).map(|j| DVec2::new(across * crate::kit::curve::fraction(j, cols - 1), v)));
    }
    Some(out)
}
