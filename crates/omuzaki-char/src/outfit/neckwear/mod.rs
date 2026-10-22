//! Collars and ties: the pieces that hang on a shirt's neck opening.
//!
//! A [`collar`] and a [`tie`] are parts of their own, built after a shirt and attached to the shirt's `collar_ring`
//! anchor (the centre of its neck opening, [`COLLAR_RING`]):
//!
//! ```json
//! { "kind": "collar", "layer": 21, "attach": { "to": "anchor", "part": "shirt", "anchor": "collar_ring" } }
//! ```
//!
//! Both read the shirt's surface back from the envelope the shirt published ([`shirt_shell`]), so they lie on it
//! whatever its ease. Neither widens the body fit any further: a collar and a tie are narrow, and a vest fitted over them
//! clears them with its own opening (the prototype cuts the vest's V above and wider than the knot).
//!
//! Both are laid out in the soft-sd torso's heights, as the prototype wrote them, and fitted to the shirt's torso in
//! fractions of its length ([`along`](crate::outfit::shirt::torso::along)).

use omuzaki_char_api::{AnchorsFact, Attach, AttachOffset, BuildCtx, BuildError, MaterialName, PartMesh};
use omuzaki_core::glam::DVec3;

use super::shirt::frame::{Column, shirt_shell};
use super::shirt::{COLLAR_RING, SHIRT_EXPONENT};
use crate::kit::grid::MeshBuilder;

pub mod collar;
pub mod tie;

/// Where a neckwear part hangs: the shirt's surface as a column, and the centre of its neck opening.
pub(crate) struct Hang {
    /// The shirt's own surface (offset 0 is the cloth).
    pub shirt: Column,
    /// The centre of the shirt's neck opening, from the `collar_ring` anchor.
    pub ring: DVec3,
    /// The opening's radius, metres: its widest half-extent.
    pub opening_radius: f64,
}

/// Finds the shirt a part is attached to and what it published.
///
/// # Errors
/// [`BuildError::Part`] if the part isn't attached to an anchor called `collar_ring`, is attached with an offset (the
/// ring fixes its place), or the shirt published no such anchor; [`BuildError::Missing`] without a body.
pub(crate) fn hang(ctx: &BuildCtx<'_>) -> Result<Hang, BuildError> {
    let attach = ctx
        .attach
        .ok_or_else(|| ctx.fail(format!("it hangs on a shirt: attach it to the shirt's `{COLLAR_RING}` anchor")))?;
    let Attach::Anchor { part, anchor, offset } = attach else {
        return Err(
            ctx.fail(format!("it hangs on a shirt: attach it to the shirt's `{COLLAR_RING}` anchor, not a bone"))
        );
    };
    if anchor.as_str() != COLLAR_RING {
        return Err(ctx.fail(format!("it hangs on the `{COLLAR_RING}` anchor, not `{anchor}`")));
    }
    if *offset != AttachOffset::default() {
        return Err(ctx.fail("the shirt's collar ring fixes its place: leave the attachment's offset at its default"));
    }
    let found = ctx.published.get::<AnchorsFact>().get(&(part.clone(), anchor.clone())).ok_or_else(|| {
        ctx.fail(format!("part `{part}` published no `{COLLAR_RING}` anchor: is it a shirt, built first?"))
    })?;
    let ring = found.position.as_dvec3();
    let shirt =
        Column::new(shirt_shell(ctx.fit()?, SHIRT_EXPONENT), SHIRT_EXPONENT).map_err(|e| ctx.fail(e.to_string()))?;
    let opening = shirt.ring(ring.y, 0.0).map_err(|e| ctx.fail(e.to_string()))?;
    Ok(Hang { shirt, ring, opening_radius: opening.radii.max_element() })
}

/// The material or texture name `<part>-<name>`.
pub(crate) fn name_of<T: std::str::FromStr>(ctx: &BuildCtx<'_>, name: &str) -> Result<T, BuildError>
where
    T::Err: std::fmt::Display,
{
    let text = format!("{}-{name}", ctx.part);
    text.parse::<T>().map_err(|e| ctx.fail(format!("`{text}` isn't a valid name: {e}")))
}

/// `builder` as the mesh `<part>-<name>` in `material`.
pub(crate) fn mesh_of(
    ctx: &BuildCtx<'_>,
    name: &str,
    builder: MeshBuilder,
    material: MaterialName,
) -> Result<PartMesh, BuildError> {
    builder.finish(format!("{}-{name}", ctx.part), material).map_err(|e| ctx.fail(format!("the `{name}` mesh: {e}")))
}
