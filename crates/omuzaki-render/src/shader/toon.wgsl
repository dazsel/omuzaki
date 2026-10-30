// Toon shading for the scene passes: the ramp with a tinted shade, the rim, the skin treatment and the lights, ported from
// the prototype's `toon.js` (`RE_Direct_Toon`, the rim hook and `SKIN_OUT`; three.js r155+ light units, so a white
// directional light of intensity 1 gives `albedo / PI` on a surface facing it, design decision D10). The mock
// (`omuzaki-render-mock/src/shade.rs`) lights per triangle with the same ramp, shade, hemisphere and skin maths, and
// `tests/toon.rs` compares the two on cube scenes before tone mapping. It is NOT the reference for point lights (it
// uses `(1 - d / range)^2`, not D10's three.js falloff) or for the rim under an orthographic camera (it takes
// `eye - centroid`; here the camera's view axis is used, as three.js `geometryViewDir` does). Those two are checked
// against an f64 oracle of the formulas instead.
//
// This file follows `common.wgsl` and `environment.wgsl`. `toon_colour` is the whole lighting model as one function of the surface, so the
// transparent pass can light its blended surfaces with the same code; `fs_main` is the Opaque pass's entry point.
//
// Per fragment, in order:
//   irradiance = sum over lights of  colour * mix(shade, 1, ramp(N.L))    (directional, point, spot: the spot's colour
//                                                                          also times its cone, so it keeps the cel look)
//              + mix(ground, sky, 0.5 + 0.5 * N.y)                        (hemisphere: no ramp, "up" is world +Y)
//              + environment_light(N)                                      (image-based light, environment.wgsl: smooth, or
//                                                                           through this ramp when the scene asks for it)
//   colour     = albedo * irradiance / PI + emissive + rim
//   skin       = keep the albedo's hue under coloured light, never fall below `floor * albedo`
//   back rim   = + the stage's back lights on the silhouette, when the scene asks for them (`back_light_rim`)
//
// Colour is linear light and the result is written unclamped to the HDR scene colour; `Resolve` tone maps.

const LUMA: vec3<f32> = vec3<f32>(0.299, 0.587, 0.114);

// ---- the surface ----

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) colour: vec4<f32>,
    @location(2) local: vec3<f32>,
    @location(3) world_position: vec3<f32>,
    @location(4) world_normal: vec3<f32>,
    // The mesh's extra attributes for the shading program (`program/mod.rs`); unused by draws with no program.
    @location(5) extra0: vec4<f32>,
    @location(6) extra1: vec4<f32>,
    @location(7) extra2: vec4<f32>,
    @location(8) extra3: vec4<f32>,
}

// The extra vertex streams, one slot each from vertex buffer 1. A mesh without a stream reads zeros.
struct Extras {
    @location(6) e0: vec4<f32>,
    @location(7) e1: vec4<f32>,
    @location(8) e2: vec4<f32>,
    @location(9) e3: vec4<f32>,
}

@vertex
fn vs_main(v: Vertex, x: Extras, @builtin(instance_index) instance: u32) -> VsOut {
    let posed = pose(v, instances[instance]);
    var out: VsOut;
    out.clip = frame.view_proj * vec4<f32>(posed.world_position, 1.0);
    out.uv = v.uv;
    out.colour = v.colour;
    out.local = v.position;
    out.world_position = posed.world_position;
    out.world_normal = posed.world_normal;
    out.extra0 = x.e0;
    out.extra1 = x.e1;
    out.extra2 = x.e2;
    out.extra3 = x.e3;
    return out;
}

// ---- lighting ----

// The terminator: `smoothstep(shift - softness, shift + softness, N.L)`, and a hard step at `shift` when the softness is
// zero (WGSL's `smoothstep` is undefined when its edges meet). The same ramp serves every light, as in toon.js.
fn terminator(n_dot_l: f32) -> f32 {
    let low = material.toon.x - material.toon.y;
    let high = material.toon.x + material.toon.y;
    if (high <= low) {
        return select(0.0, 1.0, n_dot_l >= low);
    }
    let t = saturate((n_dot_l - low) / (high - low));
    return t * t * (3.0 - 2.0 * t);
}

