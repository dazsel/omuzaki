//! The eye's close-up: one design built on a renderer, lit and framed as a photographer's macro shot of an eye, so the
//! lab test that renders the eye and the `render.eye` op an agent calls draw the same picture.
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
    /// The shot this view takes.
    pub fn shot(self) -> Shot {
        match self {
            Self::Macro => Shot {
                camera: [-0.027_25, -0.014, 0.050_02],
                look: [-0.001_75, -0.000_8, 0.010],
                roll: -5.5,
                fov: 32.0,
                gaze: [-0.146_5, 0.168_3, 0.92],
                depth_of_field: true,
                light: Lighting::default(),
            },
            Self::Front => Shot {
                camera: [0.0, 0.001, 0.07],
                look: [0.0, 0.0005, 0.008],
                roll: 0.0,
                fov: 27.0,
                gaze: [0.0, 0.015, 1.0],
                depth_of_field: false,
                light: Lighting::default(),
            },
        }
    }
}

/// Where a close-up is shot from and where the eye looks, in the eye's frame: metres from the eyeball's centre, `+z`
/// straight out of the eye, `+x` toward the nose, `+y` up. `face.eye3d.fit` finds one that frames the eye as a
/// photograph does; `render.eye` takes one in place of a view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Shot {
    /// The camera's position, metres (a macro still is 4 to 8 cm away).
    pub camera: [f32; 3],
    /// The point the camera looks at, metres.
    pub look: [f32; 3],
    /// The camera turned about its own axis, degrees, -90 to 90: positive leans the picture's top toward the nose.
    #[serde(default)]
    pub roll: f32,
    /// The vertical field of view, degrees, 5 to 90.
    pub fov: f32,
    /// The direction the eye looks, in the same frame (`[0, 0, 1]` straight ahead); its length doesn't matter.
    pub gaze: [f32; 3],
    /// A macro lens's shallow depth of field, focused on the iris; `false` keeps everything sharp.
    #[serde(default)]
    pub depth_of_field: bool,
    /// How the eye is lit; a portrait's key and fill when left out.
    #[serde(default)]
    pub light: Lighting,
}

/// How a close-up is lit: a warm key that casts shadows, a soft fill from the camera, and the studio's softbox, whose
/// reflection is the catchlight. Directions are toward the light, in the eye's frame; their lengths don't matter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Lighting {
    /// Toward the key light (`[0.55, 0.6, 0.55]`: above and on the nose's side, as a window lights a portrait).
    pub key: [f32; 3],
    /// The key's strength, 0 to 4.
    pub key_intensity: f32,
    /// The fill from the camera's side, 0 to 4: it reaches under the lids and into the corners the key leaves dark.
    pub fill: f32,
    /// The softbox, from the camera's direction: how far it sits toward the nose (`x`) and above (`y`) the camera's
    /// line; the catchlight moves with it.
    pub softbox: [f32; 2],
    /// How bright the studio round the eye is, 0 to 4: the softbox, whose reflection is the catchlight, and the
    /// room's dim walls, which light every surface a little from all round.
    pub studio: f32,
    /// Lamps close to the face (at most [`MAX_LAMPS`]), whose light fades across the frame: one side of the picture
    /// bright, the other in shade, as a lamp a hand's width away lights a macro still. None by default.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lamps: Vec<Lamp>,
}

/// The most lamps one shot takes: the renderer lights a scene with at most eight lights, and the key, the fill and
/// the room's ambient take three.
pub const MAX_LAMPS: usize = 4;

/// A lamp near the eye: unshadowed, its light fading to nothing at its reach.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lamp {
    /// Where it is, metres in the eye's frame (`[0.03, -0.03, 0.02]`: by the nose, below the eye).
    pub position: [f32; 3],
    /// Its strength, 0 to 10 (about the key's at 1).
    pub intensity: f32,
    /// How far its light reaches, metres, 0.01 to 1; the light fades over the whole of it.
    pub reach: f32,
    /// Its colour; a warm tungsten white when left out.
    #[serde(default = "warm")]
    pub colour: Colour,
}

