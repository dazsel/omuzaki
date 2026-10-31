// The DepthOfField pass: blur that grows with distance from the focal plane, in three steps on images half the internal
// size or smaller, so the cost follows the pixels and not the blur radius.
//
//   fs_prepass  the scene colour reduced to half size, with each pixel's signed circle of confusion (CoC) from the depth
//   fs_tiles    the widest blur of anything in front of the focal plane in each block of those pixels
//   vs_bokeh    with `bokeh` on, the highlights the gather leaves out: one quad per 2x2 block of the prepass image, as wide
//   fs_bokeh    as the block's blur, drawn as a disc into an image of its own (scatter, for what a gather cannot do: a
//               small light spreading into a crisp disc rather than a few lucky taps of haze). Each disc adds its block's
//               mean highlight and a coverage of 1, so where discs overlap their light averages instead of piling up
//   fs_gather   each pixel averages a disc of its neighbours, a spiral of `taps` samples, as wide as its own CoC or as the
//               widest blurred foreground near it, adds the bokeh over it, and writes that width ("reach") for Resolve
//
// `Resolve` then blends the sharp scene with the gathered image by reach (`post_resolve.wgsl`), so pixels in focus and away
// from any blurred foreground never pass through the half-size images at all. CoC and reach are in pixels of the half-size
// images; CoC is negative in front of the focal plane and positive behind it. Premultiplied RGBA is blurred as a whole, so
// a transparent background stays transparent.
//
// `packed` is a pipeline constant: when the scene is opaque (alpha is 1 everywhere) the prepass keeps each pixel's CoC in the
// alpha of the colour image, so the gather reads one texture a tap instead of two, and writes alpha 1 (all that Resolve
// reads of it). A scene with transparency keeps the CoC in an image of its own and the real alpha in the colour.
//
// DEPTH_TYPE is replaced when the module is built: `texture_depth_multisampled_2d` or `texture_depth_2d`, whichever the
// frame's depth attachment is.

struct Params {
    depth: vec4<f32>,       // m33, m23, m32, m22 of the projection: distance = (d * m33 - m23) / (d * m32 - m22)
    focus: f32,             // focal distance, metres
    half_range: f32,        // half the depth of the sharp zone, metres
    max_coc: f32,           // the largest blur radius, in half-size pixels
    taps: u32,              // gather samples
    size: vec2<f32>,        // the half-size images, pixels
    full_size: vec2<i32>,   // the scene image, pixels
    scale: vec2<f32>,       // scene pixels per pixel of the half-size images (2 unless the scene is supersampled)
    depth_samples: u32,     // depth samples compared per pixel (1 when the depth is not multisampled)
    colour_taps: u32,       // bilinear taps per axis that reduce the scene colour
    tile: i32,              // the side of the blocks of fs_tiles, in half-size pixels: at least the widest blur
    bokeh: f32,             // the gain of the highlight discs; 0 off
    bokeh_threshold: f32,   // the linear luminance above which a pixel is a highlight
    padding0: u32,          // pads the struct to the 80 bytes the Rust side has (a vec3 would align to 16)
}

// The scene colour in the prepass; the prepass's colour in the tiles and the gather.
@group(0) @binding(0) var colour: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var coc_image: texture_2d<f32>;
@group(1) @binding(0) var<uniform> params: Params;
// The gather's golden-angle spiral, once for every frame: tap `i` looks in direction `spiral[i].xy` (cos and sin of
// `i * GOLDEN_ANGLE`), which the pixel's own rotation then turns, so no tap computes a sine or a cosine.
@group(1) @binding(1) var<uniform> spiral: array<vec4<f32>, 128>;
override packed: bool = false;
@group(2) @binding(0) var depth: DEPTH_TYPE;     // prepass
@group(2) @binding(1) var tiles: texture_2d<f32>; // gather: see fs_tiles
@group(2) @binding(2) var bokeh_image: texture_2d<f32>; // gather, with bokeh on: see fs_bokeh

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