// How much of a point light's colour reaches distance `d`: three.js `getDistanceAttenuation` with decay 2 and
// `distance = range`, inverse square with a smooth window that reaches zero at `range` (D10: the prototype's candle and
// moonlit rigs use distance 7, decay 2).
fn point_falloff(d: f32, range: f32) -> f32 {
    let window = saturate(1.0 - pow(d / range, 4.0));
    return window * window / max(d * d, 0.01);
}

// How much of a spot light's colour reaches a point seen from the lamp along `to_light` (the unit vector from the point to the lamp): three.js `getSpotAttenuation`, a
// smoothstep of the cosine off the axis between the outer cone and where the penumbra starts. A hard edge (the two meet)
// is a step, as WGSL's `smoothstep` is undefined there.
fn spot_cone(l: Light, to_light: vec3<f32>) -> f32 {
    let outer = l.ground.x;
    let inner = l.ground.y;
    let angle_cos = dot(-to_light, l.axis.xyz);
    if (inner <= outer) {
        return select(0.0, 1.0, angle_cos >= outer);
    }
    let t = saturate((angle_cos - outer) / (inner - outer));
    return t * t * (3.0 - 2.0 * t);
}

// ---- shadows ----

// The shadow maps of the lights that cast (group 2), drawn by the Shadows pass: one layer per map. A map's `view_proj`
// maps world space to its clip space (depth 0..1).
struct ShadowMap {
    view_proj: mat4x4<f32>,
    // x: the casting light's slot plus one (0: this map is off), y: 1 / map size, z: depth bias in clip depth,
    // w: normal bias in metres.
    params: vec4<f32>,
    // x: the spacing of the 3 x 3 comparison taps, in texels.
    kernel: vec4<f32>,
}

// `MAX_SHADOW_MAPS` (render-api `limits`).
const SHADOW_MAPS: u32 = 4u;

struct Shadows {
    maps: array<ShadowMap, SHADOW_MAPS>,
    // The hair shadow (`shadow/map.rs`): x 1 when the hair depth holds this frame's hair, y how far toward the light the
    // march goes and z how far behind a hair surface a point still lies under it, in metres; w its strength.
    hair: vec4<f32>,
}

@group(2) @binding(0) var<uniform> shadows: Shadows;
@group(2) @binding(1) var shadow_maps: texture_depth_2d_array;
@group(2) @binding(2) var shadow_sampler: sampler_comparison;
// The hair alone from the camera, at the internal size, drawn by the Shadows pass: each texel's distance from the eye
// along the view axis, 0 where there is no hair.
@group(2) @binding(3) var hair_distance: texture_2d<f32>;

// How much of the light that map `m` belongs to reaches `position` (normal `n`): 1 lit, 0 in shadow, in between across
// a soft edge. The point is moved along the normal first (the prototype's `normalBias`) and the depth compared with the
// bias taken off, as in three.js. The test is a 3 x 3 grid of bilinear comparisons: the softness of three's
// PCFSoftShadowMap. Outside the map, and on materials that do not receive shadows (faces stay clean), it is 1.
fn shadow_visibility(position: vec3<f32>, n: vec3<f32>, m: u32) -> f32 {
    if ((material.flags.x & FLAG_RECEIVES_SHADOWS) == 0u) {
        return 1.0;
    }
    let map = shadows.maps[m];
    let clip = map.view_proj * vec4<f32>(position + n * map.params.w, 1.0);
    let uv = clip.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    let depth = clip.z - map.params.z;
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || depth > 1.0) {
        return 1.0;
    }
    // Not transformed: the map is addressed by the light's projection of the point, not by a material UV (the
    // material's `uv_transform` does not apply here).
    var sum = 0.0;
    for (var j = -1; j <= 1; j++) {
        for (var i = -1; i <= 1; i++) {
            let tap = uv + vec2<f32>(f32(i), f32(j)) * (map.params.y * map.kernel.x);
            sum += textureSampleCompareLevel(shadow_maps, shadow_sampler, tap, m, depth);
        }
    }
    return sum / 9.0;
}

