//! The wgpu renderer: [`WgpuRenderer`] implements the `omuzaki-render-api` contract on a GPU, headless or on the device an
//! application already has (spec v3 §9, §10, §21). Read `docs/design/render-api.md` for the contract and its rationale;
//! this page is the map of the implementation, written for the people who fill in its passes.
//!
//! # How a frame runs
//!
//! `render(target, scene, quality)` does, in order (`renderer.rs`):
//! 1. checks the scene and quality (`omuzaki-render-api` does the rules), then turns the scene's handles into slots, tags
//!    every draw with the passes that want it, and uploads the frame uniform, the instance records and the joint
//!    matrices (`frame/draws.rs`, `frame/uniform.rs`). Anything that can fail on the caller's data fails here, before
//!    the GPU is touched;
//! 2. makes sure the target has colour and depth attachments for this internal size and sample count
//!    (`frame/attachments.rs`);
//! 3. runs the nine passes of the fixed frame graph in order (`frame/graph.rs`), each bracketed by a GPU timestamp and
//!    timed on the CPU. A pass that has nothing to do returns `Skipped` and shows as `None` in the report;
//! 4. submits, and collects GPU timings of earlier frames, which arrive a frame or two late.
//!
//! Nothing on this path waits for the GPU: timings are mapped asynchronously and read when ready, and error scopes are
//! read without blocking. Only [`read_target`](omuzaki_render_api::Renderer::read_target) and
//! [`WgpuRenderer::wait_idle`] block, they are for tests, exports and tools, and each gives up after two minutes with
//! an error (`WAIT_LIMIT` in `gpu.rs`) rather than wait on a GPU that never answers.
//!
//! # Where things live
//!
//! | Concern | Where | Rule |
//! |---|---|---|
//! | A pass | `pass/<name>.rs` + `shader/<name>*.wgsl` | one module per pass, `FramePass` trait, same `new(&Shared)` constructor; `pass::build` lists the constructors and does not change |
//! | What a pass draws | `frame/draws.rs` | **one** draw list; each draw is tagged once, in `prepare`, with the passes that want it. A pass reads `cx.frame.draws.for_pass(Pass::X)` and keeps what it derives (a sort order, fitted matrices) in its own module |
//! | The quality asked for | `cx.frame.quality` | the whole `Quality`; `samples` and `render_scale` are already applied (`cx.frame.samples`, `internal_size`) |
//! | Shared GPU state | `shared.rs` | bind group layouts (frame = group 0, material = group 1, shadow receiver = group 2 and environment = group 3 for lit passes), default textures, the mip generator |
//! | Image-based light | `environment/`, `shader/environment.wgsl` | baked once at `create_environment` (nine harmonics, a prefiltered specular cube); a lit pass binds `cx.environment_group()` at group 3 and `toon.wgsl` adds `environment_light` to the irradiance |
//! | Resources | `resource/` | meshes, textures, materials, targets: each owns its GPU objects; materials carry the **whole** contract `Material` in one uniform |
//! | Per-frame data | `frame/` | camera and lights uniform, instance records, joint palette, draw list, attachments, timing |
//! | Shader code shared by scene passes | `shader/common.wgsl` | the frame/material structs, skinning, `surface_albedo`; a pass's shader is this file plus its own |
//! | Which passes exist | each stub's `supported()` | delete the override to list the pass in `Capabilities::passes`; `tests/conformance.rs` then requires the checks that need it, with no edit |
//! | Which shading programs exist | `program/<name>.rs` + `program/<name>.wgsl`, one file each | self-registered with `register_shading_program!`; nothing lists them (`program/mod.rs` explains how to add one); feeds `Capabilities::shading_programs` |
//! | Tests | `tests/<topic>.rs`, helpers in `tests/support/` | a task adds its tests in a file of its own |
//!
//! # Who edits what
//!
//! A pass task edits its own `pass/<name>.rs` and adds its shaders and its tests (in a file of its own). It needs no edit
//! to `renderer.rs`, `frame/draws.rs`, `frame/graph.rs`, `frame/mod.rs`, `pass/mod.rs` or `tests/conformance.rs`: its
//! draws and the quality are already on `FrameView` (`frame/mod.rs`), and the conformance suite follows
//! `Capabilities::passes`. The planned edits to files outside a task's own pass are these, and only these:
//!
//! | File | Edit |
//! |---|---|
//! | `pass/opaque.rs`, `shader/toon.wgsl` | toon shading is written first; the shadow bind group (group 2) and its lookup in the lighting come next (done); a pipeline per shading program (done: a module and pipelines per program, the extra vertex streams, the two hooks). Three changes to one file, so they land in that order |
//! | `resource/mesh.rs` | the smooth-normal stream and the extra attribute streams (done); both change `GpuMesh::upload`, so whichever lands second rebases |
//! | `frame/attachments.rs` | the field that publishes the shadow bind group to `Opaque` (done: `Attachments::shadow`). Any other lit pass (`Transparent`) binds `shadow.receiver_group()` at group 2 to use `toon.wgsl` |
//! | `resource/material.rs`, `frame/layout_probe.rs`, `lib.rs`, `renderer.rs` | the program lookup and `known_programs`, the `program[]` layout test (a hair material makes it non-zero), `mod program;`, and in `renderer.rs` the parameter check in `check_material` and the registry check at creation |
//! | `pass/resolve.rs`, `shader/post_resolve.wgsl` | grade and grain; `pass/bloom.rs` and `pass/dof.rs` are written with them |
//! | `pass/transparent.rs`, `shader/transparent.wgsl` | done: its blend pipelines are its own; the module is `program::opaque_source` plus `fs_blend`, so it reuses `shader/common.wgsl`, `shader/toon.wgsl` and the program hooks by reading them |
//!
//! # Conventions
//!
//! - **Colour.** Scene passes draw linear, premultiplied RGBA16F HDR (`SCENE_FORMAT` in `gpu.rs`); `Resolve` is the only
//!   place exposure, tone mapping, the sRGB curve and the target's alpha encoding are applied.
//! - **Coordinates.** Camera and projection come from `Camera::projection_matrix` (depth 0..1, +Y up), the same
//!   matrices the mock uses. A mirrored transform flips the front face; double-sided materials do not cull.
//! - **Errors.** Every call that touches the GPU runs inside error scopes, so a validation failure or running out of
//!   memory is a `RenderError`, never a panic.
//!
//! # Testing without a GPU competition
//!
//! `HeadlessOptions { adapter: AdapterPreference::Software }` opens the software rasteriser (lavapipe), which is what
//! tests use. `WGPU_ADAPTER_NAME` and `WGPU_BACKEND` override the choice for any program, so a bench can be pointed at
//! a real GPU.
//!
//! # Capability profiles
//!
//! The device is opened with the limits of a [`CapabilityProfile`] (`Native`, `WebGpu`, `WebGl2`), never with
//! the adapter's own limits, so a feature a laptop lacks fails on the dev machine too. `WgpuRenderer::headless_with_profile`
//! picks one in code; `OMUZAKI_RENDER_PROFILE` picks one for `headless`, so any test or bench runs under any profile.
//! [`REQUIREMENTS`] is what the renderer needs, as data; a profile that falls short fails at creation with all
//! of it listed. Today `WebGl2` falls short: instance records and joint matrices are storage buffers.
#![deny(missing_docs)]

mod arena;
mod caps;
mod environment;
mod frame;
mod gpu;
mod morph;
mod paint;
mod pass;
mod program;
mod readback;
mod renderer;
mod resource;
mod shadow;
mod shared;

// The renderer's shading programs self-register; Apps that render call this at startup (ADR 0015).
omuzaki_core::define_link!();

pub use caps::{CapabilityProfile, PROFILE_ENV, REQUIREMENTS, Requirement, Unmet, unmet};
pub use gpu::{AdapterPreference, HeadlessOptions};
pub use paint::NativePaintTarget;
pub use program::{FABRIC_PRESETS, FabricPreset, UnknownProgram, check_shading_program, fabric_preset};
pub use renderer::WgpuRenderer;
pub use resource::target::NativeTarget;
