//! Garment framework: ease, layered envelopes and fabric textures.
//!
//! Every garment kind builds the same way (the module docs of [`crate::outfit`] walk through it):
//! 1. its parameters embed a [`GarmentCommon`] as a `common` field, and the kind implements [`Garment`];
//! 2. it lofts its surfaces over the fit inside it ([`BuildCtx::fit`]) with [`Surface`](super::loft::Surface), pushed out by
//!    the ease;
//! 3. it hands the surfaces to a [`GarmentOutput`], which paints the fabric, names the meshes and materials after the
//!    part, and publishes the garment's [`Envelope`] so the next layer fits over it.
//!
//! **Layers.** A garment wraps everything built before it, so the build order is the layering order: the part's
//! `layer` ([`PartDef::layer`](omuzaki_char_api::PartDef)) says where it sits. The convention is [`LAYER_UNDERWEAR`],
//! [`LAYER_SHIRT`], [`LAYER_VEST`] and [`LAYER_OUTER`] and above; parts on one layer build in id order.
//!
//! **Envelopes.** An envelope is the fit inside it with `ease` added to both radii of every section of the regions the
//! garment covers (the prototype's `tp(y, ph, off)`, where each layer calls the same section function with a larger
//! `off`). Regions a garment doesn't cover pass through unchanged, and so do the joints and the bind pose. A garment cut
//! squarer than an ellipse ([`Surface::exponent`](super::loft::Surface::exponent) above 2) reaches past its ellipse at
//! the diagonals, so its envelope is widened by [`enclosing_scale`] to keep the next layer clear of the corners.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use omuzaki_anim_api::HumanoidPose;
use omuzaki_char_api::{
    BodyFit, BodyRegion, BuildCtx, BuildError, FitFact, JointRef, MaterialName, MaterialSpec, PartKind, PartMesh,
    PartOutput, Publishes, RegionEnd, Section, TextureName,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{UVec2, Vec2};
use omuzaki_face_api::{Check, ColourRef, DrawList, FaceRect, Paint, Validate};
use omuzaki_render_api::{Filter, Sampler, TextureDesc, Wrap};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kit::KitError;
use crate::kit::grid::MeshBuilder;

// ---------------------------------------------------------------------------------------------------------- layers

/// The layer of underwear and base layers: right on the body.
pub const LAYER_UNDERWEAR: i32 = 10;
/// The layer of shirts, blouses and dresses.
pub const LAYER_SHIRT: i32 = 20;
/// The layer of vests, cardigans and waistcoats.
pub const LAYER_VEST: i32 = 30;
/// The first layer of outer garments (blazers, robes, cloaks); later outer layers go above it.
pub const LAYER_OUTER: i32 = 40;

// ------------------------------------------------------------------------------------------------------------ ease

/// The least ease a garment may have, metres: closer than this and the toon outline of the layer below shows through.
pub const MIN_EASE: f64 = 0.001;
/// The most ease a garment may have, metres: beyond this a garment is a separate shell, not a layer.
pub const MAX_EASE: f64 = 0.08;
/// The ease of a new garment, metres: the contract's shirt, 4 mm off the body.
pub const DEFAULT_EASE: f64 = 0.004;

/// What every garment's parameters carry, as a field named `common`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct GarmentCommon {
    /// How far the garment sits outside the layer below it, metres, from [`MIN_EASE`] to [`MAX_EASE`].
    pub ease: f64,
    /// The cloth it is made of: a plain colour or a woven pattern.
    pub fabric: FabricStyle,
    /// Cloth simulation: off (the garment is skinned to the skeleton) unless enabled; a garment that cannot drape
    /// ignores it.
    pub cloth: ClothSpec,
}

impl Default for GarmentCommon {
    fn default() -> Self {
        Self { ease: DEFAULT_EASE, fabric: FabricStyle::default(), cloth: ClothSpec::default() }
    }
}

