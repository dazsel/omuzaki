//! The `legwear` garment: tights, stockings and leggings, a skin of nylon lofted over the legs.
//!
//! The port of the prototype's `buildLeg` (`body.js` 12-51) and its `tights` material, as the Director's reference for
//! #193 gives them. Each leg is a tube lofted over the leg region of the fit it is worn over, `ease` outside it. Its
//! rows follow the prototype's: eight in the lowest 8 cm of the shin and 28 up the rest of the leg (`section`); its 44
//! columns go once round the leg. The skin
//! weights are the prototype's `W(x, y)`: the hips, the thigh and the shin handing over at the hip and the knee
//! (`skin`), and the vertex colour darkens the inner thigh and the back of the knee a little.
//!
//! **What differs from the prototype.** The prototype's tights go on up into the hips and close in a dome under the
//! skirt, and close in a cap under the ankle for the boot to hide; both ends are inside the body (the torso, the foot),
//! which a garment that sits outside the fit can't be. A `waist` top stops where the leg meets the torso's lower edge
//! ([`LegTop::Waist`]) and the garments above cover that edge; the bottom is an open cuff just above the foot. The
//! styles, the `top` and the `thickness` are new design; with the defaults of [`LegwearStyle::Tights`] the rows between
//! are the prototype's, row for row.
//!
//! Layer: [`LAYER_LEGWEAR`], between the underwear and the shirt.

mod section;
mod skin;

use omuzaki_char_api::{
    BodyRegion, BuildCtx, BuildError, MaterialName, MaterialSpec, PartCategory, PartColours, PartKind, PartMeta,
    PartOutput, RecolourCtx, Recoloured, register_part_kind,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::DVec3;
use omuzaki_core::id::Slug;
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::{Outline, ParamValue, ProgramParams, Shading};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{
    FabricStyle, Garment, GarmentCommon, GarmentError, LAYER_UNDERWEAR, publish_envelope, recolour_named,
};
use super::loft::MIN_EXPONENT;
use crate::kit::KitError;
use crate::kit::grid::{GridOptions, MeshBuilder, Skin};
use section::LegSections;
use skin::{BAND_WIDTH, LegSkin, Side};

pub use section::{COLUMNS, LegTop, MIDPLANE_GAP, PROTOTYPE_LEG_LENGTH, rows_of_leg};

/// The layer legwear is worn on: just above the underwear, under the shirt (the contract's 10 to 15).
pub const LAYER_LEGWEAR: i32 = LAYER_UNDERWEAR + 2;
/// The most the cloth may be thick, metres: it only moves the layer above outward.
pub const MAX_THICKNESS: f64 = 0.01;
/// The highest and lowest a bottom may end, as a fraction of the way from the waist edge to the knee.
pub const MIN_BOTTOM: f64 = 0.05;
/// See [`MIN_BOTTOM`]: past the knee to the ankle, where the legwear ends anyway.
pub const MAX_BOTTOM: f64 = 3.0;
/// The most sheen a legwear may have, as a multiple of the prototype's nylon sheen.
pub const MAX_SHEEN: f64 = 2.0;
/// The ease of new legwear, metres: tighter than the common default, as nylon is.
pub const DEFAULT_EASE: f64 = 0.002;
/// The thickness of new legwear, metres.
pub const DEFAULT_THICKNESS: f64 = 0.0005;
/// The sheen leggings keep, against tights: cotton and elastane are far less shiny than nylon.
pub const LEGGINGS_SHEEN: f64 = 0.3;
/// The coloured outline's width, metres at the renderer's reference distance.
const OUTLINE_WIDTH: f32 = 0.0022;
/// The registered name of the sheen shading program, and its colour parameter.
const SHEEN_PROGRAM: &str = "sheen";
const SHEEN_COLOUR: &str = "colour";
/// The prototype's sheen colour `0x453a38`, as the program stores it (sRGB bytes), at strength 1.
const SHEEN_RGB: [f64; 3] = [69.0, 58.0, 56.0];
/// The mesh and the material are named after the part with these.
const MESH: &str = "legs";
const MATERIAL: &str = "nylon";

/// What the legwear is.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LegwearStyle {
    /// Sheer, to the waist: the prototype's.
    #[default]
    Tights,
    /// To the thigh, with a darker band at the top.
    Stockings,
    /// Matte, to the waist.
    Leggings,
}

impl LegwearStyle {
    /// Where this style ends at the top when the parameters don't say.
    const fn default_top(self) -> LegTop {
        match self {
            Self::Tights | Self::Leggings => LegTop::Waist,
            Self::Stockings => LegTop::Thigh,
        }
    }

    /// Whether a darker band marks the top edge.
    const fn banded(self) -> bool {
        matches!(self, Self::Stockings)
    }

    /// How much of the nylon sheen the style keeps.
    const fn sheen(self) -> f64 {
        match self {
            Self::Tights | Self::Stockings => 1.0,
            Self::Leggings => LEGGINGS_SHEEN,
        }
    }
}

/// The parameters of the `legwear` kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct LegwearParams {
    /// Ease and cloth. The garment is coloured by `colour`, not by `common.fabric`, which must stay plain.
    pub common: GarmentCommon,
    /// Tights, stockings or leggings.
    pub style: LegwearStyle,
    /// Where it ends at the top; `None` for the style's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top: Option<LegTop>,
    /// Where it ends at the bottom, as a fraction of the way from the waist edge to the knee (0.2 hot pants or the
    /// bottom of a leotard, 0.8 bike shorts, past 1 on toward the ankle); `None` above the foot, as tights do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bottom: Option<f64>,
    /// How thick the cloth is, metres: the layer above sits this much farther out.
    pub thickness: f64,
    /// The cloth's colour (the prototype's `0x3b2b27`).
    pub colour: ColourRef,
    /// The outline's colour (`0x1a1210`).
    pub line: ColourRef,
    /// The nylon sheen's strength, as a multiple of the prototype's; 0 turns it off.
    pub sheen: f64,
}

