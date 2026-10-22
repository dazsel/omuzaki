// A particle at a time (particles.md D3): render-api's `particles::evaluate`, restated for the GPU. It follows
// `common.wgsl` and reads nothing of the frame, so the parity test (`pass/particles.rs`) can run it in a compute shader
// and compare it with the reference. The hash and the index arithmetic are exact; `exp`, `sin`, `cos` and the cube root
// differ from Rust's in the last bits (D7).
//
// Group 0, binding 3 is one block of the `Particles` pass: a system's header and up to `BLOCK_BATCHES` of its spawn
// batches, bound with a dynamic offset. The layouts are `SystemUniform` (resource/particles.rs) and `BatchUniform`
// (pass/particles.rs) and must match them field for field.

const PARTICLE_TAU: f32 = 6.283185307179586;
const BLOCK_BATCHES: u32 = 91u;

// `particles::Stream`: one number per random choice about a particle.
const STREAM_LIFE: u32 = 0u;
const STREAM_KEEP: u32 = 1u;
const STREAM_POS0: u32 = 2u;
const STREAM_POS1: u32 = 3u;
const STREAM_POS2: u32 = 4u;
const STREAM_DIR0: u32 = 5u;
const STREAM_DIR1: u32 = 6u;
const STREAM_SPEED: u32 = 7u;
const STREAM_PHASE: u32 = 8u;
const STREAM_SIDE: u32 = 9u;
const STREAM_SIZE: u32 = 10u;
const STREAM_COLOUR: u32 = 11u;
const STREAM_ROLL: u32 = 12u;
const STREAM_SPIN: u32 = 13u;
const STREAM_AX0: u32 = 14u;
const STREAM_AX1: u32 = 15u;
const STREAM_CELL: u32 = 16u;

// `Orientation::code`.
const ORIENT_BILLBOARD: u32 = 0u;
const ORIENT_VELOCITY: u32 = 1u;
const ORIENT_TUMBLE: u32 = 2u;

// `EmitterShape::code`.
const EMIT_POINT: u32 = 0u;
const EMIT_SPHERE: u32 = 1u;
const EMIT_BOX: u32 = 2u;
const EMIT_RECT: u32 = 3u;

struct ParticleSystem {
    lifetime_speed: vec4<f32>,  // lifetime min, max; speed min, max
    direction: vec4<f32>,       // emission axis (emitter space); cosine of the cone's half-angle
    gravity: vec4<f32>,         // gravity; drag
    wind: vec4<f32>,            // wind; glow
    flutter: vec4<f32>,         // amplitude, frequency, 1 when on; the floor height
    size: vec4<f32>,            // size min, max; coverage; the quad's aspect
    size_curve: vec4<f32>,      // the size multiplier at life 0, 1/3, 2/3, 1
    turn: vec4<f32>,            // spin or tumble rate min, max (degrees per second); velocity stretch; edge shade
    edge: vec4<f32>,            // edge width in pixels (0: none); 1 when there is a floor; the frame's density
    codes: vec4<u32>,           // sprite shape, orientation, star points, 1 for an atlas
    atlas: vec4<u32>,           // columns, rows, cells; batches in this block
    padding: array<vec4<f32>, 5>,
}

struct Batch {
    emitter: mat4x4<f32>,       // emitter to world
    velocity: vec4<f32>,        // the anchor's velocity; the interval between births
    extent: vec4<f32>,          // `EmitterShape::extent`; age0 = time - start (computed in f64)
    ids: vec4<u32>,             // first, count, seed; emitter shape | colours << 4 | listed cells << 8
    misc: vec4<u32>,            // the block's first slot for this batch; the scale's bits
    cells: vec4<u32>,           // up to eight cells, two per word, low half first
    colours: array<vec4<u32>, 2>, // up to eight sRGB colours, RGBA8 with red in the low byte
}

struct Block {
    system: ParticleSystem,
    batches: array<Batch, BLOCK_BATCHES>,
}

@group(0) @binding(3) var<uniform> block: Block;