struct Prepass {
    @location(0) colour: vec4<f32>,
    @location(1) coc: f32,
}

// Distance in front of the camera, metres, of a depth-buffer value, for any projection.
fn distance_of(d: f32) -> f32 {
    return (d * params.depth.x - params.depth.y) / (d * params.depth.z - params.depth.w);
}

// The CoC of a distance. The sharp zone is `focus +- half_range`; outside it blur grows with the distance from the zone
// relative to how far away the point is (a thin lens: near things go out of focus faster), reaching `max_coc` for
// anything infinitely far or very close.
fn coc_of(distance: f32) -> f32 {
    let off = distance - params.focus;
    let blur = max(abs(off) - params.half_range, 0.0) / max(distance, 0.0001);
    return sign(off) * min(blur, 1.0) * params.max_coc;
}

@fragment
fn fs_prepass(@builtin(position) position: vec4<f32>) -> Prepass {
    // The nearest surface among four places in the block of scene pixels this pixel covers (and the first few depth
    // samples of each). The nearest wins so a silhouette pixel counts as the foreground and stays sharp: taking the far
    // side would smear the outline into the blurred background.
    let block = position.xy * params.scale;
    var nearest = 1.0;
    for (var y = 0; y < 2; y++) {
        for (var x = 0; x < 2; x++) {
            let spread = (vec2<f32>(f32(x), f32(y)) - 0.5) * 0.5 * params.scale;
            let at = clamp(vec2<i32>(floor(block + spread)), vec2<i32>(0, 0), params.full_size - vec2<i32>(1, 1));
            for (var s = 0; s < i32(params.depth_samples); s++) {
                nearest = min(nearest, textureLoad(depth, at, s));
            }
        }
    }
    // The colour: the average of a grid of bilinear taps across the block (one tap when the block is 2x2 pixels).
    let uv = position.xy / params.size;
    let texel = vec2<f32>(1.0, 1.0) / vec2<f32>(params.full_size);
    var sum = vec4<f32>(0.0);
    for (var y = 0u; y < params.colour_taps; y++) {
        for (var x = 0u; x < params.colour_taps; x++) {
            let spread = ((vec2<f32>(f32(x), f32(y)) + 0.5) / f32(params.colour_taps) - 0.5) * params.scale;
            sum += textureSampleLevel(colour, linear_sampler, uv + spread * texel, 0.0);
        }
    }
    var out: Prepass;
    out.colour = sum / f32(params.colour_taps * params.colour_taps);
    out.coc = coc_of(distance_of(nearest));
    if (packed) {
        out.colour.a = out.coc;
    }
    return out;
}

// For each block of `tile` x `tile` pixels, how far the widest-blurred thing in front of the focal plane spreads: a sharp
// pixel next to a blurred foreground needs to know it is within reach of it, and only this block-wise bound says so
// without every pixel searching a wide disc. A block is at least as wide as the widest blur there can be, so the blocks
// around a pixel (the one it is in and its eight neighbours) always include anything that reaches it.
@fragment
fn fs_tiles(@builtin(position) position: vec4<f32>) -> @location(0) f32 {
    let last = vec2<i32>(params.size) - vec2<i32>(1, 1);
    var nearest = 0.0;
    for (var y = 0; y < params.tile; y++) {
        for (var x = 0; x < params.tile; x++) {
            let at = min(vec2<i32>(position.xy) * params.tile + vec2<i32>(x, y), last);
            nearest = min(nearest, textureLoad(coc_image, at, 0).r);
        }
    }
    return -nearest;
}

// The widest blurred foreground within one block of this pixel.
fn foreground_reach(pixel: vec2<f32>) -> f32 {
    let block = vec2<i32>(pixel) / params.tile;
    let last = (vec2<i32>(params.size) + vec2<i32>(params.tile - 1)) / params.tile - vec2<i32>(1, 1);
    var reach = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let at = clamp(block + vec2<i32>(x, y), vec2<i32>(0, 0), last);
            reach = max(reach, textureLoad(tiles, at, 0).r);
        }
    }
    return reach;
}

