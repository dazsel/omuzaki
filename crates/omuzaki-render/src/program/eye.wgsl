// The eye program (`program/eye.rs`): a real eyeball. The mesh is the outer surface (the sclera and the cornea's bulge);
// the iris is not geometry but a surface inside it, found by refracting the view ray at the cornea, so it sits behind
// the cornea with depth and parallax and the pupil is a hole. Appended after `support.wgsl`; the offsets (P_CENTRE,
// P_FORWARD, P_UP, ...) are generated from the declaration.
//
// The base texture is the eye's atlas: the iris on the left half and the sclera on the right, each a disc (the iris in
// iris radii; the sclera as a polar map of angle from the gaze axis, out to `SCLERA_REACH`).

const SCLERA_REACH: f32 = 1.9198622;   // 110 degrees: the sclera map's rim
const ATLAS_DISC: vec2<f32> = vec2<f32>(0.24, 0.48);
// The iris texture is drawn for this pupil radius (in iris radii); a wider or narrower pupil stretches it.
const DRAWN_PUPIL: f32 = 0.3;
const EYE_F0: f32 = 0.024;             // the tear film's reflectance head-on (n = 1.376)

var<private> eye_albedo: vec3<f32>;
var<private> eye_iris: f32;            // 1 where the view ray reaches the iris, 0 on the sclera
var<private> eye_pupil: f32;           // 1 inside the pupil

struct EyeFrame {
    centre: vec3<f32>,
    right: vec3<f32>,
    up: vec3<f32>,
    forward: vec3<f32>,
}

fn eye_frame() -> EyeFrame {
    let forward = normalize(program_vec3(P_FORWARD));
    let up = normalize(program_vec3(P_UP) - forward * dot(program_vec3(P_UP), forward));
    return EyeFrame(program_vec3(P_CENTRE), cross(up, forward), up, forward);
}

fn eye_local(f: EyeFrame, w: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(w, f.right), dot(w, f.up), dot(w, f.forward));
}

fn eye_world(f: EyeFrame, l: vec3<f32>) -> vec3<f32> {
    return f.right * l.x + f.up * l.y + f.forward * l.z;
}

fn sclera_uv(d: vec3<f32>) -> vec2<f32> {
    let theta = acos(clamp(d.z, -1.0, 1.0));
    let rho = min(theta / SCLERA_REACH, 1.0);
    let phi = atan2(d.y, d.x);
    return vec2<f32>(0.75 + ATLAS_DISC.x * rho * cos(phi), 0.5 - ATLAS_DISC.y * rho * sin(phi));
}

fn iris_uv(q: vec2<f32>) -> vec2<f32> {
    // Pupil dilation: the stroma between the pupil and the rim stretches or bunches; the rim stays put.
    let r = length(q);
    let pupil = program_float(P_PUPIL);
    let drawn = DRAWN_PUPIL + (r - pupil) * (1.0 - DRAWN_PUPIL) / max(1.0 - pupil, 1e-3);
    let scaled = select(q * (drawn / max(r, 1e-6)), q, r >= 1.0);
    let clamped = scaled / max(1.0, length(scaled));
    return vec2<f32>(0.25 + ATLAS_DISC.x * clamped.x, 0.5 - ATLAS_DISC.y * clamped.y);
}

// Where the view ray through local point `p` (heading `d`, local) lands: x, y the atlas UV; z 1 on the iris, 0 on the
// sclera; w 1 inside the pupil.
struct EyeHit {
    uv: vec2<f32>,
    iris: f32,
    pupil: f32,
    q: vec2<f32>,
}

fn eye_hit(p: vec3<f32>, d: vec3<f32>, n: vec3<f32>) -> EyeHit {
    let radius = program_float(P_RADIUS);
    let iris_radius = program_float(P_IRIS_RADIUS);
    var hit: EyeHit;
    hit.iris = 0.0;
    hit.pupil = 0.0;
    hit.q = vec2<f32>(2.0);
    // On the cornea: inside the limbus, in front.
    let limbus = iris_radius * program_float(P_LIMBUS);
    if (p.z > 0.0 && length(p.xy) < limbus) {
        var t = refract(d, n, 1.0 / program_float(P_IOR));
        if (dot(t, t) < 1e-8) {
            t = d;
        }
        // The iris is a shallow cone: its pupil edge sits `bulge` in front of its rim.
        var z = program_float(P_IRIS_DEPTH) * radius;
        var h = p;
        for (var i = 0; i < 2; i++) {
            let s = (z - p.z) / min(t.z, -1e-4);
            h = p + t * s;
            let r = clamp(length(h.xy) / iris_radius, 0.0, 1.0);
            z = (program_float(P_IRIS_DEPTH) + program_float(P_IRIS_BULGE) * (1.0 - r * r)) * radius;
        }
        let q = h.xy / iris_radius;
        let r = length(q);
        hit.q = q;
        let edge = program_float(P_LIMBUS_SOFT);
        hit.iris = 1.0 - smoothstep(1.0 - edge, 1.0 + edge, r);
        hit.pupil = 1.0 - smoothstep(program_float(P_PUPIL) - 0.012, program_float(P_PUPIL) + 0.004, r);
        if (r < 1.0 + edge) {
            hit.uv = iris_uv(q);
            return hit;
        }
        hit.uv = sclera_uv(normalize(h));
        return hit;
    }
    hit.uv = sclera_uv(normalize(p));
    return hit;
}

