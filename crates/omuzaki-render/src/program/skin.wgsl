// The skin program (`program/skin.rs`): realistic skin. Light scatters under the surface (a wrapped, reddened terminator
// and reddened shadow edges), a thin oily layer reflects (GGX plus the environment), pores and fine lines break the
// surface up, and wet skin (the lid margins) is glossy. Appended after `support.wgsl`; the offsets (P_ROUGHNESS, ...) are
// generated from the declaration.
//
// The `skin-info` attribute: x how wet the skin is (0 dry, 1 the wet line of a lid), y the cavity term (1 open, 0 deep in
// a fold), z extra flush (redness), w unused.

var<private> skin_albedo: vec3<f32>;

fn skin_hash(p: vec3<f32>) -> f32 {
    let q = fract(p * vec3<f32>(0.1031, 0.1030, 0.0973));
    let r = q + dot(q, q.yxz + 33.33);
    return fract((r.x + r.y) * r.z);
}

// Value noise in 0..1 with a quintic fade.
fn skin_noise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = mix(mix(skin_hash(i), skin_hash(i + vec3<f32>(1.0, 0.0, 0.0)), u.x),
                mix(skin_hash(i + vec3<f32>(0.0, 1.0, 0.0)), skin_hash(i + vec3<f32>(1.0, 1.0, 0.0)), u.x), u.y);
    let b = mix(mix(skin_hash(i + vec3<f32>(0.0, 0.0, 1.0)), skin_hash(i + vec3<f32>(1.0, 0.0, 1.0)), u.x),
                mix(skin_hash(i + vec3<f32>(0.0, 1.0, 1.0)), skin_hash(i + vec3<f32>(1.0, 1.0, 1.0)), u.x), u.y);
    return mix(a, b, u.z);
}

// The distance from `p` to the nearest of one random point per unit cell (3D Worley).
fn skin_cells(p: vec3<f32>) -> f32 {
    let i = floor(p);
    var best = 8.0;
    for (var z = -1; z <= 1; z++) {
        for (var y = -1; y <= 1; y++) {
            for (var x = -1; x <= 1; x++) {
                let cell = i + vec3<f32>(f32(x), f32(y), f32(z));
                let jitter = vec3<f32>(skin_hash(cell), skin_hash(cell + 17.3), skin_hash(cell + 41.9));
                best = min(best, length(cell + jitter - p));
            }
        }
    }
    return best;
}

// The micro surface's height at world point `w` (metres), in units of a pore's spacing: round pores (a dimple at each
// cell's point) and a faint crosshatch of fine lines.
fn skin_height(w: vec3<f32>) -> f32 {
    let scale = program_float(P_PORE_SCALE);
    let pores = 1.0 - smoothstep(0.0, 0.24, skin_cells(w * scale));
    let lines = skin_noise(w * scale * vec3<f32>(0.25, 1.2, 0.25)) + skin_noise(w * scale * vec3<f32>(1.2, 0.25, 0.8));
    return -pores + 0.18 * (lines - 1.0);
}

fn skin_micro_normal(n: vec3<f32>, w: vec3<f32>, footprint: f32, strength: f32) -> vec3<f32> {
    let scale = program_float(P_PORE_SCALE);
    // Fade the pores out where a pixel covers several of them, so they never shimmer.
    let fade = 1.0 - smoothstep(0.25 / scale, 1.0 / scale, footprint);
    if (fade * strength <= 0.0) {
        return n;
    }
    let helper = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(n.y) > 0.9);
    let t1 = normalize(cross(n, helper));
    let t2 = cross(n, t1);
    let e = 0.2 / scale;
    let h = skin_height(w);
    let d1 = skin_height(w + t1 * e) - h;
    let d2 = skin_height(w + t2 * e) - h;
    // Height is in units of `depth / scale`: the slope is `depth * d / (e * scale)`.
    let slope = program_float(P_PORE_DEPTH) * strength * fade / (e * scale);
    return normalize(n - slope * (d1 * t1 + d2 * t2));
}

fn skin_ggx(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
    let h = normalize(v + l);
    let a = roughness * roughness;
    let a2 = a * a;
    let nh = saturate(dot(n, h));
    let nl = saturate(dot(n, l));
    let nv = max(dot(n, v), 1e-4);
    let den = nh * nh * (a2 - 1.0) + 1.0;
    let dist = a2 / (PI * den * den);
    let vis = 0.5 / max(nl * sqrt(nv * nv * (1.0 - a2) + a2) + nv * sqrt(nl * nl * (1.0 - a2) + a2), 1e-5);
    return dist * vis * nl;
}

fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let info = p.extra[0];
    let flushed = albedo * mix(vec3<f32>(1.0), program_vec3(P_FLUSH_COLOUR), saturate(info.z));
    skin_albedo = mix(flushed, program_vec3(P_WET_COLOUR), saturate(info.x) * program_float(P_WET_TINT));
    return skin_albedo;
}

fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let info = p.extra[0];
    let wet = saturate(info.x);
    let cavity = mix(1.0, saturate(info.y), program_float(P_CAVITY));
    let footprint = max(length(p.world_dx), length(p.world_dy));
    let nb = skin_micro_normal(n, p.world_position, footprint, 1.0 - wet);
    // Pores hold a little shadow of their own.
    let scale = program_float(P_PORE_SCALE);
    let pore_fade = (1.0 - smoothstep(0.25 / scale, 1.0 / scale, footprint)) * (1.0 - wet) * saturate(program_float(P_PORE_DEPTH) * 10.0);
    var pore_shade = 1.0;
    if (pore_fade > 0.0) {
        pore_shade = 1.0 - 0.18 * pore_fade * (1.0 - smoothstep(0.0, 0.18, skin_cells(p.world_position * scale)));
    }
    let wrap = program_float(P_WRAP);
    let scatter = program_vec3(P_SCATTER_COLOUR);
    let roughness = mix(program_float(P_ROUGHNESS), program_float(P_WET_ROUGHNESS), wet);
    let f0 = mix(program_float(P_SPECULAR), 0.03, wet);
    var diffuse = vec3<f32>(0.0);
    var specular = vec3<f32>(0.0);
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            diffuse += mix(l.ground.rgb, l.colour.rgb, 0.5 + 0.5 * nb.y) * cavity;
            continue;
        }
        let incident = incident_light(l, i, n, p.world_position);
        // Light scattered under the skin: the terminator wraps and reddens, and so do the shadow's edges.
        let ndl = dot(nb, incident.to_light);
        let wrapped = saturate((ndl + wrap) / (1.0 + wrap));
        let tint = mix(scatter, vec3<f32>(1.0), saturate(ndl * 1.6 + 0.15));
        let s = incident.shadow;
        let shadow = vec3<f32>(s) + scatter * (s * (1.0 - s) * 1.6);
        diffuse += incident.colour * wrapped * tint * shadow;
        let h = normalize(v + incident.to_light);
        let fresnel = f0 + (1.0 - f0) * pow(1.0 - saturate(dot(h, v)), 5.0);
        specular += incident.colour * s * skin_ggx(nb, v, incident.to_light, roughness) * fresnel;
    }
    diffuse += environment_light(nb) * cavity;
    let nv = saturate(dot(nb, v));
    let fresnel_v = f0 + (1.0 - f0) * pow(1.0 - nv, 5.0);
    specular += environment_specular(reflect(-v, nb), roughness) * fresnel_v * cavity;
    return skin_albedo * diffuse * cavity * pore_shade / PI + specular * program_float(P_SHEEN);
}