// The hair shadow (#2528): how much of a light toward `to_light` the hair leaves on `position`, 1 lit, 0 under the
// hair. Shadow maps cannot draw it: the fringe hangs a centimetre or two off the forehead, inside their normal bias.
// So the point marches toward the light through the hair's distance from the camera, and lies under the hair where
// a step passes behind a hair surface by less than the hair's thickness. Only materials that receive it (a face) look.
//
// It is marched once per fragment, toward the first shadow-casting directional light (the key), by
// `begin_hair_shadow` at the top of the lit fragment stages, and `incident_light` reads the result for every casting
// directional light: marching inside `incident_light` would copy the march into each of its many call sites (the
// software rasteriser inlines everything), and the kit's largest programs then overflow its stack.
const HAIR_STEPS: i32 = 12;
const HAIR_BIAS: f32 = 0.0015;

var<private> hair_lit: f32 = 1.0;

fn begin_hair_shadow(position: vec3<f32>) {
    hair_lit = 1.0;
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_DIRECTIONAL && l.colour.w > 0.5) {
            hair_lit = hair_shadow(position, l.vector.xyz);
            return;
        }
    }
}

fn hair_shadow(position: vec3<f32>, to_light: vec3<f32>) -> f32 {
    if ((material.flags.x & FLAG_RECEIVES_HAIR_SHADOW) == 0u || shadows.hair.x == 0.0) {
        return 1.0;
    }
    // The texture's own size, not the frame's: a lane the branch above masks off still runs on a SIMD rasteriser, and
    // the texture is one texel while the hair shadow is off.
    let size = vec2<i32>(textureDimensions(hair_distance));
    var under = 0.0;
    for (var i = 1; i <= HAIR_STEPS; i++) {
        let p = position + to_light * (shadows.hair.y * f32(i) / f32(HAIR_STEPS));
        let clip = frame.view_proj * vec4<f32>(p, 1.0);
        if (clip.w <= 0.0) {
            break;
        }
        let uv = clip.xy / clip.w * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
        if (any(uv < vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0))) {
            break;
        }
        let hair = textureLoad(hair_distance, clamp(vec2<i32>(uv * vec2<f32>(size)), vec2<i32>(0), size - 1), 0).x;
        if (hair <= 0.0) {
            continue;
        }
        // How far behind the hair the step lies, along the view axis (a perspective camera's clip w is that distance).
        let gap = clip.w - hair;
        under = max(under, smoothstep(HAIR_BIAS, HAIR_BIAS * 2.0, gap) * (1.0 - smoothstep(shadows.hair.z * 0.7, shadows.hair.z, gap)));
    }
    return 1.0 - under * shadows.hair.w;
}

// How much of the light number `index` reaches `position`: its map's visibility, or 1 for a light without a map.
fn light_shadow(position: vec3<f32>, n: vec3<f32>, index: u32) -> f32 {
    for (var m = 0u; m < SHADOW_MAPS; m++) {
        if (u32(shadows.maps[m].params.x) == index + 1u) {
            return shadow_visibility(position, n, m);
        }
    }
    return 1.0;
}

// How much of every shadowed light reaches `position`, multiplied (three.js's shadow mask), and whether any map is on.
fn shadow_mask(position: vec3<f32>, n: vec3<f32>) -> vec2<f32> {
    var mask = 1.0;
    var any_on = 0.0;
    for (var m = 0u; m < SHADOW_MAPS; m++) {
        if (shadows.maps[m].params.x != 0.0) {
            mask *= shadow_visibility(position, n, m);
            any_on = 1.0;
        }
    }
    return vec2<f32>(mask, any_on);
}

// What light number `index` (`l`, not a hemisphere light) sends toward a surface point at `position` with normal `n`:
// its colour where it arrives (D10's falloff for point and spot lights, a spot's cone), the unit vector toward it, how
// much of it the shadow lets through (a light with a shadow map; 1 for the rest), and how much the hair leaves (the
// hair shadow, which turns a surface to its shade tone like the terminator instead of taking the light away).
struct Incident {
    colour: vec3<f32>,
    to_light: vec3<f32>,
    shadow: f32,
    hair: f32,
}

