// Draws the premultiplied test image, scaled to fit and centred, over a fully transparent clear.
// `params.view` is (window width, window height, image width, image height) in pixels.

struct Params {
    view: vec4<f32>,
    // x: opacity 0..1 applied to the whole picture; y: 1.0 to write straight (non-premultiplied) colour.
    mix: vec4<f32>,
}

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle covers the window.
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let window = params.view.xy;
    let size = params.view.zw;
    let scale = min(window.x / size.x, window.y / size.y);
    let origin = (window - size * scale) * 0.5;
    let uv = (frag.xy - origin) / (size * scale);
    // textureSampleLevel keeps this branch legal in non-uniform control flow.
    let sampled = textureSampleLevel(image, image_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0);
    let inside = all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
    // The image is premultiplied; scaling every channel by the opacity keeps it premultiplied.
    var colour = select(vec4<f32>(0.0), sampled, inside) * params.mix.x;
    if (params.mix.y > 0.5 && colour.a > 0.0) {
        colour = vec4<f32>(colour.rgb / colour.a, colour.a);
    }
    return colour;
}
