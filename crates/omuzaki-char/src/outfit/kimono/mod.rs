//! The `kimono` garment (#393) and its obi (#212): a lined robe to the ankle wrapped left over right, a collar band
//! along the front edges and an under-robe's collar showing inside the V, wide sleeves with a pouch hanging from each
//! (short, or long to a furisode's), and the obi round the waist ([`obi`]).
//!
//! It is cut with the [coat framework](crate::outfit::coat): its front a wrap (the wearer's left panel crossing over
//! the right), opening into a V from the obi up; a straight column below a high waist seam, its skirt the grid
//! [`cloth.set`](crate::ops::cloth) turns into cloth; the sleeves the coat's, wide, with a
//! [pouch](crate::outfit::coat::sleeves::Pouch) under each.

pub mod obi;

use omuzaki_char_api::{BodyRegion, BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use self::obi::Obi;
use super::coat::hang::{Arms, Fall};
use super::coat::look::{self, CoatColours};
use super::coat::shell::{self, Frame};
use super::coat::sleeves::{self, Pouch, SleeveCut};
use super::coat::trims;
use super::coat::{CoatCut, Front, MAX_LENGTH, NeckShape, Neckline};
use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, LAYER_OUTER, MAX_EASE};
use super::shirt::sleeve::SleeveShape;
use crate::kit::grid::MeshBuilder;

/// The layer a kimono is worn on.
pub const LAYER_KIMONO: i32 = LAYER_OUTER - 1;
/// The cut of its rings.
pub const EXPONENT: f64 = 2.2;
/// The cloth's thickness by default, metres.
pub const THICKNESS: f64 = 0.0028;
/// The rolled edge's lip, metres.
const LIP: f64 = 0.0008;
/// The waist seam, as a fraction of the torso.
const WAIST: f64 = 0.36;

/// The sleeves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct KimonoSleeves {
    /// How much wider than the arm they are, metres.
    pub width: f64,
    /// How far the pouch under each hangs, as a fraction of the sleeve's length: about 0.3 for an everyday kimono,
    /// up to 1.5 for a furisode's; 0 for none.
    pub drop: f64,
    /// Where along the sleeve the pouch starts, from the shoulder (0) to the end (1).
    pub from: f64,
}

impl Default for KimonoSleeves {
    fn default() -> Self {
        Self { width: 0.012, drop: 0.35, from: 0.4 }
    }
}

/// The collars.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Collars {
    /// The collar band along the front edges: its width, metres (0 for none).
    pub width: f64,
    /// Its colour.
    pub colour: ColourRef,
    /// The under-robe's collar showing inside the V: its width, metres (0 for none).
    pub under: f64,
    /// Its colour.
    pub under_colour: ColourRef,
}

impl Default for Collars {
    fn default() -> Self {
        Self {
            width: 0.026,
            colour: ColourRef::rgb(0x24, 0x28, 0x36),
            under: 0.012,
            under_colour: ColourRef::rgb(0xec, 0xe6, 0xda),
        }
    }
}

/// The parameters of a kimono.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct KimonoParams {
    /// The common settings: `ease` is how far the lining stands off what is under it, `fabric` the cloth (a pattern
    /// or a plain colour), `cloth` its simulation.
    pub common: GarmentCommon,
    /// How thick the cloth is, metres.
    pub thickness: f64,
    /// Where the hem is: 0 at the hips' joints, 1 at the ankles.
    pub length: f64,
    /// How far the left panel crosses over the right past the centre front, radians.
    pub overlap: f64,
    /// Where the V opens toward the collar, as a fraction of the torso.
    pub v_point: f64,
    /// How far either side of the centre the V reaches at the collar, radians.
    pub v_width: f64,
    /// The sleeves.
    pub sleeves: KimonoSleeves,
    /// The collar bands.
    pub collars: Collars,
    /// The obi.
    pub obi: Obi,
    /// The lining, the rolled edges and the toon.
    pub colours: CoatColours,
}

impl Default for KimonoParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: 0.005,
                fabric: FabricStyle::Damask {
                    ground: ColourRef::rgb(0x34, 0x3e, 0x5c),
                    figure: ColourRef::rgb(0x4a, 0x57, 0x7c),
                    period: 0.07,
                },
                ..GarmentCommon::default()
            },
            thickness: THICKNESS,
            length: 0.95,
            overlap: 0.14,
            v_point: 0.55,
            v_width: 0.3,
            sleeves: KimonoSleeves::default(),
            collars: Collars::default(),
            obi: Obi::default(),
            colours: CoatColours {
                lining: ColourRef::rgb(0x8a, 0x5a, 0x52),
                trim: ColourRef::rgb(0x2a, 0x30, 0x44),
                ..CoatColours::default()
            },
        }
    }
}

