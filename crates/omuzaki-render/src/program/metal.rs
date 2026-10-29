//! The `metal` program: anime metal for jewellery, buckles and armour (#2413). Toon metal is not a physical BRDF: it is
//! a hard, banded highlight that reads at any size, a dark reflected horizon across the form, a bright edge, and now and
//! then a glint. Hook in `metal.wgsl`.
//!
//! - **Reflection.** The view reflected off the surface splits into a bright sky above and a dark ground below, with
//!   a hard horizon between them (the classic chrome band), tinted by the lit colour. `reflection` is how far it
//!   replaces the lit colour.
//! - **Bands.** For every spot, point and directional light, the half-vector term `dot(N, H)` is cut into one to three
//!   flat bands (`bands`), their width set by `gloss` and their edges by `sharpness`, in `shine` times the light's
//!   colour. They move with the lights, so a turning ring flashes.
//! - **Glints.** Where the highest band lies, small cells of the surface twinkle with the scene's time (`sparkle`).
//!
//! | Parameter | Meaning | Default |
//! |---|---|---|
//! | `shine` | the colour of the bands and glints (gold's is warm) | `0xfff4dc` |
//! | `bands` | how many highlight bands, `1..=3` | `2` |
//! | `gloss` | how tight the bands are, `0..=1` (1 a pin-point) | `0.7` |
//! | `sharpness` | how hard their edges are, `0..=1` | `0.85` |
//! | `reflection` | how much the reflected sky and ground replace the lit colour, `0..=1` | `0.55` |
//! | `sparkle` | how strongly the brightest band glints, `0..=1` | `0.35` |
//!
//! The toon base is a hard terminator (`softness 0.08`) with a dark shade and a thin bright rim, so the form reads as
//! hard metal before the bands are added.

use omuzaki_core::colour::Colour;
use omuzaki_render_api::{Rim, Toon};

use super::{ParamSpec, ProgramSpec, Values, register_shading_program};

/// A hard terminator and a crisp rim in the shine colour.
fn toon(values: &Values) -> Toon {
    Toon {
        shade: Colour::hex(0x0040_3830),
        shift: 0.05,
        softness: 0.08,
        rim: Rim { colour: values.colour("shine").unwrap_or(Colour::hex(0x00ff_f4dc)), power: 4.0, strength: 0.25 },
        ..Toon::default()
    }
}

register_shading_program!(
    "metal",
    ProgramSpec {
        name: "metal",
        params: &[
            ParamSpec::colour("shine", 0x00ff_f4dc),
            ParamSpec::float("bands", 2.0, 1.0, 3.0),
            ParamSpec::float("gloss", 0.7, 0.0, 1.0),
            ParamSpec::float("sharpness", 0.85, 0.0, 1.0),
            ParamSpec::float("reflection", 0.55, 0.0, 1.0),
            ParamSpec::float("sparkle", 0.35, 0.0, 1.0),
        ],
        attributes: &[],
        toon,
        wgsl: include_str!("metal.wgsl"),
    }
);
