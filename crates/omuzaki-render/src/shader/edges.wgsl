// Edge lines (#2319): a band on the near side of every depth jump, drawn over the scene colour after the hulls.
// The port of `EdgeLines::coverage` in omuzaki-render-api, which the mock runs as written: keep the two the same.
//
// DEPTH_TYPE is replaced when the module is built: `texture_depth_2d`, or `texture_depth_multisampled_2d`, whose
// `textureLoad(depth, p, 0)` reads the first sample.

struct Params {
    // The line colour, premultiplied linear.
    colour: vec4<f32>,
    // The depth-buffer-to-distance coefficients: distance = (d * x - y) / (d * z - w).
    depth: vec4<f32>,
    // x: the band's width in internal pixels; y: the step; zw: the internal size in pixels.
    shape: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(1) @binding(0) var depth: DEPTH_TYPE;

// The background's distance: further than any surface, finite so the arithmetic stays finite.
const BACKGROUND: f32 = 1e30;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

// A pixel's distance along the view direction: BACKGROUND where nothing was drawn, -1 outside the picture.
fn distance_at(p: vec2<i32>) -> f32 {
    let size = vec2<i32>(params.shape.zw);
    if (p.x < 0 || p.y < 0 || p.x >= size.x || p.y >= size.y) {
        return -1.0;
    }
    let d = textureLoad(depth, p, 0);
    if (d >= 1.0) {
        return BACKGROUND;
    }
    return (d * params.depth.x - params.depth.y) / (d * params.depth.z - params.depth.w);
}

fn on_surface(d: f32) -> bool {
    return d >= 0.0 && d < BACKGROUND;
}

// Of two slopes, the one nearer zero; zero when they disagree in sign.
fn gentler(near: f32, far: f32) -> f32 {
    if (near * far <= 0.0) {
        return 0.0;
    }
    return select(far, near, abs(near) < abs(far));
}

@fragment
fn fs_edges(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(position.xy);
    let centre = distance_at(p);
    if (!on_surface(centre)) {
        discard;
    }
    let width = params.shape.x;
    let step = params.shape.y;
    let reach = i32(ceil(width + 0.5));
    var covered = 0.0;
    for (var i = 0; i < 8; i++) {
        var dir = vec2<i32>(1, 0);
        switch i {
            case 1: { dir = vec2<i32>(-1, 0); }
            case 2: { dir = vec2<i32>(0, 1); }
            case 3: { dir = vec2<i32>(0, -1); }
            case 4: { dir = vec2<i32>(1, 1); }
            case 5: { dir = vec2<i32>(1, -1); }
            case 6: { dir = vec2<i32>(-1, 1); }
            case 7: { dir = vec2<i32>(-1, -1); }
            default: {}
        }
        let length = select(1.0, sqrt(2.0), i >= 4);
        // Behind the pixel: the surface's slope, none against the background, no line at the picture's edge.
        let behind = distance_at(p - dir);
        if (behind < 0.0) {
            continue;
        }
        var before = behind;
        var here = centre;
        for (var k = 1; k <= reach; k++) {
            let share = clamp(width - ((f32(k) - 0.5) * length - 0.5), 0.0, 1.0);
            let b = distance_at(p + k * dir);
            if (!on_surface(here) || b < 0.0 || share <= covered) {
                break;
            }
            let a = here;
            // The gap less what the gentler of the two sides' slopes explains (`EdgeLines::coverage`); a side whose
            // slope can't be told leaves it to the other.
            let d = distance_at(p + (k + 1) * dir);
            let near_known = on_surface(before);
            let far_known = on_surface(d) && on_surface(b);
            let near = a - before;
            let far = d - b;
            var slope = 0.0;
            if (near_known && far_known) {
                slope = gentler(near, far);
            } else if (near_known) {
                slope = near;
            } else if (far_known) {
                slope = far;
            }
            let off = b - a - slope;
            if (off > step * a) {
                covered = share;
                break;
            }
            if (off < -step * a) {
                break;
            }
            before = a;
            here = b;
        }
    }
    if (covered <= 0.0) {
        discard;
    }
    return params.colour * covered;
}
