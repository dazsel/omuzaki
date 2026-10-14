// The Shadows pass: casters drawn into the shadow map from the light, depth only. A pass's shader is `common.wgsl`
// followed by this file.
//
//   group 2  b0  the light's view-projection (the first member of `struct Shadow` in toon.wgsl)
//
// Opaque casters have no fragment stage at all. Masked and blended ones (hair cards, lashes) run `fs_cutout`, which
// drops the texels that would not be drawn: below the mask cutoff, and below one half of coverage for blended surfaces,
// so a glass pane or a translucent skirt casts where it is mostly there.

struct ShadowCaster {
    view_proj: mat4x4<f32>,
}

@group(2) @binding(0) var<uniform> caster: ShadowCaster;

const BLEND_CAST_COVERAGE: f32 = 0.5;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) colour: vec4<f32>,
    @location(2) local: vec3<f32>,
}

@vertex
fn vs_main(v: Vertex, @builtin(instance_index) instance: u32) -> VsOut {
    let posed = pose(v, instances[instance]);
    var out: VsOut;
    out.clip = caster.view_proj * vec4<f32>(posed.world_position, 1.0);
    out.uv = v.uv;
    out.colour = v.colour;
    out.local = v.position;
    return out;
}

@fragment
fn fs_cutout(in: VsOut) {
    let albedo = surface_albedo(in.uv, in.colour, in.local);
    let cutoff = select(BLEND_CAST_COVERAGE, material.alpha.x, material.flags.z == ALPHA_MASK);
    if (albedo.a < cutoff) {
        discard;
    }
}
