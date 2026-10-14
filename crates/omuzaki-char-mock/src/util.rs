//! Small helpers for the mock kinds: fallible id construction (library code never unwraps) and level of detail.

use std::str::FromStr;

use omuzaki_char_api::{AnchorId, BoneName, BuildCtx, BuildError, MaterialName, Quality};
use omuzaki_core::id::SlugError;

fn slug<T: FromStr<Err = SlugError>>(ctx: &BuildCtx<'_>, s: &str) -> Result<T, BuildError> {
    s.parse().map_err(|e: SlugError| ctx.fail(format!("internal id '{s}': {e}")))
}

/// A material name.
pub fn material(ctx: &BuildCtx<'_>, s: &str) -> Result<MaterialName, BuildError> {
    slug(ctx, s)
}

/// A bone name.
pub fn bone(ctx: &BuildCtx<'_>, s: &str) -> Result<BoneName, BuildError> {
    slug(ctx, s)
}

/// An anchor id.
pub fn anchor(ctx: &BuildCtx<'_>, s: &str) -> Result<AnchorId, BuildError> {
    slug(ctx, s)
}

/// Rings and segments for a round mesh: the live numbers, doubled for offline renders.
pub fn detail(quality: Quality, rings: u32, segments: u32) -> (u32, u32) {
    match quality {
        Quality::Live => (rings, segments),
        Quality::Offline => (rings * 2, segments * 2),
    }
}

/// `k / n` as a float.
pub fn frac_usize(k: usize, n: usize) -> f32 {
    #[allow(clippy::cast_precision_loss)] // small counts
    {
        k as f32 / n.max(1) as f32
    }
}
