//! The per-frame GPU data every scene pass shares: the frame uniform (camera, lights, look), one record per instance,
//! and the joint matrices. All three are filled from the [`Scene`] by plain functions here, so the CPU side is unit
//! tested without a GPU. The structs mirror `struct Frame`, `struct Light` and `struct Instance` in
//! `shader/common.wgsl`.

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::{Mat3, Mat4, UVec2, Vec2, Vec3};
use omuzaki_render_api::limits::MAX_LIGHTS;
use omuzaki_render_api::{Fog, Instance, Light, Projection, Scene};

/// `Light.vector.w` for each kind (`LIGHT_*` in the shader).
mod light_kind {
    pub(super) const DIRECTIONAL: f32 = 0.0;
    pub(super) const POINT: f32 = 1.0;
    pub(super) const HEMISPHERE: f32 = 2.0;
    pub(super) const SPOT: f32 = 3.0;
}

/// A light, packed. Colours are linear light with the intensity already multiplied in.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
pub(crate) struct LightUniform {
    /// Directional: unit direction toward the light. Point and spot: position. Hemisphere: unused. `w` is the kind.
    vector: [f32; 4],
    /// rgb: colour (hemisphere: sky). `w`: a point or spot light's range, or 1 for a directional light that casts
    /// shadows.
    colour: [f32; 4],
    /// Hemisphere: ground colour. Spot: `x` the cosine of the cone's outer angle, `y` the cosine where the penumbra
    /// starts (equal to `x` for a hard edge).
    ground: [f32; 4],
    /// Spot: the unit axis, from the lamp along the cone.
    axis: [f32; 4],
}

/// The camera, the lights and the scene-wide look.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct FrameUniform {
    view: [[f32; 4]; 4],
    proj: [[f32; 4]; 4],
    view_proj: [[f32; 4]; 4],
    /// xyz: camera position. `w`: 1 for an orthographic camera.
    camera_position: [f32; 4],
    /// xyz: the camera's back axis in world space, the view direction of an orthographic camera.
    camera_back: [f32; 4],
    /// Internal render size: width, height, 1/width, 1/height.
    target_size: [f32; 4],
    lights: [LightUniform; MAX_LIGHTS],
    /// Light count, then the frame index split into low and high words (grain and jitter seeds).
    counts: [u32; 4],
    /// `outline_light`, scene time in seconds, `back_light_rim`.
    look: [f32; 4],
    /// Fog colour in linear light (rgb) and the depth where it starts (`w`).
    fog_colour: [f32; 4],
    /// Fog: `x` the depth where surfaces are fully fogged, `y` 1 if fog is on, 0 if not.
    fog_far: [f32; 4],
}

/// One instance: its transform, the matrix for normals, and its skin.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct InstanceUniform {
    model: [[f32; 4]; 4],
    /// The columns of the normal matrix (inverse transpose of the model's upper 3x3), padded to vec4.
    normal: [[f32; 4]; 3],
    /// First joint in the palette, then flag bits (`INSTANCE_*` in the shader).
    skin: [u32; 4],
}

/// `Instance.skin.y` bit: the vertex shader skins this instance.
pub(super) const INSTANCE_SKINNED: u32 = 1;

/// Packs the camera, lights and look for a frame drawn at `internal` size, with the picture moved by `jitter` pixels
/// (x right, y down; a supersampling round's offset, zero otherwise).
pub(crate) fn frame_uniform(scene: &Scene, internal: UVec2, jitter: Vec2) -> FrameUniform {
    #[allow(clippy::cast_precision_loss)] // render sizes are far below 2^24
    let (width, height) = (internal.x as f32, internal.y as f32);
    let view = scene.camera.view();
    let proj = jittered(scene.camera.projection_matrix(width / height), jitter, Vec2::new(width, height));
    let ortho = matches!(scene.camera.projection, Projection::Orthographic { .. });
    let position = scene.camera.position();
    let back = scene.camera.transform.z_axis.truncate();

    let fog = fog_uniform(scene.fog);
    let mut lights = [LightUniform::default(); MAX_LIGHTS];
    for (slot, light) in lights.iter_mut().zip(&scene.lights) {
        *slot = light_uniform(light);
    }
    // Splitting a u64 into its two words.
    #[allow(clippy::cast_possible_truncation)]
    let [frame_lo, frame_hi] = [scene.frame.index as u32, (scene.frame.index >> 32) as u32];
    FrameUniform {
        view: view.to_cols_array_2d(),
        proj: proj.to_cols_array_2d(),
        view_proj: (proj * view).to_cols_array_2d(),
        camera_position: [position.x, position.y, position.z, f32::from(u8::from(ortho))],
        camera_back: [back.x, back.y, back.z, 0.0],
        target_size: [width, height, 1.0 / width, 1.0 / height],
        lights,
        counts: [u32::try_from(scene.lights.len().min(MAX_LIGHTS)).unwrap_or(0), frame_lo, frame_hi, 0],
        #[allow(clippy::cast_possible_truncation)] // the shader works in f32 seconds
        look: [scene.outline_light, scene.frame.time.0 as f32, scene.back_light_rim, 0.0],
        fog_colour: fog.0,
        fog_far: fog.1,
    }
}

