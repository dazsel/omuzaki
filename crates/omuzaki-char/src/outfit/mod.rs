//! Garments and trims: the `outfit` part kinds lofted over the body fit.
//!
//! [`common`] and [`loft`] are the garment framework, [`trim`] the trims every garment can carry, and each other module
//! is one garment or piece of footwear. A garment is made the same way every time:
//!
//! ```ignore
//! #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
//! #[serde(deny_unknown_fields, default)]
//! pub struct VestParams { pub common: GarmentCommon, pub trims: Vec<TrimSpec>, /* the vest's own shape */ }
//!
//! fn build(p: &VestParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
//!     // The loft's own errors become this part's build error.
//!     let fail = |e: GarmentError| ctx.fail(e.to_string());
//!     // Over the layer below.
//!     let surface = Surface::new(ctx.fit()?, BodyRegion::Torso, SOFT_SQUARE, p.common.ease).map_err(fail)?;
//!     let mut out = GarmentOutput::new(ctx, &p.common)?; // the fabric's material and texture
//!     let mut mesh = MeshBuilder::new();
//!     surface.loft(0.25, 0.85, 24, 48).map_err(fail)?.add_to(&mut mesh, out.tile()).map_err(fail)?;
//!     out.surface(ctx, "body", mesh)?;
//!     for (k, trim) in p.trims.iter().enumerate() {
//!         add_trim(&mut out, ctx, &surface, &format!("trim{k}"), trim)?;
//!     }
//!     out.finish(ctx, &p.common, &[BodyRegion::Torso], SOFT_SQUARE) // publishes the envelope for the next layer
//! }
//! ```
//!
//! The kind also implements [`Garment`](common::Garment) (its common settings, the regions it covers and its open
//! borders), and its test runs the conformance suite (`omuzaki-test-char/tests/support/outfit_conformance.rs`) on it,
//! which builds it on every template and body shape.

pub mod blazer;
pub mod boots;
pub mod cloak;
pub mod coat;
pub mod common;
pub mod drape;
pub mod dress;
pub mod hood;
pub mod legwear;
pub mod loft;
pub mod neckwear;
pub mod robe;
pub mod shirt;
pub mod shoes;
pub mod skirt;
pub mod trim;
pub mod trousers;
pub mod vest;