fn warm() -> Colour {
    Colour::hex(0x00ff_e8d0)
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            key: [0.55, 0.6, 0.55],
            key_intensity: 0.8,
            fill: 0.22,
            softbox: [0.4, 1.0],
            studio: 1.0,
            lamps: Vec::new(),
        }
    }
}

impl Lighting {
    /// Why the lighting can't be used, if it can't.
    ///
    /// # Errors
    /// What is wrong, naming the field.
    pub fn check(&self) -> Result<(), String> {
        let numbers = self.key.iter().chain(&self.softbox).chain([&self.key_intensity, &self.fill]);
        if numbers.into_iter().any(|v| !v.is_finite()) {
            return Err("light: every number must be finite".to_owned());
        }
        if Vec3::from(self.key).length() < 1e-6 {
            return Err("light.key: must point somewhere".to_owned());
        }
        if !(0.0..=4.0).contains(&self.key_intensity) || !(0.0..=4.0).contains(&self.fill) {
            return Err("light: key_intensity and fill must be 0 to 4".to_owned());
        }
        if self.softbox.iter().any(|v| v.abs() > 4.0) {
            return Err("light.softbox: each must be -4 to 4".to_owned());
        }
        if !(0.0..=4.0).contains(&self.studio) {
            return Err("light.studio: must be 0 to 4".to_owned());
        }
        if self.lamps.len() > MAX_LAMPS {
            return Err(format!("light.lamps: at most {MAX_LAMPS}"));
        }
        for (i, lamp) in self.lamps.iter().enumerate() {
            if lamp.position.iter().chain([&lamp.intensity, &lamp.reach]).any(|v| !v.is_finite()) {
                return Err(format!("light.lamps/{i}: every number must be finite"));
            }
            if Vec3::from(lamp.position).length() > 1.0 {
                return Err(format!("light.lamps/{i}/position: must be within a metre of the eye"));
            }
            if !(0.0..=10.0).contains(&lamp.intensity) || !(0.01..=1.0).contains(&lamp.reach) {
                return Err(format!("light.lamps/{i}: intensity must be 0 to 10 and reach 0.01 to 1"));
            }
        }
        Ok(())
    }
}

impl Shot {
    /// The camera's up direction, after the roll.
    pub fn up(&self) -> Vec3 {
        let roll = self.roll.to_radians();
        Vec3::new(roll.sin(), roll.cos(), 0.0)
    }

    /// The eye's gaze, unit length.
    pub fn forward(&self) -> Vec3 {
        Vec3::from(self.gaze).normalize_or(Vec3::Z)
    }

    /// Why the shot can't be taken, if it can't: a non-finite number, a camera on its target or more than a metre
    /// away, a roll or field of view out of range, or a gaze that is zero or turned away from the camera's side.
    ///
    /// # Errors
    /// What is wrong, naming the field.
    pub fn check(&self) -> Result<(), String> {
        let numbers = self.camera.iter().chain(&self.look).chain(&self.gaze).chain([&self.roll, &self.fov]);
        if numbers.into_iter().any(|v| !v.is_finite()) {
            return Err("every number must be finite".to_owned());
        }
        let (camera, look) = (Vec3::from(self.camera), Vec3::from(self.look));
        if camera.length() > 1.0 {
            return Err("camera: must be within a metre of the eye".to_owned());
        }
        if (camera - look).length() < 1e-4 {
            return Err("look: must be away from the camera".to_owned());
        }
        if !(-90.0..=90.0).contains(&self.roll) {
            return Err("roll: must be -90 to 90 degrees".to_owned());
        }
        if !(5.0..=90.0).contains(&self.fov) {
            return Err("fov: must be 5 to 90 degrees".to_owned());
        }
        let gaze = Vec3::from(self.gaze);
        if gaze.length() < 1e-6 || gaze.z <= 0.0 {
            return Err("gaze: must point out of the eye (a positive z)".to_owned());
        }
        self.light.check()
    }

