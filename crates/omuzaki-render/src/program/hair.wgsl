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
    hair_albedo = albedo * col;
    return hair_albedo;
}

// The surface colour `program_albedo` made, streak tints and strand texture included: `keep-identity` pulls the lit
// colour back toward its hue.
var<private> hair_albedo: vec3<f32>;

// The angel ring across the crown, broken into comb teeth by the strands, then the pull back toward the hair's own
// cream so coloured light (lanterns orange, moonlight grey-blue) does not change its identity.
//
// The ring is painted at a fixed polar angle (the prototype's), or follows the strands (`ring-follow`): a shifted
// Kajiya-Kay lobe on the direction each lock runs (`program_gradient` of `hair-uv.y`), lit from the eye, so the band
// bends with every lock yet lines up across neighbours where they run alike. `ring-shift` tilts the strands toward the
// normal, which slides the band up or down the head: the kit's -0.75 puts it where the painted ring is, seen from the
// front. A surface whose `hair-uv` doesn't run along it (a cap's end, a mesh without the attribute) keeps the painted one.
fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let u = p.extra[0].x;
    let seed = p.extra[1].x;
    let polar = p.extra[1].z;
    // The band's half width, how far each lock's copy of it wanders, and how much the comb teeth break it: the
    // prototype's 0.11, 0.06 and 1 draw a soft band that steps from lock to lock; a narrow, steady one reads as one arc.
    let shape = hair_ring_shape(u, seed);
    let wobble = program_float(P_RING_WOBBLE) * sin(u * 3.0 + seed * 9.0);
    let width = program_float(P_RING_WIDTH) * shape.y;
    let painted = glsl_smoothstep(width, 0.0, abs(polar - 0.92 - wobble - shape.x));
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
    let lobe = glsl_smoothstep(sin(width), 0.0, abs(dot(tilted, level) + wobble + RING_LOBE_DOWN * shape.x)) * crown;
    let follow = program_float(P_RING_FOLLOW) * select(0.0, 1.0, dot(along, along) > 0.5);
    let band = mix(painted, lobe, follow);
    let comb = glsl_smoothstep(0.35, 0.75, hair_noise(u * 6.0 + seed * 17.0));
    let teeth = mix(1.0, comb, program_float(P_RING_TEETH));
    let facing = glsl_smoothstep(0.1, 0.6, dot(n, v));
    let deep = lit * hair_deep_shade(n, p);
    let ringed = deep + program_vec3(P_RING) * band * teeth * shape.z * facing * program_float(P_RING_STRENGTH);
    let luma = dot(ringed, LUMA);
    let kept = mix(ringed, luma * hair_identity(), program_float(P_KEEP_IDENTITY));
    // The highlights and the back lights' rim go on after keep-identity: they are the lights' colour, not the hair's.
    return kept + hair_highlights(n, v, p) + hair_back_light(n, v, p);
}

// The hue `keep-identity` keeps. The prototype's is a fixed cream, `(1.05, 0.97, 0.90)`: right for the kit's cream hair,
// it pulled every other colour toward grey. With `keep-hue` it is the surface's own colour scaled to unit luma (each
// channel at most 2.2, so a pure hue doesn't blow out), warmed a touch as the prototype's is; a black surface keeps the
// cream.
fn hair_identity() -> vec3<f32> {
    let cream = vec3<f32>(1.05, 0.97, 0.90);
    let luma = dot(hair_albedo, LUMA);
    let own = select(cream, min(hair_albedo / max(luma, 1e-4), vec3<f32>(2.2)) * vec3<f32>(1.02, 1.0, 0.98), luma > 1e-3);
    return mix(cream, own, program_float(P_KEEP_HUE));
}

// Which way the strand-following ring's coordinate runs down the head, so a shape's drop moves both rings the same way.
const RING_LOBE_DOWN: f32 = -1.0;

// The painted highlight's shape across each lock (`ring-shape`, `ring-shape-depth`): how far the band's centre moves
// down the head (`x`, radians of polar angle), how wide it is against `ring-width` (`y`) and how strong (`z`), at
// `u` across the lock (`-1..1`).
//
// - 0, a band: one level edge, as before.
// - 1, a zig-zag: two teeth hang from each lock, a W whose ends meet the neighbours' at the band's height.
// - 2, a streak: short strokes along the strands, two per lock, taller than the band is wide.
// - 3, a crescent: each lock's piece is widest in its middle and narrows to points that curl up at its sides.
fn hair_ring_shape(u: f32, seed: f32) -> vec3<f32> {
    let kind = u32(round(program_float(P_RING_SHAPE)));
    let depth = program_float(P_RING_SHAPE_DEPTH);
    let width = program_float(P_RING_WIDTH);
    let a = clamp(abs(u), 0.0, 1.0);
    if (kind == 1u) {
        let teeth = 1.0 - abs(a * 2.0 - 1.0);
        return vec3<f32>(depth * 3.0 * width * teeth, 1.0, 1.0);
    }
    if (kind == 2u) {
        let stroke = abs(fract(u * 1.0 + seed * 7.0) - 0.5);
        let mask = glsl_smoothstep(0.22, 0.08, stroke);
        return vec3<f32>(0.0, 1.0 + 4.0 * depth, mask);
    }
    if (kind == 3u) {
        return vec3<f32>(-depth * 2.5 * width * a * a, max(1.0 - a * a, 0.08), 1.0);
    }
    return vec3<f32>(0.0, 1.0, 1.0);
}

