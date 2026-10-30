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

var<private> hair_albedo: vec3<f32>;

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
    hair_albedo = albedo * col;
    return hair_albedo;
}

// The angel ring across the crown, broken into comb teeth by the strands, then the pull back toward the hair's own
// cream so coloured light (lanterns orange, moonlight grey-blue) does not change its identity.
//
// The ring is painted at a fixed polar angle (the prototype's), or follows the strands (`ring-follow`): a shifted
// Kajiya-Kay lobe on the direction each lock runs (`program_gradient` of `hair-uv.y`), lit from the eye, so the band
// bends with every lock yet lines up across neighbours where they run alike. `ring-shift` tilts the strands toward the
// normal, which slides the band up or down the head: the kit's -0.75 puts it where the painted ring is, seen from the
// front. A surface whose `hair-uv` doesn't run along it (a cap's end, a mesh without the attribute) keeps the painted one.
fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let style = program_float(P_STYLE);
    let cel = hair_cel(lit, n, v, p);
    if (style >= 1.0) {
        return cel;
    }
    return mix(hair_physical(n, v, p), cel, style);
}

// The physical end of the style axis (#2607): strands lit as fibres. Kajiya-Kay diffuse (the sine to the strand)
// softened toward Lambert, and Scheuermann's two shifted specular lobes along the strand, a white primary toward the
// tip and a wider secondary tinted by the hair's colour toward the root, broken up per lock by the strand noise.
fn hair_physical(n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let u = p.extra[0].x;
    let seed = p.extra[1].x;
    let along = program_gradient(p, n, p.extra0_dx.y, p.extra0_dy.y);
    // Where `hair-uv` doesn't run along the surface, the strands are taken to fall down it.
    let fallback = normalize(vec3<f32>(0.0, -1.0, 0.0) - n * dot(vec3<f32>(0.0, -1.0, 0.0), n) + vec3<f32>(1e-4, 0.0, 0.0));
    let t = select(fallback, along, dot(along, along) > 0.5);
    let shine = program_float(P_STRAND_SHINE);
    let gloss = program_float(P_STRAND_GLOSS);
    let jitter = 0.15 * (hair_noise(u * 9.0 + seed * 13.0) - 0.5);
    let t1 = normalize(t + n * (-0.1 + jitter));
    let t2 = normalize(t + n * (0.12 + jitter));
    let e1 = mix(20.0, 400.0, gloss);
    let e2 = 0.25 * e1;
    let glint = mix(0.4, 1.0, hair_noise(u * 23.0 + seed * 5.0));
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
        let tl = dot(t, to_light);
        let kajiya = sqrt(max(1.0 - tl * tl, 0.0));
        let lambert = saturate(dot(n, to_light) * 0.7 + 0.3);
        let light = incident.colour * incident.shadow;
        diffuse += light * mix(kajiya, lambert, 0.6);
        let h = normalize(to_light + v);
        let s1 = pow(sqrt(max(1.0 - pow(dot(t1, h), 2.0), 0.0)), e1);
        let s2 = pow(sqrt(max(1.0 - pow(dot(t2, h), 2.0), 0.0)), e2);
        let facing = saturate(dot(n, to_light) * 2.0 + 0.5);
        specular += light * facing * (vec3<f32>(s1) * 0.6 + hair_albedo * s2 * glint) * shine;
    }
    diffuse += environment_light(n);
    return hair_albedo * diffuse / PI + specular + hair_back_light(n, v, p);
}

fn hair_cel(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let u = p.extra[0].x;
    let seed = p.extra[1].x;
    let polar = p.extra[1].z;
    // The band's half width, how far each lock's copy of it wanders, and how much the comb teeth break it: the
    // prototype's 0.11, 0.06 and 1 draw a soft band that steps from lock to lock; a narrow, steady one reads as one arc.
    let wobble = program_float(P_RING_WOBBLE) * sin(u * 3.0 + seed * 9.0);
    let width = program_float(P_RING_WIDTH);
    let painted = glsl_smoothstep(width, 0.0, abs(polar - 0.92 - wobble));
    let along = program_gradient(p, n, p.extra0_dx.y, p.extra0_dy.y);
    let tilted_raw = along + n * program_float(P_RING_SHIFT);
    let tilted_len = length(tilted_raw);
    let tilted = select(vec3<f32>(0.0), tilted_raw / tilted_len, tilted_len > 1e-6);
    // Where the tilted strand stands square to the eye's level direction (the view with its height taken out, so the
    // band keeps its place on the head as the camera rises or drops, as a drawn ring does), within the same angle as
    // the painted band's width.
    let level_raw = vec3<f32>(v.x, 0.0, v.z);
    let level_len = length(level_raw);
    let level = select(v, level_raw / level_len, level_len > 1e-4);
    // Only over the head, as a drawn ring is: hair hanging below it would catch stray glints where it curls square to
    // the eye.
    let crown = glsl_smoothstep(1.7, 1.3, polar);
    let lobe = glsl_smoothstep(sin(width), 0.0, abs(dot(tilted, level) + wobble)) * crown;
    let follow = program_float(P_RING_FOLLOW) * select(0.0, 1.0, dot(along, along) > 0.5);
    let band = mix(painted, lobe, follow);
    let comb = glsl_smoothstep(0.35, 0.75, hair_noise(u * 6.0 + seed * 17.0));
    let teeth = mix(1.0, comb, program_float(P_RING_TEETH));
    let facing = glsl_smoothstep(0.1, 0.6, dot(n, v));
    let ringed = lit + program_vec3(P_RING) * band * teeth * facing * program_float(P_RING_STRENGTH);
    let luma = dot(ringed, vec3<f32>(0.299, 0.587, 0.114));
    let kept = mix(ringed, luma * vec3<f32>(1.05, 0.97, 0.90), program_float(P_KEEP_IDENTITY));
    // The back lights' rim goes on after keep-identity: it is the light's colour, not the hair's.
    return kept + hair_back_light(n, v, p);
}

// The hair's own back rim (`back-light`): every spot, point or directional light behind the hair, as the eye sees it,
// adds its colour on a band at the silhouette as wide as the rim (`rim-width`), on the side of the lock that faces it,
// so coloured kickers outline the locks. 0 skips it, and so does a scene that asks for the back rim on every surface
// (`frame.look.z`), which the hair gets with the rest.
fn hair_back_light(n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let strength = program_float(P_BACK_LIGHT);
    if (strength <= 0.0 || frame.look.z > 0.0) {
        return vec3<f32>(0.0);
    }
    let width = max(program_float(P_RIM_WIDTH), 0.1);
    let edge = glsl_smoothstep(1.0 - width, 1.0 - 0.35 * width, 1.0 - saturate(dot(n, v)));
    var rim = vec3<f32>(0.0);
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            continue;
        }
        let incident = incident_light(l, i, n, p.world_position);
        let behind = saturate(-dot(incident.to_light, v));
        let side = saturate(0.5 + 0.5 * dot(n, incident.to_light));
        rim += incident.colour * incident.shadow * behind * side;
    }
    return rim * edge * strength;
}