impl Validate for KimonoParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.range("thickness", self.thickness, 0.0005, 0.008);
        if self.common.ease + self.thickness > MAX_EASE {
            c.fail(
                "thickness",
                format!("ease and thickness stand out {} m: at most {MAX_EASE}", self.common.ease + self.thickness),
            );
        }
        c.range("length", self.length, 0.3, MAX_LENGTH);
        c.range("overlap", self.overlap, 0.04, 0.5);
        c.range("v_point", self.v_point, 0.3, 0.9);
        c.range("v_width", self.v_width, 0.1, 1.0);
        c.range("sleeves/width", self.sleeves.width, 0.0, 0.04);
        c.range("sleeves/drop", self.sleeves.drop, 0.0, 1.5);
        c.range("sleeves/from", self.sleeves.from, 0.0, 0.8);
        c.range("collars/width", self.collars.width, 0.0, 0.06);
        c.range("collars/under", self.collars.under, 0.0, 0.03);
        self.obi.check(c);
    }
}

impl KimonoParams {
    /// The coat cut these parameters make.
    pub fn cut(&self) -> CoatCut {
        let sleeve = self.sleeve();
        CoatCut {
            exponent: EXPONENT,
            ease: self.common.ease,
            thickness: self.thickness,
            waist: WAIST,
            front: Front::Open { gap: -self.overlap, point: self.v_point, neck: self.v_width },
            neckline: Neckline { shape: NeckShape::High, depth: 0.0, width: 0.9, back: 0.03, rise: 0.02 },
            armhole: 0.0,
            length: self.length,
            back_drop: 0.0,
            fall: Fall::Drape,
            flare: 0.01,
            folds: (6, 0.004),
            arms: Arms::Clear {
                sleeve: (sleeve.width + self.thickness * 2.0, sleeve.width + sleeve.shape.flare + self.thickness * 2.0),
                margin: 0.004,
            },
            lip: LIP,
        }
    }

    /// The sleeves' cut: wide and lined, square at the end, with a pouch hanging from each.
    fn sleeve(&self) -> SleeveCut {
        let s = &self.sleeves;
        SleeveCut {
            width: s.width,
            ease_at_elbow: 0.006,
            length: 1.0,
            shape: SleeveShape { flare: 0.012, past: 0.0, lining: 1.0 },
            cuff: 0.0016,
            rib: 0.0,
            pouch: (s.drop > 0.0).then_some(Pouch { from: s.from, drop: s.drop, corner: 0.3 }),
        }
    }
}

/// The kimono part kind.
pub struct Kimono;

impl PartKind for Kimono {
    type Params = KimonoParams;

    fn layer() -> Option<i32> {
        Some(LAYER_KIMONO)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "kimono",
            summary: "a lined kimono to the ankle wrapped left over right: a collar band and an under-robe's collar, \
                      wide sleeves with hanging pouches (short to furisode), an obi with a drum or butterfly bow and a \
                      cord",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &KimonoParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let frame = Frame::new(ctx.fit()?, &p.cut(), super::coat::under(ctx)).map_err(fail)?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let tones = look::tones(ctx, &p.common, &p.colours)?;
        let shell = shell::build(&frame, &tones, out.tile()).map_err(fail)?;
        let (_, obi_top) = p.obi.span(&frame);
        if p.collars.width > 0.0 {
            let fabric = FabricStyle::Plain { colour: p.collars.colour.clone() };
            let from = p.obi.on.then_some(obi_top - 0.004);
            look::front_band(ctx, &mut out, &frame, (p.collars.width, from), &fabric, &p.colours, &tones)?;
        }
        let mut trim = MeshBuilder::with_colours();
        if p.collars.under > 0.0 {
            let colour = ctx.colour(&p.collars.under_colour)?.to_linear().to_array();
            trims::under_collar(&mut trim, &frame, p.collars.under, (colour, colour, colour.map(|c| c * 0.9)))
                .map_err(fail)?;
        }
        let obi_colour = ctx.colour(&p.obi.colour)?.to_linear().to_array();
        let cord = match &p.obi.cord {
            Some(c) => Some(ctx.colour(c)?.to_linear().to_array()),
            None => None,
        };
        obi::build(&mut trim, &frame, &p.obi, obi_colour, cord, tones.lining).map_err(fail)?;
        if trim.vertex_count() > 0 {
            let spec = look::cloth(ctx, &p.colours, false)?;
            let material = out.material(ctx, "trim", spec)?;
            out.mesh(ctx, "trim", trim, material)?;
        }
        let sleeves = sleeves::build(&frame, &p.sleeve(), &tones, out.tile()).map_err(fail)?;
        super::coat::finish(ctx, out, &p.common, &p.colours, &frame, shell, Some(sleeves), &Self::regions(p))
    }
}

impl Garment for Kimono {
    fn common(p: &KimonoParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &KimonoParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso, BodyRegion::ArmL, BodyRegion::ArmR]
    }

    /// The border round the open front, outside and in; an armhole in each; each lined sleeve's two ends, outside and
    /// in; two a side for the collar band and the under-collar; two for each pouch; the obi's bow.
    fn open_borders(p: &KimonoParams) -> usize {
        let collar = if p.collars.width > 0.0 { 4 } else { 0 };
        let under = if p.collars.under > 0.0 { 4 } else { 0 };
        let pouches = if p.sleeves.drop > 0.0 { 4 } else { 0 };
        2 + 4 + 8 + collar + under + pouches + p.obi.borders()
    }
}

omuzaki_char_api::register_part_kind!(Kimono);
