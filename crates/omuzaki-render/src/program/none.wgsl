// The two hooks of a draw with no program: both leave the colour alone. A program's WGSL defines the same two functions.

// The surface colour before lighting. `albedo` is linear, with the material's base colour, vertex colours, base texture
// and overlays already applied.
fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    return albedo;
}

// The lit colour after the toon model (lights, rim, skin). `n` is the unit normal facing the viewer and `v` the unit
// vector toward the eye.
fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    return lit;
}
