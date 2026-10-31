// Lens glare (render-api `Glare`), drawn inside the Bloom pass on images half the target's size, in linear premultiplied
// scene colour like bloom's glow, which it is added to.
//
//   fs_bright     half-size copy of the scene colour keeping only what is brighter than the glare's threshold
//   fs_streak     one step of a streak along one axis: seven taps `step` pixels apart, weighted by `falloff` per tap
//                 away from the centre and normalised. Three steps a quarter apart reach 63 steps of the first; each
//                 axis is one line through every light, so N streaks are N / 2 axes (Kawase's light streaks)
//   fs_composite  the axes summed (each holds the light once, so the extra copies of its core are taken out again),
//                 a halo (a ring around each light) and ghosts read from the bright image, tinted and scaled
//
// Resolve adds the result to the scene colour before it tone-maps (`post_resolve.wgsl`), with the glow.

struct Params {
    inv_size: vec2<f32>,     // 1 / size of the image this stage draws
    direction: vec2<f32>,    // streak: the axis, a unit vector in pixels (y down)
    footprint: vec2<f32>,    // bright: scene pixels per pixel of the image this stage draws
    source_texel: vec2<f32>, // bright: one scene pixel, in 0..1
    tint: vec4<f32>,         // composite: linear rgb tint
    step: f32,               // streak: pixels between taps
    falloff: f32,            // streak: the weight of each tap against the one nearer the centre
    threshold: f32,          // bright: linear luminance above which the scene flares
    strength: f32,           // composite: Glare::strength times the glare's own gain
    halo: f32,               // composite: Glare::halo
    ghosts: f32,             // composite: Glare::ghosts
    aspect: f32,             // composite: width / height
    taps: u32,               // bright: bilinear taps per axis over the footprint
    axes: u32,               // composite: how many streak axes are bound (0..=4)
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

// Bright and streak read binding 0 (and the sampler); composite reads the four axes (0, 2, 3, 4) and the bright image (5).
@group(0) @binding(0) var image0: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var image1: texture_2d<f32>;
@group(0) @binding(3) var image2: texture_2d<f32>;
@group(0) @binding(4) var image3: texture_2d<f32>;
@group(0) @binding(5) var bright: texture_2d<f32>;
@group(1) @binding(0) var<uniform> params: Params;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

fn uv_of(position: vec4<f32>) -> vec2<f32> {
    return position.xy * params.inv_size;
}

// The high-pass, as bloom's (a grid of bilinear taps over the pixel's footprint), with a soft knee half the threshold
// wide so a light that just crosses it does not pop.
@fragment
fn fs_bright(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = uv_of(position);
    var sum = vec3<f32>(0.0);
    for (var y = 0u; y < params.taps; y++) {
        for (var x = 0u; x < params.taps; x++) {
            let spread = ((vec2<f32>(f32(x), f32(y)) + 0.5) / f32(params.taps) - 0.5) * params.footprint;
            sum += textureSampleLevel(image0, linear_sampler, uv + spread * params.source_texel, 0.0).rgb;
        }
    }
    let colour = sum / f32(params.taps * params.taps);
    let luma = dot(colour, vec3<f32>(0.2126, 0.7152, 0.0722));
    let knee = params.threshold * 0.5 + 1e-3;
    let keep = smoothstep(params.threshold, params.threshold + knee, luma);
    return vec4<f32>(colour * keep, 1.0);
}

@fragment
fn fs_streak(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = uv_of(position);
    let along = params.direction * params.inv_size * params.step;
    var sum = textureSampleLevel(image0, linear_sampler, uv, 0.0).rgb;
    var weights = 1.0;
    var weight = 1.0;
    for (var i = 1; i <= 3; i++) {
        weight *= params.falloff;
        let offset = along * f32(i);
        sum += (textureSampleLevel(image0, linear_sampler, uv + offset, 0.0).rgb
              + textureSampleLevel(image0, linear_sampler, uv - offset, 0.0).rgb) * weight;
        weights += 2.0 * weight;
    }
    return vec4<f32>(sum / weights, 1.0);
}

// The halo: a ring this fraction of the height across from each light, drawn with this many taps (close enough on the
// half-size image that a light a few pixels wide makes an unbroken ring), and its gain: a tap holds a light only where the
// ring crosses it, so the ring's light is spread over the taps.
const HALO_RADIUS: f32 = 0.08;
const HALO_TAPS: u32 = 32u;
const HALO_GAIN: f32 = 4.0;

// How soft a ghost is: its disc's radius, as a fraction of the height, per unit of its scale; and the taps of the disc.
const GHOST_SOFTNESS: f32 = 0.006;
const GHOST_TAPS: u32 = 8u;

// Each ghost is the bright image scaled through the frame's centre by its factor (negative: across the centre).
const GHOSTS: array<vec2<f32>, 4> = array<vec2<f32>, 4>(
    vec2<f32>(-1.6, 0.45),
    vec2<f32>(-0.7, 0.3),
    vec2<f32>(-2.8, 0.2),
    vec2<f32>(2.2, 0.12),
);

@fragment
fn fs_composite(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = uv_of(position);
    let core = textureSampleLevel(bright, linear_sampler, uv, 0.0).rgb;
    var streaks = vec3<f32>(0.0);
    if (params.axes > 0u) {
        streaks = textureSampleLevel(image0, linear_sampler, uv, 0.0).rgb;
    }
    if (params.axes > 1u) {
        streaks += textureSampleLevel(image1, linear_sampler, uv, 0.0).rgb;
    }
    if (params.axes > 2u) {
        streaks += textureSampleLevel(image2, linear_sampler, uv, 0.0).rgb;
    }
    if (params.axes > 3u) {
        streaks += textureSampleLevel(image3, linear_sampler, uv, 0.0).rgb;
    }
    // Every axis holds what is left of the light's core; keep one copy.
    if (params.axes > 1u) {
        streaks = max(streaks - core * f32(params.axes - 1u), vec3<f32>(0.0));
    }
    var glare = streaks;
    if (params.ghosts > 0.0) {
        for (var i = 0u; i < 4u; i++) {
            let ghost = GHOSTS[i];
            let at = vec2<f32>(0.5) + (uv - vec2<f32>(0.5)) * ghost.x;
            // Out of focus: a soft disc, the wider the further the ghost is scaled, read as a centre and a ring of taps.
            let spread = GHOST_SOFTNESS * abs(ghost.x);
            var disc = textureSampleLevel(bright, linear_sampler, at, 0.0).rgb;
            for (var k = 0u; k < GHOST_TAPS; k++) {
                let angle = 6.2831853 * f32(k) / f32(GHOST_TAPS);
                let offset = vec2<f32>(cos(angle) / params.aspect, sin(angle)) * spread;
                disc += textureSampleLevel(bright, linear_sampler, at + offset, 0.0).rgb;
            }
            // Only inside the frame, fading at its edges.
            let inside = smoothstep(0.0, 0.1, min(min(at.x, 1.0 - at.x), min(at.y, 1.0 - at.y)));
            glare += disc / f32(GHOST_TAPS + 1u) * ghost.y * inside * params.ghosts;
        }
    }
    if (params.halo > 0.0) {
        // A ring around every light: this pixel is lit by what lies HALO_RADIUS of the height away from it, all round.
        var ring = vec3<f32>(0.0);
        for (var i = 0u; i < HALO_TAPS; i++) {
            let angle = 6.2831853 * (f32(i) + 0.5) / f32(HALO_TAPS);
            let offset = vec2<f32>(cos(angle) / params.aspect, sin(angle)) * HALO_RADIUS;
            ring += textureSampleLevel(bright, linear_sampler, uv + offset, 0.0).rgb;
        }
        glare += ring * (HALO_GAIN / f32(HALO_TAPS)) * params.halo;
    }
    return vec4<f32>(glare * params.tint.rgb * params.strength, 1.0);
}
