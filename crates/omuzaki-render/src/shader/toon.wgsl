// Toon shading for the scene passes: the ramp with a tinted shade, the rim, the skin treatment and the lights, ported from
// the prototype's `toon.js` (`RE_Direct_Toon`, the rim hook and `SKIN_OUT`; three.js r155+ light units, so a white
// directional light of intensity 1 gives `albedo / PI` on a surface facing it, design decision D10). The mock
// (`omuzaki-render-mock/src/shade.rs`) lights per triangle with the same ramp, shade, hemisphere and skin maths, and
// `tests/toon.rs` compares the two on cube scenes before tone mapping. It is NOT the reference for point lights (it
// uses `(1 - d / range)^2`, not D10's three.js falloff) or for the rim under an orthographic camera (it takes
// `eye - centroid`; here the camera's view axis is used, as three.js `geometryViewDir` does). Those two are checked
// against an f64 oracle of the formulas instead.
//
// This file follows `common.wgsl` and `environment.wgsl`. `toon_colour` is the whole lighting model as one function of the surface, so the
// transparent pass can light its blended surfaces with the same code; `fs_main` is the Opaque pass's entry point.
//
// Per fragment, in order:
//   irradiance = sum over lights of  colour * mix(shade, 1, ramp(N.L))    (directional, point, spot: the spot's colour
//                                                                          also times its cone, so it keeps the cel look)
//              + mix(ground, sky, 0.5 + 0.5 * N.y)                        (hemisphere: no ramp, "up" is world +Y)
//              + environment_light(N)                                      (image-based light, environment.wgsl: smooth, or
//                                                                           through this ramp when the scene asks for it)
//   colour     = albedo * irradiance / PI + emissive + rim
//   skin       = keep the albedo's hue under coloured light, never fall below `floor * albedo`
//
// Colour is linear light and the result is written unclamped to the HDR scene colour; `Resolve` tone maps.

const LUMA: vec3<f32> = vec3<f32>(0.299, 0.587, 0.114);

// ---- the surface ----

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) colour: vec4<f32>,
    @location(2) local: vec3<f32>,
    @location(3) world_position: vec3<f32>,
    @location(4) world_normal: vec3<f32>,
    // The mesh's extra attributes for the shading program (`program/mod.rs`); unused by draws with no program.
    @location(5) extra0: vec4<f32>,
    @location(6) extra1: vec4<f32>,
    @location(7) extra2: vec4<f32>,
    @location(8) extra3: vec4<f32>,
}

// The extra vertex streams, one slot each from vertex buffer 1. A mesh without a stream reads zeros.
struct Extras {
    @location(6) e0: vec4<f32>,
    @location(7) e1: vec4<f32>,
    @location(8) e2: vec4<f32>,
    @location(9) e3: vec4<f32>,
}

@vertex
fn vs_main(v: Vertex, x: Extras, @builtin(instance_index) instance: u32) -> VsOut {
    let posed = pose(v, instances[instance]);
    var out: VsOut;
    out.clip = frame.view_proj * vec4<f32>(posed.world_position, 1.0);
    out.uv = v.uv;
    out.colour = v.colour;
    out.local = v.position;
    out.world_position = posed.world_position;
    out.world_normal = posed.world_normal;
    out.extra0 = x.e0;
    out.extra1 = x.e1;
    out.extra2 = x.e2;
    out.extra3 = x.e3;
    return out;
}

// ---- lighting ----

// The terminator: `smoothstep(shift - softness, shift + softness, N.L)`, and a hard step at `shift` when the softness is
// zero (WGSL's `smoothstep` is undefined when its edges meet). The same ramp serves every light, as in toon.js.
fn terminator(n_dot_l: f32) -> f32 {
    let low = material.toon.x - material.toon.y;
    let high = material.toon.x + material.toon.y;
    if (high <= low) {
        return select(0.0, 1.0, n_dot_l >= low);
    }
    let t = saturate((n_dot_l - low) / (high - low));
    return t * t * (3.0 - 2.0 * t);
}

// How much of a point light's colour reaches distance `d`: three.js `getDistanceAttenuation` with decay 2 and
// `distance = range`, inverse square with a smooth window that reaches zero at `range` (D10: the prototype's candle and
// moonlit rigs use distance 7, decay 2).
fn point_falloff(d: f32, range: f32) -> f32 {
    let window = saturate(1.0 - pow(d / range, 4.0));
    return window * window / max(d * d, 0.01);
}

// How much of a spot light's colour reaches a point seen from the lamp along `to_light` (the unit vector from the point to the lamp): three.js `getSpotAttenuation`, a
// smoothstep of the cosine off the axis between the outer cone and where the penumbra starts. A hard edge (the two meet)
// is a step, as WGSL's `smoothstep` is undefined there.
fn spot_cone(l: Light, to_light: vec3<f32>) -> f32 {
    let outer = l.ground.x;
    let inner = l.ground.y;
    let angle_cos = dot(-to_light, l.axis.xyz);
    if (inner <= outer) {
        return select(0.0, 1.0, angle_cos >= outer);
    }
    let t = saturate((angle_cos - outer) / (inner - outer));
    return t * t * (3.0 - 2.0 * t);
}

// ---- shadows ----

// The shadow map of the light that casts (group 2), drawn by the Shadows pass. `view_proj` maps world space to the map's
// clip space (depth 0..1).
struct Shadow {
    view_proj: mat4x4<f32>,
    // x: the casting light's slot plus one (0: no shadow this frame), y: 1 / map size, z: depth bias in clip depth,
    // w: normal bias in metres.
    params: vec4<f32>,
    // x: the spacing of the 3 x 3 comparison taps, in texels.
    kernel: vec4<f32>,
}

