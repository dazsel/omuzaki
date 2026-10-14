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
    return VsOut(frame.view_proj * vec4<f32>(pushed, 1.0), pushed);
}

// The line is flat colour, dimmed by `outline_light` so it never glows in a dark scene (the prototype's OUTLINE_LIGHT).
// It covers what is behind it completely, like every opaque surface, and fades into the fog like the surface it outlines.
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(apply_fog(material.outline.rgb * frame.look.x, in.world_position), 1.0);
}
