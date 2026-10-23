//! The `boots` garment: leather boots with a chunky toe, a welted sole and low heel, a turned-down cuff, buckle straps,
//! stitching, seams, ankle creases and a pull tab (footwear, layer [`LAYER_FOOTWEAR`], over the legwear).
//!
//! Built on the left foot and mirrored for the right, skinned to the lower leg (shaft), foot, and toes (toe cap), blended
//! at the ankle. The prototype's `boots.js` is the reference; this module owns its upper, sole and cuff, and the pieces
//! the details are made of (`strap`, `buckle`, `stitch`, `tab`) are used by plain boots and by the shoes alike.
//!
//! | Module | Builds |
//! |---|---|
//! | [`profile`] | the outline `plan(y, phi, off)` every piece is laid on, the map from boot space to the body, the skin |
//! | `upper`, `sole`, `cuff` | the leather upper with its creases; the welted sole and heel block; the turned-down cuff |
//! | `strap`, `buckle`, `stitch`, `tab` | the straps; brass buckles with their leather tails; stitch rows and seam tubes; the pull tab |
//!
//! **Fitting.** Without a [`BootProfile`] the boot is derived from the body it is built over: its bottom sits one ease
//! (and the heel's drop below the sole) under the lowest point of the foot, the cuff top at [`BootHeight`] up the shin,
//! and the outline is the superellipse that holds the foot and leg, an ease clear of them at every height, as wide below
//! the foot's widest as at it. So the boot stands below the template's floor line by its sole and the ease: a boot adds
//! height, and the body underneath is not moved. With a profile (the sample character's, kept in her private parameter
//! file) the boot is exactly the prototype's, sole on the floor.
//!
//! **Detail.** `Quality::Offline` builds the prototype's tessellation. `Quality::Live` keeps every feature but with
//! fewer segments, so the details (straps, buckles, stitching, seams, tab) stay near [`LIVE_DETAIL_TRIANGLES`] a boot.

// The geometry's own names (a point, a radius, an angle, a pair of rings), and vertex colours, which the mesh stores in `f32`.
#![allow(clippy::many_single_char_names, clippy::similar_names, clippy::cast_possible_truncation)]

mod buckle;
pub(crate) mod cuff;
pub mod profile;
pub(crate) mod sole;
mod stitch;
mod strap;
mod tab;
mod upper;

use omuzaki_char_api::{
    BodyRegion, BuildCtx, BuildError, KindProvenance, MaterialClass, MaterialName, MaterialSpec, PartCategory,
    PartKind, PartMeta, PartOutput, Quality,
};
use omuzaki_core::colour::Colour;
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_render_api::{Outline, Rim, Shading, Toon};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use profile::{BOOT_TOP, Boot, BootProfile};

use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, publish_envelope};
use super::trim::{Finish, MetalStyle};
use crate::kit::curve::lerp;
use crate::kit::grid::MeshBuilder;

/// The layer footwear is built on: above the legwear and the garments of [`LAYER_OUTER`](super::common::LAYER_OUTER).
pub const LAYER_FOOTWEAR: i32 = 45;
/// The triangles one boot's details (straps, buckles, stitching, seams and tab) are held near at `Quality::Live`.
pub const LIVE_DETAIL_TRIANGLES: usize = 3000;

/// The exponent of the squarest sole outline (`roundness` 0); an ellipse at `roundness` 1.
const MAX_SQUARENESS: f64 = 3.0;

// ----------------------------------------------------------------------------------------------------- parameters

/// How high the boot reaches.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BootHeight {
    /// Just over the ankle bone.
    Ankle,
    /// Half way up the shin (the prototype's).
    #[default]
    MidCalf,
    /// Up to just under the knee.
    Knee,
}

/// The toe box.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Toe {
    /// How much wider the toe box is than the heel, as a fraction of the width, `0` to `0.3`.
    pub width: f64,
    /// How round the sole's outline is: `1` an ellipse, `0` the squarest (a chunky, boxy toe), `0` to `1`.
    pub roundness: f64,
    /// Whether the toe has a cap: the arched seam over the vamp, with its stitching.
    pub cap: bool,
}

impl Default for Toe {
    fn default() -> Self {
        Self { width: 0.075, roundness: 0.35, cap: true }
    }
}