// The PCG hash (Jarzynski and Olano 2020), as `particles::pcg`.
fn pcg(x: u32) -> u32 {
    let s = x * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

// `particles::particle_random`: 24 bits, exactly representable, so the CPU and the GPU agree bit for bit.
fn particle_random(seed: u32, index: u32, stream: u32) -> f32 {
    let h = pcg(pcg(pcg(index) ^ seed) ^ (stream * 0x9E3779B9u));
    return f32(h >> 8u) / 16777216.0;
}

struct Basis {
    a: vec3<f32>,
    b: vec3<f32>,
}

// `particles::onb`: Duff et al.'s branchless basis.
fn onb(n: vec3<f32>) -> Basis {
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    return Basis(vec3<f32>(1.0 + s * n.x * n.x * a, s * b, -s * n.x), vec3<f32>(b, s + n.y * n.y * a, -n.y));
}

fn sphere_dir(u0: f32, u1: f32) -> vec3<f32> {
    let z = 1.0 - 2.0 * u0;
    let r = sqrt(max(1.0 - z * z, 0.0));
    let phi = PARTICLE_TAU * u1;
    return vec3<f32>(r * cos(phi), r * sin(phi), z);
}

fn pick_index(u: f32, n: u32) -> u32 {
    return min(u32(u * f32(n)), max(n, 1u) - 1u);
}

// `Span::at`: `min + (max - min) u`, in that order.
fn span_at(span: vec2<f32>, u: f32) -> f32 {
    return span.x + (span.y - span.x) * u;
}

fn size_curve(curve: vec4<f32>, x: f32) -> f32 {
    let s = clamp(x, 0.0, 1.0) * 3.0;
    let i = min(u32(s), 2u);
    let f = s - f32(i);
    return curve[i] + (curve[i + 1u] - curve[i]) * f;
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

// `particles::ParticleState`, plus whether the particle is drawn at all.
struct ParticleState {
    alive: bool,
    position: vec3<f32>,
    velocity: vec3<f32>,
    extent: vec2<f32>,
    colour: vec3<f32>,
    normal: vec3<f32>,
    edge_u: vec3<f32>,
    edge_v: vec3<f32>,
    roll: f32,
    cell: u32,
}

// Particle `g` of batch `r` of the bound block, at the batch's age (D3). Not alive when it is outside the batch, unborn,
// dead, thinned by the density or below the floor.
fn particle_state(r: u32, g: u32) -> ParticleState {
    let b = block.batches[r];
    let sys = block.system;
    var out: ParticleState;
    out.alive = false;
    let first = b.ids.x;
    if (g < first || g - first >= b.ids.y) {
        return out;
    }
    let seed = b.ids.z;
    let a = b.extent.w - f32(g - first) * b.velocity.w;
    let lifetime = span_at(sys.lifetime_speed.xy, particle_random(seed, g, STREAM_LIFE));
    if (!(a >= 0.0 && a < lifetime) || particle_random(seed, g, STREAM_KEEP) >= sys.edge.z) {
        return out;
    }

    let u0 = particle_random(seed, g, STREAM_POS0);
    let u1 = particle_random(seed, g, STREAM_POS1);
    let u2 = particle_random(seed, g, STREAM_POS2);
    var local = vec3<f32>(0.0);
    switch (b.ids.w & 15u) {
        case EMIT_SPHERE: {
            let radial = select(0.0, pow(u0, 1.0 / 3.0), u0 > 0.0);
            local = b.extent.x * radial * sphere_dir(u1, u2);
        }
        case EMIT_BOX: {
            local = vec3<f32>(2.0 * u0 - 1.0, 2.0 * u1 - 1.0, 2.0 * u2 - 1.0) * b.extent.xyz;
        }
        case EMIT_RECT: {
            local = vec3<f32>((2.0 * u0 - 1.0) * b.extent.x, 0.0, (2.0 * u1 - 1.0) * b.extent.z);
        }
        default: {}
    }
    let p0 = (b.emitter * vec4<f32>(local, 1.0)).xyz;

    let raw = mat3x3<f32>(b.emitter[0].xyz, b.emitter[1].xyz, b.emitter[2].xyz) * sys.direction.xyz;
    let raw_length = length(raw);
    let axis = select(vec3<f32>(0.0, 1.0, 0.0), raw / raw_length, raw_length > 0.0 && raw_length < 3.0e38);
    let basis = onb(axis);
    let cos_t = 1.0 - particle_random(seed, g, STREAM_DIR0) * (1.0 - sys.direction.w);
    let sin_t = sqrt(max(1.0 - cos_t * cos_t, 0.0));
    let phi = PARTICLE_TAU * particle_random(seed, g, STREAM_DIR1);
    let dir = basis.a * (sin_t * cos(phi)) + basis.b * (sin_t * sin(phi)) + axis * cos_t;
    let v0 = dir * span_at(sys.lifetime_speed.zw, particle_random(seed, g, STREAM_SPEED)) + b.velocity.xyz;

    let gravity = sys.gravity.xyz;
    let k = sys.gravity.w;
    var position: vec3<f32>;
    var velocity: vec3<f32>;
    if (k > 0.0) {
        let terminal = sys.wind.xyz + gravity / k;
        let decay = exp(-k * a);
        position = p0 + terminal * a + (v0 - terminal) * ((1.0 - decay) / k);
        velocity = terminal + (v0 - terminal) * decay;
    } else {
        position = p0 + v0 * a + gravity * (0.5 * a * a);
        velocity = v0 + gravity * a;
    }
    if (sys.flutter.z > 0.5) {
        let phase = PARTICLE_TAU * particle_random(seed, g, STREAM_PHASE);
        let sway = sys.flutter.x * sin(PARTICLE_TAU * sys.flutter.y * a + phase);
        let side = PARTICLE_TAU * particle_random(seed, g, STREAM_SIDE);
        position += vec3<f32>(cos(side), 0.0, sin(side)) * sway;
    }
    if (sys.edge.y > 0.5 && position.y < sys.flutter.w) {
        return out;
    }

    let size = span_at(sys.size.xy, particle_random(seed, g, STREAM_SIZE)) * size_curve(sys.size_curve, a / lifetime)
        * bitcast<f32>(b.misc.y);
    let colours = max((b.ids.w >> 4u) & 15u, 1u);
    let pick = pick_index(particle_random(seed, g, STREAM_COLOUR), colours);
    let word = b.colours[pick / 4u][pick % 4u];
    out.colour = srgb_to_linear(unpack4x8unorm(word).rgb);
    let aspect = sys.size.w;
    out.extent = vec2<f32>(size * aspect, size);
    out.normal = vec3<f32>(0.0);
    out.edge_u = vec3<f32>(0.0);
    out.edge_v = vec3<f32>(0.0);
    out.roll = 0.0;
    let turn = radians(span_at(sys.turn.xy, particle_random(seed, g, STREAM_SPIN))) * a;
    switch (sys.codes.y) {
        case ORIENT_BILLBOARD: {
            out.roll = PARTICLE_TAU * particle_random(seed, g, STREAM_ROLL) + turn;
        }
        case ORIENT_VELOCITY: {
            out.extent = vec2<f32>(size, size + sys.turn.z * length(velocity));
        }
        default: {
            let n = sphere_dir(particle_random(seed, g, STREAM_AX0), particle_random(seed, g, STREAM_AX1));
            let turned = onb(n);
            let theta = PARTICLE_TAU * particle_random(seed, g, STREAM_ROLL) + turn;
            let s = sin(theta);
            let c = cos(theta);
            out.edge_u = turned.a * (size * aspect);
            out.edge_v = (turned.b * c + n * s) * size;
            out.normal = n * c - turned.b * s;
            out.roll = theta;
        }
    }
    out.cell = 0u;
    if (sys.codes.w == 1u) {
        let listed = (b.ids.w >> 8u) & 15u;
        let u = particle_random(seed, g, STREAM_CELL);
        if (listed > 0u) {
            let k = pick_index(u, min(listed, 8u));
            out.cell = (b.cells[k / 2u] >> (16u * (k % 2u))) & 0xffffu;
        } else {
            out.cell = pick_index(u, sys.atlas.z);
        }
    }
    out.position = position;
    out.velocity = velocity;
    out.alive = true;
    return out;
}