    /// The camera this shot uses.
    pub fn camera(&self) -> Camera {
        Camera::look_at(
            Vec3::from(self.camera),
            Vec3::from(self.look),
            self.up(),
            Projection::Perspective { fov_y_deg: self.fov, near: 0.004, far: 1.0 },
        )
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
        Self::shoot(renderer, design, &view.shot(), texture)
    }

    /// Builds `design` on `renderer` and frames it as `shot` takes it.
    ///
    /// # Errors
    /// `Invalid` for a shot [`Shot::check`] refuses; otherwise what the renderer returns when it can't make a resource,
    /// with everything made before the failure released.
    pub fn shoot(
        renderer: &mut dyn Renderer,
        design: &EyeDesign,
        shot: &Shot,
        texture: u32,
    ) -> Result<Self, RenderError> {
        shot.check().map_err(|e| RenderError::invalid("shot", e))?;
        let mut made = Vec::new();
        match build(renderer, design, shot, texture, &mut made) {
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
    shot: &Shot,
    texture: u32,
    made: &mut Vec<ResourceRef>,
) -> Result<Scene, RenderError> {
    let camera_at = Vec3::from(shot.camera);
    let forward = shot.forward();
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
    let mut scene = Scene::new(shot.camera());
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

    let real = 1.0 - design.style;
    let light = &shot.light;
    // The key, warm, as in a window-lit portrait: the ball and the lids are modelled by it.
    let key = Vec3::from(light.key).normalize_or(Vec3::Z);
    scene.lights = vec![
        Light::Directional {
            to_light: key,
            colour: Colour::hex(0x00ff_e8d0),
            intensity: light.key_intensity,
            casts_shadow: true,
        },
        // A soft fill from the camera, as a photographer bounces one: it reaches under the lids and into the corners,
        // which the key from above leaves black.
        Light::Directional {
            to_light: camera_at.normalize(),
            colour: Colour::hex(0x00ff_dcc8),
            intensity: light.fill * real,
            casts_shadow: false,
        },
        // An anime eye is lit brighter and flatter, as a cel is.
        Light::Hemisphere {
            sky: Colour::hex(0x00c8_d0e0),
            ground: Colour::hex(0x0060_4a40),
            intensity: 0.08 + 0.44 * design.style,
        },
    ];
    // The renderer's point light is inverse-square from 10 cm in, flat inside that (it is built for rooms), so at the
    // eye's scale its fade is its window alone: `intensity` here is scaled to the key's.
    scene.lights.extend(light.lamps.iter().map(|lamp| Light::Point {
        position: Vec3::from(lamp.position),
        colour: lamp.colour,
        intensity: lamp.intensity * 0.01 * real,
        range: lamp.reach,
    }));
    // The softbox sits above the camera, so its reflection lands on the cornea as the catchlight.
    let softbox = camera_at.normalize() + Vec3::new(light.softbox[0], light.softbox[1], 0.0);
    let environment = renderer.create_environment(&studio(softbox))?;
    made.push(environment.into());
    scene.environment = Some(EnvironmentUse { intensity: light.studio, ..EnvironmentUse::new(environment) });
    scene.background = Background::Solid { colour: Colour::hex(0x0030_2420) };
    // The folds' occlusion at the eye's scale: the crease, the corners, the lids on the eye. A drawn eye is shaded in
    // flat cels, so it fades out toward anime. No contact shadows: on strands as thin as lashes the screen-space march
    // hits some jittered samples and misses others, so each lash casts a stippled fan.
    scene.occlusion = (real > 0.01).then(|| Occlusion::new(0.0025, 0.7 * real));
    if shot.depth_of_field {
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
