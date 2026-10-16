// The resolve: Vello's straight-alpha output (sRGB-encoded colour) to the premultiplied sRGB values the CPU raster writes
// (Q1 (a), #1433): the face layer is `Premultiplied`, and the canvas premultiplies the encoded values, not linear light.
// One full-screen triangle, cut to the damaged rectangle by the pass's scissor; every target pixel reads the source texel at
// its own coordinates minus the rectangle's origin, because the source holds the damaged part rendered from (0, 0).

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> origin: vec4<i32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let straight = textureLoad(source, vec2<i32>(position.xy) - origin.xy, 0);
    return vec4<f32>(straight.rgb * straight.a, straight.a);
}
