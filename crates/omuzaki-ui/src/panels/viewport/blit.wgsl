// Draws the stage's render target into the viewport's rect, one texel per pixel.
//
// The target holds sRGB-encoded bytes in an Rgba8Unorm texture (render-api.md: "egui or a live output can show it
// without conversion"). Into a non-sRGB framebuffer (egui's usual one) they are copied as they are; into an sRGB one
// they are decoded first, because the hardware encodes on write.

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle that covers the viewport.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> Out {
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    var out: Out;
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

@group(0) @binding(0) var target_texture: texture_2d<f32>;
@group(0) @binding(1) var target_sampler: sampler;

@fragment
fn fs_copy(in: Out) -> @location(0) vec4<f32> {
    return textureSample(target_texture, target_sampler, in.uv);
}

fn decode(c: f32) -> f32 {
    if c <= 0.04045 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

@fragment
fn fs_decode(in: Out) -> @location(0) vec4<f32> {
    let c = textureSample(target_texture, target_sampler, in.uv);
    return vec4<f32>(decode(c.r), decode(c.g), decode(c.b), c.a);
}
