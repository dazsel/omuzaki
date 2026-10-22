//! The `robe` garment (#324, #334, #384, #194): a long lined robe, open down the front and A-line to the floor, with
//! wide bell sleeves and rolled cuffs, a band laid along the front edges (an interlace by default), a pair of clasps
//! and a chain across the chest, and a hood folded down or raised.
//!
//! It is cut with the [coat framework](crate::outfit::coat): fitted above a high waist seam, the skirt hanging free
//! below it (the grid [`cloth.set`](crate::ops::cloth) turns into cloth). The cloth is any fabric (a plain weave by
//! default); the band is painted from its own fabric, laid with its length along the band.

use omuzaki_char_api::{BodyRegion, BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::coat::hang::{Arms, Fall};
use super::coat::look::{self, CoatColours};
use super::coat::shell::{self, Frame};
use super::coat::sleeves::{self, SleeveCut};
use super::coat::trims::{self, Clasps, FrontBand};
use super::coat::{CoatCut, Front, MAX_LENGTH, MIN_LENGTH, NeckShape, Neckline};
use super::common::{
    FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, LAYER_OUTER, MAX_EASE, fabric_tile,
};
use super::hood::{self, Hood};
use super::shirt::sleeve::SleeveShape;
use super::trim::{Finish, MetalStyle};
use crate::kit::grid::MeshBuilder;

/// The layer a robe is worn on.
pub const LAYER_ROBE: i32 = LAYER_OUTER;
/// The cut of its rings.
pub const EXPONENT: f64 = 2.1;
/// The cloth's thickness by default, metres.
pub const THICKNESS: f64 = 0.003;
/// The rolled edge's lip, metres.
const LIP: f64 = 0.0009;
/// The waist seam, as a fraction of the torso: fitted over the chest, hanging free from the waist.
const WAIST: f64 = 0.36;

/// The sleeves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct RobeSleeves {
    /// How much of the arm they cover, 0.2 to 1.
    pub length: f64,
    /// How much wider they are at the end than at the elbow, metres: the bell.
    pub bell: f64,
    /// How far past the wrist they reach, metres.
    pub past: f64,
    /// The rolled cuff's radius beyond the cloth, metres.
    pub cuff: f64,
}

impl Default for RobeSleeves {
    fn default() -> Self {
        Self { length: 1.0, bell: 0.05, past: 0.03, cuff: 0.0025 }
    }
}

/// The band along the front edges.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct RobeBand {
    /// Its width, metres; 0 for none.
    pub width: f64,
    /// What it is made of: a pattern is laid with its length along the band and its width across it (the period is
    /// the band's width).
    pub fabric: FabricStyle,
}

impl Default for RobeBand {
    fn default() -> Self {
        Self {
            width: 0.03,
            fabric: FabricStyle::Interlace {
                ground: ColourRef::rgb(0x2e, 0x2a, 0x28),
                strand: ColourRef::rgb(0xa3, 0x8f, 0x6a),
                edge: ColourRef::rgb(0x5a, 0x4c, 0x3a),
                period: 0.03,
            },
        }
    }
}

/// The parameters of a robe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct RobeParams {
    /// The common settings: `ease` is how far the lining stands off what is under it, `fabric` the cloth, `cloth` its
    /// simulation.
    pub common: GarmentCommon,
    /// How thick the cloth is, metres.
    pub thickness: f64,
    /// Where the hem is: 0 at the hips' joints, 1 at the ankles.
    pub length: f64,
    /// How far apart the front edges hang below the V, radians either side of the centre.
    pub opening: f64,
    /// Where the V opens toward the collar, as a fraction of the torso.
    pub v_point: f64,
    /// How far either side of the centre the V reaches at the collar, radians.
    pub v_width: f64,
    /// How much wider it is at its lowest, metres.
    pub flare: f64,
    /// Soft folds round the skirt.
    pub folds: usize,
    /// How much longer the back is, metres.
    pub back_drop: f64,
    /// The sleeves.
    pub sleeves: RobeSleeves,
    /// The band along the front edges.
    pub band: RobeBand,
    /// The hood.
    pub hood: Hood,
    /// The clasps and the chain across the chest.
    pub clasps: Clasps,
    /// What the clasps are made of.
    pub metal: Finish,
    /// The lining, the rolled edges and the toon.
    pub colours: CoatColours,
}

