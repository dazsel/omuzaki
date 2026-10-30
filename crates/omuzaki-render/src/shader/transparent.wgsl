// The Transparent pass's entry points. They are appended to the module `program::opaque_source` builds (common,
// toon, support and the material's program hooks), so they light a blended surface with the same `toon_colour` and
// program hooks as `fs_main` does for an opaque one and reuse `vs_main`.
//
// The program's third hook, `program_alpha`, sets the alpha; only this pass calls it (an opaque surface has none).
//
// `fs_blend` draws a sorted surface. Its output is straight (not premultiplied) RGBA: the pipeline's blend state does
// `colour * alpha + behind * (1 - alpha)` and `alpha + behind_alpha * (1 - alpha)`, which keeps the scene colour
// premultiplied like the opaque pass leaves it.
//
// `fs_order_independent` draws an order-independent surface (weighted blended order-independent transparency, McGuire
// and Bavoil 2013) into two targets: the sum of each layer's premultiplied colour and alpha times a weight that favours
// near, opaque layers, and (blended `behind * (1 - alpha)`) the product of what each layer lets through. Neither sum
// depends on the order the layers arrive in; `oit.wgsl` composites them.

// The lit, fogged colour and the alpha of a blended surface, straight.
fn blended(in: VsOut, front: bool) -> vec4<f32> {
    let uv = mtoon_uv(in.uv);
    let albedo = surface_albedo(uv, in.colour, in.local);
    let program = ProgramIn(
        array<vec4<f32>, 4>(in.extra0, in.extra1, in.extra2, in.extra3),
        front,
        in.world_position,
        dpdx(in.world_position),
        dpdy(in.world_position),
        dpdx(in.extra0),
        dpdy(in.extra0),
    );
    var colour = albedo.rgb;
    if (mtoon_on()) {
        let facing = normalize(in.world_normal);
        let face = select(-1.0, 1.0, front);
        colour = mtoon_colour(albedo.rgb, uv, facing * face, in.world_position, in.colour, face);
        colour += back_light_rim(facing * face, in.world_position);
    } else if (material.flags.y == SHADING_TOON) {
        let facing = normalize(in.world_normal);
        let n = select(-facing, facing, front);
        colour = toon_colour(program_albedo(albedo.rgb, program), n, in.world_position);
        colour = program_light(colour, n, view_direction(in.world_position), program);
        if (material.rim.w > 0.0) {
            colour += back_light_rim(n, in.world_position);
        }
    }
    // Fog fades the surface colour only; the alpha still decides how much of it covers what is behind.
    return vec4<f32>(apply_fog(colour, in.world_position), program_alpha(program, albedo.a));
}

@fragment
fn fs_blend(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    return blended(in, front);
}

struct OrderIndependent {
    @location(0) sum: vec4<f32>,
    @location(1) reveal: vec4<f32>,
}

@fragment
fn fs_order_independent(in: VsOut, @builtin(front_facing) front: bool) -> OrderIndependent {
    let c = blended(in, front);
    let a = saturate(c.a);
    // The paper's weight for a depth buffer (its equation for `gl_FragCoord.z`): near, opaque layers count most, and
    // the clamp keeps the sum inside half-float range.
    let w = clamp(pow(min(1.0, a * 10.0) + 0.01, 3.0) * 1e8 * pow(1.0 - in.clip.z * 0.9, 3.0), 1e-2, 3e3);
    var out: OrderIndependent;
    out.sum = vec4<f32>(c.rgb * a, a) * w;
    out.reveal = vec4<f32>(a);
    return out;
}
