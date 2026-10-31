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
//! **Presets** are programs of their own, so a look, a garment or an agent picks a cloth by name: `fabric-cotton` (the
//! same as `fabric`), `fabric-satin`, `fabric-knit`, `fabric-leather`, `fabric-denim` and `fabric-velvet` are this
//! program with those defaults; any parameter can still be set over them.

use super::{ParamSpec, ProgramSpec, cel_toon, register_shading_program, with_cel};

/// The program's parameters with the defaults of one cloth (see the table above for what each means).
macro_rules! fabric_params {
    (
        weave: $weave:expr, scale: $scale:expr, depth: $depth:expr, roughness: $roughness:expr, specular: $specular:expr,
        anisotropy: $anisotropy:expr, wrap: $wrap:expr, sheen: $sheen:expr, sheen_roughness: $sheen_roughness:expr,
        fold_strength: $fold_strength:expr $(,)?
    ) => {
        with_cel![0.0;
            ParamSpec::float("weave", $weave, 0.0, 3.0),
            ParamSpec::float("weave-scale", $scale, 10.0, 5000.0),
            ParamSpec::float("weave-depth", $depth, 0.0, 1.0),
            ParamSpec::float("roughness", $roughness, 0.02, 1.0),
            ParamSpec::float("specular", $specular, 0.0, 0.2),
            ParamSpec::float("anisotropy", $anisotropy, 0.0, 1.0),
            ParamSpec::float("wrap", $wrap, 0.0, 1.0),
            ParamSpec::float("sheen", $sheen, 0.0, 2.0),
            ParamSpec::colour("sheen-colour", 0x00ff_ffff),
            ParamSpec::float("sheen-roughness", $sheen_roughness, 0.05, 1.0),
            ParamSpec::colour("fold-colour", 0x006a_5f8a),
            ParamSpec::float("fold-strength", $fold_strength, 0.0, 1.0),
            ParamSpec::float("fold-softness", 0.5, 0.0, 1.0),
        ]
    };
}

/// Cotton: a matte plain weave with a soft fuzz. Also the program's own defaults.
const COTTON: &[ParamSpec] = fabric_params!(
    weave: 0.0, scale: 900.0, depth: 0.5, roughness: 0.85, specular: 0.03, anisotropy: 0.0, wrap: 0.25, sheen: 0.35,
    sheen_roughness: 0.6, fold_strength: 0.4,
);

register_shading_program!(
    "fabric",
    ProgramSpec { name: "fabric", params: COTTON, attributes: &[], toon: cel_toon, wgsl: include_str!("fabric.wgsl") }
);

register_shading_program!(
    "fabric-cotton",
    ProgramSpec {
        name: "fabric-cotton",
        params: COTTON,
        attributes: &[],
        toon: cel_toon,
        wgsl: include_str!("fabric.wgsl"),
    }
);

// Satin: smooth warp floats with a bright streak of highlight across the threads.
register_shading_program!(
    "fabric-satin",
    ProgramSpec {
        name: "fabric-satin",
        params: fabric_params!(
            weave: 1.0, scale: 2500.0, depth: 0.12, roughness: 0.3, specular: 0.06, anisotropy: 0.85, wrap: 0.1,
            sheen: 0.15, sheen_roughness: 0.3, fold_strength: 0.25,
        ),
        attributes: &[],
        toon: cel_toon,
        wgsl: include_str!("fabric.wgsl"),
    }
);

// Knit: chunky loops, a deep fuzz and soft light.
register_shading_program!(
    "fabric-knit",
    ProgramSpec {
        name: "fabric-knit",
        params: fabric_params!(
            weave: 2.0, scale: 120.0, depth: 0.85, roughness: 0.95, specular: 0.02, anisotropy: 0.0, wrap: 0.4,
            sheen: 0.5, sheen_roughness: 0.8, fold_strength: 0.4,
        ),
        attributes: &[],
        toon: cel_toon,
        wgsl: include_str!("fabric.wgsl"),
    }
);

// Leather: a pebbled grain under a satin gloss.
register_shading_program!(
    "fabric-leather",
    ProgramSpec {
        name: "fabric-leather",
        params: fabric_params!(
            weave: 3.0, scale: 700.0, depth: 0.4, roughness: 0.38, specular: 0.05, anisotropy: 0.0, wrap: 0.0,
            sheen: 0.0, sheen_roughness: 0.6, fold_strength: 0.25,
        ),
        attributes: &[],
        toon: cel_toon,
        wgsl: include_str!("fabric.wgsl"),
    }
);

// Denim: a coarse weave, matte and a little fuzzy.
register_shading_program!(
    "fabric-denim",
    ProgramSpec {
        name: "fabric-denim",
        params: fabric_params!(
            weave: 0.0, scale: 450.0, depth: 0.7, roughness: 0.9, specular: 0.03, anisotropy: 0.0, wrap: 0.25,
            sheen: 0.25, sheen_roughness: 0.7, fold_strength: 0.4,
        ),
        attributes: &[],
        toon: cel_toon,
        wgsl: include_str!("fabric.wgsl"),
    }
);

// Velvet: a dense pile, dark face-on and glowing at the edges.
register_shading_program!(
    "fabric-velvet",
    ProgramSpec {
        name: "fabric-velvet",
        params: fabric_params!(
            weave: 0.0, scale: 2000.0, depth: 0.15, roughness: 1.0, specular: 0.0, anisotropy: 0.0, wrap: 0.15,
            sheen: 1.6, sheen_roughness: 0.35, fold_strength: 0.55,
        ),
        attributes: &[],
        toon: cel_toon,
        wgsl: include_str!("fabric.wgsl"),
    }
);
