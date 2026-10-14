//! The `band-highlight` program: a hard-edged highlight band where the surface faces a fixed direction in **view space**,
//! ported from the prototype's boot leather and brass (`parts/boots.js` 297-306) and the uniform's gold
//! (`parts/uniform.js` 450); hook in `shader/program_band_highlight.wgsl`.
//!
//! After lighting, `outgoing = mix(outgoing, colour, smoothstep(edge0, edge1, dot(N, dir)) * strength)`.
//!
//! **View-space semantics.** `N` is the surface normal rotated into the camera's frame (x right, y up, z toward the
//! viewer) and `dir` is a direction in that same frame, so the highlight is fixed on screen: it rides with the camera and
//! does not move when the character or the lights turn. The default is the prototype's half vector, up and to the left
//! of the eye. `dir` is normalised by the shader; a zero vector draws no highlight.
//!
//! | Parameter | Meaning | Default (prototype) |
//! |---|---|---|
//! | `dir` | view-space direction of the band centre, components in `-1..=1` | `(-0.32, 0.58, 0.75)` |
//! | `colour` | the highlight colour | `0xfff2c8` |
//! | `edge0`, `edge1` | `dot(N, dir)` where the band starts and is full (`0..=1`; keep `edge1` above `edge0`) | `0.86`, `0.90` |
//! | `strength` | how far the colour replaces the lit one at full (`0..=1`) | `0.6` |

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{ParamValue, Toon};

use super::{ParamSpec, ProgramSpec, register_shading_program};

register_shading_program!(ProgramSpec {
    name: "band-highlight",
    params: &[
        ParamSpec { name: "dir", default: ParamValue::Vec3(Vec3::new(-0.32, 0.58, 0.75)), range: Some((-1.0, 1.0)) },
        ParamSpec::colour("colour", 0x00ff_f2c8),
        ParamSpec::float("edge0", 0.86, 0.0, 1.0),
        ParamSpec::float("edge1", 0.90, 0.0, 1.0),
        ParamSpec::float("strength", 0.6, 0.0, 1.0),
    ],
    attributes: &[],
    toon: |_| Toon::default(),
    wgsl: include_str!("../shader/program_band_highlight.wgsl"),
});
