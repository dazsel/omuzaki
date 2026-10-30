//! The eye's close-up: one design built on a renderer, lit and framed as a photographer's macro shot of an eye, so the
//! lab test that tunes the eye against its reference and the `render.eye` op an agent calls draw the same picture.
//!
//! The light is a studio: a dim warm room with one bright softbox above the camera (its reflection is the catchlight on
//! the cornea), a key light with shadows and a soft fill. Occlusion is at the eye's scale (the crease, the corners, the
//! lids on the eye). The macro view focuses on the iris with a shallow depth of field.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat3, Mat4, UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render_api::{
    AlphaMode, Background, Camera, DepthOfField, EnvironmentDesc, EnvironmentUse, HairShadow, Instance, Light,
    Material, Occlusion, ProgramParams, Projection, RenderError, Renderer, ResourceRef, Scene, Shading, TextureDesc,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{EyeDesign, EyeFrame, eye_params, lash_params, meshes, skin, skin_params, textures};

/// Where the close-up looks from.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum View {
    /// Three-quarter from the temple's side and a little below, the eye looking toward the camera and up: the inner
    /// corner tucks behind the eyeball's bulge, and the iris is in focus with the lashes and skin falling out.
    #[default]
    Macro,
    /// Straight on, the eye looking into the camera, everything in focus: how the opening and the iris read.
    Front,
}

impl View {
    /// The camera's position, the point it looks at, its vertical field of view in degrees, and the gaze.
    fn shot(self) -> (Vec3, Vec3, f32, Vec3) {
        match self {
            Self::Macro => {
                (Vec3::new(-0.024, -0.014, 0.048), Vec3::new(0.002, 0.0005, 0.010), 32.0, Vec3::new(-0.52, -0.07, 0.85))
            }
            Self::Front => {
                (Vec3::new(0.0, 0.001, 0.07), Vec3::new(0.0, 0.0005, 0.008), 27.0, Vec3::new(0.0, 0.015, 1.0))
            }
        }
    }
}

/// A close-up built on a renderer: its scene and the resources it made, which [`Closeup::release`] gives back.
#[derive(Debug)]
pub struct Closeup {
    scene: Scene,
    /// Every resource made for the scene, in the order made; released in reverse, so materials go before the textures
    /// they sample.
    made: Vec<ResourceRef>,
}

impl Closeup {
    /// Builds `design` on `renderer` and frames it from `view`. `texture` is the side of the iris atlas and of the skin
    /// texture (2048 for a macro still, 1024 for a quick look).
    ///
    /// # Errors
    /// What the renderer returns when it can't make a resource; everything made before the failure is released.
    pub fn build(
        renderer: &mut dyn Renderer,
        design: &EyeDesign,
        view: View,
        texture: u32,
    ) -> Result<Self, RenderError> {
        let mut made = Vec::new();
        match build(renderer, design, view, texture, &mut made) {
            Ok(scene) => Ok(Self { scene, made }),
            Err(error) => {
                // The build's own failure is the one to report; a release refused on top of it adds nothing to fix.
                let _ = release(renderer, made);
                Err(error)
            }
        }
    }

    /// The scene to render.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Gives back every resource the close-up made.
    ///
    /// # Errors
    /// The first release the renderer refuses; the rest are still released.
    pub fn release(self, renderer: &mut dyn Renderer) -> Result<(), RenderError> {
        release(renderer, self.made)
    }
}

/// Releases `made` in reverse order, returning the first refusal.
fn release(renderer: &mut dyn Renderer, made: Vec<ResourceRef>) -> Result<(), RenderError> {
    let mut first = Ok(());
    for resource in made.into_iter().rev() {
        if let Err(error) = renderer.release(resource)
            && first.is_ok()
        {
            first = Err(error);
        }
    }
    first
}

fn program(name: &str, params: ProgramParams) -> Result<Shading, RenderError> {
    let name = Slug::new(name).map_err(|e| RenderError::invalid("shading program", e.to_string()))?;
    Ok(Shading::Program { name, params })
}

