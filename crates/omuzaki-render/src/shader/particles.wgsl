// The `Particles` pass (particles.md D4 to D6). Appended to `program::opaque_source(None)` and `particles_state.wgsl`,
// so a toon particle is lit by the surfaces' own `toon_colour` and fogged by `apply_fog`.
//
// Bind groups: group 0 is the pass's own (the frame uniform at b0 and the block at b3, both with dynamic offsets), group
// 1 the system's material (white, its shading, its atlas as the base texture), 2 the shadow receiver (never read: the
// material does not receive shadows), 3 the environment.
//
// One instance per particle slot of the block, a quad as a four-vertex strip. The vertex stage finds the slot's batch,
// evaluates the particle's centre and turns it into a quad as the mock does (`omuzaki-render-mock/src/particles.rs`):
// facing the camera and rolled, along the velocity on screen, or tumbling in the world; never under 1.5 pixels a side,
// with the coverage scaled down by the area it gained. A particle that is not drawn becomes a degenerate triangle.
//
// Coverage stands in for blending (D4). The colour draw outputs the lit colour and the coverage alpha with
// alpha-to-coverage on and writes RGB and depth; the alpha draw runs the same vertex stage (the position is
// `@invariant`), passes only where the depth equals what the colour draw left, and writes alpha 1. The depth is held
// below the cleared 1 (`z <= (1 - 2^-20) w`), so the alpha draw can never match an untouched sample.

const PARTICLE_MIN_PIXELS: f32 = 1.5;
const PARTICLE_DEPTH_CEILING: f32 = 0.99999904632568359375;

struct ParticleOut {
    @builtin(position) @invariant clip: vec4<f32>,
    // The point in the shape's own units: half heights from the centre, y up.
    @location(0) q: vec2<f32>,
    // The quad's corner coordinates, (0, 0) at the top left.
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) colour: vec3<f32>,
    @location(3) @interpolate(flat) centre: vec3<f32>,
    @location(4) @interpolate(flat) normal: vec3<f32>,
    // x: the drawn quad's width over its height; y: one pixel in half heights; z: the coverage left after the 1.5-pixel
    // minimum; w: the atlas cell.
    @location(5) @interpolate(flat) shape: vec4<f32>,
}

