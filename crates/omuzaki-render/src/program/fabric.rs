//! The `fabric` program: woven, knitted or grained cloth, so a garment reads as what it is made of rather than as matte
//! paint (#2489). Hook in `fabric.wgsl`.
//!
//! A thread pattern (`weave`: plain, satin, knit or leather grain) at `weave-scale` threads per metre shades the surface
//! and tilts its normal by `weave-depth`, fading out where a pixel covers several threads so it never shimmers (the gaps
//! still darken the cloth on average). The warp runs down the garment: the world's up direction laid onto the surface.
//! Light: a wrapped diffuse (`wrap`) whose shade side takes `fold-colour` (by `fold-strength`, over `fold-softness` of the
//! key light's falloff: the coloured fold shadows of stylized cloth); a GGX lobe (`roughness`, `specular`) that
//! `anisotropy` stretches across the warp, as satin's threads reflect; and a Charlie sheen for the fuzz at grazing angles
//! (`sheen`, `sheen-colour`, `sheen-roughness`; the fuzz takes the cloth's own hue, lightened, times `sheen-colour`), as Filament and glTF's `KHR_materials_sheen` define it.
//!
//! | Parameter | Meaning | Default (cotton) |
//! |---|---|---|
//! | `weave` | the pattern: `0` plain weave, `1` satin, `2` knit, `3` leather grain | `0` |
//! | `weave-scale` | threads (or pebbles) per metre | `900` |
//! | `weave-depth` | how strongly the pattern shades and tilts the surface, `0..=1` | `0.5` |
//! | `roughness`, `specular` | the lobe's roughness and its reflectance head-on | `0.85`, `0.03` |
//! | `anisotropy` | `0` a round highlight, `1` a streak across the warp (satin) | `0` |
//! | `wrap` | how far light wraps past the terminator, `0..=1` | `0.25` |
//! | `sheen`, `sheen-colour`, `sheen-roughness` | the fuzz: its strength, colour and spread | `0.35`, `0xffffff`, `0.6` |
//! | `fold-colour`, `fold-strength`, `fold-softness` | the shade side's tint, how far it applies and over how much of the light's falloff | `0x6a5f8a`, `0.4`, `0.5` |
//! | `style` and `cel-*` | the style axis (#2607): `0` this cloth, `1` the cel toon in `cel-*` | `0` |
//!
//! [`PRESETS`] holds the parameters of cotton, satin, knit, leather, denim and velvet.

use omuzaki_render_api::ParamValue;

use super::{ParamSpec, ProgramSpec, cel_toon, register_shading_program, with_cel};

register_shading_program!(
    "fabric",
    ProgramSpec {
        name: "fabric",
        params: with_cel![0.0;
            ParamSpec::float("weave", 0.0, 0.0, 3.0),
            ParamSpec::float("weave-scale", 900.0, 10.0, 5000.0),
            ParamSpec::float("weave-depth", 0.5, 0.0, 1.0),
            ParamSpec::float("roughness", 0.85, 0.02, 1.0),
            ParamSpec::float("specular", 0.03, 0.0, 0.2),
            ParamSpec::float("anisotropy", 0.0, 0.0, 1.0),
            ParamSpec::float("wrap", 0.25, 0.0, 1.0),
            ParamSpec::float("sheen", 0.35, 0.0, 2.0),
            ParamSpec::colour("sheen-colour", 0x00ff_ffff),
            ParamSpec::float("sheen-roughness", 0.6, 0.05, 1.0),
            ParamSpec::colour("fold-colour", 0x006a_5f8a),
            ParamSpec::float("fold-strength", 0.4, 0.0, 1.0),
            ParamSpec::float("fold-softness", 0.5, 0.0, 1.0),
        ],
        attributes: &[],
        toon: cel_toon,
        wgsl: include_str!("fabric.wgsl"),
    }
);

/// A named fabric: the `fabric` program's parameters for it (a parameter left out takes the program's default).
pub struct FabricPreset {
    /// The name a look or a garment selects it by.
    pub name: &'static str,
    /// What it reads as.
    pub summary: &'static str,
    /// Its parameters, by name.
    pub params: &'static [(&'static str, ParamValue)],
}

const fn float(v: f32) -> ParamValue {
    ParamValue::Float(v)
}

/// The fabrics a garment can be made of, each tuned by rendering it close up under a key, a fill and a rim light.
pub const PRESETS: &[FabricPreset] = &[
    FabricPreset {
        name: "cotton",
        summary: "matte plain weave with a soft fuzz",
        params: &[
            ("weave", float(0.0)),
            ("weave-scale", float(900.0)),
            ("weave-depth", float(0.5)),
            ("roughness", float(0.85)),
            ("specular", float(0.03)),
            ("sheen", float(0.35)),
            ("sheen-roughness", float(0.6)),
        ],
    },
    FabricPreset {
        name: "satin",
        summary: "smooth warp floats with a bright streak of highlight across the threads",
        params: &[
            ("weave", float(1.0)),
            ("weave-scale", float(2500.0)),
            ("weave-depth", float(0.12)),
            ("roughness", float(0.3)),
            ("specular", float(0.06)),
            ("anisotropy", float(0.85)),
            ("wrap", float(0.1)),
            ("sheen", float(0.15)),
            ("sheen-roughness", float(0.3)),
            ("fold-strength", float(0.25)),
        ],
    },
    FabricPreset {
        name: "knit",
        summary: "chunky loops, a deep fuzz and soft light",
        params: &[
            ("weave", float(2.0)),
            ("weave-scale", float(120.0)),
            ("weave-depth", float(0.85)),
            ("roughness", float(0.95)),
            ("specular", float(0.02)),
            ("wrap", float(0.4)),
            ("sheen", float(0.5)),
            ("sheen-roughness", float(0.8)),
        ],
    },
    FabricPreset {
        name: "leather",
        summary: "a pebbled grain under a satin gloss",
        params: &[
            ("weave", float(3.0)),
            ("weave-scale", float(700.0)),
            ("weave-depth", float(0.4)),
            ("roughness", float(0.38)),
            ("specular", float(0.05)),
            ("wrap", float(0.0)),
            ("sheen", float(0.0)),
            ("fold-strength", float(0.25)),
        ],
    },
    FabricPreset {
        name: "denim",
        summary: "a coarse twill, matte and a little fuzzy",
        params: &[
            ("weave", float(0.0)),
            ("weave-scale", float(450.0)),
            ("weave-depth", float(0.7)),
            ("roughness", float(0.9)),
            ("specular", float(0.03)),
            ("sheen", float(0.25)),
            ("sheen-roughness", float(0.7)),
        ],
    },
    FabricPreset {
        name: "velvet",
        summary: "a dense pile: dark face-on, glowing at the edges",
        params: &[
            ("weave", float(0.0)),
            ("weave-scale", float(2000.0)),
            ("weave-depth", float(0.15)),
            ("roughness", float(1.0)),
            ("specular", float(0.0)),
            ("wrap", float(0.15)),
            ("sheen", float(1.6)),
            ("sheen-roughness", float(0.35)),
            ("fold-strength", float(0.55)),
        ],
    },
];

/// The preset called `name`.
pub fn fabric_preset(name: &str) -> Option<&'static FabricPreset> {
    PRESETS.iter().find(|p| p.name == name)
}
