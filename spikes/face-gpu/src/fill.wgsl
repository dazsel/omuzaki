// Analytic-coverage fill: one quad per command over its bounding box; the fragment shader sums the exact signed area every
// edge of the path (and of each clip) leaves in its pixel. No tessellation, no stencil, no MSAA.

struct Cmd {
    bbox: vec4<f32>,
    ranges: vec4<u32>,   // unused x3, paint kind
    params: vec4<f32>,   // opacity, stop count, radius
    offsets: vec4<f32>,
    col: array<vec4<f32>, 4>,
    grad: vec4<f32>,
    inv0: vec4<f32>,
    inv1: vec4<f32>,
}

struct Band {
    head: vec4<u32>,    // command, geometry list start, count, clip count
    y: vec4<f32>,       // top, bottom
    c01: vec4<u32>,     // clip 0 list start, count, clip 1 list start, count
    c23: vec4<u32>,
}

struct Globals { size: vec2<f32> }

@group(0) @binding(0) var<storage, read> edges: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> cmds: array<Cmd>;
@group(0) @binding(2) var<uniform> globals: Globals;
@group(0) @binding(3) var<storage, read> indices: array<u32>;
@group(0) @binding(4) var<storage, read> bands: array<Band>;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) idx: u32,
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    let b = bands[ii];
    let c = cmds[b.head.x];
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let p = vec2<f32>(mix(c.bbox.x, c.bbox.z, corner.x), mix(b.y.x, b.y.y, corner.y));
    var o: VOut;
    o.pos = vec4<f32>(p.x / globals.size.x * 2.0 - 1.0, 1.0 - p.y / globals.size.y * 2.0, 0.0, 1.0);
    o.idx = ii;
    return o;
}

// Integral over y of max(x(y) - c, 0) for x linear from xa to xb across a unit-less dy of 1 (scaled by the caller).
fn above(xa: f32, xb: f32, c: f32) -> f32 {
    let hi = max(xa, xb) - c;
    let lo = min(xa, xb) - c;
    if lo >= 0.0 { return 0.5 * (hi + lo); }
    if hi <= 0.0 { return 0.0; }
    return hi * hi / (2.0 * (hi - lo));
}

// Winding-weighted area one edge leaves in the pixel box [0,1]^2 (edge given relative to the box's corner): the integral
// over the box of "the edge lies to the right of this point and spans this height".
fn edge_area(e: vec4<f32>) -> f32 {
    var x0 = e.x; var y0 = e.y; var x1 = e.z; var y1 = e.w;
    var dir = 1.0;
    if y0 > y1 { dir = -1.0; let tx = x0; x0 = x1; x1 = tx; let ty = y0; y0 = y1; y1 = ty; }
    if y1 <= 0.0 || y0 >= 1.0 { return 0.0; }
    let k = (x1 - x0) / (y1 - y0);
    let ya = max(y0, 0.0);
    let yb = min(y1, 1.0);
    let xa = x0 + k * (ya - y0);
    let xb = x0 + k * (yb - y0);
    let dy = yb - ya;
    // coverage to the right of the edge: dy - integral of clamp(x, 0, 1) = dy - (above(0) - above(1))
    return dir * (dy - dy * (above(xa, xb, 0.0) - above(xa, xb, 1.0)));
}

fn coverage(start: u32, count: u32, origin: vec2<f32>) -> f32 {
    var w = 0.0;
    let o = vec4<f32>(origin, origin);
    for (var i = start; i < start + count; i++) {
        let e = edges[indices[i]] - o;
        // Cheap reject: entirely above or below the pixel row.
        if (e.y <= 0.0 && e.w <= 0.0) || (e.y >= 1.0 && e.w >= 1.0) { continue; }
        w += edge_area(e);
    }
    return min(abs(w), 1.0);
}

fn gradient_t(c: Cmd, p: vec2<f32>) -> f32 {
    let local = vec2<f32>(dot(c.inv0.xyz, vec3<f32>(p, 1.0)), dot(c.inv1.xyz, vec3<f32>(p, 1.0)));
    if c.ranges.w == 1u {
        let a = c.grad.xy;
        let d = c.grad.zw - a;
        return clamp(dot(local - a, d) / dot(d, d), 0.0, 1.0);
    }
    return clamp(length(local - c.grad.xy) / c.params.z, 0.0, 1.0);
}

fn paint_colour(c: Cmd, p: vec2<f32>) -> vec4<f32> {
    if c.ranges.w == 0u { return c.col[0]; }
    let t = gradient_t(c, p);
    let n = u32(c.params.y);
    var out = c.col[0];
    if t <= c.offsets[0] { return out; }
    for (var i = 1u; i < n; i++) {
        if t <= c.offsets[i] {
            let f = (t - c.offsets[i - 1u]) / max(c.offsets[i] - c.offsets[i - 1u], 1e-6);
            return mix(c.col[i - 1u], c.col[i], f);
        }
        out = c.col[i];
    }
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    let b = bands[in.idx];
    let c = cmds[b.head.x];
    let origin = floor(in.pos.xy);
    var cov = coverage(b.head.y, b.head.z, origin);
    if cov <= 0.0 { discard; }
    let nclip = b.head.w;
    if nclip > 0u { cov *= coverage(b.c01.x, b.c01.y, origin); }
    if nclip > 1u { cov *= coverage(b.c01.z, b.c01.w, origin); }
    if nclip > 2u { cov *= coverage(b.c23.x, b.c23.y, origin); }
    if nclip > 3u { cov *= coverage(b.c23.z, b.c23.w, origin); }
    if cov <= 0.0 { discard; }
    let col = paint_colour(c, origin + vec2<f32>(0.5));
    let a = col.a * c.params.x * cov;
    return vec4<f32>(col.rgb * a, a);
}
