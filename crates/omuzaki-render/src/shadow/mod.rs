//! Shadow maps: the fit of the frustum ([`fit`]), the bounds it is fitted to ([`bounds`]) and the map and its
//! settings ([`map`]). The pass that draws them is `pass/shadows.rs`; the lookup is in `shader/toon.wgsl`.

pub(crate) mod bounds;
pub(crate) mod fit;
pub(crate) mod map;