// The deeper second tone of the shadow (`shade-deep`, `shade-deep-at`): where the lights' share of the surface turns
// further from them than `shade-deep-at` (a value of N.L) or falls in a cast shadow, the lit colour is multiplied by
// `shade-deep`, so a lock's underside and the hair under a fringe read darker and cooler than the first shadow tone.
// The lights are weighted by their brightness; white (or no lights) leaves the colour as it is.
fn hair_deep_shade(n: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let tone = program_vec3(P_SHADE_DEEP);
    if (all(tone >= vec3<f32>(0.999))) {
        return vec3<f32>(1.0);
    }
    let at = program_float(P_SHADE_DEEP_AT);
    var lit = 0.0;
    var total = 0.0;
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            continue;
        }
        let incident = incident_light(l, i, n, p.world_position);
        let weight = dot(incident.colour, LUMA);
        total += weight;
        lit += weight * incident.shadow * glsl_smoothstep(at - 0.06, at + 0.06, dot(n, incident.to_light));
    }
    let share = select(1.0, lit / total, total > 1e-4);
    return mix(tone, vec3<f32>(1.0), share);
}

// The per-light highlights (#2521): two shifted Kajiya-Kay lobes on the strand direction (`program_gradient` of
// `hair-uv.y`, root to tip), as Scheuermann's hair shading draws them. The primary is the light's colour times
// `highlight` at `highlight-strength`, `highlight-width` wide (the sine of the angle between the strand and the half
// vector), tilted along the strand by `highlight-shift`; the secondary sits a quarter further toward the tip, twice as
// wide, tinted by the hair's own colour (light that went through the strand) at `highlight-secondary`, and breaks up
// along the strands, as the glints of a real secondary do. Each strand of the texture shifts both by its own amount
// (`highlight-jitter`), so the edge of the highlight is combed rather than ruled. `highlight-cel` sharpens the lobes'
// falloff into a crisp anime band (1) or leaves them soft (0). Only the side of a lock that faces the light shows
// them, through its cast shadows; the lock's occlusion (`hair-info.y`) dims them.
fn hair_highlights(n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let primary = program_float(P_HIGHLIGHT_STRENGTH);
    let secondary = program_float(P_HIGHLIGHT_SECONDARY);
    if (primary <= 0.0 && secondary <= 0.0) {
        return vec3<f32>(0.0);
    }
    let along = program_gradient(p, n, p.extra0_dx.y, p.extra0_dy.y);
    if (dot(along, along) < 0.5) {
        return vec3<f32>(0.0);
    }
    let u = p.extra[0].x;
    let seed = p.extra[1].x;
    let shade = select(p.extra[1].y, 1.0, p.extra[1].y <= 0.0);
    // One shift per strand of the texture: a strand is a slice across the lock.
    let strand = floor((u * 0.5 + 0.5) * 24.0);
    let jitter = (hair_hash(strand * 12.9898 + seed * 78.233) - 0.5) * program_float(P_HIGHLIGHT_JITTER) * 0.3;
    let shift = program_float(P_HIGHLIGHT_SHIFT) + jitter;
    let t1 = normalize(along + n * shift);
    let t2 = normalize(along + n * (shift + 0.25));
    let width = max(program_float(P_HIGHLIGHT_WIDTH), 1e-3);
    let cel = program_float(P_HIGHLIGHT_CEL);
    let glints = mix(0.35, 1.0, hair_noise(p.extra[0].y * 40.0 + strand * 3.1 + seed * 13.0));
    let tint = program_vec3(P_HIGHLIGHT);
    let own = min(hair_albedo / max(dot(hair_albedo, LUMA), 1e-3), vec3<f32>(2.0));
    var sum = vec3<f32>(0.0);
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            continue;
        }
        let incident = incident_light(l, i, n, p.world_position);
        let h = normalize(incident.to_light + v);
        let facing = glsl_smoothstep(-0.05, 0.35, dot(n, incident.to_light));
        let one = hair_lobe(abs(dot(t1, h)), width, cel);
        let two = hair_lobe(abs(dot(t2, h)), 2.0 * width, cel) * glints;
        let light = incident.colour * incident.shadow * facing / PI;
        sum += light * (tint * one * primary + own * hair_albedo * two * secondary);
    }
    return sum * shade * shade;
}

// A highlight lobe at `x`, the cosine between the strand and the half vector (its sine's complement: 0 where the
// strand stands square to the half vector), `width` wide: a smooth bell, or with `cel` a crisp band with a narrow soft
// edge.
fn hair_lobe(x: f32, width: f32, cel: f32) -> f32 {
    let soft = glsl_smoothstep(width, 0.0, x);
    let crisp = glsl_smoothstep(width * 0.62, width * 0.48, x);
    return mix(soft * soft, crisp, cel);
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
