// Declarations every scene pass shares. A pass's shader is this file followed by its own (`concat!` at build time), so
// the bind groups, the vertex format, skinning and the material surface are written once.
//
// Bind groups (the layouts are in shared.rs and must match):
//   group 0  frame     b0 Frame uniform, b1 instances, b2 joint matrices
//   group 1  material  b0 Material uniform, b1 base texture, b2..b5 overlay textures, b6..b10 their samplers,
//                      b11 b12 the outline width texture and sampler, b13..b19 MToon textures, b20..b26 their samplers
// A pass that needs more appends groups from 2 on.
//
// Colour is linear light everywhere in the scene passes; alpha is straight in `surface_albedo` and premultiplied in what
// a pass writes to the scene colour.

const PI: f32 = 3.14159265358979;
const MAX_LIGHTS: u32 = 8u;

// Light.vector.w
const LIGHT_DIRECTIONAL: f32 = 0.0;
const LIGHT_POINT: f32 = 1.0;
const LIGHT_HEMISPHERE: f32 = 2.0;
const LIGHT_SPOT: f32 = 3.0;

// Material.flags.x
const FLAG_DOUBLE_SIDED: u32 = 1u;
const FLAG_VERTEX_COLOURS: u32 = 2u;
const FLAG_RECEIVES_SHADOWS: u32 = 4u;
const FLAG_SKIN: u32 = 8u;
const FLAG_BASE_TEXTURE: u32 = 16u;
const FLAG_OUTLINE: u32 = 32u;
const FLAG_OUTLINE_WORLD: u32 = 64u;            // the outline width is metres at any distance
const FLAG_OUTLINE_SCREEN: u32 = 128u;          // the outline width is a fraction of the target height
const FLAG_OUTLINE_WIDTH_TEXTURE: u32 = 256u;   // the width is multiplied by the width texture's green channel
const FLAG_RECEIVES_HAIR_SHADOW: u32 = 512u;   // the hair shadow darkens it (a face under its fringe)

// Material.mtoon_flags.x: lit by MToon, and which MToon textures the material has.
const MTOON_ON: u32 = 1u;
const MTOON_SHADE_TEXTURE: u32 = 2u;
const MTOON_SHIFT_TEXTURE: u32 = 4u;
const MTOON_MATCAP_TEXTURE: u32 = 8u;
const MTOON_RIM_TEXTURE: u32 = 16u;
const MTOON_EMISSIVE_TEXTURE: u32 = 32u;
const MTOON_NORMAL_TEXTURE: u32 = 64u;
const MTOON_MASK_TEXTURE: u32 = 128u;
// `Material.fx_flags.x`: what a surface fx does behind its front (`fx_kind` in resource/material.rs); 0 is none.
const FX_TINT: u32 = 1u;
const FX_DISSOLVE: u32 = 2u;

// Material.flags.y
const SHADING_TOON: u32 = 0u;
const SHADING_UNLIT: u32 = 1u;

// Material.flags.z
const ALPHA_OPAQUE: u32 = 0u;
const ALPHA_MASK: u32 = 1u;
const ALPHA_BLEND: u32 = 2u;

// Per-slot texture flags (Material.textures.x holds four bits per slot: base, overlays 0..3, the outline width texture).
const TEXTURE_DECODE_SRGB: u32 = 1u;
const TEXTURE_PREMULTIPLIED: u32 = 2u;

// Instance.skin.y
const INSTANCE_SKINNED: u32 = 1u;

struct Light {
    // Directional: unit direction toward the light. Point and spot: position. w is the kind.
    vector: vec4<f32>,
    // rgb: colour times intensity (hemisphere: sky). w: point or spot range, or 1 for a directional light that casts
    // shadows.
    colour: vec4<f32>,
    // Hemisphere: ground colour times intensity. Spot: x the cosine of the outer angle, y the cosine where the penumbra
    // starts.
    ground: vec4<f32>,
    // Spot: the unit axis from the lamp along the cone.
    axis: vec4<f32>,
}

