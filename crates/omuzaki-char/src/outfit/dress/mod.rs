//! The `dress` garment (#328): a fitted bodice with a neckline and straps or sleeves, a waist seam, and a skirt that
//! hangs from it to the hem, A-line, full (a circle) or a pencil, with soft folds and a sash at the seam.
//!
//! Without sleeves it is held up by a strap over each shoulder, or, with a high or halter neck, by a collar band round
//! the neck with the armholes cut in toward it ([`straps`](super::coat::straps), #2447).
//!
//! It is cut with the [coat framework](crate::outfit::coat): closed all round, lined, its neckline and hem rolled. Its
//! skirt is the grid [`cloth.set`](crate::ops::cloth) turns into cloth.

use omuzaki_char_api::{BodyRegion, BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::coat::edge::{Band, band};
use super::coat::hang::{Arms, Fall};
use super::coat::look::CoatColours;
use super::coat::shell::{self, Frame, Tones};
use super::coat::sleeves::{self, SleeveCut};
use super::coat::straps;
use super::coat::{CoatCut, Front, MAX_LENGTH, MIN_LENGTH, NeckShape, Neckline};
use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, LAYER_SHIRT, MAX_EASE};
use super::shirt::sleeve::SleeveShape;
use crate::kit::curve::{TAU, fraction};

/// The layer a dress is worn on: over the shirt, under the skirt's layer (a pinafore over a blouse).
pub const LAYER_DRESS: i32 = LAYER_SHIRT + 5;
/// The cut of its rings: a little squarer than an ellipse.
pub const EXPONENT: f64 = 2.15;
/// The cloth's thickness by default, metres: a light dress cloth, so its bodice lies within 4 mm of the body.
pub const THICKNESS: f64 = 0.0012;
/// How far it stands off the layer under it by default, metres.
pub const EASE: f64 = 0.0025;
/// The strap armholes' depth by default, as a fraction of the torso: their floor where the vest's is, below the arms' roots.
pub const ARMHOLE: f64 = 0.54;
/// The rolled edge's lip, metres.
const LIP: f64 = 0.0008;
/// A strap's width by default, metres.
pub const STRAP_WIDTH: f64 = 0.018;
/// The widest a strap may be, metres.
pub const MAX_STRAP_WIDTH: f64 = 0.05;
/// The least a high or halter neck stands up the neck, as a fraction of the torso: up round the throat.
pub const STANDING_RISE: f64 = 0.04;
/// Segments round the sash.
const SASH_SEGMENTS: usize = 96;

/// How the skirt is cut.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkirtCut {
    /// Widening gently to the hem.
    #[default]
    ALine,
    /// Full: a wide flare and deep folds.
    Circle,
    /// Close over the hips and legs.
    Pencil,
}

impl SkirtCut {
    /// How it falls, its flare at the lowest hem (metres) and its folds' depth (metres).
    pub fn shape(self) -> (Fall, f64, f64) {
        match self {
            Self::ALine => (Fall::Drape, 0.035, 0.006),
            Self::Circle => (Fall::Drape, 0.11, 0.012),
            Self::Pencil => (Fall::Pencil, 0.0, 0.0),
        }
    }
}

/// The sleeves.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DressSleeves {
    /// None: straps between the neckline and the armholes.
    #[default]
    Straps,
    /// Short sleeves.
    Short,
    /// Short puffed sleeves.
    Puff,
    /// Long sleeves.
    Long,
}

impl DressSleeves {
    /// The cut of the sleeves, if any.
    pub(crate) fn cut(self) -> Option<SleeveCut> {
        let plain = SleeveCut {
            width: 0.004,
            ease_at_elbow: 0.004,
            length: 0.3,
            shape: SleeveShape::default(),
            cuff: 0.0012,
            rib: 0.0,
            pouch: None,
        };
        match self {
            Self::Straps => None,
            Self::Short => Some(plain),
            Self::Puff => Some(SleeveCut { width: 0.012, length: 0.32, cuff: 0.002, ..plain }),
            Self::Long => {
                Some(SleeveCut { length: 1.0, shape: SleeveShape { flare: 0.008, ..SleeveShape::default() }, ..plain })
            }
        }
    }
}

