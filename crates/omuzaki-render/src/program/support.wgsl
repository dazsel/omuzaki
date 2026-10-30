// What every shading program shares (appended after `toon.wgsl`, see `program/mod.rs`). A program's own WGSL follows
// this file, preceded by the generated `P_<NAME>` offsets of its parameters.

// What the vertex stage hands a program: its extra vertex attributes (declaration order; unused components 0, 0, 0, 1),
// whether this is the front of the surface and where the fragment is in the world, with the screen-space derivatives of
// the position and of the first extra attribute (taken where the fragment stage starts, in uniform control flow), so a
// hook can find which way an attribute runs over the surface (`program_gradient`).
struct ProgramIn {
    extra: array<vec4<f32>, 4>,
    front: bool,
    // The fragment's position in world space (metres), for hooks that look things up in world space (the shadow map).
    world_position: vec3<f32>,
    world_dx: vec3<f32>,
    world_dy: vec3<f32>,
    extra0_dx: vec4<f32>,
    extra0_dy: vec4<f32>,
}

// The world-space direction, along the surface with normal `n`, in which a scalar attribute rises fastest, from its
// screen-space derivatives `dx` and `dy` (the cotangent frame): for hair, which way a lock runs from root to tip. Zero
// where the attribute is flat.
fn program_gradient(p: ProgramIn, n: vec3<f32>, dx: f32, dy: f32) -> vec3<f32> {
    // Framebuffer `y` runs down the screen, so `world_dx x world_dy` faces away from the eye: the crosses are taken the
    // other way round from a `y`-up derivation.
    let across_y = cross(n, p.world_dy);
    let across_x = cross(p.world_dx, n);
    let g = across_y * dx + across_x * dy;
    let len = length(g);
    return select(vec3<f32>(0.0), g / len, len > 1e-12);
}

// Parameter `i` (a float offset, `P_<NAME>`), read from the packed block.
fn program_float(i: u32) -> f32 {
    return material.program[i >> 2u][i & 3u];
}

fn program_vec2(i: u32) -> vec2<f32> {
    return vec2<f32>(program_float(i), program_float(i + 1u));
}

// Vec3 and colour parameters (colours are linear light).
fn program_vec3(i: u32) -> vec3<f32> {
    return vec3<f32>(program_float(i), program_float(i + 1u), program_float(i + 2u));
}

// GLSL's `smoothstep`, which the prototype's shaders use with reversed edges (`smoothstep(0.11, 0.0, x)`); WGSL's is
// undefined unless `e0 < e1`.
fn glsl_smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}
