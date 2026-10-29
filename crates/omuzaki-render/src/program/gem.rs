//! The `gem` program: a cut stone for rings, earrings, pendants and filigree settings (#2413). An anime gem is drawn as
//! facets of different brightness, a dark heart and a bright table, a crisp white highlight and, now and then, a
//! star-shaped sparkle; hook in `gem.wgsl`.
//!
//! - **Facets.** The normal, in view space, is snapped to a lattice (`facets` cuts across), and each facet gets its own
//!   brightness from a hash of where it points, so the stone breaks into flat planes however smooth its mesh is.
//! - **Depth.** The middle of the stone (facing the eye) darkens toward a deeper tone of its colour and its edges glow,
//!   as light leaking out of the girdle (`depth`).
//! - **Fire.** Facets shift their hue a little (`fire`), the dispersion of a real stone, drawn as flat tints.
//! - **Highlights and sparkle.** Each light makes a hard white highlight on the facets facing its half vector, and a
//!   few facets flash in turn with the scene's time (`sparkle`).
//!
//! | Parameter | Meaning | Default |
//! |---|---|---|
//! | `shine` | the highlight and sparkle colour | `0xffffff` |
//! | `facets` | how many facets across, `3..=24` | `7` |
//! | `depth` | how dark the heart and bright the edges, `0..=1` | `0.6` |
//! | `fire` | how far facets shift hue, `0..=1` | `0.35` |
//! | `sparkle` | how strongly facets flash, `0..=1` | `0.6` |
//!
//! The toon base is soft (`softness 0.4`), so the facets carry the form.

use omuzaki_core::colour::Colour;
use omuzaki_render_api::{Rim, Toon};

use super::{ParamSpec, ProgramSpec, Values, register_shading_program};

/// A soft terminator and a strong rim: light gathering at the stone's edge.
fn toon(values: &Values) -> Toon {
    Toon {
        shade: Colour::hex(0x0030_2840),
        shift: 0.1,
        softness: 0.4,
        rim: Rim { colour: values.colour("shine").unwrap_or(Colour::hex(0x00ff_ffff)), power: 2.5, strength: 0.3 },
        ..Toon::default()
    }
}

register_shading_program!(
    "gem",
    ProgramSpec {
        name: "gem",
        params: &[
            ParamSpec::colour("shine", 0x00ff_ffff),
            ParamSpec::float("facets", 7.0, 3.0, 24.0),
            ParamSpec::float("depth", 0.6, 0.0, 1.0),
            ParamSpec::float("fire", 0.35, 0.0, 1.0),
            ParamSpec::float("sparkle", 0.6, 0.0, 1.0),
        ],
        attributes: &[],
        toon,
        wgsl: include_str!("gem.wgsl"),
    }
);
