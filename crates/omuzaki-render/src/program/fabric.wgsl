// The fabric program (`program/fabric.rs`): woven, knitted or grained cloth. A thread pattern at `weave-scale` per metre
// shades the surface and tilts its normal, fading out where a pixel covers several threads; a wrapped diffuse whose
// shade side takes the fold colour; a GGX lobe, stretched along the warp for satin; and a Charlie sheen for the fuzz.
// `style` blends it toward the cel toon the renderer lit the surface with (`lit`). Appended after `support.wgsl`; the
// offsets are generated from the declaration.

var<private> fabric_albedo: vec3<f32>;

fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    fabric_albedo = albedo;
    return albedo;
}

// The warp's direction on a surface of normal `n`, for satin's streak: the world's up direction laid onto the surface (or
// x where the surface faces up), as a garment's warp hangs.
struct FabricFrame {
    across: vec3<f32>,
    along: vec3<f32>,
}

fn fabric_frame(n: vec3<f32>) -> FabricFrame {
    let up = vec3<f32>(0.0, 1.0, 0.0);
    var along = up - n * dot(n, up);
    if (dot(along, along) < 0.04) {
        let x = vec3<f32>(1.0, 0.0, 0.0);
        along = x - n * dot(n, x);
    }
    along = normalize(along);
    var frame: FabricFrame;
    frame.along = along;
    frame.across = cross(along, n);
    return frame;
}

fn fabric_hash(c: vec2<f32>) -> vec2<f32> {
    let q = vec2<f32>(dot(c, vec2<f32>(127.1, 311.7)), dot(c, vec2<f32>(269.5, 183.3)));
    return fract(sin(q) * 43758.5453);
}

// The thread's height at pattern point `q`, 0 (a gap between threads) to 1 (the top of a thread), for weave `kind`.
fn fabric_height(q: vec2<f32>, kind: u32) -> f32 {
    let cell = floor(q);
    let f = fract(q);
    let warp = sin(PI * f.x);   // a warp thread, round across x
    let weft = sin(PI * f.y);   // a weft thread, round across y
    switch kind {
        case 0u: {
            // Plain weave (cotton, linen): over one, under one.
            let warp_up = (i32(cell.x) + i32(cell.y)) % 2 == 0;
            return select(weft * (0.6 + 0.4 * warp), warp * (0.6 + 0.4 * weft), warp_up);
        }
        case 1u: {
            // Satin: warp floats over four wefts, under one, stepping by two, so the face is almost all warp.
            let under = ((i32(cell.x) * 2 + i32(cell.y)) % 5 + 5) % 5 == 0;
            return select(warp * (0.85 + 0.15 * weft), weft * 0.7, under);
        }
        case 2u: {
            // Stockinette knit: columns of V-shaped loops, two legs a column, each leg leaning in.
            let leg = fract(q.x * 2.0);
            let lean = select(f.x * 2.0, 2.0 - f.x * 2.0, f.x > 0.5) * 0.5;
            let row = fract(q.y * 0.5 + lean);
            return sin(PI * leg) * (0.45 + 0.55 * sin(PI * row));
        }
        default: {
            // Leather grain: pebbles (a cellular pattern), the creases between them low.
            var best = 8.0;
            for (var y = -1; y <= 1; y++) {
                for (var x = -1; x <= 1; x++) {
                    let c = cell + vec2<f32>(f32(x), f32(y));
                    best = min(best, length(c + fabric_hash(c) - q));
                }
            }
            return smoothstep(0.05, 0.55, best) * -1.0 + 1.0;
        }
    }
}

struct FabricSurface {
    normal: vec3<f32>,
    shade: f32,   // the albedo's multiplier: gaps between threads are darker
    along: vec3<f32>,
}

// The pattern laid on the surface from three sides (triplanar, by world axis, blended by how squarely the surface faces
// each: a pattern laid in the surface's own tangent frame would collapse, since a point projects to zero in its own
// tangent plane): its height and its rise per thread in world space. Each side lays its threads with the warp along world y (or
// along z for the side facing up), so a garment's warp hangs.
struct FabricPattern {
    height: f32,
    rise: vec3<f32>,
}

fn fabric_pattern(w: vec3<f32>, n: vec3<f32>, kind: u32) -> FabricPattern {
    var b = pow(abs(n), vec3<f32>(4.0));
    b = b / (b.x + b.y + b.z);
    // Each side: the across and along axes, and its weight.
    let across = array<vec3<f32>, 3>(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(1.0, 0.0, 0.0));
    let along = array<vec3<f32>, 3>(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0));
    let weight = array<f32, 3>(b.x, b.y, b.z);
    let e = 0.08;
    var out: FabricPattern;
    out.height = 0.0;
    out.rise = vec3<f32>(0.0);
    for (var i = 0; i < 3; i++) {
        if (weight[i] < 0.01) {
            continue;
        }
        let q = vec2<f32>(dot(w, across[i]), dot(w, along[i]));
        let h = fabric_height(q, kind);
        let dx = (fabric_height(q + vec2<f32>(e, 0.0), kind) - h) / e;
        let dy = (fabric_height(q + vec2<f32>(0.0, e), kind) - h) / e;
        out.height += h * weight[i];
        out.rise += (across[i] * dx + along[i] * dy) * weight[i];
    }
    return out;
}

