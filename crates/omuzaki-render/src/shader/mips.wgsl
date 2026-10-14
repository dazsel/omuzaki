// One step of a mip chain: a fullscreen triangle whose pixel `p` averages the source texels under it, with exact area
// weights. Every level is half the one above, rounded down, so for an odd source the last destination pixel covers three
// texels (ratio 2.5 or 3) instead of two; a plain 2x2 tap there would drop the last row and column and lose thin detail
// at the edge. For an even source every weight is 1 and this is the box filter. Reads are `textureLoad`s, so an sRGB
// source is decoded to linear before averaging and the destination encodes the result.

@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // Vertices (0,0) (2,0) (0,2) cover the whole [0,1]^2 square with one oversized triangle.
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

// How much of the unit texel `i` lies inside `lo..hi`.
fn overlap(i: i32, lo: f32, hi: f32) -> f32 {
    return max(0.0, min(f32(i) + 1.0, hi) - max(f32(i), lo));
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(source, 0));
    let destination = max(size / 2, vec2<i32>(1));
    let ratio = vec2<f32>(size) / vec2<f32>(destination);
    let lo = floor(position.xy) * ratio;
    let hi = lo + ratio;
    let first = vec2<i32>(floor(lo));
    var sum = vec4<f32>(0.0);
    var weight = 0.0;
    // The ratio is at most 3, so a pixel touches at most four texels per axis.
    for (var y = 0; y < 4; y++) {
        for (var x = 0; x < 4; x++) {
            let texel = first + vec2<i32>(x, y);
            let w = overlap(texel.x, lo.x, hi.x) * overlap(texel.y, lo.y, hi.y);
            if (w > 0.0 && texel.x < size.x && texel.y < size.y) {
                sum += textureLoad(source, texel, 0) * w;
                weight += w;
            }
        }
    }
    return sum / weight;
}
