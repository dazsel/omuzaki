//! The `stylized` program: the surface half of an animated-film look (stylized feature animation, game cel shading,
//! comic). It keeps the part's own toon (carried in the `cel-*` parameters: shade colour, terminator, rim, skin
//! treatment) and adds what a single toon terminator can't draw. Hook in `stylized.wgsl`, whose header has the maths.
//!
//! - **A second shade band** (`shade-2*`): a deeper tone of the shade where the form turns well away from the light,
//!   so a cheek or a fold reads in three values (lit, shade, deep shade) as a drawn cel does.
//! - **Cast shadows take the shade tone**, never black, as in drawn animation.
//! - **A wrapped soft ramp** (`wrap`): from hard cel (`0`) to the soft painted falloff of feature animation (`1`).
//! - **A warm terminator** (`terminator`, `terminator-width`): the saturated band where light turns to shade on skin,
//!   the "blood line" of feature animation.
//! - **A cel highlight** (`spec-*`): a hard-edged blob on the half vector, the size and softness of the drawn kind.
//! - **A lit-edge rim** (`rim-*`): a crisp band on the silhouette's lit side, the key light catching the edge.
//!
//! | Parameter | Meaning | Default |
//! |---|---|---|
//! | `shade-2` | the deep shade, as a multiplier on `cel-shade` | `0xc4b4cc` |
//! | `shade-2-shift` | `N.L` below which the deep shade starts, `-1..=1` | `-0.45` |
//! | `shade-2-softness` | half-width of its edge, `0..=1` | `0.03` |
//! | `shade-2-strength` | how much of it shows, `0..=1` (0: two tones) | `1` |
//! | `wrap` | soft wrapped ramp in place of the cel steps, `0..=1` | `0` |
//! | `terminator` | the tint across the terminator (white: none) | `0xffffff` |
//! | `terminator-width` | the band's half-width in `N.L`, `0..=1` (0: none) | `0` |
//! | `spec-colour`, `spec-strength` | the cel highlight's colour and brightness (`0..=4`; 0: none) | `0xffffff`, `0` |
//! | `spec-size`, `spec-softness` | its size and edge, `0..=1` | `0.15`, `0.2` |
//! | `rim-colour`, `rim-strength` | the lit-edge rim's colour and brightness (`0..=4`) | `0xffffff`, `1` |
//! | `rim-width` | its width as a share of the silhouette, `0..=1` (0: none) | `0` |
//! | `cel-*` | the part's toon, carried over by the look (`omuzaki-char-api`'s `look`) | the default toon |

use super::{ParamSpec, ProgramSpec, cel_toon, register_shading_program};

register_shading_program!(
    "stylized",
    ProgramSpec {
        name: "stylized",
        params: &[
            ParamSpec::colour("shade-2", 0x00c4_b4cc),
            ParamSpec::float("shade-2-shift", -0.45, -1.0, 1.0),
            ParamSpec::float("shade-2-softness", 0.03, 0.0, 1.0),
            ParamSpec::float("shade-2-strength", 1.0, 0.0, 1.0),
            ParamSpec::float("wrap", 0.0, 0.0, 1.0),
            ParamSpec::colour("terminator", 0x00ff_ffff),
            ParamSpec::float("terminator-width", 0.0, 0.0, 1.0),
            ParamSpec::colour("spec-colour", 0x00ff_ffff),
            ParamSpec::float("spec-strength", 0.0, 0.0, 4.0),
            ParamSpec::float("spec-size", 0.15, 0.0, 1.0),
            ParamSpec::float("spec-softness", 0.2, 0.0, 1.0),
            ParamSpec::colour("rim-colour", 0x00ff_ffff),
            ParamSpec::float("rim-strength", 1.0, 0.0, 4.0),
            ParamSpec::float("rim-width", 0.0, 0.0, 1.0),
            ParamSpec::colour("cel-shade", 0x009a_8f9e),
            ParamSpec::float("cel-shift", 0.0, -1.0, 1.0),
            ParamSpec::float("cel-softness", 0.12, 0.0, 1.0),
            ParamSpec::colour("cel-rim", 0x006d_74a8),
            ParamSpec::float("cel-rim-power", 3.0, 0.0, 64.0),
            ParamSpec::float("cel-rim-strength", 0.18, 0.0, 4.0),
            ParamSpec::colour("cel-emissive", 0x0000_0000),
            ParamSpec::float("cel-skin", 0.0, 0.0, 1.0),
            ParamSpec::float("cel-skin-floor", 0.42, 0.0, 1.0),
            ParamSpec::float("cel-skin-hue", 0.55, 0.0, 1.0),
        ],
        attributes: &[],
        toon: cel_toon,
        wgsl: include_str!("stylized.wgsl"),
    }
);
