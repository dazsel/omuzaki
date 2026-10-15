// Copies the picture rendered by scene.wgsl to the swap chain one pixel to one pixel, applying the opacity and, when the
// surface wants straight colour, the division by alpha. `params.mix.x` is the opacity 0..1; `params.mix.y` is 1.0 to
// write straight (non-premultiplied) colour.

struct Params {
    mix: vec4<f32>,
}

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle covers the window.
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    // The window is as large as the picture, so a texel fetch is exact (no filtering to smear the edges).
    // Scaling every channel by the opacity keeps premultiplied colour premultiplied.
    var colour = textureLoad(image, vec2<i32>(frag.xy), 0) * params.mix.x;
    if (params.mix.y > 0.5 && colour.a > 0.0) {
        colour = vec4<f32>(colour.rgb / colour.a, colour.a);
    }
    return colour;
}
