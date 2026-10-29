// The gem program (`program/gem.rs`): flat facets of different brightness, a dark heart and bright girdle, a little
// fire, hard highlights and facets that flash with the scene's time. Appended after `support.wgsl`; the offsets
// (P_SHINE, P_FACETS, P_DEPTH, P_FIRE, P_SPARKLE) are generated from the declaration.

fn gem_hash(p: vec3<f32>) -> f32 {
    return fract(sin(dot(p, vec3<f32>(127.1, 311.7, 74.7))) * 43758.5453);
}

// The view-space normal snapped to the facet lattice, and the facet's id.
fn gem_facet(n: vec3<f32>) -> vec4<f32> {
    let n_view = normalize((frame.view * vec4<f32>(n, 0.0)).xyz);
    let cuts = max(program_float(P_FACETS), 1.0);
    let id = round(n_view * cuts);
    let snapped = normalize(id / cuts + n_view * 1e-3);
    return vec4<f32>(snapped, gem_hash(id));
}

fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    return albedo;
}

fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let facet = gem_facet(n);
    let id = facet.w;
    // The facet normal back in world space, for the lights.
    let fn_world = normalize((transpose(frame.view) * vec4<f32>(facet.xyz, 0.0)).xyz);
    let facing = saturate(dot(fn_world, v));

    // Each facet its own brightness, a dark heart facing the eye and a glowing girdle.
    let depth = program_float(P_DEPTH);
    var colour = lit * mix(0.55, 1.5, id);
    colour *= mix(1.0, mix(1.6, 0.35, facing * facing), depth);
    // Fire: a flat hue shift per facet, toward the next primary along.
    let fire = program_float(P_FIRE) * (gem_hash(vec3<f32>(id, 1.0, 7.0)) - 0.5);
    colour = mix(colour, colour.gbr, max(fire, 0.0)) ;
    colour = mix(colour, colour.brg, max(-fire, 0.0));

    // Hard highlights on the facets that face each light's half vector, and flashes.
    var flash = 0.0;
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            continue;
        }
        let incident = incident_light(l, i, n, p.world_position);
        let h = normalize(incident.to_light + v);
        let s = dot(fn_world, h);
        let highlight = glsl_smoothstep(0.93, 0.96, s) * incident.shadow;
        colour += program_vec3(P_SHINE) * incident.colour / PI * highlight;
        flash = max(flash, glsl_smoothstep(0.6, 0.9, s) * incident.shadow);
    }
    let twinkle = pow(saturate(sin(frame.look.y * (1.5 + 2.5 * id) + id * 60.0)), 30.0);
    colour += program_vec3(P_SHINE) * twinkle * flash * program_float(P_SPARKLE) * 2.5;
    return colour;
}
