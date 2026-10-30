// Order-independent transparency's composite (#2507): the weighted average of the layers' colour, covering the scene
// colour as much as the layers together hide it (`pass/transparent.rs`). The output is premultiplied and blended
// `src + dst * (1 - src_alpha)`.
//
// TARGET_TYPE is replaced when the module is built: `texture_2d<f32>`, or `texture_multisampled_2d<f32>`, which is read
// sample by sample, so each sample of the scene colour gets its own layers.

@group(0) @binding(0) var sum: TARGET_TYPE;
@group(0) @binding(1) var reveal: TARGET_TYPE;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_composite(@builtin(position) position: vec4<f32>, @builtin(sample_index) sample: u32) -> @location(0) vec4<f32> {
    let p = vec2<i32>(position.xy);
    // A single-sample texture's third argument is its mip level, which is 0 for sample 0.
    let shows = textureLoad(reveal, p, i32(sample)).r;
    if (shows >= 1.0) {
        discard;
    }
    let total = textureLoad(sum, p, i32(sample));
    let colour = total.rgb / max(total.a, 1e-5);
    let covers = 1.0 - shows;
    return vec4<f32>(colour * covers, covers);
}
