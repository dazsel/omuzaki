// A port of the JS prototype's toon shading (the prototype's toon.js on three.js r170) to WGSL.
//
// What is matched, and where three.js does it (so a change here can be checked against the original):
//  - lights: directional lights add `colour * mix(shade, 1, ramp) * albedo / PI`; hemisphere light adds
//    `mix(ground, sky, 0.5 + 0.5 * dot(n, up)) * albedo / PI` (three r155+ physical units: no extra PI).
//  - ramp: `smoothstep(shift - soft, shift + soft, n.l)`, the same for every light (toon.js RE_Direct_Toon).
//  - rim: `rimColour * pow(1 - sat(n.v), power) * strength`, added after lighting (toon.js opaque_fragment hook).
//  - tone mapping: three's ACESFilmic (exposure / 0.6), then the sRGB OETF, per fragment before MSAA resolve.
//  - outline: inverted hull, vertices pushed along the normal by `width * clamp(dist / 2, 0.6, 3)`, back faces only.
//  - shadows: one shadowed directional light, ortho map, depth bias plus normal bias, PCF.
// The per-material hooks (skin, face, hair, ...) are the GLSL snippets of parts/*.js, ported one for one.

const PI: f32 = 3.14159265358979;

// Material hook bits (model.rs `Hooks`).
const HOOK_SKIN_OUT: u32 = 1u;
const HOOK_FACE_PROJECT: u32 = 2u;
const HOOK_HAIR: u32 = 4u;
const HOOK_NYLON: u32 = 8u;
const HOOK_LEATHER: u32 = 16u;
const HOOK_BRASS: u32 = 32u;

// Material option bits.
const OPT_DOUBLE_SIDED: u32 = 1u;
const OPT_VERTEX_COLORS: u32 = 2u;
const OPT_BROW_ON: u32 = 4u;
const OPT_HAS_FACE: u32 = 8u;

struct DirLight {
    dir: vec4<f32>,    // xyz: toward the light; w: 1 when this light owns the shadow map
    color: vec4<f32>,  // linear rgb, intensity folded in
}

struct Frame {
    view: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    shadow_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    hemi_sky: vec4<f32>,
    hemi_ground: vec4<f32>,
    lights: array<DirLight, 4>,
    counts: vec4<u32>,   // x: light count
    shadow: vec4<f32>,   // x: depth bias, y: normal bias (m), z: 1 / map size, w: shadows on
    post: vec4<f32>,     // x: exposure, y: outline light, z: ground shadow opacity
}

struct Material {
    base_color: vec4<f32>,
    shade: vec4<f32>,
    rim: vec4<f32>,       // rgb colour, w strength
    params: vec4<f32>,    // x shift, y soft, z rim power, w skin floor
    flags: vec4<u32>,     // x hooks, y options
    face_rect: vec4<f32>, // x0, y0, 1 / (x1 - x0), 1 / (y1 - y0)
    highlight: vec4<f32>,
    hair_root: vec4<f32>,
    hair_mid: vec4<f32>,
    hair_tip: vec4<f32>,
    hair_line: vec4<f32>,
    hair_ring: vec4<f32>,
}

struct Mesh {
    joint_base: u32,
    receive_shadow: u32,
    pad: vec2<u32>,
    outline: vec4<f32>,   // rgb colour, w width
}

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage, read> joints: array<mat4x4<f32>>;
@group(0) @binding(2) var shadow_map: texture_depth_2d;
@group(0) @binding(3) var shadow_sampler: sampler_comparison;

@group(2) @binding(0) var<uniform> mat: Material;
@group(2) @binding(1) var base_tex: texture_2d<f32>;
@group(2) @binding(2) var base_samp: sampler;
@group(2) @binding(3) var face_tex: texture_2d<f32>;
@group(2) @binding(4) var brow_tex: texture_2d<f32>;
@group(2) @binding(5) var clamp_samp: sampler;

@group(1) @binding(0) var<uniform> mesh: Mesh;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) joints: vec4<u32>,
    @location(5) weights: vec4<f32>,
    @location(6) hair_uv: vec2<f32>,
    @location(7) hair_info: vec3<f32>,
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) bind_pos: vec3<f32>,
    @location(5) hair_uv: vec2<f32>,
    @location(6) hair_info: vec3<f32>,
}

