//! A feature's close-up: its mesh built on a renderer with the `skin` program, lit as the eye's close-up is (a dim
//! warm studio, a softbox, a key light with shadows and a soft fill) and framed from one of four sides, so a feature is
//! judged from every angle a camera will see it from, the undersides included.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::mesh::MeshData;
use omuzaki_render_api::{
    Background, Camera, EnvironmentUse, Instance, Light, Material, Occlusion, ParamValue, ProgramParams, Projection,
    RenderError, Renderer, ResourceRef, Scene, Shading,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::eye3d::closeup::studio;

/// Where the close-up looks from.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeatureView {
    /// Straight on.
    #[default]
    Front,
    /// Turned 40 degrees toward the person's left and a little from above.
    ThreeQuarter,
    /// From the side.
    Profile,
    /// From below and in front, looking up: the nostrils, the lips' underside.
    Below,
}

impl FeatureView {
    /// Every view.
    pub const ALL: [Self; 4] = [Self::Front, Self::ThreeQuarter, Self::Profile, Self::Below];

    /// The direction from the subject to the camera.
    fn toward(self) -> Vec3 {
        let (turn, lift) = match self {
            Self::Front => (0.0f32, 0.0f32),
            Self::ThreeQuarter => (40.0, 8.0),
            Self::Profile => (90.0, 0.0),
            Self::Below => (12.0, -48.0),
        };
        let (turn, lift) = (turn.to_radians(), lift.to_radians());
        Vec3::new(turn.sin() * lift.cos(), lift.sin(), turn.cos() * lift.cos())
    }
}

/// The `skin` program's parameters at `style` (0 photoreal, 1 anime): toward anime the skin reads as a flat painted
/// tone, with no pores, little scattering or shine, and light that wraps softly.
pub fn skin_params(style: f32) -> ProgramParams {
    let real = 1.0 - style.clamp(0.0, 1.0);
    let mut p = ProgramParams::new();
    let mut put = |name: &str, value: ParamValue| {
        if let Ok(slug) = Slug::new(name) {
            p.insert(slug, value);
        }
    };
    put("pore-depth", ParamValue::Float(0.08 * real));
    put("roughness", ParamValue::Float(0.48 + 0.42 * (1.0 - real)));
    put("sheen", ParamValue::Float(0.8 * real));
    put("sss", ParamValue::Float(0.3 + 0.7 * real));
    put("wrap", ParamValue::Float(0.4 + 0.5 * (1.0 - real)));
    put("specular", ParamValue::Float(0.028 * real));
    put("wet-tint", ParamValue::Float(0.3 * real));
    p
}

/// A feature built on a renderer: its scene and the resources it made, which [`Closeup::release`] gives back.
#[derive(Debug)]
pub struct Closeup {
    scene: Scene,
    made: Vec<ResourceRef>,
}

impl Closeup {
    /// Builds `mesh` (a feature's [`super::skin_mesh`] inside `min`–`max`) at `style` and frames it from `view`.
    ///
    /// # Errors
    /// What the renderer returns when it can't make a resource; everything made before the failure is released.
    pub fn build(
        renderer: &mut dyn Renderer,
        mesh: &MeshData,
        bounds: (Vec3, Vec3),
        style: f32,
        view: FeatureView,
    ) -> Result<Self, RenderError> {
        let mut made = Vec::new();
        match build(renderer, mesh, bounds, style, view, &mut made) {
            Ok(scene) => Ok(Self { scene, made }),
            Err(error) => {
                for resource in made.into_iter().rev() {
                    // The build's own failure is the one to report.
                    let _ = renderer.release(resource);
                }
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
        let mut first = Ok(());
        for resource in self.made.into_iter().rev() {
            if let Err(error) = renderer.release(resource)
                && first.is_ok()
            {
                first = Err(error);
            }
        }
        first
    }
}

fn build(
    renderer: &mut dyn Renderer,
    mesh: &MeshData,
    (min, max): (Vec3, Vec3),
    style: f32,
    view: FeatureView,
    made: &mut Vec<ResourceRef>,
) -> Result<Scene, RenderError> {
    // Framed on the box's middle, near its front, far enough back that its height fills about four fifths of the frame.
    let fov = 27.0f32;
    let look =
        Vec3::new(f32::midpoint(min.x, max.x), f32::midpoint(min.y, max.y), max.z - 0.35 * (max.z - min.z).min(0.03));
    let reach = 0.5 * (max.y - min.y).max(max.x - min.x) / (0.5 * fov.to_radians()).tan() / 0.8;
    let camera_at = look + view.toward() * reach;
    let mut scene = Scene::new(Camera::look_at(
        camera_at,
        look,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: fov, near: 0.004, far: 1.0 },
    ));
    let name = Slug::new("skin").map_err(|e| RenderError::invalid("shading program", e.to_string()))?;
    let material = Material {
        shading: Shading::Program { name, params: skin_params(style) },
        vertex_colours: true,
        ..Material::toon(Colour::hex(0x00ff_ffff))
    };
    let mesh_id = renderer.create_mesh(mesh)?;
    made.push(mesh_id.into());
    let material_id = renderer.create_material(&material)?;
    made.push(material_id.into());
    scene.instances.push(Instance::new(mesh_id, material_id, Mat4::IDENTITY));

    // The key from above and to the person's right, as for the eye: under the nose and the lip, its shadow.
    let key = Vec3::new(-0.35, 0.72, 0.6);
    scene.lights = vec![
        Light::Directional { to_light: key, colour: Colour::hex(0x00ff_f2e4), intensity: 0.8, casts_shadow: true },
        Light::Hemisphere {
            sky: Colour::hex(0x00c8_d0e0),
            ground: Colour::hex(0x0060_4a40),
            intensity: 0.14 + 0.4 * style,
        },
    ];
    let softbox = Vec3::new(0.0, 0.0, 1.0) + Vec3::new(0.25, 0.55, 0.0);
    let environment = renderer.create_environment(&studio(softbox))?;
    made.push(environment.into());
    scene.environment = Some(EnvironmentUse::new(environment));
    scene.background = Background::Solid { colour: Colour::hex(0x0030_2420) };
    let real = 1.0 - style.clamp(0.0, 1.0);
    scene.occlusion = (real > 0.01).then(|| Occlusion::new(0.004, 0.6 * real));
    Ok(scene)
}