// The thread pattern at this fragment: its tilt of the normal and its shading, faded out where a pixel covers several
// threads (so it never shimmers); the gaps still darken on average once it has faded.
fn fabric_surface(n: vec3<f32>, p: ProgramIn) -> FabricSurface {
    let scale = program_float(P_WEAVE_SCALE);
    let depth = program_float(P_WEAVE_DEPTH);
    let kind = u32(clamp(round(program_float(P_WEAVE)), 0.0, 3.0));
    var out: FabricSurface;
    out.normal = n;
    out.along = fabric_frame(n).along;
    let average = 1.0 - 0.22 * depth;
    out.shade = average;
    let footprint = max(length(p.world_dx), length(p.world_dy));
    let fade = 1.0 - smoothstep(0.3 / scale, 1.2 / scale, footprint);
    if (fade <= 0.0 || depth <= 0.0) {
        return out;
    }
    let pattern = fabric_pattern(p.world_position * scale, n, kind);
    // The rise is per thread; the tilt is `depth` times a third of it, along the surface, so a thread leans at most ~40 deg.
    let rise = pattern.rise - n * dot(pattern.rise, n);
    out.normal = normalize(mix(n, normalize(n - rise * (depth * 0.35)), fade));
    out.shade = mix(average, 1.0 - 0.44 * depth * (1.0 - pattern.height), fade);
    return out;
}

fn fabric_ggx(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
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

// Satin's highlight: threads along `t` reflect as fibres do (Kajiya and Kay), a streak across the warp. Normalised so its
// peak matches the GGX lobe's order of magnitude at the same roughness.
fn fabric_fibre(t: vec3<f32>, n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
    let h = normalize(v + l);
    let th = dot(t, h);
    let sin_th = sqrt(max(1.0 - th * th, 0.0));
    // Fibres reflect in a band much narrower than a rough surface's lobe: the exponent grows as roughness^-4.
    let r2 = roughness * roughness;
    let exponent = min(2.0 / max(r2 * r2, 1e-4), 2048.0);
    return pow(sin_th, exponent) * (exponent + 2.0) / (8.0 * PI) * saturate(dot(n, l));
}

// The Charlie sheen distribution (Estevez and Kulla) with Neubelt's visibility, times N.L.
fn fabric_sheen(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
    let h = normalize(v + l);
    let r = max(roughness * roughness, 1e-3);
    let sin2 = max(1.0 - pow(saturate(dot(n, h)), 2.0), 1e-4);
    let dist = (2.0 + 1.0 / r) * pow(sin2, 0.5 / r) / (2.0 * PI);
    let nl = saturate(dot(n, l));
    let nv = max(saturate(dot(n, v)), 1e-4);
    let vis = 1.0 / (4.0 * (nl + nv - nl * nv));
    return dist * vis * nl;
}

fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let style = program_float(P_STYLE);
    if (style >= 1.0) {
        return lit;
    }
    let surface = fabric_surface(n, p);
    let nb = surface.normal;
    let roughness = program_float(P_ROUGHNESS);
    let f0 = program_float(P_SPECULAR);
    let anisotropy = program_float(P_ANISOTROPY);
    let wrap = program_float(P_WRAP);
    // The fuzz is the cloth's own fibres catching the light: it takes the cloth's hue (lightened), times sheen-colour.
    let hue = fabric_albedo / max(max(fabric_albedo.r, max(fabric_albedo.g, fabric_albedo.b)), 1e-4);
    let sheen = program_float(P_SHEEN) * program_vec3(P_SHEEN_COLOUR) * mix(vec3<f32>(1.0), hue, 0.6);
    let sheen_roughness = program_float(P_SHEEN_ROUGHNESS);
    let fold = program_vec3(P_FOLD_COLOUR);
    let fold_strength = program_float(P_FOLD_STRENGTH);
    let fold_softness = max(program_float(P_FOLD_SOFTNESS), 1e-3);
    var diffuse = vec3<f32>(0.0);
    var specular = vec3<f32>(0.0);
    // How lit the cloth is by its strongest direct light: the fold colour takes the shade side of it.
    var key = 0.0;
    var strongest = 0.0;
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            diffuse += mix(l.ground.rgb, l.colour.rgb, 0.5 + 0.5 * nb.y);
            continue;
        }
        let incident = incident_light(l, i, nb, p.world_position);
        let to_light = incident.to_light;
        let s = incident.shadow * incident.hair;
        let received = saturate((dot(nb, to_light) + wrap) / (1.0 + wrap)) * s;
        diffuse += incident.colour * received;
        let power = dot(incident.colour, vec3<f32>(0.2126, 0.7152, 0.0722));
        if (power > strongest) {
            strongest = power;
            key = received;
        }
        let h = normalize(v + to_light);
        let fresnel = f0 + (1.0 - f0) * pow(1.0 - saturate(dot(h, v)), 5.0);
        let lobe = mix(
            fabric_ggx(nb, v, to_light, roughness),
            fabric_fibre(surface.along, nb, v, to_light, roughness),
            anisotropy,
        );
        specular += incident.colour * s * (lobe * fresnel + sheen * fabric_sheen(nb, v, to_light, sheen_roughness));
    }
    diffuse += environment_light(nb);
    let nv = saturate(dot(nb, v));
    let fresnel_v = f0 + (1.0 - f0) * pow(1.0 - nv, 5.0);
    specular += environment_specular(reflect(-v, nb), mix(roughness, 1.0, anisotropy * 0.5)) * fresnel_v;
    specular += environment_light(nb) * sheen * (0.25 * pow(1.0 - nv, 3.0)) / PI;
    // The fold ramp: the shade side of the key light (and every fold it does not reach) tinted toward the fold colour.
    let ramp = smoothstep(0.0, fold_softness, key);
    let tint = mix(vec3<f32>(1.0), mix(fold, vec3<f32>(1.0), ramp), fold_strength);
    let physical = fabric_albedo * surface.shade * diffuse * tint * (1.0 - f0) / PI + specular;
    return mix(physical, lit, style);
}