fn skin_matrix(v: VsIn) -> mat4x4<f32> {
    let b = mesh.joint_base;
    return joints[b + v.joints.x] * v.weights.x + joints[b + v.joints.y] * v.weights.y
        + joints[b + v.joints.z] * v.weights.z + joints[b + v.joints.w] * v.weights.w;
}

fn vertex_out(v: VsIn, skin: mat4x4<f32>, local_pos: vec3<f32>) -> VsOut {
    var o: VsOut;
    let world = skin * vec4<f32>(local_pos, 1.0);
    o.clip = frame.view_proj * world;
    o.world_pos = world.xyz;
    o.world_normal = (skin * vec4<f32>(v.normal, 0.0)).xyz;
    o.uv = v.uv;
    o.color = v.color;
    o.bind_pos = v.position;
    o.hair_uv = v.hair_uv;
    o.hair_info = v.hair_info;
    return o;
}

@vertex
fn vs_main(v: VsIn) -> VsOut {
    return vertex_out(v, skin_matrix(v), v.position);
}

// Inverted hull: the offset is applied in bind space and skinned with the vertex, as three's outline() does.
@vertex
fn vs_outline(v: VsIn) -> VsOut {
    let skin = skin_matrix(v);
    let dist = max(-(frame.view * (skin * vec4<f32>(v.position, 1.0))).z, 0.1);
    let grown = v.position + normalize(v.normal) * mesh.outline.w * clamp(dist / 2.0, 0.6, 3.0);
    return vertex_out(v, skin, grown);
}

struct ShadowOut {
    @builtin(position) clip: vec4<f32>,
}

@vertex
fn vs_shadow(v: VsIn) -> ShadowOut {
    var o: ShadowOut;
    o.clip = frame.shadow_view_proj * (skin_matrix(v) * vec4<f32>(v.position, 1.0));
    return o;
}

// ---- colour pipeline ----

// GLSL smoothstep semantics: valid when edge0 > edge1 (the hair ring uses a reversed one); WGSL's is not.
fn sstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn sat(x: f32) -> f32 {
    return clamp(x, 0.0, 1.0);
}

// three.js ACESFilmicToneMapping (Stephen Hill's RRT + ODT fit).
fn aces(color_in: vec3<f32>) -> vec3<f32> {
    let input_mat = mat3x3<f32>(
        vec3<f32>(0.59719, 0.07600, 0.02840),
        vec3<f32>(0.35458, 0.90834, 0.13383),
        vec3<f32>(0.04823, 0.01566, 0.83777));
    let output_mat = mat3x3<f32>(
        vec3<f32>(1.60475, -0.10208, -0.00327),
        vec3<f32>(-0.53108, 1.10813, -0.07276),
        vec3<f32>(-0.07367, -0.00605, 1.07602));
    var c = input_mat * (color_in * (frame.post.x / 0.6));
    let a = c * (c + vec3<f32>(0.0245786)) - vec3<f32>(0.000090537);
    let b = c * (0.983729 * c + vec3<f32>(0.4329510)) + vec3<f32>(0.238081);
    c = output_mat * (a / b);
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = pow(c, vec3<f32>(1.0 / 2.4)) * 1.055 - vec3<f32>(0.055);
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

// The canvas in the JS prototype holds sRGB-encoded values, so the target does too (and MSAA resolves in that space).
fn finish(linear: vec3<f32>) -> vec4<f32> {
    return vec4<f32>(linear_to_srgb(aces(linear)), 1.0);
}

// ---- shadows ----

fn shadow_factor(world_pos: vec3<f32>, normal: vec3<f32>) -> f32 {
    let p = world_pos + normal * frame.shadow.y;
    let sc = frame.shadow_view_proj * vec4<f32>(p, 1.0);
    let uv = sc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    let z = sc.z + frame.shadow.x;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || z > 1.0) {
        return 1.0;
    }
    // 3x3 bilinear-compare taps spaced one texel apart: about the softness of three's PCFSoftShadowMap.
    let t = frame.shadow.z;
    var sum = 0.0;
    for (var j = -1; j <= 1; j++) {
        for (var i = -1; i <= 1; i++) {
            sum += textureSampleCompareLevel(shadow_map, shadow_sampler, uv + vec2<f32>(f32(i), f32(j)) * t, z);
        }
    }
    return sum / 9.0;
}