/// The sash at the waist seam.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Sash {
    /// Its height, metres; 0 for none.
    pub height: f64,
    /// Its colour.
    pub colour: ColourRef,
}

impl Default for Sash {
    fn default() -> Self {
        Self { height: 0.018, colour: ColourRef::rgb(0x2c, 0x2a, 0x36) }
    }
}

/// The parameters of a dress.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct DressParams {
    /// The common settings: `ease` is how far the lining stands off the layer below, `fabric` the cloth, `cloth` its
    /// simulation.
    pub common: GarmentCommon,
    /// How thick the cloth is, metres.
    pub thickness: f64,
    /// The waist seam, as a fraction of the torso.
    pub waist: f64,
    /// The neckline.
    pub neckline: Neckline,
    /// The sleeves, or straps.
    pub sleeves: DressSleeves,
    /// How far down from the torso's top the armholes' floor is when it has straps, as a fraction of the torso.
    pub armhole: f64,
    /// How wide each strap over the shoulder is without sleeves, metres; 0 for none (strapless). A high or halter neck
    /// has a collar band instead.
    pub strap_width: f64,
    /// How the skirt is cut.
    pub skirt: SkirtCut,
    /// Where the hem is: 0 at the hips' joints, 1 at the ankles.
    pub length: f64,
    /// How much longer the back is, metres.
    pub back_drop: f64,
    /// The flare at the hem, metres; `None` for the skirt cut's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flare: Option<f64>,
    /// How many soft folds go round the skirt (their depth is the cut's).
    pub folds: usize,
    /// The sash.
    pub sash: Sash,
    /// The lining, the rolled edges and the toon.
    pub colours: CoatColours,
}

impl Default for DressParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: EASE,
                fabric: FabricStyle::Plain { colour: ColourRef::rgb(0x4e, 0x58, 0x7a) },
                ..GarmentCommon::default()
            },
            thickness: THICKNESS,
            waist: 0.36,
            neckline: Neckline { shape: NeckShape::Scoop, depth: 0.12, width: 0.8, back: 0.05, rise: -0.08 },
            sleeves: DressSleeves::Straps,
            armhole: ARMHOLE,
            strap_width: STRAP_WIDTH,
            skirt: SkirtCut::ALine,
            length: 0.5,
            back_drop: 0.0,
            flare: None,
            folds: 16,
            sash: Sash::default(),
            colours: CoatColours { lining: ColourRef::rgb(0xd8, 0xd2, 0xc4), ..CoatColours::default() },
        }
    }
}

impl Validate for DressParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.range("thickness", self.thickness, 0.0005, 0.01);
        if self.common.ease + self.thickness > MAX_EASE {
            c.fail(
                "thickness",
                format!("ease and thickness stand out {} m: at most {MAX_EASE}", self.common.ease + self.thickness),
            );
        }
        c.range("waist", self.waist, 0.1, 0.7);
        self.neckline.check(c);
        c.range("armhole", self.armhole, 0.0, 0.6);
        c.range("strap_width", self.strap_width, 0.0, MAX_STRAP_WIDTH);
        if self.neckline.shape == NeckShape::Halter && self.sleeves != DressSleeves::Straps {
            c.fail("neckline/shape", "a halter neck leaves the shoulders bare: set sleeves to straps");
        }
        c.range("length", self.length, MIN_LENGTH, MAX_LENGTH);
        c.range("back_drop", self.back_drop, 0.0, 0.3);
        if let Some(flare) = self.flare {
            c.range("flare", flare, 0.0, 0.3);
        }
        if self.folds > 64 {
            c.fail("folds", format!("{}: at most 64", self.folds));
        }
        c.range("sash/height", self.sash.height, 0.0, 0.08);
    }
}

impl DressParams {
    /// The coat cut these parameters make.
    pub fn cut(&self) -> CoatCut {
        let (fall, flare, depth) = self.skirt.shape();
        let sleeves = self.sleeves.cut();
        CoatCut {
            exponent: EXPONENT,
            ease: self.common.ease,
            thickness: self.thickness,
            waist: self.waist,
            front: Front::Closed,
            neckline: standing(&self.neckline),
            armhole: if sleeves.is_some() { 0.0 } else { self.armhole },
            length: self.length,
            back_drop: self.back_drop,
            fall,
            flare: self.flare.unwrap_or(flare),
            folds: (self.folds, depth),
            arms: Arms::Clear {
                sleeve: sleeves
                    .map_or((0.0, 0.0), |s| (s.width + self.thickness, s.width + s.shape.flare + self.thickness)),
                margin: 0.004,
            },
            lip: LIP,
        }
    }
}