impl Default for RobeParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: 0.006,
                fabric: FabricStyle::Weave {
                    warp: ColourRef::rgb(0x2c, 0x2a, 0x30),
                    weft: ColourRef::rgb(0x38, 0x35, 0x3e),
                    thread: 0.0015,
                },
                ..GarmentCommon::default()
            },
            thickness: THICKNESS,
            length: 0.9,
            opening: 0.08,
            v_point: 0.62,
            v_width: 0.42,
            flare: 0.14,
            folds: 14,
            back_drop: 0.04,
            sleeves: RobeSleeves::default(),
            band: RobeBand::default(),
            hood: Hood::default(),
            clasps: Clasps { at: 0.55, ..Clasps::default() },
            metal: Finish {
                colour: ColourRef::rgb(0xa8, 0x94, 0x6c),
                metal: Some(MetalStyle { highlight: ColourRef::rgb(0xf2, 0xe8, 0xd0), ..MetalStyle::default() }),
            },
            colours: CoatColours {
                lining: ColourRef::rgb(0x76, 0x6c, 0x64),
                trim: ColourRef::rgb(0x3c, 0x36, 0x30),
                ..CoatColours::default()
            },
        }
    }
}

impl Validate for RobeParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.range("thickness", self.thickness, 0.0005, 0.01);
        if self.common.ease + self.thickness > MAX_EASE {
            c.fail(
                "thickness",
                format!("ease and thickness stand out {} m: at most {MAX_EASE}", self.common.ease + self.thickness),
            );
        }
        c.range("length", self.length, MIN_LENGTH, MAX_LENGTH);
        c.range("opening", self.opening, 0.0, 1.0);
        c.range("v_point", self.v_point, 0.2, 0.95);
        c.range("v_width", self.v_width, 0.05, 1.2);
        c.range("flare", self.flare, 0.0, 0.4);
        if self.folds > 64 {
            c.fail("folds", format!("{}: at most 64", self.folds));
        }
        c.range("back_drop", self.back_drop, 0.0, 0.3);
        c.range("sleeves/length", self.sleeves.length, 0.2, 1.0);
        c.range("sleeves/bell", self.sleeves.bell, 0.0, 0.12);
        c.range("sleeves/past", self.sleeves.past, 0.0, 0.1);
        c.range("sleeves/cuff", self.sleeves.cuff, 0.0, 0.01);
        c.range("band/width", self.band.width, 0.0, 0.1);
        c.nested("band/fabric", &self.band.fabric);
        self.hood.check(c);
        self.clasps.check(c);
        c.nested("metal", &self.metal);
    }
}

impl RobeParams {
    /// The coat cut these parameters make.
    pub fn cut(&self) -> CoatCut {
        let sleeve = self.sleeve();
        CoatCut {
            exponent: EXPONENT,
            ease: self.common.ease,
            thickness: self.thickness,
            waist: WAIST,
            front: Front::Open { gap: self.opening, point: self.v_point, neck: self.v_width.max(self.opening) },
            neckline: Neckline { shape: NeckShape::High, depth: 0.0, width: 0.9, back: 0.0, rise: 0.04 },
            armhole: 0.0,
            length: self.length,
            back_drop: self.back_drop,
            fall: Fall::Drape,
            flare: self.flare,
            folds: (self.folds, 0.01),
            arms: Arms::Clear {
                sleeve: (sleeve.width + 2.0 * self.thickness, sleeve.width + sleeve.shape.flare + 2.0 * self.thickness),
                margin: 0.006,
            },
            lip: LIP,
        }
    }

    /// The bell sleeves' cut: lined, rolled at the cuff.
    fn sleeve(&self) -> SleeveCut {
        let s = &self.sleeves;
        SleeveCut {
            width: 0.008,
            ease_at_elbow: 0.01,
            length: s.length,
            shape: SleeveShape { flare: s.bell, past: s.past, lining: 1.0 },
            cuff: s.cuff,
        }
    }
}

