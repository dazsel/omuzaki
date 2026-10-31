// The stylized program (`program/stylized.rs`): the surface half of an animated-film look. The renderer's own toon
// (`lit`, from the `cel-*` parameters) is set aside and the surface is relit here, per light:
//   first  = the toon terminator (cel-shift, cel-softness) times the light's cast shadow, so a cast shadow takes the
//            shade tone instead of going black
//   second = a step at `shade-2-shift`: below it the deep shade (`cel-shade` times `shade-2`), by `shade-2-strength`
//   tone   = mix(mix(deep, shade, second), 1, first), then blended toward a wrapped soft ramp by `wrap`, then tinted
//            by `terminator` across a band `terminator-width` wide either side of the terminator
//   diffuse += light colour * tone;  specular += a cel highlight on the half vector;  rim += a band on the lit edge
// then colour = albedo * diffuse / PI (the skin treatment on it) + emissive + the toon's fresnel rim + specular + rim.
// Appended after `support.wgsl`; the offsets are generated from the declaration.

var<private> stylized_albedo: vec3<f32>;

fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    stylized_albedo = albedo;
    return albedo;
}

// A step at `edge` softened over `softness` either side; a hard step when the softness is zero.
fn stylized_step(edge: f32, softness: f32, x: f32) -> f32 {
    if (softness <= 0.0) {
        return select(0.0, 1.0, x >= edge);
    }
    let t = saturate((x - (edge - softness)) / (2.0 * softness));
    return t * t * (3.0 - 2.0 * t);
}

fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let shade = material.shade.rgb;
    let deep = shade * program_vec3(P_SHADE_2);
    let deep_on = program_float(P_SHADE_2_STRENGTH);
    let deep_shift = program_float(P_SHADE_2_SHIFT);
    let deep_softness = program_float(P_SHADE_2_SOFTNESS);
    let wrap = program_float(P_WRAP);
    let warm = program_vec3(P_TERMINATOR);
    let warm_width = program_float(P_TERMINATOR_WIDTH);
    // The highlight's size is how far from the mirror direction it reaches: N.H above 1 - 0.2 * size.
    let spec_edge = 1.0 - 0.2 * program_float(P_SPEC_SIZE);
    let spec_softness = 0.05 * program_float(P_SPEC_SOFTNESS);
    let spec = program_vec3(P_SPEC_COLOUR) * program_float(P_SPEC_STRENGTH);
    let rim_width = program_float(P_RIM_WIDTH);
    let rim_colour = program_vec3(P_RIM_COLOUR) * program_float(P_RIM_STRENGTH);
    let nv = saturate(dot(n, v));

    var diffuse = environment_light(n);
    var specular = vec3<f32>(0.0);
    var edge = vec3<f32>(0.0);
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            diffuse += mix(l.ground.rgb, l.colour.rgb, 0.5 + 0.5 * n.y);
            continue;
        }
        let incident = incident_light(l, i, n, p.world_position);
        let ndl = dot(n, incident.to_light);
        let first = terminator(ndl) * incident.hair * incident.shadow;
        let second = mix(1.0, stylized_step(deep_shift, deep_softness, ndl), deep_on);
        var tone = mix(mix(deep, shade, second), vec3<f32>(1.0), first);
        let soft = saturate((ndl + wrap) / (1.0 + wrap)) * incident.hair * incident.shadow;
        tone = mix(tone, mix(shade, vec3<f32>(1.0), soft), wrap);
        if (warm_width > 0.0) {
            let band = 1.0 - smoothstep(0.0, warm_width, abs(ndl - material.toon.x));
            tone = tone * mix(vec3<f32>(1.0), warm, band);
        }
        diffuse += incident.colour * tone;
        let h = normalize(v + incident.to_light);
        specular += incident.colour * spec * (first * stylized_step(spec_edge, spec_softness, dot(n, h)));
        if (rim_width > 0.0) {
            let band = stylized_step(1.0 - rim_width, 0.02, 1.0 - nv);
            edge += incident.colour * rim_colour * (band * saturate(4.0 * ndl) * incident.shadow);
        }
    }
    var colour = stylized_albedo * diffuse / PI;
    if ((material.flags.x & FLAG_SKIN) != 0u) {
        colour = skin_adjust(colour, stylized_albedo);
    }
    let fresnel = material.rim.rgb * pow(1.0 - nv, material.toon.z) * material.rim.w;
    return colour + material.emissive.rgb + fresnel + (specular + edge) / PI;
}
