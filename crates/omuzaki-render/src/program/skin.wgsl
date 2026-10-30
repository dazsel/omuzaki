// The skin program (`program/skin.rs`): realistic skin. Light scatters under the surface (a wrapped, reddened terminator
// and reddened shadow edges), a thin oily layer reflects (GGX plus the environment), pores and fine lines break the
// surface up, and wet skin (the lid margins) is glossy. Appended after `support.wgsl`; the offsets (P_ROUGHNESS, ...) are
// generated from the declaration.
//
// The `skin-info` attribute: x how wet the skin is (0 dry, 1 the wet line of a lid), y the cavity term (1 open, 0 deep in
// a fold), z extra flush (redness), w the surface's curvature in 1/m (0: flat) for the scattering.

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

// Skin's diffusion profile: d'Eon and Luebke's six Gaussians (variances in mm^2, weights per channel).
const SKIN_VARIANCE: array<f32, 6> = array<f32, 6>(0.0064, 0.0484, 0.187, 0.567, 1.99, 7.41);
const SKIN_WEIGHT: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>(0.233, 0.455, 0.649),
    vec3<f32>(0.100, 0.336, 0.344),
    vec3<f32>(0.118, 0.198, 0.0),
    vec3<f32>(0.113, 0.007, 0.007),
    vec3<f32>(0.358, 0.004, 0.0),
    vec3<f32>(0.078, 0.0, 0.0),
);
const SKIN_TAPS: i32 = 24;

// The profile at distance `d` (mm), per channel.
fn skin_profile(d: f32, spread: f32) -> vec3<f32> {
    var r = vec3<f32>(0.0);
    for (var i = 0; i < 6; i++) {
        let v = SKIN_VARIANCE[i] * spread * spread;
        r += SKIN_WEIGHT[i] * exp(-d * d / (2.0 * v)) / (2.0 * PI * v);
    }
    return r;
}

// Pre-integrated subsurface scattering (Penner): the light a point on a curved surface of radius `radius_mm` receives
// once the profile has carried light round the curve from its neighbours, for each colour channel's `cosines` (the
// cosine of the angle between the light and that channel's normal). Light reaches past the terminator and reddens there.
fn skin_scatter(cosines: vec3<f32>, radius_mm: f32, spread: f32) -> vec3<f32> {
    // Only the arc within a few millimetres matters: past that the profile is nothing.
    let reach = min(PI, 2.0 * asin(min(1.0, 3.0 * spread / max(radius_mm, 1e-3))));
    let theta = acos(clamp(cosines, vec3<f32>(-1.0), vec3<f32>(1.0)));
    var sum = vec3<f32>(0.0);
    var weight = vec3<f32>(0.0);
    for (var k = 0; k < SKIN_TAPS; k++) {
        let x = reach * ((f32(k) + 0.5) / f32(SKIN_TAPS) * 2.0 - 1.0);
        let d = 2.0 * radius_mm * sin(abs(x) * 0.5);
        let w = skin_profile(d, spread);
        sum += max(cos(theta + x), vec3<f32>(0.0)) * w;
        weight += w;
    }
    return sum / max(weight, vec3<f32>(1e-6));
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
    // Light that travels further under the skin sees the surface's small bumps less: red diffuses from a smoother
    // normal than blue.
    let n_red = normalize(mix(n, nb, 0.2));
    let n_green = normalize(mix(n, nb, 0.6));
    // The surface's radius of curvature (`skin-info` w is its curvature, 1/m); flat where it is not given.
    let radius_mm = 1000.0 / max(info.w, 1.0);
    let spread = program_float(P_SSS);
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
        let to_light = incident.to_light;
        let cosines = vec3<f32>(dot(n_red, to_light), dot(n_green, to_light), dot(nb, to_light));
        var received: vec3<f32>;
        if (spread > 0.0) {
            received = skin_scatter(cosines, radius_mm, spread);
        } else {
            // Without the profile: a wrapped, reddened terminator.
            let ndl = cosines.z;
            received = vec3<f32>(saturate((ndl + wrap) / (1.0 + wrap))) * mix(scatter, vec3<f32>(1.0), saturate(ndl * 1.6 + 0.15));
        }
        // The shadow's edge is scattered too: it reddens as it softens.
        let s = incident.shadow;
        let shadow = vec3<f32>(s) + scatter * (s * (1.0 - s) * 1.6);
        diffuse += incident.colour * received * shadow;
        let h = normalize(v + to_light);
        let fresnel = f0 + (1.0 - f0) * pow(1.0 - saturate(dot(h, v)), 5.0);
        specular += incident.colour * s * skin_ggx(nb, v, to_light, roughness) * fresnel;
    }
    diffuse += environment_light(n_green) * cavity;
    let nv = saturate(dot(nb, v));
    let fresnel_v = f0 + (1.0 - f0) * pow(1.0 - nv, 5.0);
    specular += environment_specular(reflect(-v, nb), roughness) * fresnel_v * cavity;
    return skin_albedo * diffuse * cavity * pore_shade / PI + specular * program_float(P_SHEEN);
}