// A static per-pixel angle (interleaved gradient noise, Jorge Jimenez, "Next Generation Post Processing in Call of Duty:
// Advanced Warfare", SIGGRAPH 2014; a published formula, its constants are his): it turns the gather's regular spiral into fine noise.
fn rotation_at(pixel: vec2<f32>) -> f32 {
    return 6.2831853 * fract(52.9829189 * fract(dot(pixel, vec2<f32>(0.06711056, 0.00583715))));
}

struct Gathered {
    @location(0) colour: vec4<f32>,
    @location(1) reach: f32,
}

// One place in the prepass image: its colour (alpha 1 when the CoC is packed in it) and its CoC.
struct Sample {
    colour: vec4<f32>,
    coc: f32,
}

// Under this blur radius (half-size pixels) a highlight stays in the gather: the disc would be a dot.
const BOKEH_MIN_COC: f32 = 1.5;
// The coverage, in blocks, a disc's light is shared over at least: a light of this many blocks (12 half-size pixels) or
// more fills its disc at its own brightness (times `bokeh`); a smaller one is dimmer in proportion, so a speck does not
// open into a bright disc.
const BOKEH_FLOOR: f32 = 3.0;

// The part of a premultiplied colour that is drawn as a bokeh disc rather than gathered: what is above the threshold, once
// the pixel is blurred enough for a disc to read. Zero with bokeh off, so the gather is exactly as without it.
fn highlight(c: vec4<f32>, coc: f32) -> vec3<f32> {
    if (params.bokeh <= 0.0) {
        return vec3<f32>(0.0);
    }
    let luma = dot(c.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let over = max(luma - params.bokeh_threshold * c.a, 0.0);
    let blurred = smoothstep(BOKEH_MIN_COC, 2.0 * BOKEH_MIN_COC, abs(coc));
    return c.rgb * (over / max(luma, 1e-6) * blurred);
}

fn sample_at(uv: vec2<f32>) -> Sample {
    var out: Sample;
    out.colour = textureSampleLevel(colour, linear_sampler, uv, 0.0);
    if (packed) {
        out.coc = out.colour.a;
        out.colour.a = 1.0;
    } else {
        out.coc = textureSampleLevel(coc_image, linear_sampler, uv, 0.0).r;
    }
    // The highlight leaves the gather; fs_bokeh draws it as a disc.
    out.colour = vec4<f32>(out.colour.rgb - highlight(out.colour, out.coc), out.colour.a);
    return out;
}

@fragment
fn fs_gather(@builtin(position) position: vec4<f32>) -> Gathered {
    let uv = position.xy / params.size;
    let middle = sample_at(uv);
    let centre = middle.colour;
    let centre_coc = middle.coc;
    let own = abs(centre_coc);
    let reach = max(own, foreground_reach(position.xy));
    var out: Gathered;
    out.reach = reach;
    if (reach < 0.5) {
        out.colour = centre;
        return out;
    }
    var sum = centre;
    var weights = 1.0;
    // The spiral turned by this pixel's angle: (cos a, sin a) of each tap rotated by (cos r, sin r).
    let rotation = rotation_at(position.xy);
    let turn = vec2<f32>(cos(rotation), sin(rotation));
    let inverse_taps = 1.0 / f32(params.taps);
    for (var i = 0u; i < params.taps; i++) {
        // A golden-angle spiral covers the disc evenly whatever the number of taps.
        let radius = sqrt((f32(i) + 0.5) * inverse_taps) * reach;
        let step = spiral[i].xy;
        let direction = vec2<f32>(step.x * turn.x - step.y * turn.y, step.y * turn.x + step.x * turn.y);
        let at = (position.xy + radius * direction) / params.size;
        let tap = sample_at(at);
        // A neighbour in front of this pixel only reaches it if its own blur is wide enough: a sharp foreground (the
        // character) never bleeds into the blurred background behind it, and a blurred foreground spreads over what it
        // covers, by the share of this pixel's disc it fills. A neighbour at or behind this pixel counts as itself
        // within this pixel's own blur; beyond it (only a pixel near a blurred foreground looks that far) the pixel
        // stands in for it, so what is behind the foreground stays as sharp as it was.
        if (tap.coc < centre_coc - 0.25) {
            let weight = clamp(abs(tap.coc) - radius + 1.0, 0.0, 1.0);
            sum += tap.colour * weight;
            weights += weight;
        } else {
            sum += select(centre, tap.colour, radius <= own);
            weights += 1.0;
        }
    }
    out.colour = sum / weights;
    if (params.bokeh > 0.0) {
        // The discs over this pixel: their mean light, dimmed for small lights, added in front of the blur. Over a
        // transparent background the disc brings its own coverage, as bloom's glow does.
        let b = textureLoad(bokeh_image, vec2<i32>(position.xy), 0);
        let disc = b.rgb / max(b.a, BOKEH_FLOOR) * params.bokeh;
        let coverage = min(max(disc.r, max(disc.g, disc.b)), 1.0);
        out.colour = vec4<f32>(out.colour.rgb + disc, out.colour.a + (1.0 - out.colour.a) * coverage);
    }
    return out;
}

struct BokehVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) offset: vec2<f32>,  // from the disc's centre, half-size pixels
    @location(1) colour: vec3<f32>,  // the block's mean highlight
    @location(2) radius: f32,        // half-size pixels
}