struct Frame {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    camera_position: vec4<f32>,  // xyz; w is 1 for an orthographic camera
    camera_back: vec4<f32>,      // xyz: the camera's +Z axis in world space (the view direction when orthographic)
    target_size: vec4<f32>,      // internal width, height, 1/width, 1/height
    lights: array<Light, MAX_LIGHTS>,
    counts: vec4<u32>,           // x: light count; y, z: low and high words of the frame index
    look: vec4<f32>,             // x: outline_light; y: scene time in seconds; z: back_light_rim
    fog_colour: vec4<f32>,       // rgb: linear fog colour; w: the depth where fog starts
    fog_far: vec4<f32>,          // x: the depth where surfaces are fully fogged; y: 1 if fog is on
}

struct Instance {
    model: mat4x4<f32>,
    normal_x: vec4<f32>,         // the columns of the normal matrix
    normal_y: vec4<f32>,
    normal_z: vec4<f32>,
    skin: vec4<u32>,             // x: first joint in the palette; y: INSTANCE_* flags
}

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;
@group(0) @binding(2) var<storage, read> joints: array<mat4x4<f32>>;

struct Overlay {
    rect: vec4<f32>,             // planar mapping: min x, min y, max x, max y in the mesh's local space
    params: vec4<f32>,           // x: strength; y, z: fade from and to (local z); w: 1 for planar, 0 for UV
}

struct Material {
    base: vec4<f32>,             // linear rgb, straight alpha
    shade: vec4<f32>,            // toon shade multiplier, rgb
    rim: vec4<f32>,              // rim colour rgb, strength
    emissive: vec4<f32>,         // emissive rgb, skin hue-keep
    outline: vec4<f32>,          // outline colour rgb, width in metres (0: none)
    toon: vec4<f32>,             // shift, softness, rim power, skin floor
    flags: vec4<u32>,            // FLAG_* bits, SHADING_*, ALPHA_*, overlay count
    alpha: vec4<f32>,            // x: mask cutoff
    textures: vec4<u32>,         // x: four bits of TEXTURE_* flags per slot (base, overlay 0..3, outline width)
    overlays: array<Overlay, 4>,
    program: array<vec4<f32>, 16>, // shading program parameters (the programs task)
    uv_transform: vec4<f32>,     // xy: UV scale, zw: UV offset (applied as uv * xy + zw)
    outline_mix: vec4<f32>,      // x: how much the lit surface tints the outline, 0..=1
    mtoon_flags: vec4<u32>,      // x: MTOON_* bits; y: four bits of TEXTURE_* flags per MToon slot
    mtoon_shade: vec4<f32>,      // shade rgb, shading shift texture scale
    mtoon_shading: vec4<f32>,    // shading shift, toony, GI equalisation, rim lighting mix
    mtoon_rim: vec4<f32>,        // rim rgb, fresnel power
    mtoon_matcap: vec4<f32>,     // matcap rgb, rim lift
    mtoon_uv: vec4<f32>,         // UV scroll per second xy, rotation per second, normal map scale
    fx_flags: vec4<u32>,         // surface fx: FX_* kind (0: none), seed
    fx: vec4<f32>,               // amount, 1 / blob size, edge width, glow
    fx_colour: vec4<f32>,        // colour behind the front rgb, sweep mix
    fx_edge: vec4<f32>,          // edge band rgb, sweep start
    fx_sweep: vec4<f32>,         // sweep unit direction xyz (zero: none), sweep end
}

