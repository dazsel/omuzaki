// The hair program (`program/hair.rs`): the hooks of the prototype's `parts/hair.js`. Appended after `support.wgsl`;
// the parameter offsets are generated from the declaration. Attributes: extra[0].xy = hair-uv (across -1..1, along 0..1),
// extra[1].xyz = hair-info (strand seed, shade multiplier, polar angle).

// The prototype's sine hash and its smooth value noise: what breaks strands and the ring into locks.
fn hair_hash(n: f32) -> f32 {
    return fract(sin(n) * 43758.5453);
}

fn hair_noise(x: f32) -> f32 {
    let i = floor(x);
    var f = fract(x);
    f = f * f * (3.0 - 2.0 * f);
    return mix(hair_hash(i), hair_hash(i + 1.0), f);
}

// Root-to-tip colour, strand lines running along the lock, darker clump edges, the inner-layer shade and a darker back.
fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let u = p.extra[0].x;
    let v = p.extra[0].y;
    let seed = p.extra[1].x;
    // A mesh without `hair-info` supplies zeros; a shade of exactly 0 would draw it black, so it reads as "unset".
    let shade = select(p.extra[1].y, 1.0, p.extra[1].y <= 0.0);
    var col = mix(program_vec3(P_ROOT), program_vec3(P_MID), glsl_smoothstep(0.0, 0.25, v));
    col = mix(col, program_vec3(P_TIP), glsl_smoothstep(0.55, 1.0, v));
    // Strand lines, gently wavy.
    let w = u * 2.6 + seed * 11.0 + 0.35 * sin(v * 14.0 + seed * 20.0);
    let s1 = hair_noise(w * 3.0);
    let s2 = hair_noise(w * 7.0 + 3.0);
    let lines = glsl_smoothstep(0.62, 0.92, s1) * 0.6 + glsl_smoothstep(0.7, 0.95, s2) * 0.4;
    col = mix(col, program_vec3(P_LINE), lines * program_float(P_STRAND_LINES));
    col *= mix(1.03, 0.97, s2);
    // The edges of each lock a touch darker, so overlapping locks separate.
    col *= mix(1.0, 1.0 - program_float(P_EDGE_DARKENING), glsl_smoothstep(0.55, 1.0, abs(u)));
    // Inner layers sit in the shadow of outer ones.
    col *= shade;
    col *= select(0.80, 1.0, p.front);
    return albedo * col;
}

// The painted angel ring across the crown, broken into comb teeth by the strands, then the pull back toward the hair's
// own cream so coloured light (lanterns orange, moonlight grey-blue) does not change its identity.
fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let u = p.extra[0].x;
    let seed = p.extra[1].x;
    let polar = p.extra[1].z;
    let band = glsl_smoothstep(0.11, 0.0, abs(polar - 0.92 - 0.06 * sin(u * 3.0 + seed * 9.0)));
    let teeth = glsl_smoothstep(0.35, 0.75, hair_noise(u * 6.0 + seed * 17.0));
    let facing = glsl_smoothstep(0.1, 0.6, dot(n, v));
    let ringed = lit + program_vec3(P_RING) * band * teeth * facing * program_float(P_RING_STRENGTH);
    let luma = dot(ringed, vec3<f32>(0.299, 0.587, 0.114));
    return mix(ringed, luma * vec3<f32>(1.05, 0.97, 0.90), program_float(P_KEEP_IDENTITY));
}
