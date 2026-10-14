// The Bloom pass: three.js's UnrealBloomPass, stage for stage, so the prototype's numbers (strength 0.35, radius 0.5,
// threshold 0.9) give the prototype's look. Everything here works on linear, premultiplied scene colour.
//
//   fs_bright     bilinear half-size copy of the scene colour, keeping only what is brighter than the threshold
//   fs_blur       one direction of a gaussian blur; five mips (each half the size of the one before) x two directions
//   fs_composite  the five blurs weighted and summed into one glow image
//
// Resolve adds the glow image to the scene colour before it tone-maps (`post_resolve.wgsl`).

struct Params {
    direction: vec2<f32>,   // blur: (1, 0) or (0, 1)
    inv_size: vec2<f32>,    // 1 / size of the image this stage draws
    radius: f32,            // blur: the kernel's taps and its sigma, in texels of this mip; composite: Bloom::radius
    strength: f32,          // composite: Bloom::strength
    gain: f32,              // composite: the glow's own scale (see Bloom's module docs)
    threshold: f32,         // bright: luminance above which the scene glows
    footprint: vec2<f32>,   // bright: scene pixels per pixel of the image this stage draws (2 when the scene is twice the size)
    source_texel: vec2<f32>,// bright: one scene pixel, in 0..1
    taps: u32,              // bright: bilinear taps per axis over the footprint
    padding: u32,
}

// Bright and blur read binding 0 (and the sampler); composite also reads the other four mips.
@group(0) @binding(0) var mip0: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var mip1: texture_2d<f32>;
@group(0) @binding(3) var mip2: texture_2d<f32>;
@group(0) @binding(4) var mip3: texture_2d<f32>;
@group(0) @binding(5) var mip4: texture_2d<f32>;
@group(1) @binding(0) var<uniform> params: Params;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

// Where the pixel being drawn sits in the source, in 0..1. `position` is the pixel centre, so this is the centre of the
// source area the pixel covers.
fn uv_of(position: vec4<f32>) -> vec2<f32> {
    return position.xy * params.inv_size;
}

// three.js's LuminosityHighPassShader: a smoothstep over 0.01 above the threshold on Rec. 601 luma, applied to the
// bilinear sample, which at a 2:1 reduction is the plain average of a 2x2 block of scene pixels. A larger scene image
// (`render_scale` above 1) takes a grid of such samples across the pixel's footprint and averages them first, so a thin
// bright line is not missed.
@fragment
fn fs_bright(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = uv_of(position);
    var sum = vec3<f32>(0.0);
    for (var y = 0u; y < params.taps; y++) {
        for (var x = 0u; x < params.taps; x++) {
            let spread = ((vec2<f32>(f32(x), f32(y)) + 0.5) / f32(params.taps) - 0.5) * params.footprint;
            sum += textureSampleLevel(mip0, linear_sampler, uv + spread * params.source_texel, 0.0).rgb;
        }
    }
    let colour = sum / f32(params.taps * params.taps);
    let luma = dot(colour, vec3<f32>(0.299, 0.587, 0.114));
    return vec4<f32>(colour * smoothstep(params.threshold, params.threshold + 0.01, luma), 1.0);
}

// The gaussian is `radius` taps long on each side with sigma = radius (in texels of the image being drawn), normalised by
// the sum of its weights, so the scale of the textbook formula cancels. The source may be larger than the target (the
// first blur of each mip reads the mip before): the bilinear fetch is what reduces it.
@fragment
fn fs_blur(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = uv_of(position);
    let taps = i32(params.radius);
    var sum = textureSampleLevel(mip0, linear_sampler, uv, 0.0).rgb;
    var weights = 1.0;
    for (var i = 1; i < taps; i++) {
        let x = f32(i);
        let weight = exp(-0.5 * x * x / (params.radius * params.radius));
        let offset = params.direction * params.inv_size * x;
        sum += (textureSampleLevel(mip0, linear_sampler, uv + offset, 0.0).rgb
              + textureSampleLevel(mip0, linear_sampler, uv - offset, 0.0).rgb) * weight;
        weights += 2.0 * weight;
    }
    return vec4<f32>(sum / weights, 1.0);
}

// Wider mips count for less: factors 1.0 .. 0.2, and `radius` slides each toward its mirror image (1.2 - factor), so
// radius 1 weights the widest blur most. The factors always sum to 3 whatever the radius.
fn factor(base: f32) -> f32 {
    return mix(base, 1.2 - base, params.radius);
}

@fragment
fn fs_composite(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = uv_of(position);
    let glow = factor(1.0) * textureSampleLevel(mip0, linear_sampler, uv, 0.0).rgb
             + factor(0.8) * textureSampleLevel(mip1, linear_sampler, uv, 0.0).rgb
             + factor(0.6) * textureSampleLevel(mip2, linear_sampler, uv, 0.0).rgb
             + factor(0.4) * textureSampleLevel(mip3, linear_sampler, uv, 0.0).rgb
             + factor(0.2) * textureSampleLevel(mip4, linear_sampler, uv, 0.0).rgb;
    return vec4<f32>(glow * params.strength * params.gain, 1.0);
}
