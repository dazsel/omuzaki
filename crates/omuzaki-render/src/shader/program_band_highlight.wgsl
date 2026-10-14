// The band-highlight program (`program/band_highlight.rs`): the prototype's boot and gold highlight,
// `outgoingLight = mix(outgoingLight, colour, smoothstep(e0, e1, dot(N, dir)) * strength)`, with `N` and `dir` in VIEW
// space (the highlight rides with the camera). Appended after `support.wgsl`; the offsets (P_DIR, P_COLOUR, P_EDGE0,
// P_EDGE1, P_STRENGTH) are generated from the declaration.

// `glsl_smoothstep` with a guard: equal or reversed edges give a hard step at `e0`, not NaN.
fn band_step(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / max(e1 - e0, 1e-6), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    return albedo;
}

// `n` arrives in world space; the camera's view matrix is a rotation plus translation, so w = 0 rotates it into view
// space (x right, y up, z toward the viewer, as three.js's `normal`).
fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let n_view = normalize((frame.view * vec4<f32>(n, 0.0)).xyz);
    let dir = program_vec3(P_DIR);
    // A zero direction cannot be normalised: it draws no highlight.
    let length_dir = length(dir);
    if (length_dir < 1e-6) {
        return lit;
    }
    let band = band_step(program_float(P_EDGE0), program_float(P_EDGE1), dot(n_view, dir / length_dir));
    return mix(lit, program_vec3(P_COLOUR), band * program_float(P_STRENGTH));
}