/// The robe part kind.
pub struct Robe;

impl PartKind for Robe {
    type Params = RobeParams;

    fn layer() -> Option<i32> {
        Some(LAYER_ROBE)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "robe",
            summary: "a long lined robe open down the front: bell sleeves with rolled cuffs, a patterned band along the \
                      front edges, clasps and a chain across the chest, a hood down or up",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &RobeParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let frame = Frame::new(ctx.fit()?, &p.cut(), super::coat::under(ctx)).map_err(fail)?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let tones = look::tones(ctx, &p.common, &p.colours)?;
        let shell = shell::build(&frame, &tones, out.tile()).map_err(fail)?;
        if p.band.width > 0.0 {
            add_band(ctx, &mut out, &frame, p, &tones)?;
        }
        if let Some(hood) = hood::build(ctx, &frame, &p.hood, &tones)? {
            out.surface(ctx, "hood", hood)?;
        }
        let mut metal = MeshBuilder::new();
        trims::clasps(&mut metal, &frame, &p.clasps).map_err(fail)?;
        if metal.vertex_count() > 0 {
            let (spec, class) = look::metal(ctx, &p.metal, ctx.colour(&p.colours.line)?)?;
            let material = out.material(ctx, "metal", spec)?;
            out.classify(&material, class);
            out.mesh(ctx, "clasps", metal, material)?;
        }
        let sleeves = sleeves::build(&frame, &p.sleeve(), &tones, out.tile()).map_err(fail)?;
        super::coat::finish(ctx, out, &p.common, &p.colours, &frame, shell, Some(sleeves), &Self::regions(p))
    }
}

/// The band along the front edges, in its own material: its fabric painted with the band's width as the repeat.
fn add_band(
    ctx: &BuildCtx<'_>,
    out: &mut GarmentOutput,
    frame: &Frame,
    p: &RobeParams,
    tones: &shell::Tones,
) -> Result<(), BuildError> {
    let fabric = with_period(&p.band.fabric, p.band.width);
    let tile = fabric_tile(&fabric, 1, |c| ctx.colour(c), |m| ctx.fail(m))?;
    let mut spec = look::cloth(ctx, &p.colours, false)?;
    let (top, textured) = match (&fabric, tile) {
        (FabricStyle::Plain { colour }, _) => (ctx.colour(colour)?.to_linear().to_array(), false),
        (_, Some(tile)) => {
            let data = tile.paint(|size, rect, list, desc| ctx.paint_texture(size, rect, list, desc))?;
            spec.base_texture = Some(out.texture(ctx, "band", data)?);
            ([1.0; 3], true)
        }
        (_, None) => return Err(ctx.fail("the band's pattern painted no tile")),
    };
    let band = FrontBand { width: p.band.width, colours: (top, tones.lining, tones.edge), textured };
    let mut mesh = MeshBuilder::with_colours();
    trims::front_band(&mut mesh, frame, &band).map_err(|e| ctx.fail(e.to_string()))?;
    let material = out.material(ctx, "band", spec)?;
    out.mesh(ctx, "band", mesh, material)
}

/// `fabric` with its repeat set to `period` (a band's pattern repeats once per width along it).
fn with_period(fabric: &FabricStyle, period: f64) -> FabricStyle {
    let mut fabric = fabric.clone();
    match &mut fabric {
        FabricStyle::Interlace { period: p, .. } | FabricStyle::Damask { period: p, .. } => *p = period,
        FabricStyle::Plain { .. }
        | FabricStyle::Weave { .. }
        | FabricStyle::Stripes { .. }
        | FabricStyle::Plaid { .. } => {}
    }
    fabric
}

impl Garment for Robe {
    fn common(p: &RobeParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &RobeParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso, BodyRegion::ArmL, BodyRegion::ArmR]
    }

    /// The border round the open front, outside and in; an armhole in each; each lined sleeve's two ends, outside and
    /// in; two a side for the band; the hood's.
    fn open_borders(p: &RobeParams) -> usize {
        let band = if p.band.width > 0.0 { 4 } else { 0 };
        2 + 4 + 8 + band + p.hood.borders()
    }
}

omuzaki_char_api::register_part_kind!(Robe);