fn incident_light(l: Light, index: u32, n: vec3<f32>, position: vec3<f32>) -> Incident {
    var out: Incident;
    out.to_light = l.vector.xyz;
    out.colour = l.colour.rgb;
    out.shadow = 1.0;
    out.hair = 1.0;
    if (l.vector.w != LIGHT_DIRECTIONAL) {
        // Point and spot lights: D10's falloff, and a spot is also confined to its cone (no spot shadows yet).
        let offset = l.vector.xyz - position;
        let distance = length(offset);
        out.to_light = offset / max(distance, 1e-6);
        out.colour = out.colour * point_falloff(distance, l.colour.w);
        if (l.vector.w == LIGHT_SPOT) {
            out.colour = out.colour * spot_cone(l, out.to_light);
        }
    } else {
        out.shadow = light_shadow(position, n, index);
        if (l.colour.w > 0.5) {
            out.hair = hair_lit;
        }
    }
    return out;
}

// The light a surface point at `position` with normal `n` receives from light number `index` (`l`), before the albedo:
// for directional and point lights the colour tinted by the ramp (the shadow side shows `material.shade`, never grey).
// The light that casts shadows has its colour multiplied by `shadow_visibility` before the ramp, as in three.js:
// `colour * shadow * mix(shade, 1, ramp)`.
fn light_irradiance(l: Light, index: u32, n: vec3<f32>, position: vec3<f32>) -> vec3<f32> {
    if (l.vector.w == LIGHT_HEMISPHERE) {
        return mix(l.ground.rgb, l.colour.rgb, 0.5 + 0.5 * n.y);
    }
    let incident = incident_light(l, index, n, position);
    let ramp = terminator(dot(n, incident.to_light)) * incident.hair;
    return incident.colour * incident.shadow * mix(material.shade.rgb, vec3<f32>(1.0), ramp);
}

// The unit vector from a surface point toward the eye. An orthographic camera looks along one axis everywhere.
fn view_direction(position: vec3<f32>) -> vec3<f32> {
    if (frame.camera_position.w > 0.5) {
        return normalize(frame.camera_back.xyz);
    }
    return normalize(frame.camera_position.xyz - position);
}

// toon.js SKIN_OUT: pull the lit colour back toward the albedo's hue by `hue_keep` (so coloured light does not turn
// skin green), then never let it fall below `floor` of the albedo (so shadows do not go muddy). Both knobs travel in
// the material: `emissive.w` is the hue-keep and `toon.w` the floor.
fn skin_adjust(lit: vec3<f32>, albedo: vec3<f32>) -> vec3<f32> {
    let keep_hue = albedo * (dot(lit, LUMA) / max(dot(albedo, LUMA), 1e-3));
    return max(mix(lit, keep_hue, material.emissive.w), albedo * material.toon.w);
}

// The lit colour of a surface: its linear `albedo`, unit normal `n` facing the viewer, and world `position`.
fn toon_colour(albedo: vec3<f32>, n: vec3<f32>, position: vec3<f32>) -> vec3<f32> {
    var irradiance = environment_light(n);
    for (var i = 0u; i < frame.counts.x; i++) {
        irradiance += light_irradiance(frame.lights[i], i, n, position);
    }
    let rim = material.rim.rgb * pow(1.0 - saturate(dot(n, view_direction(position))), material.toon.z) * material.rim.w;
    var colour = albedo * irradiance / PI + material.emissive.rgb + rim;
    if ((material.flags.x & FLAG_SKIN) != 0u) {
        colour = skin_adjust(colour, albedo);
    }
    return colour;
}

// A stage's back light (`Scene::back_light_rim`, `frame.look.z`): light arriving from behind a surface, as the eye sees
// it, edges the silhouette in the light's colour. The toon ramp cannot do this on its own, because it tints both sides
// of a form with every light it reaches. Each spot, point or directional light adds its colour (falloff, cone and shadow
// included) times how far behind the surface it sits from the eye, times the side of the form that faces it, on a crisp
// band at the silhouette (a cel rim, not a soft fresnel glow, which would light the whole of a thin arm); the sum is added to the lit colour after the skin treatment, so a cool rim reads on skin too. 0 (the
// default) skips it, and so does a toon material with no rim of its own (a floor, a backdrop card).
const BACK_RIM_BAND: vec2<f32> = vec2<f32>(0.55, 0.8);