impl Toe {
    /// The superellipse exponent of the outline at the sole.
    pub fn squareness(&self) -> f64 {
        lerp(MAX_SQUARENESS, 2.0, self.roundness)
    }
}

impl Validate for Toe {
    fn check(&self, c: &mut Check) {
        c.range("width", self.width, 0.0, 0.3);
        c.range("roundness", self.roundness, 0.0, 1.0);
    }
}

/// The heel block under the sole.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Heel {
    /// How far forward from the back of the boot the heel's breast is, metres.
    pub length: f64,
    /// The heel's height above the floor line of the sole's bottom, metres.
    pub height: f64,
}

impl Default for Heel {
    fn default() -> Self {
        Self { length: 0.056, height: 0.020 }
    }
}

impl Validate for Heel {
    fn check(&self, c: &mut Check) {
        c.range("length", self.length, 0.01, 0.12);
        c.range("height", self.height, 0.002, 0.06);
    }
}

/// The sole.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Sole {
    /// How thick it is under the foot, metres.
    pub thickness: f64,
    /// How far the welt stands out round the upper, metres.
    pub welt: f64,
    /// The sole's colour.
    pub colour: ColourRef,
    /// Its shadow side.
    pub shade: ColourRef,
    /// Its outline.
    pub line: ColourRef,
}

impl Default for Sole {
    fn default() -> Self {
        Self {
            thickness: 0.0115,
            welt: 0.0025,
            colour: ColourRef::rgb(0x24, 0x18, 0x12),
            shade: ColourRef::rgb(0x8a, 0x7a, 0x78),
            line: ColourRef::rgb(0x12, 0x0a, 0x06),
        }
    }
}

impl Validate for Sole {
    fn check(&self, c: &mut Check) {
        c.range("thickness", self.thickness, 0.004, 0.04);
        c.range("welt", self.welt, 0.0, 0.01);
    }
}

/// The leather, besides its base colour (the garment's `fabric`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Leather {
    /// Its shadow side.
    pub shade: ColourRef,
    /// Its outline.
    pub line: ColourRef,
    /// Whether the ankle creases dent the front of the boot.
    pub creases: bool,
}

impl Default for Leather {
    fn default() -> Self {
        Self { shade: ColourRef::rgb(0x84, 0x70, 0x7a), line: ColourRef::rgb(0x24, 0x14, 0x0c), creases: true }
    }
}

impl Validate for Leather {
    fn check(&self, _: &mut Check) {}
}

/// The turned-down cuff.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Cuff {
    /// Half the roll's height, metres.
    pub height: f64,
    /// Half the roll's thickness: how far the fold stands out, metres.
    pub fold: f64,
}

impl Default for Cuff {
    fn default() -> Self {
        Self { height: 0.0100, fold: 0.0034 }
    }
}

impl Validate for Cuff {
    fn check(&self, c: &mut Check) {
        c.range("height", self.height, 0.002, 0.03);
        c.range("fold", self.fold, 0.001, 0.01);
    }
}

/// The buckle straps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Straps {
    /// How many, `0` to `3`: spread between the prototype's two. Each strap has its own buckle.
    pub count: u8,
    /// Half the lowest strap's height, metres; the upper ones are a little narrower.
    pub height: f64,
}

impl Default for Straps {
    fn default() -> Self {
        Self { count: 2, height: 0.0060 }
    }
}

/// The most straps a boot may have.
pub const MAX_STRAPS: u8 = 3;

impl Validate for Straps {
    fn check(&self, c: &mut Check) {
        c.range("count", f64::from(self.count), 0.0, f64::from(MAX_STRAPS));
        c.range("height", self.height, 0.002, 0.012);
    }
}

/// How a buckle looks.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuckleStyle {
    /// A rounded-rectangle frame on a hinged prong.
    #[default]
    Frame,
}

/// The buckles.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Buckle {
    /// Its shape.
    pub style: BuckleStyle,
    /// What it is made of: brass by default.
    pub finish: Finish,
}

impl Default for Buckle {
    fn default() -> Self {
        Self {
            style: BuckleStyle::Frame,
            finish: Finish { colour: ColourRef::rgb(0xdc, 0xb6, 0x6c), metal: Some(MetalStyle::default()) },
        }
    }
}

impl Validate for Buckle {
    fn check(&self, c: &mut Check) {
        c.nested("finish", &self.finish);
    }
}