impl Validate for GarmentCommon {
    fn check(&self, c: &mut Check) {
        c.range("ease", self.ease, MIN_EASE, MAX_EASE);
        c.nested("fabric", &self.fabric);
        c.nested("cloth", &self.cloth);
    }
}

pub use super::drape::ClothSpec;

// ---------------------------------------------------------------------------------------------------------- fabrics

/// The smallest pattern period (or weave thread), metres.
pub const MIN_PERIOD: f64 = 0.0005;
/// The largest pattern period, metres.
pub const MAX_PERIOD: f64 = 0.2;
/// The most bands a plaid may have.
pub const MAX_PLAID_BANDS: usize = 8;
/// Texture density of a fabric tile, pixels per metre (0.25 mm a pixel).
pub const FABRIC_PX_PER_M: f64 = 4000.0;
/// The largest side of a fabric tile, pixels; a larger period is painted at a lower density.
pub const MAX_TILE_PX: u32 = 512;
/// The smallest side of a fabric tile, pixels; a smaller period is painted at a higher density.
pub const MIN_TILE_PX: u32 = 8;
/// How strongly a plaid's crossing bands show through each other.
const PLAID_CROSS_OPACITY: f32 = 0.55;

/// The cloth a garment is made of.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FabricStyle {
    /// One colour, no texture.
    Plain {
        /// The colour.
        colour: ColourRef,
    },
    /// A plain weave: warp and weft threads crossing over and under each other.
    Weave {
        /// The lengthwise threads.
        warp: ColourRef,
        /// The crosswise threads.
        weft: ColourRef,
        /// The width of one thread, metres.
        thread: f64,
    },
    /// Even stripes on a ground.
    Stripes {
        /// The ground colour.
        ground: ColourRef,
        /// The stripe colour.
        stripe: ColourRef,
        /// From one stripe to the next, metres, measured across the stripes.
        period: f64,
        /// The width of a stripe, metres, less than `period`.
        width: f64,
        /// Which way the stripes run.
        direction: StripeDirection,
    },
    /// Bands crossing at right angles over a ground (a tartan's sett, the same both ways).
    Plaid {
        /// The ground colour.
        ground: ColourRef,
        /// The sett's size, metres: the pattern repeats this often both ways.
        period: f64,
        /// The bands of one repeat, at most [`MAX_PLAID_BANDS`].
        bands: Vec<PlaidBand>,
    },
}

impl Default for FabricStyle {
    fn default() -> Self {
        Self::Plain { colour: ColourRef::rgb(0x4a, 0x55, 0x78) }
    }
}

/// Which way stripes run on the garment's texture.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StripeDirection {
    /// Along the garment's length (pinstripes).
    Vertical,
    /// Around the garment (a sailor's stripes).
    Horizontal,
    /// At 45 degrees (a tie's stripes).
    Diagonal,
}

/// One band of a plaid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaidBand {
    /// Its colour.
    pub colour: ColourRef,
    /// Where it starts in the repeat, metres from `0` to the period.
    pub offset: f64,
    /// Its width, metres, less than the period.
    pub width: f64,
}

impl Validate for FabricStyle {
    fn check(&self, c: &mut Check) {
        match self {
            Self::Plain { .. } => {}
            Self::Weave { thread, .. } => c.range("thread", *thread, MIN_PERIOD, MAX_PERIOD / 2.0),
            Self::Stripes { period, width, .. } => {
                c.range("period", *period, MIN_PERIOD, MAX_PERIOD);
                band_width(c, "width", *width, *period);
            }
            Self::Plaid { period, bands, .. } => {
                c.range("period", *period, MIN_PERIOD, MAX_PERIOD);
                if bands.len() > MAX_PLAID_BANDS {
                    c.fail("bands", format!("{} bands: a plaid has at most {MAX_PLAID_BANDS}", bands.len()));
                }
                for (k, band) in bands.iter().enumerate() {
                    let at = format!("bands/{k}");
                    c.range(&format!("{at}/offset"), band.offset, 0.0, *period);
                    band_width(c, &format!("{at}/width"), band.width, *period);
                }
            }
        }
    }
}