@group(1) @binding(0) var<uniform> material: Material;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var overlay_texture_0: texture_2d<f32>;
@group(1) @binding(3) var overlay_texture_1: texture_2d<f32>;
@group(1) @binding(4) var overlay_texture_2: texture_2d<f32>;
@group(1) @binding(5) var overlay_texture_3: texture_2d<f32>;
@group(1) @binding(6) var base_sampler: sampler;
@group(1) @binding(7) var overlay_sampler_0: sampler;
@group(1) @binding(8) var overlay_sampler_1: sampler;
@group(1) @binding(9) var overlay_sampler_2: sampler;
@group(1) @binding(10) var overlay_sampler_3: sampler;
// Read by the outline hull's vertex stage only.
@group(1) @binding(11) var outline_width_texture: texture_2d<f32>;
@group(1) @binding(12) var outline_width_sampler: sampler;
// MToon's textures (toon.wgsl), in the slot order of `MTOON_*_TEXTURE`.
@group(1) @binding(13) var mtoon_shade_texture: texture_2d<f32>;
@group(1) @binding(14) var mtoon_shift_texture: texture_2d<f32>;
@group(1) @binding(15) var mtoon_matcap_texture: texture_2d<f32>;
@group(1) @binding(16) var mtoon_rim_texture: texture_2d<f32>;
@group(1) @binding(17) var mtoon_emissive_texture: texture_2d<f32>;
@group(1) @binding(18) var mtoon_normal_texture: texture_2d<f32>;
@group(1) @binding(19) var mtoon_mask_texture: texture_2d<f32>;
@group(1) @binding(20) var mtoon_shade_sampler: sampler;
@group(1) @binding(21) var mtoon_shift_sampler: sampler;
@group(1) @binding(22) var mtoon_matcap_sampler: sampler;
@group(1) @binding(23) var mtoon_rim_sampler: sampler;
@group(1) @binding(24) var mtoon_emissive_sampler: sampler;
@group(1) @binding(25) var mtoon_normal_sampler: sampler;
@group(1) @binding(26) var mtoon_mask_sampler: sampler;

// The one vertex format (resource/mesh.rs), with neutral defaults for what a mesh lacks.
struct Vertex {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) colour: vec4<f32>,
    @location(4) joints: vec4<u32>,
    @location(5) weights: vec4<f32>,
}

// ---- fog ----

// `Fog::amount`: linear in the depth along the view direction (the camera's -Z in view space, so the same for an
// orthographic camera), clamped. The width is validated positive (`Fog::validate`) and the division is the contract's own, so even a one-ulp range matches. With fog off the end is 1 and the start 0, and the result is not used.
fn fog_amount(world_position: vec3<f32>) -> f32 {
    let depth = -(frame.view * vec4<f32>(world_position, 1.0)).z;
    return saturate((depth - frame.fog_colour.w) / (frame.fog_far.x - frame.fog_colour.w));
}

// A surface's linear colour faded toward the fog colour (the scene's fog, if any). Applied after lighting and before the
// HDR scene colour reaches post, so exposure and tone mapping see the fogged value. Alpha is not part of it.
fn apply_fog(colour: vec3<f32>, world_position: vec3<f32>) -> vec3<f32> {
    if (frame.fog_far.y < 0.5) {
        return colour;
    }
    return mix(colour, frame.fog_colour.rgb, fog_amount(world_position));
}

// ---- skinning ----

// The weighted sum of up to four joint matrices. Joints with zero weight are skipped: exporters often leave index 0
// there, which may be past the palette.
fn skin_matrix(first: u32, ids: vec4<u32>, weights: vec4<f32>) -> mat4x4<f32> {
    var sum = mat4x4<f32>(vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0));
    for (var i = 0u; i < 4u; i++) {
        if (weights[i] != 0.0) {
            sum = sum + joints[first + ids[i]] * weights[i];
        }
    }
    return sum;
}

struct Posed {
    world_position: vec3<f32>,
    world_normal: vec3<f32>,
}

// A vertex after skinning (if the instance is skinned) and the model transform.
fn pose(v: Vertex, inst: Instance) -> Posed {
    var position = vec4<f32>(v.position, 1.0);
    var normal = v.normal;
    if ((inst.skin.y & INSTANCE_SKINNED) != 0u) {
        let skin = skin_matrix(inst.skin.x, v.joints, v.weights);
        position = skin * position;
        normal = (skin * vec4<f32>(normal, 0.0)).xyz;
    }
    var out: Posed;
    out.world_position = (inst.model * position).xyz;
    out.world_normal = normalize(mat3x3<f32>(inst.normal_x.xyz, inst.normal_y.xyz, inst.normal_z.xyz) * normal);
    return out;
}