/// The stitching and seams.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Stitching {
    /// The thread's colour.
    pub colour: ColourRef,
    /// The distance from one stitch to the next along the welt, metres; the other rows keep their proportion to it.
    pub pitch: f64,
    /// The holes in the strap tails.
    pub hole: ColourRef,
    /// The seam lines.
    pub seam: ColourRef,
}

impl Default for Stitching {
    fn default() -> Self {
        Self {
            colour: ColourRef::rgb(0xd9, 0xb9, 0x8f),
            pitch: stitch::PITCH,
            hole: ColourRef::rgb(0x24, 0x15, 0x10),
            seam: ColourRef::rgb(0x3a, 0x22, 0x14),
        }
    }
}

impl Validate for Stitching {
    fn check(&self, c: &mut Check) {
        c.range("pitch", self.pitch, 0.002, 0.02);
    }
}

/// The parameters of `boots`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct BootsParams {
    /// Ease, and the leather's base colour (`fabric`, which must be plain).
    pub common: GarmentCommon,
    /// How high the boot reaches (when it is derived from the body).
    pub height: BootHeight,
    /// The outline as explicit knots, built exactly as given; `None` derives it from the body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<BootProfile>,
    /// The toe box.
    pub toe: Toe,
    /// The heel block.
    pub heel: Heel,
    /// The sole.
    pub sole: Sole,
    /// The leather's other colours and creases.
    pub leather: Leather,
    /// The turned-down cuff.
    pub cuff: Cuff,
    /// The buckle straps.
    pub straps: Straps,
    /// The buckles.
    pub buckle: Buckle,
    /// The stitching and seams.
    pub stitching: Stitching,
    /// Whether a pull tab hangs off the back of the cuff.
    pub pull_tab: bool,
}

/// The prototype's leather, `LEATHER`.
const LEATHER: ColourRef = ColourRef::rgb(0x4a, 0x2e, 0x22);

impl Default for BootsParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon { fabric: FabricStyle::Plain { colour: LEATHER }, ..GarmentCommon::default() },
            height: BootHeight::default(),
            profile: None,
            toe: Toe::default(),
            heel: Heel::default(),
            sole: Sole::default(),
            leather: Leather::default(),
            cuff: Cuff::default(),
            straps: Straps::default(),
            buckle: Buckle::default(),
            stitching: Stitching::default(),
            pull_tab: true,
        }
    }
}

impl Validate for BootsParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        if !matches!(self.common.fabric, FabricStyle::Plain { .. }) {
            c.fail("common/fabric", "boots are leather: use a plain colour");
        }
        if let Some(profile) = &self.profile {
            c.nested("profile", profile);
        }
        c.nested("toe", &self.toe);
        c.nested("heel", &self.heel);
        c.nested("sole", &self.sole);
        c.nested("leather", &self.leather);
        c.nested("cuff", &self.cuff);
        c.nested("straps", &self.straps);
        c.nested("buckle", &self.buckle);
        c.nested("stitching", &self.stitching);
    }
}

// ------------------------------------------------------------------------------------------------ tessellation

/// How finely each piece is tessellated.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Tess {
    upper_seg: usize,
    upper_rows: [usize; 3],
    sole_seg: usize,
    /// Segments round the boot and round the cross-section.
    cuff: (usize, usize),
    strap: (usize, usize),
    frame_seg: usize,
    prong_seg: usize,
    hinge: (usize, usize),
    tail_rows: usize,
    tail_cols: usize,
    seam_seg: usize,
    seam_toe: usize,
    seam_back: usize,
    tab_seg: usize,
}

impl Tess {
    /// The prototype's tessellation.
    const OFFLINE: Self = Self {
        upper_seg: 72,
        upper_rows: [6, 9, 26],
        sole_seg: 84,
        cuff: (72, 20),
        strap: (80, 14),
        frame_seg: 7,
        prong_seg: 6,
        hinge: (10, 8),
        tail_rows: 18,
        tail_cols: 8,
        seam_seg: 5,
        seam_toe: 30,
        seam_back: 16,
        tab_seg: 10,
    };

    /// Every feature, fewer segments.
    const LIVE: Self = Self {
        upper_seg: 48,
        upper_rows: [4, 6, 16],
        sole_seg: 48,
        cuff: (48, 10),
        strap: (36, 8),
        frame_seg: 5,
        prong_seg: 4,
        hinge: (6, 5),
        tail_rows: 10,
        tail_cols: 4,
        seam_seg: 4,
        seam_toe: 20,
        seam_back: 10,
        tab_seg: 6,
    };