/// A band's width must be positive and less than the period, or the pattern has no ground left.
fn band_width(c: &mut Check, name: &str, width: f64, period: f64) {
    if !(width.is_finite() && width > 0.0 && width < period) {
        c.fail(name, format!("{width} must be greater than 0 and less than the period ({period})"));
    }
}

/// One repeat of a fabric's pattern, ready to paint: the drawing, the square of the plane it covers, and its size in
/// pixels. The texture repeats ([`Wrap::Repeat`]), so a garment's UVs count tiles ([`FabricTile::size`]).
#[derive(Clone, Debug)]
pub struct FabricTile {
    /// The pattern, drawn so that it repeats seamlessly across the edges of [`FabricTile::rect`].
    pub list: DrawList,
    /// The repeat, metres, from the origin: `size` by `size`.
    pub rect: FaceRect,
    /// The repeat's side, metres.
    pub size: f64,
    /// The texture's side, pixels.
    pub px: u32,
}

impl FabricTile {
    /// The texture description a fabric tile is painted with: sRGB colour, repeating, mip-mapped and filtered.
    pub fn desc(&self) -> TextureDesc {
        let mut desc = TextureDesc::colour(UVec2::splat(self.px));
        desc.sampler = Sampler { filter: Filter::Linear, wrap: Wrap::Repeat, anisotropy: 4 };
        desc
    }

    /// Paints the tile through `paint` (a [`BuildCtx::paint_texture`] or `RecolourCtx::paint_texture`).
    ///
    /// # Errors
    /// Whatever `paint` reports.
    pub fn paint(
        &self,
        paint: impl FnOnce(UVec2, FaceRect, &DrawList, TextureDesc) -> Result<omuzaki_char_api::TextureData, BuildError>,
    ) -> Result<omuzaki_char_api::TextureData, BuildError> {
        paint(UVec2::splat(self.px), self.rect, &self.list, self.desc())
    }
}

/// The tile `fabric` paints, its colours resolved by `colour`; `None` for a plain fabric, which needs no texture.
/// `repeats` paints that many repeats each way into one texture (1 for a garment; tests paint 2 to see the seams).
///
/// # Errors
/// Whatever `colour` reports for a colour it can't resolve, and [`BuildError::Part`] (through `fail`) for a fabric
/// whose numbers [`Validate`] would refuse.
pub fn fabric_tile(
    fabric: &FabricStyle,
    repeats: u32,
    colour: impl Fn(&ColourRef) -> Result<Colour, BuildError>,
    fail: impl Fn(String) -> BuildError,
) -> Result<Option<FabricTile>, BuildError> {
    fabric.validate().map_err(|e| fail(format!("fabric: {e}")))?;
    if repeats == 0 {
        return Err(fail("a fabric tile needs at least one repeat".to_owned()));
    }
    let mut list = DrawList::new();
    let period = match fabric {
        FabricStyle::Plain { .. } => return Ok(None),
        FabricStyle::Weave { warp, weft, thread } => {
            weave(&mut list, colour(warp)?, colour(weft)?, *thread, repeats);
            2.0 * thread
        }
        FabricStyle::Stripes { ground, stripe, period, width, direction } => {
            stripes(&mut list, colour(ground)?, colour(stripe)?, *period, *width, *direction, repeats)
        }
        FabricStyle::Plaid { ground, period, bands } => {
            let bands = bands
                .iter()
                .map(|b| Ok::<_, BuildError>((colour(&b.colour)?, b.offset, b.width)))
                .collect::<Result<Vec<_>, _>>()?;
            plaid(&mut list, colour(ground)?, *period, &bands, repeats);
            *period
        }
    };
    let px = tile_px(period);
    let size = period * f64::from(repeats);
    #[allow(clippy::cast_possible_truncation)] // a tile is at most a few metres; the plane is f32
    let rect = FaceRect { x0: 0.0, x1: size as f32, y0: 0.0, y1: size as f32 };
    Ok(Some(FabricTile { list, rect, size, px: px * repeats }))
}