@group(2) @binding(0) var<uniform> shadow: Shadow;
@group(2) @binding(1) var shadow_map: texture_depth_2d;
@group(2) @binding(2) var shadow_sampler: sampler_comparison;

// How much of the light reaches `position` (normal `n`): 1 lit, 0 in shadow, in between across a soft edge. The point is
// moved along the normal first (the prototype's `normalBias`) and the depth compared with the bias taken off, as in
// three.js. The test is a 3 x 3 grid of bilinear comparisons: the softness of three's PCFSoftShadowMap. Outside the
// map, and on materials that do not receive shadows (faces stay clean), it is 1.
fn shadow_visibility(position: vec3<f32>, n: vec3<f32>) -> f32 {
    if ((material.flags.x & FLAG_RECEIVES_SHADOWS) == 0u) {
        return 1.0;
    }
    let clip = shadow.view_proj * vec4<f32>(position + n * shadow.params.w, 1.0);
    let uv = clip.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    let depth = clip.z - shadow.params.z;
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || depth > 1.0) {
        return 1.0;
    }
    var sum = 0.0;
    for (var j = -1; j <= 1; j++) {
        for (var i = -1; i <= 1; i++) {
            let tap = uv + vec2<f32>(f32(i), f32(j)) * (shadow.params.y * shadow.kernel.x);
            sum += textureSampleCompareLevel(shadow_map, shadow_sampler, tap, depth);
        }
    }
    return sum / 9.0;
}

// The light a surface point at `position` with normal `n` receives from light number `index` (`l`), before the albedo:
// for directional and point lights the colour tinted by the ramp (the shadow side shows `material.shade`, never grey).
// The light that casts shadows has its colour multiplied by `shadow_visibility` before the ramp, as in three.js:
// `colour * shadow * mix(shade, 1, ramp)`.
fn light_irradiance(l: Light, index: u32, n: vec3<f32>, position: vec3<f32>) -> vec3<f32> {
    if (l.vector.w == LIGHT_HEMISPHERE) {
        return mix(l.ground.rgb, l.colour.rgb, 0.5 + 0.5 * n.y);
    }
    var to_light = l.vector.xyz;
    var colour = l.colour.rgb;
    if (l.vector.w != LIGHT_DIRECTIONAL) {
        // Point and spot lights: D10's falloff, and a spot is also confined to its cone (no spot shadows yet).
        let offset = l.vector.xyz - position;
        let distance = length(offset);
        to_light = offset / max(distance, 1e-6);
        colour = colour * point_falloff(distance, l.colour.w);
        if (l.vector.w == LIGHT_SPOT) {
            colour = colour * spot_cone(l, to_light);
        }
    } else if (u32(shadow.params.x) == index + 1u) {
        colour = colour * shadow_visibility(position, n);
    }
    return colour * mix(material.shade.rgb, vec3<f32>(1.0), terminator(dot(n, to_light)));
}

// The unit vector from a surface point toward the eye. An orthographic camera looks along one axis everywhere.
fn view_direction(position: vec3<f32>) -> vec3<f32> {
    if (frame.camera_position.w > 0.5) {
        return normalize(frame.camera_back.xyz);
    }
    return normalize(frame.camera_position.xyz - position);
}

// toon.js SKIN_OUT: pull the lit colour back toward the albedo's hue by `hue_keep` (so coloured light does not turn
// skin green), then never let it fall below `floor` of the albedo (so shadows do not go muddy). Both knobs travel in
// the material: `emissive.w` is the hue-keep and `toon.w` the floor.
fn skin_adjust(lit: vec3<f32>, albedo: vec3<f32>) -> vec3<f32> {
    let keep_hue = albedo * (dot(lit, LUMA) / max(dot(albedo, LUMA), 1e-3));
    return max(mix(lit, keep_hue, material.emissive.w), albedo * material.toon.w);
}

// The lit colour of a surface: its linear `albedo`, unit normal `n` facing the viewer, and world `position`.
fn toon_colour(albedo: vec3<f32>, n: vec3<f32>, position: vec3<f32>) -> vec3<f32> {
    var irradiance = environment_light(n);
    for (var i = 0u; i < frame.counts.x; i++) {
        irradiance += light_irradiance(frame.lights[i], i, n, position);
    }
    let rim = material.rim.rgb * pow(1.0 - saturate(dot(n, view_direction(position))), material.toon.z) * material.rim.w;
    var colour = albedo * irradiance / PI + material.emissive.rgb + rim;
    if ((material.flags.x & FLAG_SKIN) != 0u) {
        colour = skin_adjust(colour, albedo);
    }
    return colour;
}

// ---- the Opaque pass ----

@fragment
fn fs_main(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let albedo = surface_albedo(in.uv, in.colour, in.local);
    if (material.flags.z == ALPHA_MASK && albedo.a < material.alpha.x) {
        discard;
    }
    let program = ProgramIn(array<vec4<f32>, 4>(in.extra0, in.extra1, in.extra2, in.extra3), front, in.world_position);
    var colour = albedo.rgb;
    if (material.flags.y == SHADING_TOON) {
        // The back of a double-sided surface is lit as a face of its own (three.js: `normal *= faceDirection`). A
        // shading program colours the surface before the lights and the lit result after them.
        let facing = normalize(in.world_normal);
        let n = select(-facing, facing, front);
        colour = toon_colour(program_albedo(albedo.rgb, program), n, in.world_position);
        colour = program_light(colour, n, view_direction(in.world_position), program);
    }
    // Opaque and masked surfaces cover what is behind them completely. Fog fades unlit surfaces too.
    return vec4<f32>(apply_fog(colour, in.world_position), 1.0);
}