fn eye_sample(uv: vec2<f32>, dx: vec2<f32>, dy: vec2<f32>) -> vec3<f32> {
    // A jump between the atlas halves (the limbus) would ask for the coarsest level: clamp the footprint.
    let cap = 0.004;
    let gx = dx * min(1.0, cap / max(length(dx), 1e-9));
    let gy = dy * min(1.0, cap / max(length(dy), 1e-9));
    return texel(textureSampleGrad(base_texture, base_sampler, uv, gx, gy), texture_flags(0u)).rgb;
}

fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let f = eye_frame();
    let to_eye = view_direction(p.world_position);
    let d = eye_local(f, -to_eye);
    // The surface normal of the mesh, in the eye's frame: the sphere's or the cornea's (see `program/eye.rs`).
    let local = eye_local(f, p.world_position - f.centre);
    let n = eye_surface_normal(local);
    let hit = eye_hit(local, d, n);
    let hx = eye_hit(eye_local(f, p.world_position + p.world_dx - f.centre), d, n);
    let hy = eye_hit(eye_local(f, p.world_position + p.world_dy - f.centre), d, n);
    var colour = eye_sample(hit.uv, hx.uv - hit.uv, hy.uv - hit.uv);
    if (hit.iris > 0.0 && hit.iris < 1.0) {
        // The limbus: the iris's rim fades into the white seen through the cornea's clear edge.
        let white = eye_sample(sclera_uv(normalize(local)), vec2<f32>(0.0), vec2<f32>(0.0));
        colour = mix(white * program_vec3(P_LIMBUS_TINT), colour, hit.iris);
    }
    colour = mix(colour, program_vec3(P_PUPIL_COLOUR), hit.pupil);
    eye_albedo = colour;
    eye_iris = hit.iris;
    eye_pupil = hit.pupil;
    return colour;
}

// The outer surface's normal at local point `p`: the cornea's sphere in front of the limbus, the eyeball's elsewhere.
fn eye_surface_normal(p: vec3<f32>) -> vec3<f32> {
    let radius = program_float(P_RADIUS);
    let iris_radius = program_float(P_IRIS_RADIUS) * program_float(P_LIMBUS);
    if (p.z > 0.0 && length(p.xy) < iris_radius) {
        let apex = radius * (1.0 + program_float(P_CORNEA_RISE));
        let limbus_z = sqrt(max(radius * radius - iris_radius * iris_radius, 0.0));
        let sag = apex - limbus_z;
        let rc = (iris_radius * iris_radius + sag * sag) / (2.0 * sag);
        return normalize(p - vec3<f32>(0.0, 0.0, apex - rc));
    }
    return normalize(p);
}

fn eye_ggx(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32) -> f32 {
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

fn eye_fresnel(cos_theta: f32) -> f32 {
    return EYE_F0 + (1.0 - EYE_F0) * pow(1.0 - saturate(cos_theta), 5.0);
}

fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    let f = eye_frame();
    // The iris faces forward (tilted a little by its cone); the sclera faces out of the ball.
    let iris_n = normalize(f.forward + 0.25 * eye_world(f, vec3<f32>(eye_local(f, n).xy, 0.0)));
    let wrap = program_float(P_SCATTER);
    let scatter = program_vec3(P_SCATTER_COLOUR);
    var diffuse = vec3<f32>(0.0);
    var specular = vec3<f32>(0.0);
    let roughness = mix(program_float(P_SCLERA_ROUGHNESS), program_float(P_CORNEA_ROUGHNESS), eye_iris);
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            diffuse += mix(l.ground.rgb, l.colour.rgb, 0.5 + 0.5 * n.y);
            continue;
        }
        let incident = incident_light(l, i, n, p.world_position);
        let light = incident.colour * incident.shadow;
        // The sclera scatters: light wraps past the terminator, reddened.
        let ndl = dot(n, incident.to_light);
        let wrapped = saturate((ndl + wrap) / (1.0 + wrap));
        let sclera = wrapped * mix(scatter, vec3<f32>(1.0), saturate(ndl * 2.0 + 0.3));
        // The iris is lit through the cornea, which gathers the light onto the far side of the iris (the caustic).
        let l_local = eye_local(f, incident.to_light);
        let far_side = saturate(-dot(normalize(eye_local(f, iris_n).xy + vec2<f32>(1e-5)), l_local.xy));
        let iris = saturate(dot(iris_n, incident.to_light) * 0.7 + 0.3)
            * (1.0 + program_float(P_CAUSTIC) * far_side * length(l_local.xy));
        diffuse += light * mix(sclera, vec3<f32>(iris), eye_iris);
        specular += light * eye_ggx(n, v, incident.to_light, roughness) * eye_fresnel(dot(normalize(v + incident.to_light), v));
    }
    diffuse += environment_light(mix(n, iris_n, eye_iris));
    let reflected = reflect(-v, n);
    specular += environment_specular(reflected, roughness) * eye_fresnel(dot(n, v));
    return eye_albedo * diffuse / PI + specular * program_float(P_GLOSS);
}