/// The pixel side of one repeat of `period` metres, clamped to [`MIN_TILE_PX`]..=[`MAX_TILE_PX`].
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a positive period times a fixed density
fn tile_px(period: f64) -> u32 {
    ((period * FABRIC_PX_PER_M).round() as u32).clamp(MIN_TILE_PX, MAX_TILE_PX)
}

/// An axis-aligned rectangle `[x0, x1] x [y0, y1]` of the plane, filled with `colour`.
#[allow(clippy::cast_possible_truncation)] // the drawing plane is f32
fn rect(list: &mut DrawList, colour: Colour, x0: f64, x1: f64, y0: f64, y1: f64, opacity: f32) {
    let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].map(|(x, y)| Vec2::new(x as f32, y as f32));
    let path = list.path().polyline(&corners).close().finish();
    list.push(omuzaki_face_api::Cmd::Fill { path, paint: Paint::Solid(colour), opacity });
}

/// A plain weave of `repeats` x `repeats` repeats: each repeat is 2 x 2 threads, warp showing on one diagonal and weft
/// on the other, with a darker line down the middle of each thread for its twist.
fn weave(list: &mut DrawList, warp: Colour, weft: Colour, thread: f64, repeats: u32) {
    let cells = 2 * repeats;
    let size = thread * f64::from(cells);
    rect(list, warp, 0.0, size, 0.0, size, 1.0);
    for i in 0..cells {
        for j in 0..cells {
            if (i + j) % 2 == 1 {
                let (x, y) = (thread * f64::from(i), thread * f64::from(j));
                rect(list, weft, x, x + thread, y, y + thread, 1.0);
            }
        }
    }
    let shade = Colour::rgba(0, 0, 0, 255);
    let line = thread * 0.2;
    for k in 0..cells {
        let mid = thread * (f64::from(k) + 0.5);
        // Warp threads run vertically, weft threads horizontally; each gets a faint line along its middle.
        rect(list, shade, mid - line / 2.0, mid + line / 2.0, 0.0, size, 0.12);
        rect(list, shade, 0.0, size, mid - line / 2.0, mid + line / 2.0, 0.12);
    }
}

/// Stripes of `width` every `period` over `ground`, `repeats` x `repeats` repeats. Returns the side of one repeat: the
/// period, or for diagonal stripes the period times root 2 (the horizontal distance between two 45-degree stripes).
fn stripes(
    list: &mut DrawList,
    ground: Colour,
    stripe: Colour,
    period: f64,
    width: f64,
    direction: StripeDirection,
    repeats: u32,
) -> f64 {
    let tile = match direction {
        StripeDirection::Diagonal => period * std::f64::consts::SQRT_2,
        StripeDirection::Vertical | StripeDirection::Horizontal => period,
    };
    let size = tile * f64::from(repeats);
    rect(list, ground, 0.0, size, 0.0, size, 1.0);
    match direction {
        StripeDirection::Vertical | StripeDirection::Horizontal => {
            for k in 0..repeats {
                let at = period * f64::from(k);
                let (a, b) = (at, at + width);
                if direction == StripeDirection::Vertical {
                    rect(list, stripe, a, b, 0.0, size, 1.0);
                } else {
                    rect(list, stripe, 0.0, size, a, b, 1.0);
                }
            }
        }
        StripeDirection::Diagonal => {
            // A band between the lines x - y = c and x - y = c + w (w is the width measured horizontally), one per
            // repeat, from a repeat before the tile to one past it so every corner is covered. Each band is centred in
            // its repeat, so its edges don't run through the corners, where the tile's own pixel grid would sample
            // exactly on an edge.
            let w = width * std::f64::consts::SQRT_2;
            let start = (tile - w) / 2.0;
            let first = -i64::from(repeats) - 1;
            for k in first..=i64::from(repeats) {
                #[allow(clippy::cast_precision_loss)] // a handful of repeats
                let c = start + tile * k as f64;
                #[allow(clippy::cast_possible_truncation)] // the drawing plane is f32
                let corners = [(c, 0.0), (c + w, 0.0), (c + w + size, size), (c + size, size)]
                    .map(|(x, y): (f64, f64)| Vec2::new(x as f32, y as f32));
                let path = list.path().polyline(&corners).close().finish();
                list.fill(path, Paint::Solid(stripe));
            }
        }
    }
    tile
}