// ---- hair (parts/hair.js) ----

fn hh(n: f32) -> f32 {
    return fract(sin(n) * 43758.5453);
}

fn hn(x: f32) -> f32 {
    let i = floor(x);
    var f = fract(x);
    f = f * f * (3.0 - 2.0 * f);
    return mix(hh(i), hh(i + 1.0), f);
}

fn hair_albedo(albedo: vec3<f32>, in: VsOut, front: bool) -> vec3<f32> {
    let u = in.hair_uv.x;
    let v = in.hair_uv.y;
    let sd = in.hair_info.x;
    var col = mix(mat.hair_root.rgb, mat.hair_mid.rgb, sstep(0.0, 0.25, v));
    col = mix(col, mat.hair_tip.rgb, sstep(0.55, 1.0, v));
    let w = u * 2.6 + sd * 11.0 + 0.35 * sin(v * 14.0 + sd * 20.0);
    let s1 = hn(w * 3.0);
    let s2 = hn(w * 7.0 + 3.0);
    let lines = sstep(0.62, 0.92, s1) * 0.6 + sstep(0.7, 0.95, s2) * 0.4;
    col = mix(col, mat.hair_line.rgb, lines * 0.10);
    col *= mix(1.03, 0.97, s2);
    col *= mix(1.0, 0.86, sstep(0.55, 1.0, abs(u)));
    col *= in.hair_info.y;
    col *= select(0.80, 1.0, front);
    return albedo * col;
}

// Brows painted on their own canvas show through the bangs (anime convention).
fn hair_brows(albedo: vec3<f32>, in: VsOut) -> vec3<f32> {
    let p = vec2<f32>((in.bind_pos.x - mat.face_rect.x) * mat.face_rect.z, (in.bind_pos.y - mat.face_rect.y) * mat.face_rect.w);
    let bc = textureSample(brow_tex, clamp_samp, p);
    let inside = (mat.flags.y & OPT_BROW_ON) != 0u && in.bind_pos.z > 0.0 && p.x > 0.0 && p.x < 1.0 && p.y > 0.0 && p.y < 1.0;
    return select(albedo, mix(albedo, bc.rgb, bc.a * 0.85), inside);
}

fn hair_ring(light_in: vec3<f32>, in: VsOut, n: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    let pol = in.hair_info.z;
    let band = sstep(0.11, 0.0, abs(pol - 0.92 - 0.06 * sin(in.hair_uv.x * 3.0 + in.hair_info.x * 9.0)));
    let teeth = sstep(0.35, 0.75, hn(in.hair_uv.x * 6.0 + in.hair_info.x * 17.0));
    let facing = sstep(0.1, 0.6, dot(n, v));
    var out = light_in + mat.hair_ring.rgb * band * teeth * facing * 0.22;
    // Keep the cream identity under strongly coloured light.
    let lum = dot(out, vec3<f32>(0.299, 0.587, 0.114));
    out = mix(out, lum * vec3<f32>(1.05, 0.97, 0.90), 0.36);
    return out;
}

// ---- skin (toon.js SKIN_OUT) ----

fn skin_out(light_in: vec3<f32>, albedo: vec3<f32>) -> vec3<f32> {
    let luma = vec3<f32>(0.299, 0.587, 0.114);
    let lum_s = dot(light_in, luma);
    let alb_l = max(dot(albedo, luma), 1e-3);
    var out = mix(light_in, albedo * (lum_s / alb_l), 0.55);
    out = max(out, albedo * mat.params.w);
    return out;
}

