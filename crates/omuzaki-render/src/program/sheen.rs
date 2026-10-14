//! The `sheen` program: a faint glow on the faces that look straight at the camera, ported from the prototype's
//! nylon sheen on the tights (`parts/body.js`, `fragOut` of the `tights` material; hook in `shader/program_sheen.wgsl`).
//!
//! The lit colour gets `colour * smoothstep(edge0, edge1, dot(N, V))` added, `N` being the unit normal facing the viewer
//! and `V` the unit vector toward the eye. At `dot = 1` (a face square to the camera) exactly `colour` is added, in linear
//! light, before tone mapping. The base, toon shading, texture, overlays and outline are the material's own.
//!
//! | Parameter | Meaning | Default (prototype) |
//! |---|---|---|
//! | `colour` | the light added; linear `(0.060, 0.042, 0.040)` | `0x453a38` |
//! | `edge0` | `dot(N, V)` where the sheen starts (`0..=1`) | `0.62` |
//! | `edge1` | `dot(N, V)` where it is at full strength (`0..=1`; keep above `edge0`, else it is a hard step at `edge0`) | `0.96` |

use omuzaki_render_api::Toon;

use super::{ParamSpec, ProgramSpec, register_shading_program};

register_shading_program!(ProgramSpec {
    name: "sheen",
    params: &[
        ParamSpec::colour("colour", 0x0045_3a38),
        ParamSpec::float("edge0", 0.62, 0.0, 1.0),
        ParamSpec::float("edge1", 0.96, 0.0, 1.0),
    ],
    attributes: &[],
    toon: |_| Toon::default(),
    wgsl: include_str!("../shader/program_sheen.wgsl"),
});
