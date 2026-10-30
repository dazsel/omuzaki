//! The `physical` program: a physically based surface for everything the style axis (#2607) takes away from cel that
//! has no program of its own (cloth, leather, plastic, wood, stone). Hook in `physical.wgsl`.
//!
//! Energy-conserving diffuse (Lambert, with an optional `wrap` for thin translucent cloth) under every light and the
//! environment, a GGX specular lobe with Schlick's Fresnel (`roughness`, `specular`), and a cloth sheen (Charlie, as
//! Filament and glTF's `KHR_materials_sheen` define it: `sheen`, `sheen-colour`, `sheen-roughness`) for the fuzz of
//! fabric at grazing angles. The surface colour is the material's, vertex colours and material layers included.
//!
//! | Parameter | Meaning | Default |
//! |---|---|---|
//! | `roughness` | the specular lobe's roughness, `0.02..=1` | `0.6` |
//! | `specular` | the reflectance head-on (`0.04` for most dielectrics) | `0.04` |
//! | `wrap` | how far light wraps past the terminator, `0..=1` | `0` |
//! | `sheen`, `sheen-colour`, `sheen-roughness` | cloth fuzz: its strength, colour and spread | `0`, `0xffffff`, `0.5` |
//! | `style` and `cel-*` | the style axis: `0` this surface, `1` the cel toon in `cel-*` | `0` |

use super::{ParamSpec, ProgramSpec, cel_toon, register_shading_program, with_cel};

register_shading_program!(
    "physical",
    ProgramSpec {
        name: "physical",
        params: with_cel![0.0;
            ParamSpec::float("roughness", 0.6, 0.02, 1.0),
            ParamSpec::float("specular", 0.04, 0.0, 0.2),
            ParamSpec::float("wrap", 0.0, 0.0, 1.0),
            ParamSpec::float("sheen", 0.0, 0.0, 2.0),
            ParamSpec::colour("sheen-colour", 0x00ff_ffff),
            ParamSpec::float("sheen-roughness", 0.5, 0.05, 1.0),
        ],
        attributes: &[],
        toon: cel_toon,
        wgsl: include_str!("physical.wgsl"),
    }
);