// ---- colour ----

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let low = c * 12.92;
    let high = pow(c, vec3<f32>(1.0 / 2.4)) * 1.055 - vec3<f32>(0.055);
    return select(high, low, c <= vec3<f32>(0.0031308));
}

// A sampled texel as straight, linear RGBA. `flags` says what the hardware did not already do for us (resource/texture.rs).
fn texel(sampled: vec4<f32>, flags: u32) -> vec4<f32> {
    var rgb = sampled.rgb;
    if ((flags & TEXTURE_PREMULTIPLIED) != 0u && sampled.a > 0.0) {
        rgb = min(rgb / sampled.a, vec3<f32>(1.0));
    }
    if ((flags & TEXTURE_DECODE_SRGB) != 0u) {
        rgb = srgb_to_linear(rgb);
    }
    return vec4<f32>(rgb, sampled.a);
}

fn texture_flags(slot: u32) -> u32 {
    return (material.textures.x >> (4u * slot)) & 15u;
}

// Overlay `i`'s texel. Each overlay has its own binding, so they cannot be indexed: a switch on the (uniform) index.
fn sample_overlay(i: u32, uv: vec2<f32>) -> vec4<f32> {
    switch (i) {
        case 0u: { return texel(textureSample(overlay_texture_0, overlay_sampler_0, uv), texture_flags(1u)); }
        case 1u: { return texel(textureSample(overlay_texture_1, overlay_sampler_1, uv), texture_flags(2u)); }
        case 2u: { return texel(textureSample(overlay_texture_2, overlay_sampler_2, uv), texture_flags(3u)); }
        default: { return texel(textureSample(overlay_texture_3, overlay_sampler_3, uv), texture_flags(4u)); }
    }
}

// How much of a planar overlay shows at local depth z: a smooth fade, or a hard cut when the ends meet.
fn overlay_fade(start: f32, end: f32, z: f32) -> f32 {
    if (end <= start) {
        return select(0.0, 1.0, z > start);
    }
    return smoothstep(start, end, z);
}

// The UV a UV-addressed texture reads: the mesh's UV scaled, then offset, as the mock's `UvTransform::apply` does. The
// base texture and `Mapping::Uv` overlays go through it. A planar overlay is placed in the mesh's local space and never
// reads the UV, so it does not.
fn material_uv(uv: vec2<f32>) -> vec2<f32> {
    return uv * material.uv_transform.xy + material.uv_transform.zw;
}

// ---- surface fx (`SurfaceFx` in omuzaki-render-api; `SurfaceFx::field` is the same function on the CPU) ----

// lowbias32 of the cell and the seed: bit-identical to the CPU's `fx_hash`. 0..1, 24 bits.
fn fx_hash(cell: vec3<i32>, seed: u32) -> f32 {
    let c = bitcast<vec3<u32>>(cell);
    var h = (c.x * 0x8da6b343u) ^ (c.y * 0xd8163841u) ^ (c.z * 0xcb1ab31fu) ^ (seed * 0x9e3779b9u);
    h ^= h >> 16u;
    h *= 0x7feb352du;
    h ^= h >> 15u;
    h *= 0x846ca68bu;
    h ^= h >> 16u;
    return f32(h >> 8u) / 16777216.0;
}

