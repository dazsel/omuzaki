// The metal program (`program/metal.rs`): a reflected sky and ground with a hard horizon, banded highlights from every
// light, and glints on the brightest band. Appended after `support.wgsl`; the offsets (P_SHINE, P_BANDS, P_GLOSS,
// P_SHARPNESS, P_REFLECTION, P_SPARKLE) are generated from the declaration.

fn metal_hash(p: vec3<f32>) -> f32 {
    return fract(sin(dot(p, vec3<f32>(12.9898, 78.233, 37.719))) * 43758.5453);
}

// A step from 0 to 1 at `edge`, `width` wide either side (a hard step at width 0).
fn metal_step(edge: f32, width: f32, x: f32) -> f32 {
    return glsl_smoothstep(edge - width - 1e-5, edge + width + 1e-5, x);
}

var<private> metal_albedo: vec3<f32>;

fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    metal_albedo = albedo;
    return albedo;
}

// The style axis (#2607): 1 is the anime metal below, 0 a physical conductor (GGX with the surface colour as its
// reflectance, under every light and the environment), and the two blend between.
fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let style = program_float(P_STYLE);
    let cel = metal_cel(lit, n, v, p);
    if (style >= 1.0) {
        return cel;
    }
    return mix(metal_physical(n, v, p), cel, style);
}

fn metal_ggx(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
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

fn metal_physical(n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let roughness = program_float(P_ROUGHNESS);
    let f0 = metal_albedo;
    var colour = vec3<f32>(0.0);
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            continue;
        }
        let incident = incident_light(l, i, n, p.world_position);
        let h = normalize(v + incident.to_light);
        let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - saturate(dot(h, v)), 5.0);
        colour += incident.colour * incident.shadow * metal_ggx(n, v, incident.to_light, roughness) * fresnel;
    }
    let nv = saturate(dot(n, v));
    let fresnel_v = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - nv, 5.0);
    return colour + environment_specular(reflect(-v, n), roughness) * fresnel_v;
}

fn metal_cel(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let sharp = program_float(P_SHARPNESS);
    let width = mix(0.08, 0.004, sharp);
    // The reflected view: sky above a hard horizon, a dark ground below it, a lighter floor bounce further down.
    let r = reflect(-v, n);
    let horizon = metal_step(-0.02, width * 2.0, r.y);
    let floor_bounce = 0.35 * metal_step(-0.75, 0.2, -r.y);
    let sky = lit * 1.45 + program_vec3(P_SHINE) * 0.08;
    let ground = lit * (0.28 + floor_bounce);
    var colour = mix(lit, mix(ground, sky, horizon), program_float(P_REFLECTION));

    // Banded highlights: dot(N, H) cut into flat bands, each band a step up in brightness.
    let gloss = program_float(P_GLOSS);
    let bands = clamp(round(program_float(P_BANDS)), 1.0, 3.0);
    let top = mix(0.9, 0.995, gloss);
    var peak = 0.0;
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            continue;
        }
        let incident = incident_light(l, i, n, p.world_position);
        let h = normalize(incident.to_light + v);
        let s = dot(n, h);
        var band = 0.0;
        for (var b = 0.0; b < bands; b += 1.0) {
            // The outermost band first, each one further in narrower and brighter.
            let edge = mix(top - 0.12 * (1.0 - gloss) - 0.04, top, (b + 1.0) / bands);
            band += metal_step(edge, width, s) / bands;
        }
        let strength = band * incident.shadow * saturate(dot(n, incident.to_light) * 4.0);
        colour += program_vec3(P_SHINE) * incident.colour / PI * strength;
        peak = max(peak, strength * metal_step(top, width, s));
    }

    // Glints: small cells on the brightest band twinkle with the scene's time.
    let cell = floor(p.world_position * 900.0);
    let phase = metal_hash(cell);
    let twinkle = pow(saturate(sin(frame.look.y * (2.0 + 3.0 * phase) + phase * 40.0)), 24.0);
    colour += program_vec3(P_SHINE) * twinkle * peak * program_float(P_SPARKLE) * 3.0;
    return colour;
}
