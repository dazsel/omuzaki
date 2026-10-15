// The resolve: Vello's straight-alpha output (sRGB-encoded colour) to the premultiplied sRGB values the CPU raster writes
// (Q1 (a), #1433): the face layer is `Premultiplied`, and the canvas premultiplies the encoded values, not linear light.
// One full-screen triangle; every target pixel reads the source texel at its own coordinates.

@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let straight = textureLoad(source, vec2<i32>(position.xy), 0);
    return vec4<f32>(straight.rgb * straight.a, straight.a);
}
