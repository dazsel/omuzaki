// Straight alpha to what the material samples. A GPU painter (Vello) writes straight-alpha colour into a storage
// texture; filtering that directly blends colour with the transparent black around it and darkens every edge. This pass
// reads the painter's texels, decodes sRGB, premultiplies in linear light and writes the result into the sampled texture,
// whose `Rgba8UnormSrgb` format encodes it again, so the sampler (anisotropy, wrap, trilinear mips) filters
// premultiplied linear colour, which is the correct thing to average.

@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

// For an sRGB colour texture: the target encodes, so write linear premultiplied.
@fragment
fn fs_srgb(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = textureLoad(source, vec2<i32>(position.xy), 0);
    return vec4<f32>(srgb_to_linear(texel.rgb) * texel.a, texel.a);
}

// For a linear-data texture: nothing to decode or encode.
@fragment
fn fs_linear(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = textureLoad(source, vec2<i32>(position.xy), 0);
    return vec4<f32>(texel.rgb * texel.a, texel.a);
}