fn back_light_rim(n: vec3<f32>, position: vec3<f32>) -> vec3<f32> {
    let strength = frame.look.z;
    if (strength <= 0.0) {
        return vec3<f32>(0.0);
    }
    let v = view_direction(position);
    let edge = smoothstep(BACK_RIM_BAND.x, BACK_RIM_BAND.y, 1.0 - saturate(dot(n, v)));
    var rim = vec3<f32>(0.0);
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            continue;
        }
        let incident = incident_light(l, i, n, position);
        let behind = saturate(-dot(incident.to_light, v));
        let side = saturate(0.5 + 0.5 * dot(n, incident.to_light));
        rim += incident.colour * incident.shadow * behind * side;
    }
    return rim * edge * strength;
}

// ---- MToon ----
//
// A material with `MTOON_ON` is lit by VRMC_materials_mtoon 1.0 in place of the ramp above, ported from three-vrm's
// `mtoon.frag` (@pixiv/three-vrm-materials-mtoon, MIT) in the same light units, with the spec's GI equalisation as
// UniVRM defines it (three-vrm leaves it out). Per fragment:
//   uv        = the mesh's UV turned about (0.5, 0.5) and scrolled by the scene time, both times the mask's blue channel
//   per light   shading = linearstep(-1 + toony, 1 - toony, N.L + shift) * shadow
//               direct += colour * mix(shade, lit, shading) / PI;  reaching += colour
//   ambient   = mix(E(N), (E(up) + E(down)) / 2, gi)        E: the hemisphere lights and the environment, no ramp
//   indirect  = ambient * lit / PI;  reaching += ambient
//   rim       = (rim colour * (1 - N.V + lift)^fresnel + matcap) * rim texture
//   colour    = direct + indirect + mix(1, reaching / PI, rim lighting mix) * rim + emissive
// Every MToon texture is read in uniform control flow: each read sits behind a test of the material's own flags.

fn mtoon_on() -> bool {
    return (material.mtoon_flags.x & MTOON_ON) != 0u;
}

fn mtoon_has(bit: u32) -> bool {
    return (material.mtoon_flags.x & bit) != 0u;
}

fn mtoon_texture_flags(slot: u32) -> u32 {
    return (material.mtoon_flags.y >> (4u * slot)) & 15u;
}

// The UV every texture of an MToon material reads (through `material_uv`), from the mesh's: turned by
// `rotation * time * mask` about the centre, then scrolled by `scroll * time * mask`. The mask is the blue channel of
// the animation mask texture at the mesh's own UV (1 without one). Any other material reads the mesh's UV unchanged.
fn mtoon_uv(uv: vec2<f32>) -> vec2<f32> {
    if (!mtoon_on()) {
        return uv;
    }
    var mask = 1.0;
    if (mtoon_has(MTOON_MASK_TEXTURE)) {
        let sampled = textureSample(mtoon_mask_texture, mtoon_mask_sampler, material_uv(uv));
        mask = texel(sampled, mtoon_texture_flags(6u)).b;
    }
    let time = frame.look.y * mask;
    let phase = material.mtoon_uv.z * time;
    let c = cos(phase);
    let s = sin(phase);
    let turned = mat2x2<f32>(c, -s, s, c) * (uv - vec2<f32>(0.5)) + vec2<f32>(0.5);
    return turned + material.mtoon_uv.xy * time;
}

// MToon's terminator: `linearstep(-1 + toony, 1 - toony, n_dot_l + shift)` times the shadow, a step when toony is 1.
fn mtoon_shading(n_dot_l: f32, shadow: f32, shift: f32) -> f32 {
    let low = -1.0 + material.mtoon_shading.y;
    let high = 1.0 - material.mtoon_shading.y;
    let x = n_dot_l + shift;
    var shading = select(0.0, 1.0, x >= low);
    if (high > low) {
        shading = saturate((x - low) / (high - low));
    }
    return shading * shadow;
}

