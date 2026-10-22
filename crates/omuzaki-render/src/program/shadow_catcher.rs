//! The `shadow-catcher` program: an invisible ground that shows only the shadows falling on it, as the prototype's three.js
//! `ShadowMaterial` on the stage's ground disc (radius 0.8, 48 segments, opacity 0.25). Hook: `shader/program_shadow_catcher.wgsl`.
//!
//! The surface is black and its alpha is `opacity * (1 - shadow_mask(world_position, up))`: nothing where the light
//! reaches it, `opacity` black in full shadow, a soft edge between (the shadow map's own PCF). Under several shadowed
//! lights the mask is the product of their visibilities, as three's. The material must be
//! `AlphaMode::Blend` (the program draws in the Transparent pass) and receive shadows, and the ground **must not cast**
//! (`Instance::casts_shadow = false`): the shadow map is fitted to the casters, so a caster ground would widen the fit
//! and shrink every other shadow's resolution.
//!
//! | Parameter | Meaning | Default |
//! |---|---|---|
//! | `opacity` | alpha of full shadow, `0..=1` | `0.25` (the G-JS stage) |

use omuzaki_render_api::Toon;

use super::{ParamSpec, ProgramSpec, register_shading_program};

register_shading_program!(ProgramSpec {
    name: "shadow-catcher",
    params: &[ParamSpec::float("opacity", 0.25, 0.0, 1.0)],
    attributes: &[],
    toon: |_| Toon::default(),
    wgsl: include_str!("../shader/program_shadow_catcher.wgsl"),
});