// One quad per 2x2 block of the prepass image: the block's highlights summed, at the blur radius they average (weighted
// by their brightness). A block with no highlight is a quad of no area, which rasterises nothing.
@vertex
fn vs_bokeh(@builtin(vertex_index) index: u32, @builtin(instance_index) block: u32) -> BokehVertex {
    var out: BokehVertex;
    out.position = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    let blocks_x = (u32(params.size.x) + 1u) / 2u;
    let origin = vec2<i32>(i32(block % blocks_x), i32(block / blocks_x)) * 2;
    let last = vec2<i32>(params.size) - vec2<i32>(1, 1);
    var light = vec3<f32>(0.0);
    var radius = 0.0;
    var weight = 0.0;
    for (var i = 0; i < 4; i++) {
        let at = min(origin + vec2<i32>(i & 1, i >> 1u), last);
        var c = textureLoad(colour, at, 0);
        var coc = 0.0;
        if (packed) {
            coc = c.a;
            c.a = 1.0;
        } else {
            coc = textureLoad(coc_image, at, 0).r;
        }
        let h = highlight(c, coc);
        let w = dot(h, vec3<f32>(0.2126, 0.7152, 0.0722));
        light += h;
        radius += abs(coc) * w;
        weight += w;
    }
    if (weight < 1e-5) {
        return out;
    }
    radius = radius / weight;
    out.radius = radius;
    out.colour = light * 0.25;
    // Two triangles: corners 0 1 2 and 2 1 3 of the unit square, as vertex indices 0..6.
    let corners = array<u32, 6>(0u, 1u, 2u, 2u, 1u, 3u);
    let k = corners[index % 6u];
    let unit = vec2<f32>(f32(k & 1u), f32(k >> 1u)) * 2.0 - 1.0;
    out.offset = unit * (radius + 1.0);
    let centre = vec2<f32>(origin) + 1.0;
    let pixel = centre + out.offset;
    out.position = vec4<f32>(pixel.x / params.size.x * 2.0 - 1.0, 1.0 - pixel.y / params.size.y * 2.0, 0.0, 1.0);
    return out;
}

// A disc with an edge one pixel soft: the block's light and a coverage of one, summed where discs overlap.
@fragment
fn fs_bokeh(in: BokehVertex) -> @location(0) vec4<f32> {
    let inside = clamp(in.radius - length(in.offset) + 0.5, 0.0, 1.0);
    return vec4<f32>(in.colour * inside, inside);
}