    /// Segments round the sole.
    pub(crate) fn sole_seg(&self) -> usize {
        self.sole_seg
    }

    pub(crate) fn of(quality: Quality) -> Self {
        match quality {
            Quality::Live => Self::LIVE,
            Quality::Offline => Self::OFFLINE,
        }
    }
}

// ------------------------------------------------------------------------------------------------------ the kind

/// The `boots` part kind.
pub struct Boots;

/// The pieces of one boot, by material.
struct Builders {
    leather: MeshBuilder,
    sole: MeshBuilder,
    brass: MeshBuilder,
    thread: MeshBuilder,
    holes: MeshBuilder,
    seams: MeshBuilder,
}

impl Builders {
    fn new() -> Self {
        Self {
            leather: MeshBuilder::with_colours(),
            sole: MeshBuilder::new(),
            brass: MeshBuilder::new(),
            thread: MeshBuilder::new(),
            holes: MeshBuilder::new(),
            seams: MeshBuilder::new(),
        }
    }

    /// The left boot with the right one (its mirror image) added.
    fn with_mirror(mut self) -> Result<Self, GarmentError> {
        for builder in
            [&mut self.leather, &mut self.sole, &mut self.brass, &mut self.thread, &mut self.holes, &mut self.seams]
        {
            let mirror = builder.mirrored();
            builder.merge(&mirror)?;
        }
        Ok(self)
    }
}

/// Builds the left boot's pieces, in the order the prototype added them.
fn build_left(boot: &Boot, tess: &Tess, p: &BootsParams) -> Result<Builders, GarmentError> {
    let mut b = Builders::new();
    upper::add(boot, tess, p.leather.creases, &mut b.leather)?;
    cuff::add(boot, tess, p.cuff.height, p.cuff.fold, &mut b.leather)?;
    let straps = strap::layout(&p.straps);
    for s in &straps {
        strap::add(boot, tess, s, &mut b.leather)?;
    }
    if p.pull_tab {
        tab::add(boot, tess, &mut b.leather)?;
    }
    for s in &straps {
        buckle::add(boot, tess, s, &mut b)?;
    }
    stitch::add_rows(boot, &p.stitching, (p.cuff.height, p.cuff.fold), p.toe.cap, &mut b.thread)?;
    stitch::add_seams(boot, tess, p.toe.cap, &mut b.seams)?;
    sole::add_sole(boot, tess, &p.sole, &mut b.sole)?;
    sole::add_heel(boot, tess, &p.sole, &p.heel, 0.0, &mut b.sole)?;
    Ok(b)
}

/// The boundary loops of one boot's meshes (the sole, heel and rolls are closed; the upper and tab are open tubes; every
/// dash, hole and tongue is a loop of its own).
fn borders_of_one(p: &BootsParams) -> usize {
    let upper = 2;
    let tab = if p.pull_tab { 2 } else { 0 };
    let buckles: usize = strap::layout(&p.straps).iter().map(|s| buckle::open_borders(s.half_height)).sum();
    let seams = if p.toe.cap { 4 } else { 2 };
    upper + tab + buckles + seams + stitch::dash_count(&p.stitching, p.toe.cap)
}

impl Garment for Boots {
    fn common(params: &BootsParams) -> &GarmentCommon {
        &params.common
    }

    fn regions(_: &BootsParams) -> Vec<BodyRegion> {
        vec![BodyRegion::LegL, BodyRegion::LegR, BodyRegion::FootL, BodyRegion::FootR]
    }

    fn open_borders(params: &BootsParams) -> usize {
        2 * borders_of_one(params)
    }
}

/// A toon material with the prototype's shading controls.
pub(crate) fn toon(base: Colour, shade: Colour, shift: f32, softness: f32, rim: (Colour, f32)) -> MaterialSpec {
    let default = Toon::default();
    let toon = Toon { shade, shift, softness, rim: Rim { colour: rim.0, strength: rim.1, ..default.rim }, ..default };
    MaterialSpec { shading: Shading::Toon(toon), ..MaterialSpec::toon(base) }
}

/// A flat, unlit material.
pub(crate) fn unlit(base: Colour) -> MaterialSpec {
    MaterialSpec { shading: Shading::Unlit, ..MaterialSpec::toon(base) }
}