impl Default for LegwearParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon { ease: DEFAULT_EASE, ..GarmentCommon::default() },
            style: LegwearStyle::default(),
            top: None,
            bottom: None,
            thickness: DEFAULT_THICKNESS,
            colour: ColourRef::rgb(0x3b, 0x2b, 0x27),
            line: ColourRef::rgb(0x1a, 0x12, 0x10),
            sheen: 1.0,
        }
    }
}

impl Validate for LegwearParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        if !matches!(self.common.fabric, FabricStyle::Plain { .. }) {
            c.fail("common/fabric", "legwear is coloured by its `colour`: leave the fabric plain");
        }
        if let Some(bottom) = self.bottom {
            c.range("bottom", bottom, MIN_BOTTOM, MAX_BOTTOM);
        }
        c.range("thickness", self.thickness, 0.0, MAX_THICKNESS);
        c.range("sheen", self.sheen, 0.0, MAX_SHEEN);
    }
}

impl LegwearParams {
    /// Where the legwear ends at the top.
    pub fn top(&self) -> LegTop {
        self.top.unwrap_or_else(|| self.style.default_top())
    }
}

/// The `legwear` kind.
pub struct Legwear;

impl PartKind for Legwear {
    type Params = LegwearParams;

    fn layer() -> Option<i32> {
        Some(LAYER_LEGWEAR)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "legwear",
            summary: "Build tights, stockings or leggings over the legs.",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &LegwearParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fit = ctx.fit()?;
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let fail_kit = |e: KitError| ctx.fail(e.to_string());
        let mut builder = MeshBuilder::with_colours();
        for side in Side::BOTH {
            let legs = LegSections::new(&fit, side.region(), p.common.ease, p.top(), p.bottom).map_err(fail)?;
            let weights = LegSkin::new(fit.as_ref(), side).map_err(fail)?;
            let grid = legs.grid().map_err(fail_kit)?;
            // The row of a vertex: its band is known by row, and rows are `COLUMNS` wide.
            let band = |i: usize| p.style.banded() && legs.depth_below_top(i) <= BAND_WIDTH;
            let inside = |i: usize, _: usize| legs.centre(i);
            let skin = |v: DVec3, _: usize, _: usize| weights.at(v.y);
            let colour = |i: usize, j: usize, v: DVec3| {
                let k = weights.tone(j, v.y, band(i));
                [k, k, k, 1.0]
            };
            let options = GridOptions {
                wrap_j: true,
                inside: Some(&inside),
                colour: Some(&colour),
                skin: Skin::Per(&skin),
                ..GridOptions::default()
            };
            builder.add_grid(&grid, &options).map_err(fail_kit)?;
        }
        let material_id = MaterialName::new(format!("{}-{MATERIAL}", ctx.part)).map_err(|e| ctx.fail(e.to_string()))?;
        let mesh = builder.finish(format!("{}-{MESH}", ctx.part), material_id.clone()).map_err(fail_kit)?;
        let material = material(p, ctx)?;
        // The cloth has a thickness the layer above must clear.
        let envelope = publish_envelope(ctx, &Self::regions(p), p.common.ease + p.thickness, MIN_EXPONENT)?;
        Ok(PartOutput::new()
            .with_meshes(vec![mesh])
            .with_materials(vec![(material_id, material)])
            .with_publishes(envelope))
    }

    /// The nylon is the one material, made by the build's own function; the vertex colours are tones, not colours.
    fn recolour(p: &LegwearParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        Ok(recolour_named(output, ctx.part, vec![(MATERIAL, material(p, ctx)?)], Vec::new()))
    }
}

/// The nylon: a vertex-coloured toon with a coloured outline and the sheen program, which adds the faint glow on the
/// faces that look at the camera (`tights`, `body.js` 166-172).
///
/// The sheen program shades with the renderer's default toon, so the prototype's own shade colour, terminator shift and
/// rim (`0x8a7880`, `-0.12`, `0x8a6a78`) don't reach the screen.
fn material<C: PartColours>(p: &LegwearParams, ctx: &C) -> Result<MaterialSpec, BuildError> {
    let strength = p.sheen * p.style.sheen();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to a byte
    let channel = |v: f64| (v * strength).round().clamp(0.0, 255.0) as u8;
    let sheen = Colour::rgb(channel(SHEEN_RGB[0]), channel(SHEEN_RGB[1]), channel(SHEEN_RGB[2]));
    let name = Slug::new(SHEEN_PROGRAM).map_err(|e| ctx.fail(e.to_string()))?;
    let colour = Slug::new(SHEEN_COLOUR).map_err(|e| ctx.fail(e.to_string()))?;
    let mut spec = MaterialSpec::toon(ctx.colour(&p.colour)?);
    spec.shading = Shading::Program { name, params: ProgramParams::from([(colour, ParamValue::Colour(sheen))]) };
    spec.vertex_colours = true;
    spec.outline = Some(Outline { colour: ctx.colour(&p.line)?, width: OUTLINE_WIDTH });
    Ok(spec)
}

impl Garment for Legwear {
    fn common(p: &LegwearParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &LegwearParams) -> Vec<BodyRegion> {
        vec![BodyRegion::LegL, BodyRegion::LegR]
    }

    /// Each leg's top edge and its cuff above the foot.
    fn open_borders(_: &LegwearParams) -> usize {
        2 * 2
    }
}

register_part_kind!(Legwear);