// The batch slot `slot` belongs to: the last batch of the block whose first slot is at or before it.
fn find_batch(slot: u32) -> u32 {
    var lo = 0u;
    var hi = max(block.system.atlas.w, 1u);
    while (hi - lo > 1u) {
        let mid = (lo + hi) / 2u;
        if (block.batches[mid].misc.x <= slot) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    return lo;
}

fn hidden() -> ParticleOut {
    var out: ParticleOut;
    out.clip = vec4<f32>(2.0, 2.0, 2.0, 1.0);
    out.q = vec2<f32>(0.0);
    out.uv = vec2<f32>(0.0);
    out.colour = vec3<f32>(0.0);
    out.centre = vec3<f32>(0.0);
    out.normal = vec3<f32>(0.0, 0.0, 1.0);
    out.shape = vec4<f32>(1.0, 1.0, 0.0, 0.0);
    return out;
}

@vertex
fn vs_particle(@builtin(vertex_index) corner: u32, @builtin(instance_index) slot: u32) -> ParticleOut {
    let r = find_batch(slot);
    let g = block.batches[r].ids.x + (slot - block.batches[r].misc.x);
    let state = particle_state(r, g);
    if (!state.alive) {
        return hidden();
    }
    let centre_clip = frame.view_proj * vec4<f32>(state.position, 1.0);
    // Behind the eye or beyond the far plane: the depth clamp below must never pull such a particle into view.
    if (centre_clip.w <= 0.0 || centre_clip.z > centre_clip.w) {
        return hidden();
    }

    let to_viewer = view_direction(state.position);
    let right = normalize(vec3<f32>(frame.view[0].x, frame.view[1].x, frame.view[2].x));
    let up = normalize(vec3<f32>(frame.view[0].y, frame.view[1].y, frame.view[2].y));
    var along_u: vec3<f32>;
    var along_v: vec3<f32>;
    switch (block.system.codes.y) {
        case ORIENT_BILLBOARD: {
            let s = sin(state.roll);
            let c = cos(state.roll);
            along_u = right * c + up * s;
            along_v = up * c - right * s;
        }
        case ORIENT_VELOCITY: {
            let across = state.velocity - to_viewer * dot(state.velocity, to_viewer);
            let across_length = length(across);
            let long = select(up, across / across_length, across_length > 1e-20);
            let side = cross(to_viewer, long);
            along_u = select(vec3<f32>(0.0), normalize(side), length(side) > 0.0);
            along_v = long;
        }
        default: {
            along_u = normalize(state.edge_u);
            along_v = normalize(state.edge_v);
        }
    }

    let per_metre = frame.proj[1][1] * frame.target_size.y * 0.5 / max(centre_clip.w, 1e-6);
    var width = state.extent.x;
    var height = state.extent.y;
    if (width <= 0.0 || height <= 0.0) {
        return hidden();
    }
    let area = width * height;
    width = max(width, PARTICLE_MIN_PIXELS / per_metre);
    height = max(height, PARTICLE_MIN_PIXELS / per_metre);

    let u = f32(corner & 1u);
    let v = f32(corner >> 1u);
    let world = state.position + along_u * (width * 0.5 * (2.0 * u - 1.0)) + along_v * (height * 0.5 * (1.0 - 2.0 * v));
    var clip = frame.view_proj * vec4<f32>(world, 1.0);
    clip.z = min(clip.z, PARTICLE_DEPTH_CEILING * clip.w);

    var out: ParticleOut;
    out.clip = clip;
    let aspect = width / height;
    out.q = vec2<f32>((2.0 * u - 1.0) * aspect, 1.0 - 2.0 * v);
    out.uv = vec2<f32>(u, v);
    out.colour = state.colour;
    out.centre = state.position;
    // Lit once, at the centre (D5): a camera-facing quad faces the viewer, a tumbling one turns its own side to it.
    var normal = state.normal;
    if (all(normal == vec3<f32>(0.0))) {
        normal = to_viewer;
    } else if (dot(normal, to_viewer) < 0.0) {
        normal = -normal;
    }
    out.normal = normal;
    out.shape = vec4<f32>(aspect, 2.0 / max(height * per_metre, 1e-3), area / (width * height), f32(state.cell));
    return out;
}

// The star of `particles.rs` in the mock: `n` points of radius 1, inner vertices at 0.45, a point at the top.
fn star_distance(p: vec2<f32>, n: f32) -> f32 {
    let sector = PI / n;
    let angle = atan2(p.x, p.y);
    let wrapped = angle + sector;
    let period = 2.0 * sector;
    let folded = abs(wrapped - floor(wrapped / period) * period - sector);
    let q = vec2<f32>(cos(folded), sin(folded)) * length(p);
    let tip = vec2<f32>(1.0, 0.0);
    let inner = vec2<f32>(cos(sector), sin(sector)) * 0.45;
    let edge = inner - tip;
    let t = clamp(dot(q - tip, edge) / dot(edge, edge), 0.0, 1.0);
    let distance = length(q - (tip + edge * t));
    let perp_q = edge.x * (q - tip).y - edge.y * (q - tip).x;
    let perp_centre = edge.x * (-tip).y - edge.y * (-tip).x;
    return select(distance, -distance, perp_q * perp_centre > 0.0);
}

// A square turned 45 degrees with a disc on each upper side.
fn heart_distance(p: vec2<f32>) -> f32 {
    let side = 1.17;
    let r = 0.7071067811865476;
    let centre = vec2<f32>(0.0, -1.0 + side * r);
    let local = p - centre;
    let turned = vec2<f32>(local.x + local.y, local.y - local.x) * r;
    let square = max(abs(turned.x), abs(turned.y)) - side * 0.5;
    let lobe = vec2<f32>(side * 0.5 * r, -1.0 + 1.5 * side * r);
    let discs = min(length(p - lobe), length(p - vec2<f32>(-lobe.x, lobe.y))) - side * 0.5;
    return min(square, discs);
}

// The signed distance to the sprite shape (`SpriteShape::code`) in half heights, negative inside, for a quad `aspect`
// wide per unit of height. The mock's `shape_distance` is the same function.
fn shape_distance(shape: u32, points: u32, p: vec2<f32>, aspect: f32) -> f32 {
    switch (shape) {
        case 0u, 1u: {
            return max(abs(p.x) - aspect, abs(p.y) - 1.0);
        }
        case 2u: {
            return length(p) - 1.0;
        }
        case 3u: {
            let c = vec2<f32>(0.75, 0.0);
            return max(length(p - c) - 1.25, length(p + c) - 1.25);
        }
        case 4u: {
            return star_distance(p, f32(max(points, 2u)));
        }
        case 5u: {
            return heart_distance(p);
        }
        default: {
            let r = min(aspect, 1.0);
            let reach = 1.0 - r;
            return length(p - vec2<f32>(0.0, clamp(p.y, -reach, reach))) - r;
        }
    }
}

// The lit, fogged colour of a particle's fragment and its coverage alpha.
fn particle_colour(in: ParticleOut) -> vec4<f32> {
    let sys = block.system;
    let columns = max(sys.atlas.x, 1u);
    let rows = max(sys.atlas.y, 1u);
    let cell = u32(in.shape.w);
    let cell_corner = vec2<f32>(f32(cell % columns), f32(cell / columns));
    let at = (cell_corner + clamp(in.uv, vec2<f32>(0.0), vec2<f32>(1.0))) / vec2<f32>(f32(columns), f32(rows));
    // Sampled for every sprite, in uniform control flow; a shape's material holds the white stand-in.
    let sampled = texel(textureSample(base_texture, base_sampler, at), texture_flags(0u));

    var albedo = in.colour;
    var alpha = 1.0;
    var edge = false;
    if (sys.codes.w == 1u) {
        albedo = albedo * sampled.rgb;
        alpha = sampled.a;
    } else {
        let pixel = in.shape.y;
        let d = shape_distance(sys.codes.x, sys.codes.z, in.q, in.shape.x);
        alpha = clamp(0.5 - d / pixel, 0.0, 1.0);
        edge = sys.edge.x > 0.0 && d > -sys.edge.x * pixel;
    }
    var colour = albedo;
    if (material.flags.y == SHADING_TOON) {
        colour = toon_colour(albedo, in.normal, in.centre);
    }
    colour = colour * (1.0 + sys.wind.w);
    if (edge) {
        colour = colour * (sys.turn.w * frame.look.x);
    }
    return vec4<f32>(apply_fog(colour, in.centre), alpha * sys.size.z * in.shape.z);
}

// The colour draw with alpha-to-coverage (two or more samples).
@fragment
fn fs_particle(in: ParticleOut) -> @location(0) vec4<f32> {
    return particle_colour(in);
}

// The colour draw at one sample: coverage is a 0.5 alpha test.
@fragment
fn fs_particle_tested(in: ParticleOut) -> @location(0) vec4<f32> {
    let colour = particle_colour(in);
    if (colour.a <= 0.5) {
        discard;
    }
    return colour;
}

// The alpha draw: alpha 1 wherever the colour draw left this particle's depth.
@fragment
fn fs_particle_alpha() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
