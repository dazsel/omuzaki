// The Outlines pass: inverted-hull lines. Every vertex is pushed out along its smooth normal and only the faces turned
// away from the camera are drawn, so the line shows as a band around whatever the model covers and hides behind it
// everywhere else. A pass's shader is `common.wgsl` followed by this file.
//
// The width is `Outline::world_width` of the contract (omuzaki-render-api, material.rs): the material's width in metres at
// the reference distance, scaled by the distance of the surface from the camera plane and clamped. A test in
// pass/outlines.rs keeps the constants below equal to the ones there.

const OUTLINE_REFERENCE_DISTANCE: f32 = 2.0;
const OUTLINE_MIN_SCALE: f32 = 0.6;
const OUTLINE_MAX_SCALE: f32 = 3.0;
// The nearest distance the scale is computed at, so a surface level with or behind the camera still gets a sane width.
const OUTLINE_MIN_DEPTH: f32 = 0.1;

// `Outline::world_width`: metres.
fn outline_width(width: f32, view_depth: f32) -> f32 {
    let scale = max(view_depth, OUTLINE_MIN_DEPTH) / OUTLINE_REFERENCE_DISTANCE;
    return width * clamp(scale, OUTLINE_MIN_SCALE, OUTLINE_MAX_SCALE);
}

// The hull vertex: posed like the surface it grows from (skinned, then placed), then moved along its smooth normal.
// The shading normal in `Vertex` is not used, the smooth one (vertex buffer slot 1) takes its place in `pose`.
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // The pushed vertex, for the fog.
    @location(0) world_position: vec3<f32>,
    // The surface's UV and vertex colour, for a masked material's cut (#649).
    @location(1) uv: vec2<f32>,
    @location(2) colour: vec4<f32>,
}

@vertex
fn vs_main(
    v: Vertex,
    @location(6) smooth_normal: vec3<f32>,
    @builtin(instance_index) instance: u32,
) -> VsOut {
    var hull = v;
    hull.normal = smooth_normal;
    let posed = pose(hull, instances[instance]);
    let view_depth = -(frame.view * vec4<f32>(posed.world_position, 1.0)).z;
    let pushed = posed.world_position + posed.world_normal * outline_width(material.outline.w, view_depth);
    return VsOut(frame.view_proj * vec4<f32>(pushed, 1.0), pushed, v.uv, v.colour);
}

// The coverage an alpha mask compares with its cutoff: the albedo's alpha as `surface_albedo` makes it (the overlays
// never change it), without the colour work.
fn mask_coverage(uv: vec2<f32>, vertex_colour: vec4<f32>) -> f32 {
    var coverage = material.base.a;
    if ((material.flags.x & FLAG_VERTEX_COLOURS) != 0u) {
        coverage *= vertex_colour.a;
    }
    if ((material.flags.x & FLAG_BASE_TEXTURE) != 0u) {
        coverage *= texel(textureSample(base_texture, base_sampler, material_uv(uv)), texture_flags(0u)).a;
    }
    return coverage;
}

// The line is flat colour, dimmed by `outline_light` so it never glows in a dark scene (the prototype's OUTLINE_LIGHT).
// It covers what is behind it completely, like every opaque surface, and fades into the fog like the surface it outlines.
// A masked material's hull is cut where its surface is, so a cut-out card's line follows the visible shape (#649, as
// three-vrm's MToon outline keeps the cutoff).
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (material.flags.z == ALPHA_MASK && mask_coverage(in.uv, in.colour) < material.alpha.x) {
        discard;
    }
    return vec4<f32>(apply_fog(material.outline.rgb * frame.look.x, in.world_position), 1.0);
}
