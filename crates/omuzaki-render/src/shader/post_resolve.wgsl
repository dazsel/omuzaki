// The Resolve pass: scene colour (linear, premultiplied, HDR) to the target's sRGB-encoded bytes.
//
// For each output pixel it takes one or more taps of the scene colour (more when the internal render is larger than the
// target, which is how `render_scale` above 1 supersamples). For each tap it blends in the depth-of-field image, adds the
// bloom glow, undoes premultiplication, applies exposure and the tone curve, sRGB-encodes, and averages the encoded
// colour weighted by alpha. Tone mapping before averaging is what keeps a bright edge from fizzing; it matches the
// spike's per-fragment tone mapping. After the average come the effects that live in display space: grade, grain and a
// whisper of dither, all on the straight colour, so alpha is never touched and a transparent pixel stays transparent.
// Then a backdrop shown as authored (a gradient or a texture) is laid under the result in those encoded values, as a
// compositor lays a transparent output over a picture. Last comes the drawn look (`Stylize` in render-api): a lens
// fringe (red and blue read from slightly different places, so it is done while gathering), posterized tones, split
// toning, a halftone screen, ink hatching, paper and a vignette, on the straight colour of the composite, so it covers
// the backdrop too and never changes alpha.

struct Params {
    exposure: f32,
    tonemap: u32,        // 0: ACES, 1: clamp
    premultiplied: u32,  // 1: the output stores colour multiplied by alpha
    taps: u32,           // 0: one exact texel (internal size equals target size); otherwise taps x taps bilinear samples
    ratio: vec2<f32>,    // internal size / target size
    internal: vec2<f32>, // internal size in pixels
    saturation: f32,     // grade
    contrast: f32,       // grade
    grain: f32,          // grain amplitude, display units
    grain_cell: f32,     // grain size in output pixels
    seed: vec2<u32>,     // the frame index, low and high word
    flags: u32,          // see FLAG_*
    dither: f32,         // amplitude of the dither, display units (one output level)
    backdrop: u32,       // 0: none, 1: the gradient, 2: the texture over the gradient
    backdrop_flags: u32, // the backdrop texture's DECODE_SRGB (1) and PREMULTIPLIED (2), as for materials
    uv_scale: vec2<f32>, // where the texture is read: uv = frame position (0..1) * uv_scale + uv_offset
    top: vec4<f32>,      // the gradient's ends, premultiplied, sRGB-encoded
    bottom: vec4<f32>,
    uv_offset: vec2<f32>,
    style_flags: u32,    // see STYLE_*
    posterize: f32,      // tone levels
    // The stylize settings, packed by `pass/resolve.rs` (`style_params`): sizes are in output pixels, inks and tints are
    // sRGB-encoded display values.
    //   0: halftone cell, cos, sin, threshold      1: halftone ink, strength
    //   2: hatch spacing, width, cos, sin          3: hatch ink, strength
    //   4: hatch threshold, cross threshold, paper strength, paper scale
    //   5: paper tint, vignette strength           6: vignette radius, softness, fringe, split balance
    //   7: split shadows, split strength           8: split highlights, 0
    //   9: output width, output height, 0, 0
    style: array<vec4<f32>, 10>,
}

const BACKDROP_GRADIENT: u32 = 1u;
const BACKDROP_TEXTURE: u32 = 2u;
const DECODE_SRGB: u32 = 1u;
const PREMULTIPLIED: u32 = 2u;

const FLAG_GLOW: u32 = 1u;
const FLAG_DEPTH_OF_FIELD: u32 = 2u;
const FLAG_GRADE: u32 = 4u;
const FLAG_GRAIN: u32 = 8u;
const FLAG_DITHER: u32 = 16u;
const FLAGS_AFTER_AVERAGE: u32 = 28u; // grade, grain and dither

const STYLE_FRINGE: u32 = 1u;
const STYLE_POSTERIZE: u32 = 2u;
const STYLE_SPLIT: u32 = 4u;
const STYLE_HALFTONE: u32 = 8u;
const STYLE_HATCH: u32 = 16u;
const STYLE_PAPER: u32 = 32u;
const STYLE_VIGNETTE: u32 = 64u;
const DISPLAY_LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

