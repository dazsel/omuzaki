// The environment backdrop (`environment/backdrop.rs`): one triangle over the whole picture at the far plane, drawn after
// the opaque surfaces with a `<=` depth test, so it lands only where nothing was drawn. Each pixel reads the sky's
// equirectangular picture in its view direction. Conventions are the contract's (`omuzaki_render_api::EnvironmentDesc`):
// `u = atan2(z, x) / 2π + 0.5`, rows from straight up to straight down, linear light, premultiplied (opaque) output.

struct Backdrop {
    // The columns of the matrix from normalised device coordinates (x, y, 1) to a direction in the sky's frame.
    rays: array<vec4<f32>, 3>,
    // x: the intensity.
    params: vec4<f32>,
}

@group(0) @binding(0) var<uniform> backdrop: Backdrop;
@group(0) @binding(1) var sky: texture_2d<f32>;
@group(0) @binding(2) var sky_sampler: sampler;

const PI: f32 = 3.14159265358979;

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    // (-1, -1), (3, -1), (-1, 3): one triangle that covers the picture.
    let xy = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u)) * 2.0 - 1.0;
    var out: VertexOut;
    out.clip = vec4<f32>(xy, 1.0, 1.0);
    out.ndc = xy;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let rays = mat3x3<f32>(backdrop.rays[0].xyz, backdrop.rays[1].xyz, backdrop.rays[2].xyz);
    let d = normalize(rays * vec3<f32>(in.ndc, 1.0));
    let uv = vec2<f32>(atan2(d.z, d.x) / (2.0 * PI) + 0.5, acos(clamp(d.y, -1.0, 1.0)) / PI);
    // Level 0 always: the seam where u wraps would otherwise pick a coarse level from the jump in the derivative.
    let radiance = textureSampleLevel(sky, sky_sampler, uv, 0.0).rgb * backdrop.params.x;
    return vec4<f32>(radiance, 1.0);
}