// The normal map applied to unit normal `n`: three.js `getTangentFrame` (the tangents from the screen-space
// derivatives of the position and the UV, as three-vrm does for a mesh without tangents), flipped with the face.
// WGSL's framebuffer y runs down where GL's runs up, so both y derivatives are negated to build GL's frame. That frame's
// bitangent points along +v, and glTF's green channel points along -v (its UV origin is the top left), so the map's y is
// negated, as three's GLTFLoader does with `normalScale.y` for derivative tangents.
fn mtoon_normal(n: vec3<f32>, position: vec3<f32>, uv: vec2<f32>, face: f32) -> vec3<f32> {
    let sampled = texel(textureSample(mtoon_normal_texture, mtoon_normal_sampler, uv), mtoon_texture_flags(5u)).xyz;
    let map_scale = material.mtoon_uv.w * vec2<f32>(1.0, -1.0);
    let map_n = vec3<f32>((sampled.xy * 2.0 - 1.0) * map_scale, sampled.z * 2.0 - 1.0);
    let q0 = dpdx(position);
    let q1 = -dpdy(position);
    let st0 = dpdx(uv);
    let st1 = -dpdy(uv);
    let q1perp = cross(q1, n);
    let q0perp = cross(n, q0);
    let t = q1perp * st0.x + q0perp * st1.x;
    let b = q1perp * st0.y + q0perp * st1.y;
    let det = max(dot(t, t), dot(b, b));
    var scale = 0.0;
    if (det > 0.0) {
        scale = inverseSqrt(det) * face;
    }
    return normalize(t * (scale * map_n.x) + b * (scale * map_n.y) + n * map_n.z);
}

// The matcap toward unit normal `n` seen along `v` (toward the eye): three-vrm's sphere mapping in view space.
fn mtoon_matcap(n: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    let to_view = mat3x3<f32>(frame.view[0].xyz, frame.view[1].xyz, frame.view[2].xyz);
    let view_v = normalize(to_view * v);
    let view_n = normalize(to_view * n);
    let x = normalize(vec3<f32>(view_v.z, 0.0, -view_v.x));
    let y = cross(view_v, x);
    let sphere = 0.5 + 0.5 * vec2<f32>(dot(x, view_n), -dot(y, view_n));
    let sampled = textureSample(mtoon_matcap_texture, mtoon_matcap_sampler, sphere);
    return material.mtoon_matcap.rgb * texel(sampled, mtoon_texture_flags(2u)).rgb;
}

// The ambient light on a surface facing `n`: the hemisphere lights and the environment's harmonics (never through the
// kit's ramp), flattened toward the mean of straight up and straight down by the GI equalisation.
fn mtoon_ambient(n: vec3<f32>) -> vec3<f32> {
    var toward = vec3<f32>(0.0);
    var flat = vec3<f32>(0.0);
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            toward += mix(l.ground.rgb, l.colour.rgb, 0.5 + 0.5 * n.y);
            flat += 0.5 * (l.ground.rgb + l.colour.rgb);
        }
    }
    let intensity = environment.params.x;
    if (intensity != 0.0) {
        toward += sh_irradiance(sky_direction(n)) * intensity;
        let up = sh_irradiance(sky_direction(vec3<f32>(0.0, 1.0, 0.0)));
        let down = sh_irradiance(sky_direction(vec3<f32>(0.0, -1.0, 0.0)));
        flat += 0.5 * (up + down) * intensity;
    }
    return mix(toward, flat, material.mtoon_shading.z);
}

