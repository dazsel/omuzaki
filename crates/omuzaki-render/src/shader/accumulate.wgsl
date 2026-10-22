// Supersampling's average (`frame/supersample.rs`): one round's resolved scene colour, read texel for texel, added into
// the sum with the blend constant as its weight (1 / rounds). Linear and premultiplied in, the same out, so the post
// passes read the average exactly as they would read a single round.

@group(0) @binding(0) var colour: texture_2d<f32>;
@group(0) @binding(1) var colour_sampler: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VsOut;
    out.clip = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureLoad(colour, vec2<i32>(in.clip.xy), 0);
}
