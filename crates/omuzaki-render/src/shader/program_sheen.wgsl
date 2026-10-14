// The sheen program (`program/sheen.rs`): the prototype's nylon sheen, `outgoingLight += colour * smoothstep(e0, e1, N.V)`.
// Appended after `support.wgsl`; the parameter offsets (P_COLOUR, P_EDGE0, P_EDGE1) are generated from the declaration.

// `glsl_smoothstep` with a guard: equal or reversed edges (a material may set them so) give a hard step at `e0`, not NaN.
fn sheen_step(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / max(e1 - e0, 1e-6), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    return albedo;
}

fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let facing = sheen_step(program_float(P_EDGE0), program_float(P_EDGE1), saturate(dot(n, v)));
    return lit + program_vec3(P_COLOUR) * facing;
}
