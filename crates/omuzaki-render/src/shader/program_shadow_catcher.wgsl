// The shadow-catcher program (`program/shadow_catcher.rs`): black, with alpha `opacity * (1 - shadow_visibility)`. Appended
// after `support.wgsl`; the offset P_OPACITY is generated from the declaration. Only the Transparent pass calls
// `program_alpha`; an opaque draw of this material is black.

fn program_albedo(albedo: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    return vec3<f32>(0.0);
}

// Black whatever the lights and the rim add.
fn program_light(lit: vec3<f32>, n: vec3<f32>, v: vec3<f32>, p: ProgramIn) -> vec3<f32> {
    return vec3<f32>(0.0);
}

// "Up" is world +Y: the catcher is the ground. Under several shadowed lights the shadows multiply, as three's
// ShadowMaterial does, so where two overlap the ground is darker. `shadow_visibility` is 1 on a material that does not
// receive shadows, so such a catcher is simply invisible. With no map on this frame (shadows off, or nothing left to
// cast) the maps keep their last matrices and textures, which the lookup would still sample, so the alpha is 0 then.
fn program_alpha(p: ProgramIn, alpha: f32) -> f32 {
    let mask = shadow_mask(p.world_position, vec3<f32>(0.0, 1.0, 0.0));
    if (mask.y == 0.0) {
        return 0.0;
    }
    return program_float(P_OPACITY) * (1.0 - mask.x);
}