/// `proj` moved by `jitter` pixels of a `size` picture. The shift is applied after the projection in clip space
/// (`x + t·w`), so it is the same sub-pixel shift at every depth, for perspective and orthographic cameras alike.
fn jittered(proj: Mat4, jitter: Vec2, size: Vec2) -> Mat4 {
    if jitter == Vec2::ZERO {
        return proj;
    }
    // Pixels to normalised device units: two per side, and y points up there.
    let shift = Vec2::new(2.0 * jitter.x / size.x, -2.0 * jitter.y / size.y);
    Mat4::from_translation(shift.extend(0.0)) * proj
}

/// The fog as the two vec4s of [`FrameUniform`]: colour and start, then end and the on flag. With no fog the end is 1
/// and the flag 0, so the shader's division is always defined.
fn fog_uniform(fog: Option<Fog>) -> ([f32; 4], [f32; 4]) {
    match fog {
        Some(Fog { colour, near, far }) => {
            let c = colour.to_linear();
            ([c.x, c.y, c.z, near], [far, 1.0, 0.0, 0.0])
        }
        None => ([0.0; 4], [1.0, 0.0, 0.0, 0.0]),
    }
}

fn light_uniform(light: &Light) -> LightUniform {
    let linear = |c: omuzaki_render_api::Colour, intensity: f32| {
        let c = c.to_linear() * intensity;
        [c.x, c.y, c.z]
    };
    match *light {
        Light::Directional { to_light, colour, intensity, casts_shadow } => {
            let d = to_light.normalize_or_zero();
            let [r, g, b] = linear(colour, intensity);
            LightUniform {
                vector: [d.x, d.y, d.z, light_kind::DIRECTIONAL],
                colour: [r, g, b, f32::from(u8::from(casts_shadow))],
                ground: [0.0; 4],
                axis: [0.0; 4],
            }
        }
        Light::Hemisphere { sky, ground, intensity } => {
            let [sr, sg, sb] = linear(sky, intensity);
            let [gr, gg, gb] = linear(ground, intensity);
            LightUniform {
                vector: [0.0, 1.0, 0.0, light_kind::HEMISPHERE],
                colour: [sr, sg, sb, 0.0],
                ground: [gr, gg, gb, 0.0],
                axis: [0.0; 4],
            }
        }
        Light::Point { position, colour, intensity, range } => {
            let [r, g, b] = linear(colour, intensity);
            LightUniform {
                vector: [position.x, position.y, position.z, light_kind::POINT],
                colour: [r, g, b, range],
                ground: [0.0; 4],
                axis: [0.0; 4],
            }
        }
        Light::Spot { position, direction, colour, intensity, range, cone_outer_deg, penumbra, .. } => {
            let [r, g, b] = linear(colour, intensity);
            let a = direction.normalize_or_zero();
            // three.js `coneCos` and `penumbraCos`: the light is full strength inside the second angle's cosine and
            // fades to nothing at the first.
            let outer = cone_outer_deg.to_radians();
            LightUniform {
                vector: [position.x, position.y, position.z, light_kind::SPOT],
                colour: [r, g, b, range],
                ground: [outer.cos(), (outer * (1.0 - penumbra)).cos(), 0.0, 0.0],
                axis: [a.x, a.y, a.z, 0.0],
            }
        }
    }
}

