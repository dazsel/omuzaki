// The Outlines pass: inverted-hull lines. Every vertex is pushed out along its smooth normal and only the faces turned
// away from the camera are drawn, so the line shows as a band around whatever the model covers and hides behind it
// everywhere else. A pass's shader is the Opaque pass's toon shader (`program::opaque_source(None)`), which lights the
// surface for the lighting mix, followed by this file.
//
// The width follows the material's `OutlineStyle` (#647, MToon's outline): `Outline::world_width` of the contract by
// default (the material's width in metres at the reference distance, scaled by the distance of the surface from the
// camera plane and clamped), metres at any distance, or a fraction of the target's height pushed in clip space; and it
// is multiplied by the width texture's green channel when the material has one. A test in pass/outlines.rs keeps the
// constants below equal to the ones there.

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

// MToon's `screenCoordinates`: the posed vertex pushed in clip space along its projected normal by `width` of the
// target's height, so the line is the same on screen at any distance and through any lens. The push is normalised in
// pixels (as wide across as down) and scaled by clip w, so the perspective divide leaves it a fixed size in NDC.
fn outline_screen(posed: Posed, width: f32) -> vec4<f32> {
    let clip = frame.view_proj * vec4<f32>(posed.world_position, 1.0);
    let projected = (frame.view_proj * vec4<f32>(posed.world_normal, 0.0)).xy;
    let aspect = frame.target_size.x * frame.target_size.w;
    let in_pixels = vec2<f32>(projected.x * aspect, projected.y);
    let along = length(in_pixels);
    if (along < 1e-6) {
        return clip;
    }
    // `width` of the height is `2 * width` in NDC down the target and `2 * width / aspect` across it.
    let offset = vec2<f32>(in_pixels.x / aspect, in_pixels.y) / along * 2.0 * width * clip.w;
    return vec4<f32>(clip.xy + offset, clip.zw);
}

// The hull vertex: posed like the surface it grows from (skinned, then placed), then moved along its smooth normal.
// The shading normal in `Vertex` is not used, the smooth one (vertex buffer slot 1) takes its place in `pose`.
struct HullOut {
    @builtin(position) clip: vec4<f32>,
    // The pushed vertex (the surface's, for a screen-space width), for the fog and the lighting mix.
    @location(0) world_position: vec3<f32>,
    // The surface's UV and vertex colour, for a masked material's cut (#649) and the lighting mix.
    @location(1) uv: vec2<f32>,
    @location(2) colour: vec4<f32>,
    // The surface's local position and smooth normal, for the lighting mix.
    @location(3) local: vec3<f32>,
    @location(4) world_normal: vec3<f32>,
}

@vertex
fn vs_outline(
    v: Vertex,
    @location(6) smooth_normal: vec3<f32>,
    @builtin(instance_index) instance: u32,
) -> HullOut {
    var hull = v;
    hull.normal = smooth_normal;
    let posed = pose(hull, instances[instance]);
    var width = material.outline.w;
    if ((material.flags.x & FLAG_OUTLINE_WIDTH_TEXTURE) != 0u) {
        let sampled = textureSampleLevel(outline_width_texture, outline_width_sampler, material_uv(v.uv), 0.0);
        width *= texel(sampled, texture_flags(5u)).g;
    }
    var out: HullOut;
    if ((material.flags.x & FLAG_OUTLINE_SCREEN) != 0u) {
        out.clip = outline_screen(posed, width);
        out.world_position = posed.world_position;
    } else {
        var metres = width;
        if ((material.flags.x & FLAG_OUTLINE_WORLD) == 0u) {
            metres = outline_width(width, -(frame.view * vec4<f32>(posed.world_position, 1.0)).z);
        }
        let pushed = posed.world_position + posed.world_normal * metres;
        out.clip = frame.view_proj * vec4<f32>(pushed, 1.0);
        out.world_position = pushed;
    }
    out.uv = v.uv;
    out.colour = v.colour;
    out.local = v.position;
    out.world_normal = posed.world_normal;
    return out;
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

// The line is its colour times a light that mixes from `outline_light` (the prototype's OUTLINE_LIGHT, so a kit line
// never glows in a dark scene) to the surface's own lit colour by the lighting mix (MToon's
// `outlineLightingMixFactor`). An MToon material mixes from 1 instead, as three-vrm does, and its lit colour is its MToon
// colour at the animated UV. It covers what is behind it completely, like every opaque surface, and fades into the
// fog like the surface it outlines. A masked material's hull is cut where its surface is, so a cut-out card's line
// follows the visible shape (#649, as three-vrm's MToon outline keeps the cutoff).
@fragment
fn fs_outline(in: HullOut) -> @location(0) vec4<f32> {
    let uv = mtoon_uv(in.uv);
    if (material.flags.z == ALPHA_MASK && mask_coverage(uv, in.colour) < material.alpha.x) {
        discard;
    }
    var light = vec3<f32>(frame.look.x);
    let lighting_mix = material.outline_mix.x;
    if (mtoon_on()) {
        light = vec3<f32>(1.0);
        if (lighting_mix > 0.0) {
            let lit = surface_albedo(uv, in.colour, in.local).rgb;
            let n = normalize(in.world_normal);
            light = mix(light, mtoon_colour(lit, uv, n, in.world_position, in.colour, 1.0), lighting_mix);
        }
    } else if (lighting_mix > 0.0) {
        var lit = surface_albedo(uv, in.colour, in.local).rgb;
        if (material.flags.y == SHADING_TOON) {
            lit = toon_colour(lit, normalize(in.world_normal), in.world_position);
        }
        light = mix(light, lit, lighting_mix);
    }
    return vec4<f32>(apply_fog(material.outline.rgb * light, in.world_position), 1.0);
}
