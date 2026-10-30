// The Transparent pass's entry point. It is appended to the module `program::opaque_source` builds (common,
// toon, support and the material's program hooks), so it lights a blended surface with the same `toon_colour` and
// program hooks as `fs_main` does for an opaque one and reuses `vs_main`.
//
// The program's third hook, `program_alpha`, sets the alpha; only this pass calls it (an opaque surface has none).
//
// The output is straight (not premultiplied) RGBA: the pipeline's blend state does `colour * alpha + behind * (1 - alpha)`
// and `alpha + behind_alpha * (1 - alpha)`, which keeps the scene colour premultiplied like the opaque pass leaves it.

@fragment
fn fs_blend(in: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
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