@group(0) @binding(0) var colour: texture_2d<f32>;
@group(0) @binding(1) var colour_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;
// The backdrop texture, read through its own sampler and mips; a 1x1 stand-in when there is none.
@group(1) @binding(1) var backdrop_image: texture_2d<f32>;
@group(1) @binding(2) var backdrop_sampler: sampler;
// Bloom's glow: linear and premultiplied, already scaled by the effect's strength.
@group(2) @binding(0) var glow_image: texture_2d<f32>;
@group(2) @binding(1) var glow_sampler: sampler;
// Depth of field: the blurred scene and the width of the blur at each pixel, in pixels of the half-size images.
@group(3) @binding(0) var blurred_image: texture_2d<f32>;
@group(3) @binding(1) var blurred_sampler: sampler;
@group(3) @binding(2) var reach_image: texture_2d<f32>;

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

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    let low = c * 12.92;
    let high = pow(c, vec3<f32>(1.0 / 2.4)) * 1.055 - vec3<f32>(0.055);
    return select(high, low, c <= vec3<f32>(0.0031308));
}

// three.js ACESFilmicToneMapping (Stephen Hill's RRT + ODT fit; matrices and coefficients as in three.js, MIT, see
// NOTICES.md beside this file), including its exposure / 0.6 pre-scale, so the
// prototype's exposure values carry over.
fn aces(c: vec3<f32>) -> vec3<f32> {
    let input_matrix = mat3x3<f32>(
        vec3<f32>(0.59719, 0.07600, 0.02840),
        vec3<f32>(0.35458, 0.90834, 0.13383),
        vec3<f32>(0.04823, 0.01566, 0.83777));
    let output_matrix = mat3x3<f32>(
        vec3<f32>(1.60475, -0.10208, -0.00327),
        vec3<f32>(-0.53108, 1.10813, -0.07276),
        vec3<f32>(-0.07367, -0.00605, 1.07602));
    let v = input_matrix * (c * (params.exposure / 0.6));
    let a = v * (v + vec3<f32>(0.0245786)) - vec3<f32>(0.000090537);
    let b = v * (0.983729 * v + vec3<f32>(0.4329510)) + vec3<f32>(0.238081);
    return clamp(output_matrix * (a / b), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn tonemap(c: vec3<f32>) -> vec3<f32> {
    if (params.tonemap == 0u) {
        return aces(c);
    }
    return clamp(c * params.exposure, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn display_colour(linear: vec3<f32>) -> vec3<f32> {
    return srgb_encode(tonemap(linear));
}

fn has(flag: u32) -> bool {
    return (params.flags & flag) != 0u;
}

// One tap: sRGB-encoded display colour multiplied by alpha, and alpha.
//
// Without glow this is the scene's colour tone-mapped as it stands. With glow the scene is lit by it (added in linear light
// before the tone curve, as the prototype's composer does), and the glow also shows through whatever the scene does not
// cover, so a halo reaches past a transparent background's silhouette: over black that part looks exactly as it would on
// an opaque background, and its alpha is the display brightness of the glow, which is what lets a straight or a
// premultiplied consumer blend it over anything.
fn display(premultiplied: vec4<f32>, glow: vec3<f32>) -> vec4<f32> {
    let alpha = premultiplied.a;
    let straight = select(vec3<f32>(0.0), premultiplied.rgb / alpha, alpha > 0.0);
    if (!has(FLAG_GLOW)) {
        return vec4<f32>(display_colour(straight) * alpha, alpha);
    }
    let lit = display_colour(straight + glow);
    let halo = display_colour(glow);
    let coverage = max(halo.r, max(halo.g, halo.b));
    return vec4<f32>(lit * alpha + halo * (1.0 - alpha), alpha + (1.0 - alpha) * coverage);
}

// The scene at one tap, with depth of field blended in where the pixel is out of focus, then through `display`. `uv` is
// where the tap sits in the internal image, 0..1: the half-size images are read there with bilinear filtering.
fn shade(scene: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
    var pixel = scene;
    if (has(FLAG_DEPTH_OF_FIELD)) {
        let reach = textureSampleLevel(reach_image, blurred_sampler, uv, 0.0).r;
        let blurred = textureSampleLevel(blurred_image, blurred_sampler, uv, 0.0);
        pixel = mix(scene, blurred, smoothstep(0.5, 2.0, reach));
    }
    var glow = vec3<f32>(0.0);
    if (has(FLAG_GLOW)) {
        glow = textureSampleLevel(glow_image, glow_sampler, uv, 0.0).rgb;
    }
    return display(pixel, glow);
}

// PCG hash (M. E. O'Neill's PCG family; this is the 32-bit variant of Jarzynski and Olano, "Hash Functions for GPU
// Rendering", JCGT 2020): well mixed, and integer-only, so the same on every GPU.
fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

// A uniform random number in 0..1 for a grain cell, a frame and a stream (so grain and dither differ).
fn random(cell: vec2<u32>, stream: u32) -> f32 {
    let h = pcg(cell.x ^ pcg(cell.y ^ pcg(params.seed.x ^ pcg(params.seed.y ^ stream))));
    return f32(h >> 8u) * (1.0 / 16777216.0);
}

// Two uniform numbers added make a triangular distribution on -1..1: no bias, and no visible pattern.
fn triangular(cell: vec2<u32>, stream: u32) -> f32 {
    return random(cell, stream) + random(cell, stream + 1u) - 1.0;
}

// Saturation lerps away from the colour's display-space luma; contrast bends each channel around mid grey with a curve
// that keeps black and white where they are (a straight stretch would clip and band), slope `contrast` at the middle:
// below the middle it is 0.5 * (2x)^c, above it the mirror image 1 - 0.5 * (2(1 - x))^c, one power per channel.
fn grade(c: vec3<f32>) -> vec3<f32> {
    let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let saturated = clamp(mix(vec3<f32>(luma), c, params.saturation), vec3<f32>(0.0), vec3<f32>(1.0));
    let below = saturated < vec3<f32>(0.5);
    let distance = 2.0 * select(vec3<f32>(1.0) - saturated, saturated, below);
    // pow is undefined at zero for a power of zero (contrast 0 is a flat grey), so keep the base just above it.
    let bent = 0.5 * pow(max(distance, vec3<f32>(0.000001)), vec3<f32>(params.contrast));
    return select(vec3<f32>(1.0) - bent, bent, below);
}

// What happens to a pixel's straight display colour after the taps are averaged.
fn finish(straight: vec3<f32>, pixel: vec2<f32>) -> vec3<f32> {
    var c = straight;
    if (has(FLAG_GRADE)) {
        c = grade(c);
    }
    let cell = vec2<u32>(floor(pixel / params.grain_cell));
    if (has(FLAG_GRAIN)) {
        // One value for all three channels: grain is brightness noise, and colour speckle would desaturate the picture.
        c += vec3<f32>(params.grain * triangular(cell, 1u));
    }
    if (has(FLAG_DITHER)) {
        // Per output pixel, whatever the grain's cell size: the dither is for the 8-bit rounding that follows.
        c += vec3<f32>(params.dither * triangular(vec2<u32>(floor(pixel)), 3u));
    }
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

// What the backdrop shows at frame position `at` (0..1, rows top to bottom), premultiplied and sRGB-encoded, given the
// texture's sample there (`texel`, read in uniform control flow so its mips are chosen right).
fn backdrop_at(at: vec2<f32>, uv: vec2<f32>, texel: vec4<f32>) -> vec4<f32> {
    let behind = mix(params.top, params.bottom, clamp(at.y, 0.0, 1.0));
    let inside = all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
    if (params.backdrop != BACKDROP_TEXTURE || !inside) {
        return behind;
    }
    let alpha = texel.a;
    var rgb = texel.rgb;
    if ((params.backdrop_flags & PREMULTIPLIED) != 0u) {
        rgb = select(vec3<f32>(0.0), rgb / alpha, alpha > 0.0);
    }
    if ((params.backdrop_flags & DECODE_SRGB) == 0u) {
        // Sampled in linear light (an sRGB view decodes it, a linear texture is linear): back to display values.
        rgb = srgb_encode(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    }
    return vec4<f32>(rgb * alpha, alpha) + behind * (1.0 - alpha);
}

// ---- the drawn look ----

fn style_on(flag: u32) -> bool {
    return (params.style_flags & flag) != 0u;
}

// A fixed lattice value in 0..1 for the paper: a function of the cell alone, so the paper is the same on every frame.
fn lattice(cell: vec2<i32>) -> f32 {
    let c = bitcast<vec2<u32>>(cell);
    return f32(pcg(c.x ^ pcg(c.y ^ 0x9e3779b9u)) >> 8u) * (1.0 / 16777216.0);
}

// Smooth value noise at `q` (lattice units).
fn value_noise(q: vec2<f32>) -> f32 {
    let i = vec2<i32>(floor(q));
    let f = fract(q);
    let u = f * f * (3.0 - 2.0 * f);
    let a = lattice(i);
    let b = lattice(i + vec2<i32>(1, 0));
    let c = lattice(i + vec2<i32>(0, 1));
    let d = lattice(i + vec2<i32>(1, 1));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Coverage of an edge at `edge`, `x` running across it, anti-aliased over `aa` either side: 1 inside, 0 outside.
fn inside(x: f32, edge: f32, aa: f32) -> f32 {
    return 1.0 - smoothstep(edge - aa, edge + aa, x);
}

// How far below `threshold` the luma is, 0..1: 0 at and above the threshold, 1 at black.
fn darkness(luma: f32, threshold: f32) -> f32 {
    return saturate((threshold - luma) / max(threshold, 1e-3));
}

// `p` turned by the angle whose cosine and sine are `cs`.
fn turned(p: vec2<f32>, cs: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(cs.x * p.x - cs.y * p.y, cs.y * p.x + cs.x * p.y);
}

// The drawn look on a straight display colour `c` at output pixel `p` (pixel centres at .5).
fn stylize(colour_in: vec3<f32>, p: vec2<f32>) -> vec3<f32> {
    var c = colour_in;
    let s = params.style;
    let size = s[9].xy;
    let centre = size * 0.5;
    if (style_on(STYLE_POSTERIZE)) {
        // On luma, the colour scaled to the step, so hue survives every step.
        let luma = dot(c, DISPLAY_LUMA);
        if (luma > 1e-4) {
            let stepped = floor(luma * params.posterize + 0.5) / params.posterize;
            c = c * (stepped / luma);
        }
    }
    if (style_on(STYLE_SPLIT)) {
        let luma = dot(c, DISPLAY_LUMA);
        let pivot = 0.5 + 0.5 * s[6].w;
        let t = smoothstep(0.0, 1.0, saturate(luma - pivot + 0.5));
        let tint = mix(s[7].rgb, s[8].rgb, t);
        // The tint at the pixel's own luma, so the grade moves hue and never brightness.
        let toned = tint * (luma / max(dot(tint, DISPLAY_LUMA), 1e-3));
        c = mix(c, toned, s[7].w);
    }
    let luma = dot(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), DISPLAY_LUMA);
    if (style_on(STYLE_HALFTONE)) {
        // A spot-function screen: dot area grows with the darkness, so a flat tone prints as an even field of dots.
        let cell = s[0].x;
        let q = turned(p - centre, s[0].yz) / cell;
        let distance = length(fract(q) - vec2<f32>(0.5)) * 2.0;
        let dark = darkness(luma, s[0].w);
        let radius = 2.0 * sqrt(dark / 3.14159265);
        let cover = select(0.0, inside(distance, radius, 1.0 / cell), dark > 0.0);
        c = c * mix(vec3<f32>(1.0), s[1].rgb, cover * s[1].w);
    }
    if (style_on(STYLE_HATCH)) {
        let spacing = s[2].x;
        let q = turned(p - centre, s[2].zw) / spacing;
        let aa = 1.0 / spacing;
        let first = darkness(luma, s[4].x);
        let along = abs(fract(q.y) - 0.5) * 2.0;
        var cover = select(0.0, inside(along, s[2].y * first, aa), first > 0.0);
        let second = darkness(luma, s[4].y);
        let across = abs(fract(q.x) - 0.5) * 2.0;
        cover = max(cover, select(0.0, inside(across, s[2].y * second, aa), second > 0.0 && s[4].y > 0.0));
        c = c * mix(vec3<f32>(1.0), s[3].rgb, cover * s[3].w);
    }
    if (style_on(STYLE_PAPER)) {
        let q = p / s[4].w;
        // Two octaves, the coarse one stretched along x, read as fibres more than as noise.
        let n = 0.65 * value_noise(q * vec2<f32>(0.35, 1.0)) + 0.35 * value_noise(q * 2.7 + vec2<f32>(17.0, 3.0));
        c = c * (1.0 - s[4].z * n) * s[5].rgb;
    }
    if (style_on(STYLE_VIGNETTE)) {
        let d = length((p - centre) / (0.5 * size.y));
        c = c * (1.0 - s[5].w * smoothstep(s[6].x, s[6].x + s[6].y, d));
    }
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

// The averaged scene at output position `p` (output pixels, centres at .5), premultiplied and sRGB-encoded: one exact
// texel when the sizes match and `exact`, else bilinear taps over the pixel's footprint in the internal image.
fn gather(p: vec2<f32>, exact: bool) -> vec4<f32> {
    if (params.taps == 0u) {
        if (exact) {
            return shade(textureLoad(colour, vec2<i32>(p), 0), p / params.internal);
        }
        let uv = p * params.ratio / params.internal;
        return shade(textureSampleLevel(colour, colour_sampler, uv, 0.0), uv);
    }
    var sum = vec4<f32>(0.0);
    let corner = p - vec2<f32>(0.5);
    for (var y = 0u; y < params.taps; y++) {
        for (var x = 0u; x < params.taps; x++) {
            // Taps spread evenly over the output pixel's footprint in the internal image.
            let offset = (vec2<f32>(f32(x), f32(y)) + 0.5) / f32(params.taps);
            let uv = (corner + offset) * params.ratio / params.internal;
            sum = sum + shade(textureSampleLevel(colour, colour_sampler, uv, 0.0), uv);
        }
    }
    return sum / f32(params.taps * params.taps);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let at = in.clip.xy * params.ratio / params.internal;
    let uv = at * params.uv_scale + params.uv_offset;
    let texel = textureSample(backdrop_image, backdrop_sampler, uv);
    var gathered = gather(in.clip.xy, true);
    if (style_on(STYLE_FRINGE)) {
        // A lens fringe: red is read a little nearer the centre and blue a little farther out (so red spreads outward),
        // growing with the distance from the centre: `fringe` output heights at one half-height out.
        let shift = (in.clip.xy - params.style[9].xy * 0.5) * 2.0 * params.style[6].z;
        let red = gather(in.clip.xy - shift, false);
        let blue = gather(in.clip.xy + shift, false);
        gathered = vec4<f32>(red.r, gathered.g, blue.b, gathered.a);
    }
    let alpha = gathered.a;
    var encoded = gathered.rgb;          // premultiplied, encoded
    if ((params.flags & FLAGS_AFTER_AVERAGE) != 0u) {
        // Grade, grain and dither act on the straight colour, then the output's alpha encoding applies as usual.
        let straight = select(vec3<f32>(0.0), encoded / alpha, alpha > 0.0);
        encoded = finish(straight, in.clip.xy) * alpha;
    }
    var coverage = alpha;
    if (params.backdrop != 0u) {
        let behind = backdrop_at(at, uv, texel);
        encoded = encoded + behind.rgb * (1.0 - alpha);
        coverage = alpha + behind.a * (1.0 - alpha);
    }
    if ((params.style_flags & ~STYLE_FRINGE) != 0u && coverage > 0.0) {
        encoded = stylize(encoded / coverage, in.clip.xy) * coverage;
    }
    if (params.premultiplied == 1u) {
        return vec4<f32>(encoded, coverage);
    }
    return vec4<f32>(select(vec3<f32>(0.0), encoded / coverage, coverage > 0.0), coverage);
}