/// An outline `width` metres wide at the reference distance.
pub(crate) fn outline(colour: Colour, width: f32) -> Outline {
    Outline { colour, width }
}

impl PartKind for Boots {
    type Params = BootsParams;

    fn layer() -> Option<i32> {
        Some(LAYER_FOOTWEAR)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "boots",
            summary: "build leather boots with a welted sole, cuff, buckle straps and stitching",
            category: PartCategory::Footwear,
            requires: &["body"],
        }
    }

    fn provenance() -> KindProvenance {
        KindProvenance::ProjectOwned
    }

    fn build(p: &BootsParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        p.validate().map_err(|e| ctx.fail(format!("the parameters are invalid: {e}")))?;
        let FabricStyle::Plain { colour: leather } = &p.common.fabric else {
            return Err(ctx.fail("boots are leather: use a plain colour"));
        };
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let fit = ctx.fit()?;
        let boot = Boot::new(p, fit.as_ref()).map_err(fail)?;
        let tess = Tess::of(ctx.quality);
        let b = build_left(&boot, &tess, p).and_then(Builders::with_mirror).map_err(fail)?;

        let mut out = PartOutput::new();
        let part = ctx.part.as_str().to_owned();
        let mut add = |name: &str, builder: MeshBuilder, spec: MaterialSpec, shadow: bool| -> Result<(), BuildError> {
            if builder.triangle_count() == 0 {
                return Ok(());
            }
            let id: MaterialName =
                format!("{part}-{name}").parse().map_err(|e| ctx.fail(format!("`{name}` isn't a valid name: {e}")))?;
            let mut mesh = builder
                .finish(format!("{part}-{name}"), id.clone())
                .map_err(|e| ctx.fail(format!("the `{name}` surface: {e}")))?;
            mesh.casts_shadow = shadow;
            out.meshes.push(mesh);
            out.materials.push((id, spec));
            Ok(())
        };

        let leather_spec = MaterialSpec {
            vertex_colours: true,
            outline: Some(outline(ctx.colour(&p.leather.line)?, 0.0018)),
            ..toon(ctx.colour(leather)?, ctx.colour(&p.leather.shade)?, -0.10, 0.16, (Colour::hex(0x00b8_8a6a), 0.12))
        };
        add("leather", b.leather, leather_spec, true)?;
        let sole_spec = MaterialSpec {
            outline: Some(outline(ctx.colour(&p.sole.line)?, 0.0014)),
            ..toon(
                ctx.colour(&p.sole.colour)?,
                ctx.colour(&p.sole.shade)?,
                -0.05,
                0.14,
                (Colour::hex(0x009a_8070), 0.10),
            )
        };
        add("sole", b.sole, sole_spec, true)?;
        let brass_base = ctx.colour(&p.buckle.finish.colour)?;
        let brass_spec = match &p.buckle.finish.metal {
            Some(metal) => {
                let spec =
                    metal.material(brass_base, ctx.colour(&metal.highlight)?).map_err(|e| ctx.fail(e.to_string()))?;
                MaterialSpec { outline: Some(outline(Colour::hex(0x005a_4020), 0.0008)), ..spec }
            }
            None => MaterialSpec {
                outline: Some(outline(Colour::hex(0x005a_4020), 0.0008)),
                ..MaterialSpec::toon(brass_base)
            },
        };
        let metal = p.buckle.finish.metal.is_some();
        add("brass", b.brass, brass_spec, true)?;
        let thread = ctx.colour(&p.stitching.colour)?;
        add("stitches", b.thread, toon(thread, Colour::hex(0x00a8_907c), -0.1, 0.2, (Colour::hex(0), 0.0)), false)?;
        add("holes", b.holes, unlit(ctx.colour(&p.stitching.hole)?), false)?;
        add("seams", b.seams, unlit(ctx.colour(&p.stitching.seam)?), false)?;

        if metal && let Some((brass, _)) = out.materials.iter().find(|(m, _)| m.as_str().ends_with("-brass")) {
            out.material_classes.push((brass.clone(), MaterialClass::Metal));
        }
        out.publishes = publish_envelope(ctx, &Self::regions(p), p.common.ease, 2.0)?;
        Ok(out)
    }
}

omuzaki_char_api::register_part_kind!(Boots);