/// Packs one instance. `skinned` says the mesh and the instance both carry skin data.
pub(crate) fn instance_uniform(instance: &Instance, skinned: bool) -> InstanceUniform {
    let model = instance.transform;
    let linear = Mat3::from_mat4(model);
    // A flattened instance (scale 0) has no inverse; its normals are meaningless, so keep them finite.
    let normal = if linear.determinant() == 0.0 { Mat3::IDENTITY } else { linear.inverse().transpose() };
    let column = |v: Vec3| [v.x, v.y, v.z, 0.0];
    let (first_joint, flags) = match instance.skin {
        Some(range) if skinned => (range.start, INSTANCE_SKINNED),
        _ => (0, 0),
    };
    InstanceUniform {
        model: model.to_cols_array_2d(),
        normal: [column(normal.x_axis), column(normal.y_axis), column(normal.z_axis)],
        skin: [first_joint, flags, 0, 0],
    }
}

/// True if the instance's transform flips triangle winding, so its front face must be declared clockwise.
pub(super) fn mirrors(transform: &Mat4) -> bool {
    transform.determinant() < 0.0
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::Vec3;
    use omuzaki_render_api::{Camera, Colour, JointRange, MaterialId, MeshId};

    use super::*;

    #[test]
    fn gpu_structs_have_the_sizes_the_shader_declares() {
        assert_eq!(size_of::<LightUniform>(), 64);
        assert_eq!(size_of::<FrameUniform>(), 3 * 64 + 3 * 16 + MAX_LIGHTS * 64 + 4 * 16);
        assert_eq!(size_of::<InstanceUniform>(), 64 + 3 * 16 + 16);
    }

    #[test]
    fn lights_fold_intensity_into_linear_colour() {
        let mut scene = Scene::new(Camera::default());
        scene.lights = vec![
            Light::Directional {
                to_light: Vec3::new(0.0, 2.0, 0.0),
                colour: Colour::WHITE,
                intensity: 2.0,
                casts_shadow: true,
            },
            Light::Hemisphere { sky: Colour::WHITE, ground: Colour::BLACK, intensity: 0.5 },
            Light::Point { position: Vec3::new(1.0, 2.0, 3.0), colour: Colour::WHITE, intensity: 1.0, range: 4.0 },
        ];
        let frame = frame_uniform(&scene, UVec2::new(64, 32), Vec2::ZERO);
        assert_eq!(frame.counts[0], 3);
        assert_eq!(frame.lights[0].vector, [0.0, 1.0, 0.0, light_kind::DIRECTIONAL], "direction is normalised");
        assert_eq!(frame.lights[0].colour, [2.0, 2.0, 2.0, 1.0], "intensity times white, shadow flag in w");
        assert_eq!(frame.lights[1].colour[0], 0.5);
        assert_eq!(frame.lights[1].ground[0], 0.0);
        assert_eq!(frame.lights[2].vector, [1.0, 2.0, 3.0, light_kind::POINT]);
        assert_eq!(frame.lights[2].colour[3], 4.0, "range");
        assert_eq!(frame.target_size, [64.0, 32.0, 1.0 / 64.0, 1.0 / 32.0]);
    }

    #[test]
    fn a_spot_packs_its_axis_and_the_cosines_of_its_cone() {
        let mut scene = Scene::new(Camera::default());
        let lamp = |penumbra| Light::Spot {
            position: Vec3::new(0.0, 3.0, 0.0),
            direction: Vec3::new(0.0, -4.0, 0.0),
            colour: Colour::WHITE,
            intensity: 2.0,
            range: 7.0,
            cone_outer_deg: 60.0,
            penumbra,
            casts_shadow: true,
        };
        scene.lights = vec![lamp(0.5)];
        let spot = frame_uniform(&scene, UVec2::ONE, Vec2::ZERO).lights[0];
        assert_eq!(spot.vector, [0.0, 3.0, 0.0, light_kind::SPOT]);
        assert_eq!(spot.colour, [2.0, 2.0, 2.0, 7.0], "intensity folded in, range in w");
        assert_eq!(spot.axis, [0.0, -1.0, 0.0, 0.0], "the axis is normalised");
        assert!((spot.ground[0] - 0.5).abs() < 1e-6, "cos 60 degrees");
        assert!((spot.ground[1] - 30.0_f32.to_radians().cos()).abs() < 1e-6, "the penumbra starts at half the angle");
        scene.lights = vec![lamp(0.0)];
        let hard = frame_uniform(&scene, UVec2::ONE, Vec2::ZERO).lights[0];
        assert_eq!(hard.ground[0], hard.ground[1], "no penumbra: the two edges meet");
    }

    #[test]
    fn fog_packs_linear_colour_and_a_defined_range_when_off() {
        let mut scene = Scene::new(Camera::default());
        let off = frame_uniform(&scene, UVec2::ONE, Vec2::ZERO);
        assert_eq!((off.fog_far[0], off.fog_far[1]), (1.0, 0.0), "off: flag 0, a range that divides");
        scene.fog = Some(Fog { colour: Colour::WHITE, near: 2.0, far: 9.0 });
        let on = frame_uniform(&scene, UVec2::ONE, Vec2::ZERO);
        assert_eq!(on.fog_colour, [1.0, 1.0, 1.0, 2.0]);
        assert_eq!((on.fog_far[0], on.fog_far[1]), (9.0, 1.0));
    }

    #[test]
    fn a_one_ulp_fog_range_survives_packing_and_is_full_at_far() {
        let mut scene = Scene::new(Camera::default());
        let far = f32::from_bits(1.0_f32.to_bits() + 1);
        let fog = Fog { colour: Colour::WHITE, near: 1.0, far };
        scene.fog = Some(fog);
        let packed = frame_uniform(&scene, UVec2::ONE, Vec2::ZERO);
        // The shader computes saturate((depth - near) / (far - near)) from exactly these two numbers.
        let (near, far) = (packed.fog_colour[3], packed.fog_far[0]);
        assert_eq!((near, far), (1.0, 1.000_000_1));
        assert_eq!(((far - near) / (far - near)).clamp(0.0, 1.0), 1.0, "the shader's amount at depth = far");
        assert_eq!(fog.amount(far), 1.0);
    }

    #[test]
    fn a_jitter_moves_every_depth_by_the_same_pixels_for_both_projections() {
        let size = UVec2::new(200, 100);
        let pixel = |frame: &FrameUniform, point: Vec3| {
            let clip = Mat4::from_cols_array_2d(&frame.view_proj) * point.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            Vec2::new((ndc.x + 1.0) * 100.0, (1.0 - ndc.y) * 50.0)
        };
        for projection in [Camera::default().projection, Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 }]
        {
            let mut scene = Scene::new(Camera::look_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y, projection));
            scene.camera.projection = projection;
            let still = frame_uniform(&scene, size, Vec2::ZERO);
            let moved = frame_uniform(&scene, size, Vec2::new(0.25, -0.375));
            for point in [Vec3::ZERO, Vec3::new(0.3, -0.2, 1.5), Vec3::new(-0.5, 0.4, -3.0)] {
                let shift = pixel(&moved, point) - pixel(&still, point);
                assert!((shift - Vec2::new(0.25, -0.375)).length() < 1e-3, "{projection:?} at {point}: {shift}");
            }
            assert_eq!(moved.view, still.view, "only the projection moves");
        }
    }

    #[test]
    fn the_frame_index_is_split_into_words() {
        let mut scene = Scene::new(Camera::default());
        scene.frame.index = 0x0000_0007_0000_0003;
        let frame = frame_uniform(&scene, UVec2::ONE, Vec2::ZERO);
        assert_eq!((frame.counts[1], frame.counts[2]), (3, 7));
    }

    #[test]
    fn instances_skin_only_when_mesh_and_instance_agree() {
        let mut inst = Instance::new(MeshId::from_parts(0, 1), MaterialId::from_parts(0, 1), Mat4::IDENTITY);
        assert_eq!(instance_uniform(&inst, true).skin, [0, 0, 0, 0], "no joint range: rigid");
        inst.skin = Some(JointRange { start: 5, len: 2 });
        assert_eq!(instance_uniform(&inst, true).skin, [5, INSTANCE_SKINNED, 0, 0]);
        assert_eq!(instance_uniform(&inst, false).skin, [0, 0, 0, 0], "a rigid mesh ignores the range");
    }

    #[test]
    fn normals_use_the_inverse_transpose_and_mirrors_are_detected() {
        let stretched = Mat4::from_scale(Vec3::new(2.0, 1.0, 1.0));
        let u =
            instance_uniform(&Instance::new(MeshId::from_parts(0, 1), MaterialId::from_parts(0, 1), stretched), false);
        assert_eq!(u.normal[0], [0.5, 0.0, 0.0, 0.0], "a normal along a stretched axis shrinks");
        assert!(mirrors(&Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0))));
        assert!(!mirrors(&Mat4::from_rotation_y(1.0)));
        let flat = Mat4::from_scale(Vec3::new(0.0, 1.0, 1.0));
        let u = instance_uniform(&Instance::new(MeshId::from_parts(0, 1), MaterialId::from_parts(0, 1), flat), false);
        assert!(u.normal.iter().flatten().all(|v| v.is_finite()));
    }
}
