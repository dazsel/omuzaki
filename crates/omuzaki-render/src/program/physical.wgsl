// The physical program (`program/physical.rs`): Lambert diffuse with an optional wrap, GGX specular with Schlick's
// Fresnel and a Charlie cloth sheen, under every light and the environment; `style` blends it toward the cel toon the
// renderer lit the surface with (`lit`). Appended after `support.wgsl`; the offsets are generated from the declaration.

var<private> physical_albedo: vec3<f32>;

fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    physical_albedo = albedo;
    return albedo;
}

fn physical_ggx(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
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

// The Charlie sheen distribution (Estevez and Kulla) with Neubelt's visibility, times N.L.
fn physical_sheen(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
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
    let roughness = program_float(P_ROUGHNESS);
    let f0 = program_float(P_SPECULAR);
    let wrap = program_float(P_WRAP);
    let sheen = program_float(P_SHEEN) * program_vec3(P_SHEEN_COLOUR);
    let sheen_roughness = program_float(P_SHEEN_ROUGHNESS);
    var diffuse = vec3<f32>(0.0);
    var specular = vec3<f32>(0.0);
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            diffuse += mix(l.ground.rgb, l.colour.rgb, 0.5 + 0.5 * n.y);
            continue;
        }
        let incident = incident_light(l, i, n, p.world_position);
        let to_light = incident.to_light;
        let ndl = dot(n, to_light);
        let received = saturate((ndl + wrap) / (1.0 + wrap));
        diffuse += incident.colour * received * incident.shadow;
        let h = normalize(v + to_light);
        let fresnel = f0 + (1.0 - f0) * pow(1.0 - saturate(dot(h, v)), 5.0);
        specular += incident.colour * incident.shadow
            * (physical_ggx(n, v, to_light, roughness) * fresnel + sheen * physical_sheen(n, v, to_light, sheen_roughness));
    }
    diffuse += environment_light(n);
    let nv = saturate(dot(n, v));
    let fresnel_v = f0 + (1.0 - f0) * pow(1.0 - nv, 5.0);
    specular += environment_specular(reflect(-v, n), roughness) * fresnel_v;
    // The sheen's grazing glow under the environment, approximated by its directional albedo at this angle.
    specular += environment_light(n) * sheen * (0.25 * pow(1.0 - nv, 3.0)) / PI;
    let physical = physical_albedo * diffuse * (1.0 - f0) / PI + specular;
    return mix(physical, lit, style);
}
