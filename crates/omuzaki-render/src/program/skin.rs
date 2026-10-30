//! The `skin` program: realistic skin for close-ups (hook in `skin.wgsl`). Light scatters under the surface, so the
//! terminator softens and reddens and so do shadow edges (`sss`, `scatter-colour`); a thin oily layer reflects every
//! light and the environment (`roughness`, `specular`, `sheen`); pores and fine lines break the surface up at
//! `pore-scale` per metre and `pore-depth`, fading out where a pixel covers several; the `skin-info` attribute marks
//! wet skin (the lid's wet line: glossy, tinted toward `wet-colour`), folds (the cavity term darkens them) and flush.
//!
//! | Parameter | Meaning | Default |
//! |---|---|---|
//! | `sss` | subsurface scattering: the diffusion profile's spread (1 is human skin, 0 off) | `1` |
//! | `wrap` | with `sss` 0: how far light wraps past the terminator, `0..=1` | `0.4` |
//! | `scatter-colour` | the colour of scattered light at the terminator and shadow edges | `0xd8584a` |
//! | `roughness`, `wet-roughness` | the oily layer's roughness, dry and wet | `0.42`, `0.08` |
//! | `specular` | the oily layer's reflectance head-on | `0.028` |
//! | `sheen` | reflections, as a multiplier | `1` |
//! | `pore-scale`, `pore-depth` | pores per metre, and their depth | `2600`, `0.1` |
//! | `cavity` | how much the cavity term darkens folds, `0..=1` | `1` |
//! | `flush-colour` | the multiplier full flush applies | `0xffc8c0` |
//! | `wet-colour`, `wet-tint` | the wet line's colour and how far wet skin takes it | `0xd88a88`, `0.6` |
//!
//! | `style` and `cel-*` | the style axis (#2607): `0` this physical skin, `1` the cel toon in `cel-*` | `0` |
//!
//! Attribute: `skin-info` (wet, cavity, flush, curvature in 1/m).
//!
//! **Subsurface scattering** is pre-integrated (Penner): for each light the diffusion profile (d'Eon and Luebke's six
//! Gaussians for skin) is integrated round a ring of the surface's own curvature, so light bleeds past the terminator,
//! reddening, more where the surface curves tightly (lids, nose, ears). Red diffuses from a smoother normal than blue.

use super::{AttributeSpec, ParamSpec, ProgramSpec, cel_toon, register_shading_program, with_cel};

register_shading_program!(
    "skin",
    ProgramSpec {
        name: "skin",
        params: with_cel![0.0;
            ParamSpec::float("sss", 1.0, 0.0, 4.0),
            ParamSpec::float("wrap", 0.4, 0.0, 1.0),
            ParamSpec::colour("scatter-colour", 0x00d8_584a),
            ParamSpec::float("roughness", 0.42, 0.02, 1.0),
            ParamSpec::float("wet-roughness", 0.08, 0.02, 1.0),
            ParamSpec::float("specular", 0.028, 0.0, 0.2),
            ParamSpec::float("sheen", 1.0, 0.0, 4.0),
            ParamSpec::float("pore-scale", 2600.0, 10.0, 1.0e5),
            ParamSpec::float("pore-depth", 0.1, 0.0, 2.0),
            ParamSpec::float("cavity", 1.0, 0.0, 1.0),
            ParamSpec::colour("flush-colour", 0x00ff_c8c0),
            ParamSpec::colour("wet-colour", 0x00d8_8a88),
            ParamSpec::float("wet-tint", 0.6, 0.0, 1.0),
        ],
        attributes: &[AttributeSpec { name: "skin-info", components: 4 }],
        // The program lights itself at `style` 0; the toon is the cel look it blends toward.
        toon: cel_toon,
        wgsl: include_str!("skin.wgsl"),
    }
);
