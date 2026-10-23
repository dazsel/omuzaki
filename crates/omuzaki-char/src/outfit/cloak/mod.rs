//! The `cloak` garment (#330): a lined cloak hung from the shoulders over the arms, from a short cape to the floor,
//! open at the front and fastened at the neck by a pair of clasps and a chain, with a hood folded down or raised.
//!
//! It is cut with the [coat framework](crate::outfit::coat): its waist seam just under the collar, so the bodice is a
//! narrow band round the neck and everything below it hangs over the shoulders, enclosing the arms. That hanging skirt is the grid
//! [`cloth.set`](crate::ops::cloth) turns into cloth.

use omuzaki_char_api::{BodyRegion, BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput};
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::coat::hang::{Arms, Fall};
use super::coat::look::{self, CoatColours};
use super::coat::shell::{self, Frame};
use super::coat::trims::{self, Clasps};
use super::coat::{CoatCut, Front, MAX_LENGTH, MIN_LENGTH, NeckShape, Neckline};
use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, LAYER_OUTER, MAX_EASE};
use super::hood::{self, Hood};
use super::trim::{Finish, MetalStyle};
use crate::kit::grid::MeshBuilder;

/// The layer a cloak is worn on: over everything else on the body.
pub const LAYER_CLOAK: i32 = LAYER_OUTER + 8;
/// The cut of its rings.
pub const EXPONENT: f64 = 2.1;
/// The cloth's thickness by default, metres.
pub const THICKNESS: f64 = 0.003;
/// The rolled edge's lip, metres.
const LIP: f64 = 0.0008;
/// The waist seam, as a share of the collar's rise above the torso's top: the cloak hangs from just under its collar,
/// over the shoulders and the arms.
const SEAM: f64 = 0.4;

/// The parameters of a cloak.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct CloakParams {
    /// The common settings: `ease` is how far the lining stands off what is under it, `fabric` the cloth, `cloth` its
    /// simulation.
    pub common: GarmentCommon,
    /// How thick the cloth is, metres.
    pub thickness: f64,
    /// Where the hem is: 0 at the hips' joints, 1 at the ankles (negative: a short cape above the hips).
    pub length: f64,
    /// How far apart the front edges hang, radians either side of the centre.
    pub opening: f64,
    /// How much wider it is at its lowest, metres.
    pub flare: f64,
    /// Soft folds round it.
    pub folds: usize,
    /// How much longer the back is, metres.
    pub back_drop: f64,
    /// How far the collar stands above the torso's top, as a fraction of the torso.
    pub rise: f64,
    /// The hood.
    pub hood: Hood,
    /// The clasps at the neck.
    pub clasps: Clasps,
    /// What the clasps are made of.
    pub metal: Finish,
    /// The lining, the rolled edges and the toon.
    pub colours: CoatColours,
}

impl Default for CloakParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: 0.006,
                fabric: FabricStyle::Plain { colour: ColourRef::rgb(0x4a, 0x46, 0x52) },
                ..GarmentCommon::default()
            },
            thickness: THICKNESS,
            length: 0.55,
            opening: 0.12,
            flare: 0.08,
            folds: 12,
            back_drop: 0.03,
            rise: 0.05,
            hood: Hood::default(),
            clasps: Clasps::default(),
            metal: pewter(),
            colours: CoatColours {
                lining: ColourRef::rgb(0x6c, 0x64, 0x5c),
                trim: ColourRef::rgb(0x3a, 0x36, 0x40),
                ..CoatColours::default()
            },
        }
    }
}

/// The clasps' default metal: a neutral pewter.
fn pewter() -> Finish {
    Finish {
        colour: ColourRef::rgb(0xa8, 0xa4, 0x9c),
        metal: Some(MetalStyle { highlight: ColourRef::rgb(0xf4, 0xf2, 0xee), ..MetalStyle::default() }),
    }
}

impl Validate for CloakParams {
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
        c.range("opening", self.opening, 0.02, 1.0);
        c.range("flare", self.flare, 0.0, 0.4);
        if self.folds > 64 {
            c.fail("folds", format!("{}: at most 64", self.folds));
        }
        c.range("back_drop", self.back_drop, 0.0, 0.3);
        c.range("rise", self.rise, 0.02, 0.2);
        self.hood.check(c);
        self.clasps.check(c);
        c.nested("metal", &self.metal);
    }
}

impl CloakParams {
    /// The coat cut these parameters make.
    pub fn cut(&self) -> CoatCut {
        CoatCut {
            exponent: EXPONENT,
            ease: self.common.ease,
            thickness: self.thickness,
            waist: 1.0 + SEAM * self.rise,
            front: Front::Open { gap: self.opening, point: 1.0, neck: self.opening },
            neckline: Neckline { shape: NeckShape::High, depth: 0.0, width: 0.9, back: 0.0, rise: self.rise },
            armhole: 0.0,
            length: self.length,
            back_drop: self.back_drop,
            fall: Fall::Drape,
            flare: self.flare,
            folds: (self.folds, 0.008),
            arms: Arms::Enclose,
            lip: LIP,
        }
    }
}

/// The cloak part kind.
pub struct Cloak;

impl PartKind for Cloak {
    type Params = CloakParams;

    fn layer() -> Option<i32> {
        Some(LAYER_CLOAK)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "cloak",
            summary: "a lined cloak hung from the shoulders over the arms, short cape to floor length, clasped at the \
                      neck, with a hood down or up",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &CloakParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let frame = Frame::new(ctx.fit()?, &p.cut(), super::coat::under(ctx)).map_err(fail)?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let tones = look::tones(ctx, &p.common, &p.colours)?;
        let shell = shell::build(&frame, &tones, out.tile()).map_err(fail)?;
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
        super::coat::finish(ctx, out, &p.common, &p.colours, &frame, shell, None, &Self::regions(p))
    }
}

impl Garment for Cloak {
    fn common(p: &CloakParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &CloakParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso]
    }

    /// The border round the open front, outside and in, and the hood's.
    fn open_borders(p: &CloakParams) -> usize {
        2 + p.hood.borders()
    }
}

omuzaki_char_api::register_part_kind!(Cloak);