fn fx_noise(p: vec3<f32>, seed: u32) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);
    let b = vec3<i32>(i);
    let x00 = mix(fx_hash(b, seed), fx_hash(b + vec3<i32>(1, 0, 0), seed), u.x);
    let x10 = mix(fx_hash(b + vec3<i32>(0, 1, 0), seed), fx_hash(b + vec3<i32>(1, 1, 0), seed), u.x);
    let x01 = mix(fx_hash(b + vec3<i32>(0, 0, 1), seed), fx_hash(b + vec3<i32>(1, 0, 1), seed), u.x);
    let x11 = mix(fx_hash(b + vec3<i32>(0, 1, 1), seed), fx_hash(b + vec3<i32>(1, 1, 1), seed), u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

// The front's field at the bind-pose position `local`, 0..1: blobs, mixed with the position along the sweep.
fn fx_field(local: vec3<f32>) -> f32 {
    let seed = material.fx_flags.y;
    let p = local * material.fx.y;
    let noise = saturate(fx_noise(p, seed) * 0.571 + fx_noise(p * 2.03, seed) * 0.286 + fx_noise(p * 4.01, seed) * 0.143);
    let mix_amount = material.fx_colour.w;
    if (mix_amount <= 0.0 || dot(material.fx_sweep.xyz, material.fx_sweep.xyz) < 1e-12) {
        return noise;
    }
    let start = material.fx_edge.w;
    let span = max(abs(material.fx_sweep.w - start), 1e-4);
    let line = saturate((dot(local, material.fx_sweep.xyz) - start) / span);
    return mix(noise, line, saturate(mix_amount));
}

// What the front does at `local`: x is 1 behind it, y how much of the edge band shows (0..1).
fn fx_at(local: vec3<f32>) -> vec2<f32> {
    if (material.fx_flags.x == 0u) {
        return vec2<f32>(0.0);
    }
    let amount = material.fx.x;
    let v = fx_field(local);
    if (v < amount) {
        return vec2<f32>(1.0, 0.0);
    }
    let into = (v - amount) / max(material.fx.z, 1e-4);
    return vec2<f32>(0.0, select(0.0, 1.0 - into * into, amount > 0.0 && into < 1.0));
}

// Whether a dissolve has cut the surface away at `local`.
fn fx_cut(at: vec2<f32>) -> bool {
    return material.fx_flags.x == FX_DISSOLVE && at.x > 0.5;
}

// The albedo with the front's colours laid on: the tint behind it, the edge band at it.
fn fx_albedo(albedo: vec3<f32>, at: vec2<f32>) -> vec3<f32> {
    var out = albedo;
    if (material.fx_flags.x == FX_TINT) {
        out = mix(out, material.fx_colour.rgb, at.x);
    }
    return mix(out, material.fx_edge.rgb, at.y);
}

// The edge band's own light, added after the lights.
fn fx_glow(at: vec2<f32>) -> vec3<f32> {
    return material.fx_edge.rgb * material.fx.w * at.y;
}

// The surface colour before lighting: base colour, vertex colours, base texture, then the overlays in order (the
// painted face goes through these). Straight linear RGBA. Must run in uniform control flow (it samples textures).
fn surface_albedo(uv: vec2<f32>, vertex_colour: vec4<f32>, local: vec3<f32>) -> vec4<f32> {
    var albedo = material.base;
    if ((material.flags.x & FLAG_VERTEX_COLOURS) != 0u) {
        albedo = albedo * vertex_colour;
    }
    if ((material.flags.x & FLAG_BASE_TEXTURE) != 0u) {
        albedo = albedo * texel(textureSample(base_texture, base_sampler, material_uv(uv)), texture_flags(0u));
    }
    for (var i = 0u; i < material.flags.w; i++) {
        let overlay = material.overlays[i];
        var overlay_uv = material_uv(uv);
        var weight = 1.0;
        if (overlay.params.w > 0.5) {
            // The top row of the texture is at max.y.
            let size = overlay.rect.zw - overlay.rect.xy;
            overlay_uv = vec2<f32>((local.x - overlay.rect.x) / size.x, (overlay.rect.w - local.y) / size.y);
            let inside = all(overlay_uv >= vec2<f32>(0.0)) && all(overlay_uv <= vec2<f32>(1.0));
            weight = select(0.0, overlay_fade(overlay.params.y, overlay.params.z, local.z), inside);
        }
        let painted = sample_overlay(i, overlay_uv);
        albedo = vec4<f32>(mix(albedo.rgb, painted.rgb, painted.a * overlay.params.x * weight), albedo.a);
    }
    return albedo;
}