// The MToon colour of a surface: its `lit` colour (the albedo: the base colour, texture and vertex colour), the
// animated `uv`, the unit normal `n` facing the viewer (`face` -1 on a back face), the world `position` and the vertex
// colour. Samples first (uniform control flow), then lights.
fn mtoon_colour(lit: vec3<f32>, uv: vec2<f32>, n: vec3<f32>, position: vec3<f32>, vertex_colour: vec4<f32>, face: f32) -> vec3<f32> {
    let texture_uv = material_uv(uv);
    var shade = material.mtoon_shade.rgb;
    if (mtoon_has(MTOON_SHADE_TEXTURE)) {
        shade *= texel(textureSample(mtoon_shade_texture, mtoon_shade_sampler, texture_uv), mtoon_texture_flags(0u)).rgb;
    }
    if ((material.flags.x & FLAG_VERTEX_COLOURS) != 0u) {
        shade *= vertex_colour.rgb;
    }
    var shift = material.mtoon_shading.x;
    if (mtoon_has(MTOON_SHIFT_TEXTURE)) {
        let sampled = textureSample(mtoon_shift_texture, mtoon_shift_sampler, texture_uv);
        shift += texel(sampled, mtoon_texture_flags(1u)).r * material.mtoon_shade.w;
    }
    var normal = n;
    if (mtoon_has(MTOON_NORMAL_TEXTURE)) {
        normal = mtoon_normal(n, position, texture_uv, face);
    }
    var emissive = material.emissive.rgb;
    if (mtoon_has(MTOON_EMISSIVE_TEXTURE)) {
        let sampled = textureSample(mtoon_emissive_texture, mtoon_emissive_sampler, texture_uv);
        emissive *= texel(sampled, mtoon_texture_flags(4u)).rgb;
    }
    var rim_texture = vec3<f32>(1.0);
    if (mtoon_has(MTOON_RIM_TEXTURE)) {
        rim_texture = texel(textureSample(mtoon_rim_texture, mtoon_rim_sampler, texture_uv), mtoon_texture_flags(3u)).rgb;
    }
    // three-vrm's view direction: toward the eye's position, for either projection.
    let v = normalize(frame.camera_position.xyz - position);
    var matcap = vec3<f32>(0.0);
    if (mtoon_has(MTOON_MATCAP_TEXTURE)) {
        matcap = mtoon_matcap(normal, v);
    }

    var direct = vec3<f32>(0.0);
    var reaching = vec3<f32>(0.0);
    for (var i = 0u; i < frame.counts.x; i++) {
        let l = frame.lights[i];
        if (l.vector.w == LIGHT_HEMISPHERE) {
            continue;
        }
        // The shadow is looked up along the surface's own normal, the light's angle taken against the mapped one.
        let incident = incident_light(l, i, n, position);
        let n_dot_l = clamp(dot(normal, incident.to_light), -1.0, 1.0);
        let shading = mtoon_shading(n_dot_l, incident.shadow * incident.hair, shift);
        direct += incident.colour * mix(shade, lit, shading) / PI;
        reaching += incident.colour;
    }
    let ambient = mtoon_ambient(normal);
    let indirect = ambient * lit / PI;
    reaching += ambient;

    let rim_mix = mix(vec3<f32>(1.0), reaching / PI, material.mtoon_shading.w);
    let fresnel = max(saturate(1.0 - dot(v, normal) + material.mtoon_matcap.w), 1e-6);
    let rim = (material.mtoon_rim.rgb * pow(fresnel, material.mtoon_rim.w) + matcap) * rim_texture;
    return direct + indirect + rim_mix * rim + emissive;
}

// ---- the Opaque pass ----

@fragment
fn fs_main(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    // Transformed: the base texture and the UV overlays inside `surface_albedo` read `material_uv(in.uv)` (common.wgsl).
    // The environment cube (`environment_light`, environment.wgsl) is addressed by direction, not UV: not transformed.
    // An MToon material's UV animation moves every texture it reads, the base texture among them.
    // The derivatives a program reads are taken first, while every fragment of the quad still runs.
    let program = ProgramIn(
        array<vec4<f32>, 4>(in.extra0, in.extra1, in.extra2, in.extra3),
        front,
        in.world_position,
        dpdx(in.world_position),
        dpdy(in.world_position),
        dpdx(in.extra0),
        dpdy(in.extra0),
    );
    let uv = mtoon_uv(in.uv);
    let albedo = surface_albedo(uv, in.colour, in.local);
    if (material.flags.z == ALPHA_MASK && albedo.a < material.alpha.x) {
        discard;
    }
    begin_hair_shadow(in.world_position);
    var colour = albedo.rgb;
    if (mtoon_on()) {
        let facing = normalize(in.world_normal);
        let face = select(-1.0, 1.0, front);
        colour = mtoon_colour(albedo.rgb, uv, facing * face, in.world_position, in.colour, face);
        colour += back_light_rim(facing * face, in.world_position);
    } else if (material.flags.y == SHADING_TOON) {
        // The back of a double-sided surface is lit as a face of its own (three.js: `normal *= faceDirection`). A
        // shading program colours the surface before the lights and the lit result after them.
        let facing = normalize(in.world_normal);
        let n = select(-facing, facing, front);
        colour = toon_colour(program_albedo(albedo.rgb, program), n, in.world_position);
        colour = program_light(colour, n, view_direction(in.world_position), program);
        if (material.rim.w > 0.0) {
            colour += back_light_rim(n, in.world_position);
        }
    }
    // Opaque and masked surfaces cover what is behind them completely. Fog fades unlit surfaces too.
    return vec4<f32>(apply_fog(colour, in.world_position), 1.0);
}
