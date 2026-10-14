// Image-based light (group 3), appended after `common.wgsl` and before `toon.wgsl` (which adds `environment_light` to the
// irradiance of `toon_colour`). Everything here is derived once when the environment is created (`environment/bake.rs`);
// a fragment only evaluates nine harmonics, or picks between two precomputed colours for the toon ramp.
//
// With no environment in the scene, group 3 holds an all-zero stand-in: intensity 0 returns black at once.
//
// Conventions are the contract's (`omuzaki_render_api::EnvironmentDesc`): linear light; the sky turns about +Y by
// `to_sky` (the inverse of the scene's rotation, so a world direction becomes a direction in the sky's own frame);
// irradiance is `integral L max(n.w, 0) dw`, which `toon_colour` divides by PI like every other light.

struct Environment {
    // Nine spherical-harmonic coefficients of the sky's radiance times the cosine lobe's band factors, rgb in xyz.
    sh: array<vec4<f32>, 9>,
    // The columns of the rotation from world to sky.
    to_sky: array<vec4<f32>, 3>,
    // xyz: the sky's luminance-weighted mean direction (sky frame); w: 1 when the toon ramp is on.
    toward: vec4<f32>,
    // The irradiance facing away from `toward` and facing it (rgb), for the toon ramp.
    dark: vec4<f32>,
    bright: vec4<f32>,
    // x: intensity; y: the last mip of the specular cube (its roughness 1).
    params: vec4<f32>,
}

@group(3) @binding(0) var<uniform> environment: Environment;
@group(3) @binding(1) var environment_cube: texture_cube<f32>;
@group(3) @binding(2) var environment_sampler: sampler;

// A world-space direction in the sky's frame.
fn sky_direction(world: vec3<f32>) -> vec3<f32> {
    let to_sky = mat3x3<f32>(environment.to_sky[0].xyz, environment.to_sky[1].xyz, environment.to_sky[2].xyz);
    return to_sky * world;
}

// The sky's diffuse irradiance towards the sky-frame unit normal `n`: the harmonics times their basis.
fn sh_irradiance(n: vec3<f32>) -> vec3<f32> {
    var e = environment.sh[0].xyz * 0.282095;
    e += environment.sh[1].xyz * (0.488603 * n.y);
    e += environment.sh[2].xyz * (0.488603 * n.z);
    e += environment.sh[3].xyz * (0.488603 * n.x);
    e += environment.sh[4].xyz * (1.092548 * n.x * n.y);
    e += environment.sh[5].xyz * (1.092548 * n.y * n.z);
    e += environment.sh[6].xyz * (0.315392 * (3.0 * n.z * n.z - 1.0));
    e += environment.sh[7].xyz * (1.092548 * n.x * n.z);
    e += environment.sh[8].xyz * (0.546274 * (n.x * n.x - n.y * n.y));
    // The nine-term fit can ring slightly below zero beside a very bright source.
    return max(e, vec3<f32>(0.0));
}

// The environment's diffuse light on a surface with world-space unit normal `n`, before the albedo. With the toon ramp
// on, the surface takes the irradiance facing away from the sky's brightest side where it faces away and the one facing
// it where it faces it, blended by the material's own ramp (`terminator`, in toon.wgsl) over the dot of normal and
// brightest direction: the sky lights in the cel bands.
fn environment_light(n: vec3<f32>) -> vec3<f32> {
    let intensity = environment.params.x;
    if (intensity == 0.0) {
        return vec3<f32>(0.0);
    }
    let sky_n = sky_direction(n);
    if (environment.toward.w > 0.5) {
        return mix(environment.dark.xyz, environment.bright.xyz, terminator(dot(sky_n, environment.toward.xyz))) * intensity;
    }
    return sh_irradiance(sky_n) * intensity;
}

// The prefiltered specular radiance towards world-space `direction` for a lobe of `roughness` (0 mirror, 1 widest): the
// chain's level is the roughness. v1's toon materials have no specular term; matcaps and programs read this.
fn environment_specular(direction: vec3<f32>, roughness: f32) -> vec3<f32> {
    let level = clamp(roughness, 0.0, 1.0) * environment.params.y;
    return textureSampleLevel(environment_cube, environment_sampler, sky_direction(direction), level).rgb * environment.params.x;
}
