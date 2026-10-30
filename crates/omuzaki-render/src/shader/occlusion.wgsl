// Ambient occlusion and contact shadows (#2507), multiplied over the scene colour after the opaque surfaces
// (`pass/occlusion.rs`). Positions and normals are rebuilt from the depth; the camera looks down -Z.
//
// DEPTH_TYPE is replaced when the module is built: `texture_depth_2d`, or `texture_depth_multisampled_2d`, whose
// `textureLoad(depth, p, 0)` reads the first sample.

struct Params {
    // The depth-buffer-to-distance coefficients: distance = (d * x - y) / (d * z - w).
    depth: vec4<f32>,
    // xy: the lens (tan of the half field of view across and up); zw: the internal size in pixels.
    lens: vec4<f32>,
    // x: the radius in metres; y: the strength; z: the contact shadows' reach in metres (0: none); w: the frame's seed.
    shape: vec4<f32>,
    // xyz: toward the shadow-casting light, camera space; w: 1 when there is one.
    light: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(1) @binding(0) var depth: DEPTH_TYPE;

const DIRECTIONS: i32 = 8;
const STEPS: i32 = 6;
const CONTACT_STEPS: i32 = 16;
// The widest search on screen, in pixels: past this the cost grows and the result barely changes.
const MAX_REACH_PX: f32 = 96.0;
const TAU: f32 = 6.2831853;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

// A pixel's distance along the view direction; 0 where nothing was drawn or outside the picture.
fn distance_at(p: vec2<i32>) -> f32 {
    let size = vec2<i32>(params.lens.zw);
    if (p.x < 0 || p.y < 0 || p.x >= size.x || p.y >= size.y) {
        return 0.0;
    }
    let d = textureLoad(depth, p, 0);
    if (d >= 1.0) {
        return 0.0;
    }
    return (d * params.depth.x - params.depth.y) / (d * params.depth.z - params.depth.w);
}

// The camera-space point at pixel position `q` (pixels, centres at .5) and distance `d`.
fn view_point(q: vec2<f32>, d: f32) -> vec3<f32> {
    let ndc = vec2<f32>(q.x / params.lens.z * 2.0 - 1.0, 1.0 - q.y / params.lens.w * 2.0);
    return vec3<f32>(ndc * params.lens.xy * d, -d);
}

// Where camera-space point `v` lands on the picture, in pixels.
fn to_pixel(v: vec3<f32>) -> vec2<f32> {
    let ndc = v.xy / (max(-v.z, 1e-6) * params.lens.xy);
    return vec2<f32>((ndc.x + 1.0) * 0.5 * params.lens.z, (1.0 - ndc.y) * 0.5 * params.lens.w);
}

fn point_at(p: vec2<i32>) -> vec3<f32> {
    return view_point(vec2<f32>(p) + 0.5, distance_at(p));
}

// Of the two neighbours' differences, the one on the same surface (the smaller), so normals stay sharp at edges.
fn nearer(centre: vec3<f32>, a: vec3<f32>, b: vec3<f32>, da: f32, db: f32) -> vec3<f32> {
    let ea = select(1e9, abs(a.z - centre.z), da > 0.0);
    let eb = select(1e9, abs(b.z - centre.z), db > 0.0);
    return select(centre - b, a - centre, ea < eb);
}

fn hash(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(12.9898, 78.233)) + params.shape.w * 0.618) * 43758.547);
}

@fragment
fn fs_occlusion(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(position.xy);
    let d = distance_at(p);
    if (d <= 0.0) {
        discard;
    }
    let centre = view_point(position.xy, d);
    let px = vec2<i32>(1, 0);
    let py = vec2<i32>(0, 1);
    let dx = nearer(centre, point_at(p + px), point_at(p - px), distance_at(p + px), distance_at(p - px));
    let dy = nearer(centre, point_at(p + py), point_at(p - py), distance_at(p + py), distance_at(p - py));
    var n = normalize(cross(dx, dy));
    // Facing the camera.
    if (dot(n, -centre) < 0.0) {
        n = -n;
    }
    let radius = params.shape.x;
    let strength = params.shape.y;
    let jitter = hash(position.xy);
    let spin = hash(position.yx + 17.0) * TAU;

    // Ambient occlusion: how much of the hemisphere round the normal nearby surfaces cover.
    var open = 1.0;
    let reach_px = min(radius / (d * params.lens.y) * 0.5 * params.lens.w, MAX_REACH_PX);
    if (reach_px >= 1.0) {
        var occluded = 0.0;
        for (var i = 0; i < DIRECTIONS; i++) {
            let angle = spin + TAU * f32(i) / f32(DIRECTIONS);
            let dir = vec2<f32>(cos(angle), sin(angle));
            for (var s = 0; s < STEPS; s++) {
                let t = (f32(s) + jitter) / f32(STEPS);
                let q = position.xy + dir * reach_px * t;
                let qd = distance_at(vec2<i32>(q));
                if (qd <= 0.0) {
                    continue;
                }
                let v = view_point(q, qd) - centre;
                let len = length(v);
                if (len < 1e-7 || len > radius) {
                    continue;
                }
                let falloff = 1.0 - (len * len) / (radius * radius);
                occluded += max(dot(n, v / len) - 0.1, 0.0) * falloff;
            }
        }
        open = 1.0 - strength * saturate(2.0 * occluded / f32(DIRECTIONS * STEPS));
    }

    // Contact shadows: march toward the light; a step behind drawn depth, within a thickness, is blocked.
    var lit = 1.0;
    let contact = params.shape.z;
    if (params.light.w > 0.0 && contact > 0.0 && dot(n, params.light.xyz) > 0.0) {
        let start = centre + n * (0.02 * contact);
        let thickness = max(0.5 * contact, 0.0005);
        for (var s = 1; s <= CONTACT_STEPS; s++) {
            let t = contact * (f32(s) - jitter) / f32(CONTACT_STEPS);
            let v = start + params.light.xyz * t;
            let qd = distance_at(vec2<i32>(to_pixel(v)));
            let behind = -v.z - qd;
            if (qd > 0.0 && behind > 0.0002 * d && behind < thickness) {
                lit = 1.0 - strength * (1.0 - f32(s) / f32(CONTACT_STEPS + 1)) * 0.85;
                break;
            }
        }
    }
    return vec4<f32>(vec3<f32>(open * lit), 1.0);
}