@fragment
fn fs_toon(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let hooks = mat.flags.x;
    let opts = mat.flags.y;
    let geo_normal = normalize(in.world_normal);
    // Back faces of double-sided materials are lit as faces of their own (three's `normal *= faceDirection`).
    let n = select(-geo_normal, geo_normal, front || (opts & OPT_DOUBLE_SIDED) == 0u);
    let v = normalize(frame.cam_pos.xyz - in.world_pos);

    // Albedo: colour * map * vertex colour, then the material's own diffuse hooks.
    var albedo = mat.base_color.rgb * textureSample(base_tex, base_samp, in.uv).rgb;
    if ((opts & OPT_VERTEX_COLORS) != 0u) {
        albedo *= in.color.rgb;
    }
    let face_uv = vec2<f32>(
        (in.bind_pos.x - mat.face_rect.x) * mat.face_rect.z, (in.bind_pos.y - mat.face_rect.y) * mat.face_rect.w);
    let face = textureSample(face_tex, clamp_samp, face_uv);
    if ((hooks & HOOK_FACE_PROJECT) != 0u && (opts & OPT_HAS_FACE) != 0u) {
        let inside = step(0.0, face_uv.x) * step(face_uv.x, 1.0) * step(0.0, face_uv.y) * step(face_uv.y, 1.0);
        albedo = mix(albedo, face.rgb, face.a * sstep(0.0, 0.07, in.bind_pos.z) * inside);
    }
    if ((hooks & HOOK_HAIR) != 0u) {
        albedo = hair_albedo(albedo, in, front);
        albedo = hair_brows(albedo, in);
    }

    // Lighting: hemisphere + directional ramp.
    let up_weight = 0.5 * n.y + 0.5;
    var light = mix(frame.hemi_ground.rgb, frame.hemi_sky.rgb, up_weight) * albedo / PI;
    let shade = mat.shade.rgb;
    let ramp_lo = mat.params.x - mat.params.y;
    let ramp_hi = mat.params.x + mat.params.y;
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        var color = l.color.rgb;
        if (l.dir.w > 0.5 && frame.shadow.w > 0.5 && mesh.receive_shadow != 0u) {
            color *= shadow_factor(in.world_pos, geo_normal);
        }
        let t = sstep(ramp_lo, ramp_hi, dot(n, l.dir.xyz));
        light += color * mix(shade, vec3<f32>(1.0), t) * albedo / PI;
    }

    // Rim, then the material's own output hook.
    light += mat.rim.rgb * pow(1.0 - sat(dot(n, v)), mat.params.z) * mat.rim.w;
    let n_view = normalize((frame.view * vec4<f32>(n, 0.0)).xyz);
    if ((hooks & HOOK_SKIN_OUT) != 0u) {
        light = skin_out(light, albedo);
    }
    if ((hooks & HOOK_HAIR) != 0u) {
        light = hair_ring(light, in, n, v);
    }
    if ((hooks & HOOK_NYLON) != 0u) {
        light += vec3<f32>(0.060, 0.042, 0.040) * sstep(0.62, 0.96, sat(dot(n, v)));
    }
    if ((hooks & HOOK_LEATHER) != 0u) {
        let hl = dot(n_view, normalize(vec3<f32>(-0.32, 0.58, 0.75)));
        let core = sstep(0.955, 0.972, hl);
        let halo = sstep(0.91, 0.95, hl);
        light = mix(light, mat.highlight.rgb, clamp(core * 0.55 + halo * 0.12, 0.0, 0.65));
    }
    if ((hooks & HOOK_BRASS) != 0u) {
        let hl = dot(n_view, normalize(vec3<f32>(-0.3, 0.6, 0.74)));
        light = mix(light, mat.highlight.rgb, sstep(0.86, 0.9, hl) * 0.6);
    }
    return finish(light);
}

// MeshBasicMaterial: flat colour, tone mapped like everything else.
@fragment
fn fs_basic(in: VsOut) -> @location(0) vec4<f32> {
    var color = mat.base_color.rgb * textureSample(base_tex, base_samp, in.uv).rgb;
    if ((mat.flags.y & OPT_VERTEX_COLORS) != 0u) {
        color *= in.color.rgb;
    }
    return finish(color);
}

@fragment
fn fs_outline(in: VsOut) -> @location(0) vec4<f32> {
    return finish(mesh.outline.rgb * frame.post.y);
}

// The shadow catcher under her feet (three's ShadowMaterial): black, as opaque as the shadow is dark.
@fragment
fn fs_ground(in: VsOut) -> @location(0) vec4<f32> {
    let lit = shadow_factor(in.world_pos, vec3<f32>(0.0, 1.0, 0.0));
    return vec4<f32>(0.0, 0.0, 0.0, frame.post.z * (1.0 - lit));
}
