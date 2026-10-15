// The GPU morph blend: what `morph/cpu.rs` does on the CPU, one thread to a vertex, in the same order of arithmetic.
//
// A mesh's sparse targets are regrouped once, at creation, by vertex: `first[v]..first[v + 1]` are the entries of
// vertex `v`, in target order, so a thread sums its own vertex's deltas with no two threads ever writing one vertex
// and no atomics. Per frame only `params` changes: the vertex count, and one weight per target.
//
//   position = base + sum of weight * position delta                        (a target of weight 0 is skipped)
//   normal   = normalize(base normal + sum of weight * normal delta)        (only if a normal delta reached the vertex)
//
// The second entry point re-welds the outline directions: each vertex gets the normalised sum of the blended normals of
// every vertex at its point (`slot_of`, `group_first`, `members` are the mesh's creation-time weld table), or its own
// normal if they cancel out, as `resmooth` does on the CPU.
//
// Bindings are numbered apart so one module serves both entry points; each pipeline's layout lists only its own.

struct Entry {
    which: u32,
    has_normal: u32,
    px: f32,
    py: f32,
    pz: f32,
    nx: f32,
    ny: f32,
    nz: f32,
}

struct Params {
    count: u32,
    targets: u32,
    pad0: u32,
    pad1: u32,
    weights: array<f32>,
}

// Group 0: the mesh's data, uploaded once.
@group(0) @binding(0) var<storage, read> base: array<f32>;
@group(0) @binding(1) var<storage, read> first: array<u32>;
@group(0) @binding(2) var<storage, read> entries: array<Entry>;
@group(0) @binding(3) var<storage, read> slot_of: array<u32>;
@group(0) @binding(4) var<storage, read> group_first: array<u32>;
@group(0) @binding(5) var<storage, read> members: array<u32>;

// Group 1: the instance's, `shapes` six floats a vertex (position, normal) and `directions` three.
@group(1) @binding(0) var<storage, read_write> shapes: array<f32>;
@group(1) @binding(1) var<storage, read> params: Params;
@group(1) @binding(2) var<storage, read> blended: array<f32>;
@group(1) @binding(3) var<storage, read_write> directions: array<f32>;

// Below this the squared length is no direction: glam's `try_normalize` fails there too.
const MIN_LENGTH_SQUARED: f32 = 1e-30;

@compute @workgroup_size(64)
fn blend(@builtin(global_invocation_id) id: vec3<u32>) {
    let v = id.x;
    if (v >= params.count) {
        return;
    }
    let at = v * 6u;
    var position = vec3<f32>(base[at], base[at + 1u], base[at + 2u]);
    let own = vec3<f32>(base[at + 3u], base[at + 4u], base[at + 5u]);
    var normal = own;
    var moved = false;
    for (var e = first[v]; e < first[v + 1u]; e = e + 1u) {
        let entry = entries[e];
        let weight = params.weights[entry.which];
        if (weight == 0.0) {
            continue;
        }
        position = position + vec3<f32>(entry.px, entry.py, entry.pz) * weight;
        if (entry.has_normal != 0u) {
            normal = normal + vec3<f32>(entry.nx, entry.ny, entry.nz) * weight;
            moved = true;
        }
    }
    if (moved) {
        let squared = dot(normal, normal);
        normal = select(own, normal * inverseSqrt(squared), squared > MIN_LENGTH_SQUARED);
    }
    shapes[at] = position.x;
    shapes[at + 1u] = position.y;
    shapes[at + 2u] = position.z;
    shapes[at + 3u] = normal.x;
    shapes[at + 4u] = normal.y;
    shapes[at + 5u] = normal.z;
}

@compute @workgroup_size(64)
fn weld(@builtin(global_invocation_id) id: vec3<u32>) {
    let v = id.x;
    if (v >= params.count) {
        return;
    }
    let point = slot_of[v];
    var sum = vec3<f32>(0.0);
    for (var m = group_first[point]; m < group_first[point + 1u]; m = m + 1u) {
        let at = members[m] * 6u + 3u;
        sum = sum + vec3<f32>(blended[at], blended[at + 1u], blended[at + 2u]);
    }
    let own_at = v * 6u + 3u;
    let own = vec3<f32>(blended[own_at], blended[own_at + 1u], blended[own_at + 2u]);
    let squared = dot(sum, sum);
    let direction = select(own, sum * inverseSqrt(squared), squared > MIN_LENGTH_SQUARED);
    directions[v * 3u] = direction.x;
    directions[v * 3u + 1u] = direction.y;
    directions[v * 3u + 2u] = direction.z;
}