/// `neckline` as cut: a high or halter neck stands at least [`STANDING_RISE`] up the neck.
pub fn standing(neckline: &Neckline) -> Neckline {
    let mut n = neckline.clone();
    if n.shape.stands() {
        n.rise = n.rise.max(STANDING_RISE);
    }
    n
}

/// What holds up a coat without sleeves: a collar band for a high or halter neck, else a strap `width` wide over each
/// shoulder (none for 0). Returns the open borders it adds.
///
/// # Errors
/// As [`straps::straps`] and [`straps::collar`].
pub fn hold_up(
    mesh: &mut crate::kit::grid::MeshBuilder,
    frame: &Frame,
    width: f64,
    tones: &Tones,
) -> Result<usize, GarmentError> {
    if frame.cut.neckline.shape.stands() {
        straps::collar(frame, tones, mesh)?;
        Ok(0)
    } else if width > 0.0 {
        straps::straps(frame, width, tones, mesh)
    } else {
        Ok(0)
    }
}

/// The dress part kind.
pub struct Dress;

impl PartKind for Dress {
    type Params = DressParams;

    fn layer() -> Option<i32> {
        Some(LAYER_DRESS)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "dress",
            summary: "a lined dress: fitted bodice with a neckline and straps or sleeves, a sash at the waist and an A-line, \
                      full or pencil skirt with soft folds",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &DressParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let cut = p.cut();
        let frame = Frame::new(ctx.fit()?, &cut, super::coat::under(ctx)).map_err(fail)?;
        let out = GarmentOutput::new(ctx, &p.common)?;
        let tones = super::coat::look::tones(ctx, &p.common, &p.colours)?;
        let mut shell = shell::build(&frame, &tones, out.tile()).map_err(fail)?;
        if p.sash.height > 0.0 {
            let colour = ctx.colour(&p.sash.colour)?.to_linear().to_array();
            add_sash(&mut shell.mesh, &frame, p.sash.height, colour).map_err(fail)?;
        }
        let sleeves = if let Some(cut) = p.sleeves.cut() {
            Some(sleeves::build(&frame, &cut, &tones, out.tile()).map_err(fail)?)
        } else {
            hold_up(&mut shell.mesh, &frame, p.strap_width, &tones).map_err(fail)?;
            None
        };
        super::coat::finish(ctx, out, &p.common, &p.colours, &frame, shell, sleeves, &Self::regions(p))
    }
}

/// The sash round the waist seam, on the outside.
fn add_sash(
    mesh: &mut crate::kit::grid::MeshBuilder,
    frame: &Frame,
    height: f64,
    colour: [f32; 3],
) -> Result<(), GarmentError> {
    let y = frame.waist();
    let surface =
        |k: usize, dy: f64| frame.on(y + dy, -std::f64::consts::PI + TAU * fraction(k, SASH_SEGMENTS), 0.0004);
    band(mesh, &Band { around: SASH_SEGMENTS, surface: &surface, height, thickness: 0.0025, colour, ribs: 0 })
}

impl Garment for Dress {
    fn common(p: &DressParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(p: &DressParams) -> Vec<BodyRegion> {
        match p.sleeves {
            DressSleeves::Straps => vec![BodyRegion::Torso],
            _ => vec![BodyRegion::Torso, BodyRegion::ArmL, BodyRegion::ArmR],
        }
    }

    /// The hem and the neckline, outside and in; with sleeves, an armhole in each and two ends to each sleeve; with
    /// straps, two for each strap.
    fn open_borders(p: &DressParams) -> usize {
        4 + if p.sleeves.cut().is_some() {
            4 + 4
        } else if p.strap_width > 0.0 && !p.neckline.shape.stands() {
            4
        } else {
            0
        }
    }
}

omuzaki_char_api::register_part_kind!(Dress);