/// A plaid: the bands across (horizontal) drawn solid, then the bands along (vertical) at [`PLAID_CROSS_OPACITY`], so
/// where they cross the colours mix as woven bands do. A band that runs past the end of a repeat continues at its start.
fn plaid(list: &mut DrawList, ground: Colour, period: f64, bands: &[(Colour, f64, f64)], repeats: u32) {
    let size = period * f64::from(repeats);
    rect(list, ground, 0.0, size, 0.0, size, 1.0);
    for vertical in [false, true] {
        let opacity = if vertical { PLAID_CROSS_OPACITY } else { 1.0 };
        for k in 0..repeats {
            for &(colour, offset, width) in bands {
                let a = period * f64::from(k) + offset;
                // A band past the tile's end wraps round to its start, so the tile repeats without a seam.
                let spans = [(a, a + width), (a - size, a + width - size)];
                for (a, b) in spans.into_iter().take(if a + width > size { 2 } else { 1 }) {
                    if vertical {
                        rect(list, colour, a, b, 0.0, size, opacity);
                    } else {
                        rect(list, colour, 0.0, size, a, b, opacity);
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------------- envelopes

/// How much wider than its radii a superellipse of `exponent` reaches, at most, measured against the ellipse of the same
/// radii: `2^(1/2 - 1/exponent)`, at the diagonals; 1 for an ellipse (exponent 2). An envelope scales its radii by this
/// so the ellipse it publishes encloses the garment's cut.
pub fn enclosing_scale(exponent: f64) -> f64 {
    2.0_f64.powf(0.5 - 1.0 / exponent)
}

/// Covered sections worked out, by region and `t` (its bits).
type Known = HashMap<(BodyRegion, u32), Option<Section>>;

/// The fit outside a garment: the fit inside it with `ease` added to the radii of the regions it covers (widened for a
/// square cut by [`enclosing_scale`]). Joints, the bind pose and every other region are the inner fit's.
#[derive(Debug, Clone)]
pub struct Envelope {
    inner: Arc<dyn BodyFit>,
    regions: Vec<BodyRegion>,
    ease: f32,
    exponent: f64,
    scale: f32,
    /// The covered sections already worked out, by region and `t`: a garment's ring clears its neighbours with a search
    /// of the fit inside it, and the layers over it ask for the same sections many times.
    known: Arc<Mutex<Known>>,
    /// Where the garment ends partway down a region it covers ([`Envelope::ending`]): the region, the `t` of its edge,
    /// and how far below that, in `t`, the envelope has eased back onto the fit inside it.
    end: Option<(BodyRegion, f32, f32)>,
}

impl Envelope {
    /// The envelope of a garment over `inner` that covers `regions` with `ease` metres, cut with superellipse
    /// `exponent` (2 for an ellipse).
    ///
    /// # Errors
    /// [`GarmentError::Invalid`] for an ease outside [`MIN_EASE`]..=[`MAX_EASE`], an exponent outside
    /// [`MIN_EXPONENT`](super::loft::MIN_EXPONENT)..=[`MAX_EXPONENT`](super::loft::MAX_EXPONENT), or no regions.
    pub fn new(
        inner: Arc<dyn BodyFit>,
        regions: &[BodyRegion],
        ease: f64,
        exponent: f64,
    ) -> Result<Self, GarmentError> {
        if !(ease.is_finite() && (MIN_EASE..=MAX_EASE).contains(&ease)) {
            return Err(GarmentError::invalid("ease", format!("{ease} m isn't in {MIN_EASE}..={MAX_EASE}")));
        }
        super::loft::check_exponent(exponent)?;
        if regions.is_empty() {
            return Err(GarmentError::invalid("regions", "an envelope covers at least one region".to_owned()));
        }
        let mut regions = regions.to_vec();
        regions.sort_unstable();
        regions.dedup();
        #[allow(clippy::cast_possible_truncation)] // a fit is f32
        Ok(Self {
            inner,
            regions,
            ease: ease as f32,
            exponent,
            scale: enclosing_scale(exponent) as f32,
            known: Arc::default(),
            end: None,
        })
    }

    /// The same envelope for a garment whose edge lies at `t` of `region` (a shirt's hem on the torso): below it the
    /// envelope eases back onto the fit inside it over `fade` (in `t`), so a layer over the garment comes back in
    /// to the body under the edge instead of standing out where there is no garment, and doesn't step where it does.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // a fit is f32
    pub fn ending(mut self, region: BodyRegion, t: f64, fade: f64) -> Self {
        self.end = Some((region, t as f32, fade.max(f64::EPSILON) as f32));
        self.known = Arc::default();
        self
    }

    /// The fit inside this one.
    pub fn inner(&self) -> &Arc<dyn BodyFit> {
        &self.inner
    }

    /// The regions it covers, sorted.
    pub fn regions(&self) -> &[BodyRegion] {
        &self.regions
    }

    /// The ease it adds, metres.
    pub fn ease(&self) -> f32 {
        self.ease
    }
}

impl BodyFit for Envelope {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        if self.regions.binary_search(&region).is_err() {
            return self.inner.section(region, t);
        }
        let key = (region, t.to_bits());
        if let Some(known) = self.known.lock().ok().and_then(|known| known.get(&key).copied()) {
            return known;
        }
        // The garment's own ring, which also clears what closes into the region (the thighs round the hips).
        let section = self.inner.section(region, t).map(|inner| {
            let ring =
                super::loft::ring_at(self.inner.as_ref(), region, f64::from(t), self.exponent, f64::from(self.ease));
            let radii = ring.map_or(inner.radii + Vec2::splat(self.ease), |ring| ring.radii.as_vec2()) * self.scale;
            // Below the garment's edge, the radii ease back onto the inner fit's.
            let cover = match self.end {
                Some((edge, at, fade)) if edge == region => ((t - (at - fade)) / fade).clamp(0.0, 1.0),
                _ => 1.0,
            };
            let cover = cover * cover * (3.0 - 2.0 * cover);
            Section { radii: inner.radii + (radii - inner.radii) * cover, ..inner }
        });
        if let Ok(mut known) = self.known.lock() {
            known.insert(key, section);
        }
        section
    }

    /// The caps are the body's: a garment closes over none.
    fn cap(&self, region: BodyRegion, end: RegionEnd, height: f32) -> Option<Section> {
        self.inner.cap(region, end, height)
    }

    fn joint(&self, joint: &JointRef) -> Option<omuzaki_core::glam::Vec3> {
        self.inner.joint(joint)
    }

    fn bind(&self) -> HumanoidPose {
        self.inner.bind()
    }
}

/// What a garment publishes: its [`Envelope`] over the fit inside it, so the parts after it fit over the garment.
///
/// # Errors
/// [`BuildError::Missing`] if no body was built before the garment; [`BuildError::Part`] for an envelope
/// [`Envelope::new`] refuses.
pub fn publish_envelope(
    ctx: &BuildCtx<'_>,
    regions: &[BodyRegion],
    ease: f64,
    exponent: f64,
) -> Result<Publishes, BuildError> {
    let envelope = Envelope::new(ctx.fit()?, regions, ease, exponent).map_err(|e| ctx.fail(e.to_string()))?;
    Ok(Publishes::default().with::<FitFact>(Arc::new(envelope)))
}

// --------------------------------------------------------------------------------------------------------- garments

/// Why a garment helper could not build what it was asked for.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum GarmentError {
    /// A number or choice the helper can't use.
    #[error("{what}: {message}")]
    Invalid {
        /// What was wrong.
        what: &'static str,
        /// Why, and what would work.
        message: String,
    },
    /// The fit has no section where the garment needs one.
    #[error("the fit has no {region:?} section at t = {t}")]
    NoSection {
        /// The region.
        region: BodyRegion,
        /// Where along it.
        t: f64,
    },
    /// The geometry kit refused the shape.
    #[error(transparent)]
    Kit(#[from] KitError),
}

impl GarmentError {
    /// An [`GarmentError::Invalid`].
    pub fn invalid(what: &'static str, message: impl Into<String>) -> Self {
        Self::Invalid { what, message: message.into() }
    }
}

/// A garment kind: a [`PartKind`] whose parameters carry a [`GarmentCommon`] and that covers some body regions. The
/// conformance suite builds any `Garment` on every template and shape and checks it; implementing this is all a new
/// garment does to be tested.
pub trait Garment: PartKind {
    /// The common garment settings inside `params`.
    fn common(params: &Self::Params) -> &GarmentCommon;

    /// The body regions `params` cover (the regions its envelope adds ease to).
    fn regions(params: &Self::Params) -> Vec<BodyRegion>;

    /// How many open borders (hems, cuffs, necklines) the built meshes have, together: the suite checks every other edge
    /// is shared by exactly two triangles. A closed garment says 0.
    fn open_borders(params: &Self::Params) -> usize;
}

/// The output of a garment as it is assembled: the fabric's material and texture, then a mesh per surface, then the
/// envelope. Every material, texture and mesh is named after the part (`<part>-fabric`, `<part>-<surface>`).
pub struct GarmentOutput {
    part: String,
    fabric: MaterialName,
    tile: Option<f64>,
    output: PartOutput,
}

impl GarmentOutput {
    /// Starts the output of the part `ctx` builds: resolves the fabric's colours and paints its texture.
    ///
    /// # Errors
    /// [`BuildError::Colour`] for a palette slot the palette lacks; [`BuildError::Missing`] for a patterned fabric
    /// when the build has no rasteriser; [`BuildError::Part`] for a fabric that fails validation.
    pub fn new(ctx: &BuildCtx<'_>, common: &GarmentCommon) -> Result<Self, BuildError> {
        let part = ctx.part.as_str().to_owned();
        let fabric = id_of::<MaterialName>(ctx, &format!("{part}-fabric"))?;
        let tile = fabric_tile(&common.fabric, 1, |c| ctx.colour(c), |m| ctx.fail(m))?;
        let mut output = PartOutput::new();
        let tile_size = tile.as_ref().map(|t| t.size);
        let material = match (&common.fabric, tile) {
            (FabricStyle::Plain { colour }, _) => MaterialSpec::toon(ctx.colour(colour)?),
            (_, Some(tile)) => {
                let texture_name = id_of::<TextureName>(ctx, &format!("{part}-fabric"))?;
                let texture = tile.paint(|size, rect, list, desc| ctx.paint_texture(size, rect, list, desc))?;
                output.textures.push((texture_name.clone(), texture));
                MaterialSpec { base_texture: Some(texture_name), ..MaterialSpec::toon(Colour::rgb(255, 255, 255)) }
            }
            (_, None) => return Err(ctx.fail("a patterned fabric painted no tile")),
        };
        output.materials.push((fabric.clone(), material));
        Ok(Self { part, fabric, tile: tile_size, output })
    }

    /// The side of one repeat of the fabric's pattern, metres, for [`Loft::add_to`](super::loft::Loft::add_to); `None`
    /// for a plain fabric.
    pub fn tile(&self) -> Option<f64> {
        self.tile
    }

    /// The name of the fabric material, for a surface built by hand.
    pub fn fabric(&self) -> &MaterialName {
        &self.fabric
    }

    /// Adds the mesh in `builder` as the surface `<part>-<name>`, in the fabric.
    ///
    /// # Errors
    /// [`BuildError::Part`] for an empty or invalid mesh.
    pub fn surface(&mut self, ctx: &BuildCtx<'_>, name: &str, builder: MeshBuilder) -> Result<(), BuildError> {
        let material = self.fabric.clone();
        self.mesh(ctx, name, builder, material)
    }

    /// Adds the mesh in `builder` as `<part>-<name>` in `material`, which must be one of this output's materials.
    ///
    /// # Errors
    /// [`BuildError::Part`] for an empty or invalid mesh, or a material the output doesn't have.
    pub fn mesh(
        &mut self,
        ctx: &BuildCtx<'_>,
        name: &str,
        builder: MeshBuilder,
        material: MaterialName,
    ) -> Result<(), BuildError> {
        if builder.triangle_count() == 0 {
            return Err(ctx.fail(format!("the garment's `{name}` surface has no triangles")));
        }
        if !self.output.materials.iter().any(|(m, _)| *m == material) {
            return Err(ctx.fail(format!("the garment has no material `{material}`")));
        }
        let mesh: PartMesh = builder
            .finish(format!("{}-{name}", self.part), material)
            .map_err(|e| ctx.fail(format!("the garment's `{name}` surface: {e}")))?;
        self.output.meshes.push(mesh);
        Ok(())
    }

    /// Adds a material named `<part>-<name>`, and returns its name.
    ///
    /// # Errors
    /// [`BuildError::Part`] if the name isn't a valid id or the output already has it.
    pub fn material(&mut self, ctx: &BuildCtx<'_>, name: &str, spec: MaterialSpec) -> Result<MaterialName, BuildError> {
        let id = id_of::<MaterialName>(ctx, &format!("{}-{name}", self.part))?;
        if self.output.materials.iter().any(|(m, _)| *m == id) {
            return Err(ctx.fail(format!("the garment already has a material `{id}`")));
        }
        self.output.materials.push((id.clone(), spec));
        Ok(id)
    }

    /// Declares the class of `material` for the look (metal for a gold button; a material not declared is inferred).
    pub fn classify(&mut self, material: &MaterialName, class: omuzaki_char_api::MaterialClass) {
        self.output.material_classes.push((material.clone(), class));
    }

    /// Adds a texture named `<part>-<name>`, and returns its name.
    ///
    /// # Errors
    /// [`BuildError::Part`] if the name isn't a valid id or the output already has it.
    pub fn texture(
        &mut self,
        ctx: &BuildCtx<'_>,
        name: &str,
        data: omuzaki_char_api::TextureData,
    ) -> Result<TextureName, BuildError> {
        let id = id_of::<TextureName>(ctx, &format!("{}-{name}", self.part))?;
        if self.output.textures.iter().any(|(t, _)| *t == id) {
            return Err(ctx.fail(format!("the garment already has a texture `{id}`")));
        }
        self.output.textures.push((id.clone(), data));
        Ok(id)
    }

    /// Ends the garment: publishes its [`Envelope`] over `regions` with the ease of `common`, cut with `exponent`.
    ///
    /// # Errors
    /// As [`publish_envelope`]; [`BuildError::Part`] if no surface was added.
    pub fn finish(
        mut self,
        ctx: &BuildCtx<'_>,
        common: &GarmentCommon,
        regions: &[BodyRegion],
        exponent: f64,
    ) -> Result<PartOutput, BuildError> {
        if self.output.meshes.is_empty() {
            return Err(ctx.fail("the garment built no surface"));
        }
        self.output.publishes = publish_envelope(ctx, regions, common.ease, exponent)?;
        Ok(self.output)
    }
}

/// `text` as an id of type `T`, or a [`BuildError::Part`] naming it.
fn id_of<T: std::str::FromStr>(ctx: &BuildCtx<'_>, text: &str) -> Result<T, BuildError>
where
    T::Err: std::fmt::Display,
{
    text.parse::<T>().map_err(|e| ctx.fail(format!("`{text}` isn't a valid name: {e}")))
}