fn build(
    renderer: &mut dyn Renderer,
    design: &EyeDesign,
    view: View,
    texture: u32,
    made: &mut Vec<ResourceRef>,
) -> Result<Scene, RenderError> {
    let (camera_at, look, fov, gaze) = view.shot();
    let forward = gaze.normalize();
    let frame = EyeFrame { centre: Vec3::ZERO, forward, up: Vec3::Y };
    let built = meshes(design, camera_at, forward);

    let atlas = textures::atlas(design, texture);
    let atlas_tex =
        renderer.create_texture(&TextureDesc::colour(UVec2::new(atlas.width, atlas.height)), Some(&atlas.rgba))?;
    made.push(atlas_tex.into());
    let skin = skin::texture(design, texture);
    let skin_tex =
        renderer.create_texture(&TextureDesc::colour(UVec2::new(skin.width, skin.height)), Some(&skin.to_srgb()))?;
    made.push(skin_tex.into());

    let white = Colour::hex(0x00ff_ffff);
    let eye = Material {
        base_texture: Some(atlas_tex),
        shading: program("eye", eye_params(design, frame))?,
        hair_shadow: HairShadow::RECEIVES,
        ..Material::toon(white)
    };
    let skin = Material {
        base_texture: Some(skin_tex),
        shading: program("skin", skin_params(design))?,
        hair_shadow: HairShadow::RECEIVES,
        ..Material::toon(white)
    };
    let lashes = Material {
        shading: program("skin", lash_params())?,
        double_sided: true,
        ..Material::toon(design.lashes.colour)
    };
    let shell =
        Material { alpha: AlphaMode::Blend, vertex_colours: true, receives_shadows: false, ..Material::unlit(white) };

    let right = frame.up.cross(forward).normalize();
    let up = forward.cross(right);
    let turned = Mat4::from_mat3(Mat3::from_cols(right, up, forward));
    let mut scene = Scene::new(Camera::look_at(
        camera_at,
        look,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: fov, near: 0.004, far: 1.0 },
    ));
    for (mesh, material, transform) in [
        (&built.eyeball, &eye, turned),
        (&built.region, &skin, Mat4::IDENTITY),
        (&built.lashes, &lashes, Mat4::IDENTITY),
        (&built.shell, &shell, Mat4::IDENTITY),
    ] {
        // A design without lashes has an empty lash mesh, which is no instance at all.
        if mesh.indices.is_empty() {
            continue;
        }
        let mesh = renderer.create_mesh(mesh)?;
        made.push(mesh.into());
        let material = renderer.create_material(material)?;
        made.push(material.into());
        scene.instances.push(Instance::new(mesh, material, transform));
    }

    // The key from above and the nose's side, warm, as in a window-lit portrait: the socket toward the nose falls into
    // shadow and the ball and the lids are modelled by it.
    let key = Vec3::new(0.55, 0.6, 0.55);
    scene.lights = vec![
        Light::Directional { to_light: key, colour: Colour::hex(0x00ff_e8d0), intensity: 0.85, casts_shadow: true },
        // An anime eye is lit brighter and flatter, as a cel is.
        Light::Hemisphere {
            sky: Colour::hex(0x00c8_d0e0),
            ground: Colour::hex(0x0060_4a40),
            intensity: 0.08 + 0.44 * design.style,
        },
    ];
    // The softbox sits above the camera, so its reflection lands on the cornea as the catchlight.
    let softbox = camera_at.normalize() + Vec3::new(0.25, 0.55, 0.0);
    let environment = renderer.create_environment(&studio(softbox))?;
    made.push(environment.into());
    scene.environment = Some(EnvironmentUse::new(environment));
    scene.background = Background::Solid { colour: Colour::hex(0x0030_2420) };
    // The folds' occlusion at the eye's scale: the crease, the corners, the lids on the eye. A drawn eye is shaded in
    // flat cels, so it fades out toward anime. No contact shadows: on strands as thin as lashes the screen-space march
    // hits some jittered samples and misses others, so each lash casts a stippled fan.
    let real = 1.0 - design.style;
    scene.occlusion = (real > 0.01).then(|| Occlusion::new(0.0025, 0.7 * real));
    if view == View::Macro {
        // A macro lens: sharp on the iris, the lashes and the skin falling out of focus.
        scene.post.depth_of_field = Some(DepthOfField {
            focus_distance: (camera_at - forward * 0.0105).length(),
            focus_range: 0.005,
            max_blur: 0.025,
        });
    }
    Ok(scene)
}

/// A studio: a dim warm room and one bright softbox toward `key`, equirectangular (three.js layout).
#[allow(clippy::cast_precision_loss, reason = "pixel coordinates of a 512 by 256 map")]
pub(crate) fn studio(key: Vec3) -> EnvironmentDesc {
    let (w, h) = (512u32, 256u32);
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    let key = key.normalize();
    let side = key.cross(Vec3::Y).normalize();
    let up = side.cross(key).normalize();
    for y in 0..h {
        for x in 0..w {
            let u = (x as f32 + 0.5) / w as f32;
            let v = (y as f32 + 0.5) / h as f32;
            let phi = (u - 0.5) * std::f32::consts::TAU;
            let elevation = (0.5 - v) * std::f32::consts::PI;
            let d = Vec3::new(elevation.cos() * phi.cos(), elevation.sin(), elevation.cos() * phi.sin());
            let room = 0.05 + 0.06 * (0.5 + 0.5 * d.y);
            let mut c = Vec3::new(room * 1.05, room, room * 0.92);
            // The softbox: a rectangle about 30 by 22 degrees round the key, measured along the horizon and up.
            if d.dot(key) > 0.0 {
                let (a, b) = (d.dot(side) / d.dot(key), d.dot(up) / d.dot(key));
                let inside = (1.0 - ((a.abs() - 0.26) / 0.02).clamp(0.0, 1.0))
                    * (1.0 - ((b.abs() - 0.2) / 0.02).clamp(0.0, 1.0));
                c += Vec3::new(15.0, 14.2, 13.0) * inside;
            }
            rgb.extend_from_slice(&[c.x, c.y, c.z]);
        }
    }
    EnvironmentDesc::Hdri { size: UVec2::new(w, h), rgb }
}
