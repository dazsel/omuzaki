//! The `eye` program: a real eyeball (hook in `eye.wgsl`). The mesh is the eye's outer surface: the white of the eye
//! (a sphere of `radius`) and the clear cornea bulging in front of the iris. The iris is not geometry: the shader
//! refracts the view ray at the cornea (`ior`) and follows it to the iris, a shallow cone `iris-depth` behind the eye's
//! centre plane, so the iris sits behind the cornea with real depth and parallax, the pupil is a hole and the limbus
//! softens as the cornea's edge seen side on. The base texture is the eye's atlas (`omuzaki-eye`): the iris on the
//! left half, the sclera's polar map on the right.
//!
//! Lighting is its own: the sclera wraps light past the terminator and reddens there (`scatter`, `scatter-colour`);
//! the iris is lit through the cornea, brighter on the side away from the light (`caustic`). Reflections come in two
//! layers, each with a Fresnel term, for every light and the environment: the white's own satin sheen
//! (`sclera-roughness`), and over it the tear film, a clear coat (`tear-coat`) that is sharp (`tear-roughness`) but
//! uneven (`tear-wobble`) over the white and is the cornea itself over the iris (`cornea-roughness`); `gloss` scales
//! both.
//!
//! The eye's frame (`centre`, `forward` the gaze, `up`) is in world space: whoever moves the eye updates them with the
//! instance's transform.
//!
//! | Parameter | Meaning | Default |
//! |---|---|---|
//! | `centre`, `forward`, `up` | the eyeball's centre and axes, world space | origin, `+Z`, `+Y` |
//! | `radius` | the eyeball's radius, metres | `0.012` |
//! | `iris-radius` | the iris's radius, metres | `0.0059` |
//! | `pupil` | the pupil's radius, in iris radii | `0.3` |
//! | `iris-depth`, `iris-bulge` | the iris's rim and the pupil edge's rise, in eyeball radii from the centre | `0.82`, `0.035` |
//! | `ior` | the cornea's refractive index | `1.376` |
//! | `limbus`, `limbus-soft` | the cornea's edge, in iris radii; the iris's soft rim | `1.04`, `0.03` |
//! | `limbus-tint` | the white seen through the cornea's clear edge, as a multiplier | `0.82, 0.8, 0.8` |
//! | `cornea-rise` | the cornea's apex past the eyeball's sphere, in radii | `0.09` |
//! | `pupil-colour` | the pupil's colour | `0x060404` |
//! | `scatter`, `scatter-colour` | the white's light wrap and its colour in the wrap | `0.35`, `0xe8a090` |
//! | `caustic` | how much brighter the iris's far side is | `0.6` |
//! | `sclera-roughness` | the white's own sheen under the tear film | `0.35` |
//! | `tear-coat`, `tear-roughness` | how much of the white the tear film covers, and its roughness there | `1`, `0.03` |
//! | `cornea-roughness` | the cornea's roughness | `0.04` |
//! | `gloss` | the reflections, as a multiplier | `1` |
//! | `iris-relief` | how far the iris's light fibres stand out from its dark gaps, lit by each light | `0.008` |
//! | `tear-wobble` | how unevenly the tear film over the sclera reflects (its normal's tilt) | `0.05` |

use omuzaki_core::colour::Colour;
use omuzaki_render_api::{Rim, Toon};

use super::{ParamSpec, ProgramSpec, Values, register_shading_program};

/// The program lights itself; the toon base is only what `toon_colour` computes before `program_light` replaces it.
fn toon(_: &Values) -> Toon {
    Toon {
        shade: Colour::hex(0x00ff_ffff),
        softness: 1.0,
        rim: Rim { strength: 0.0, ..Rim::default() },
        ..Toon::default()
    }
}

register_shading_program!(
    "eye",
    ProgramSpec {
        name: "eye",
        params: &[
            ParamSpec::vec3("centre", [0.0, 0.0, 0.0], -1.0e4, 1.0e4),
            ParamSpec::vec3("forward", [0.0, 0.0, 1.0], -1.0, 1.0),
            ParamSpec::vec3("up", [0.0, 1.0, 0.0], -1.0, 1.0),
            ParamSpec::float("radius", 0.012, 1.0e-4, 1.0),
            ParamSpec::float("iris-radius", 0.0059, 1.0e-4, 1.0),
            ParamSpec::float("pupil", 0.3, 0.05, 0.9),
            ParamSpec::float("iris-depth", 0.82, 0.0, 1.0),
            ParamSpec::float("iris-bulge", 0.035, 0.0, 0.3),
            ParamSpec::float("ior", 1.376, 1.0, 2.0),
            ParamSpec::float("limbus", 1.04, 1.0, 1.4),
            ParamSpec::float("limbus-soft", 0.03, 0.001, 0.3),
            ParamSpec::vec3("limbus-tint", [0.82, 0.8, 0.8], 0.0, 1.0),
            ParamSpec::float("cornea-rise", 0.09, 0.0, 0.5),
            ParamSpec::colour("pupil-colour", 0x0006_0404),
            ParamSpec::float("scatter", 0.35, 0.0, 1.0),
            ParamSpec::colour("scatter-colour", 0x00e8_a090),
            ParamSpec::float("caustic", 0.6, 0.0, 3.0),
            ParamSpec::float("sclera-roughness", 0.35, 0.01, 1.0),
            ParamSpec::float("cornea-roughness", 0.04, 0.01, 1.0),
            ParamSpec::float("gloss", 1.0, 0.0, 4.0),
            ParamSpec::float("iris-relief", 0.008, 0.0, 0.1),
            ParamSpec::float("tear-wobble", 0.05, 0.0, 0.5),
            ParamSpec::float("tear-coat", 1.0, 0.0, 1.0),
            ParamSpec::float("tear-roughness", 0.03, 0.005, 1.0),
        ],
        attributes: &[],
        toon,
        wgsl: include_str!("eye.wgsl"),
    }
);
