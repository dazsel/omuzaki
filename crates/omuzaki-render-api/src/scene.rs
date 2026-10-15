//! The scene description: everything one frame needs, as a plain value the stage rebuilds every frame.
//!
//! Why immediate (rebuilt each frame) rather than a retained scene graph the renderer owns:
//! - **Determinism.** A frame is a pure function of the scene, the resources and [`Quality`](crate::Quality). There is
//!   no hidden scene state to replay, so renders are reproducible (principle 4) and the mock needs no bookkeeping.
//! - **One source of truth.** The runtime's entities (spec §8) already hold the poses, lights and cameras; mirroring
//!   them into a second graph would be a synchronisation bug waiting to happen.
//! - **No allocation in the hot loop.** [`Scene::clear`] keeps the vectors' capacity, so after the first frame
//!   rebuilding the scene allocates nothing.

use omuzaki_core::glam::camera::rh::proj::directx;
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::limits::{MAX_JOINTS, MAX_LIGHTS, MAX_MORPH_WEIGHTS};
use crate::{Colour, EnvironmentUse, MaterialId, MeshId, RenderError};

/// How the camera projects the world.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Projection {
    /// Perspective. The aspect ratio always comes from the render target, so a camera never disagrees with its output.
    Perspective {
        /// Vertical field of view in degrees, `(0, 180)`.
        fov_y_deg: f32,
        /// Near plane distance in metres, `> 0`.
        near: f32,
        /// Far plane distance in metres, `> near`.
        far: f32,
    },
    /// Orthographic (sprite sheets, pixel style, EX-07).
    Orthographic {
        /// Height of the view volume in metres; the width follows the target's aspect ratio.
        height: f32,
        /// Near plane distance in metres.
        near: f32,
        /// Far plane distance in metres, `> near`.
        far: f32,
    },
}

/// A viewpoint: where it is and how it projects.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Camera {
    /// Camera-to-world: columns are the camera's right (+X), up (+Y) and back (+Z) axes and its position. The camera
    /// looks down its own -Z, as in glTF and three.js.
    pub transform: Mat4,
    /// The projection.
    pub projection: Projection,
}

impl Default for Camera {
    /// At the origin looking down -Z with the prototype's 30° lens.
    fn default() -> Self {
        Self {
            transform: Mat4::IDENTITY,
            projection: Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 80.0 },
        }
    }
}

impl Camera {
    /// A camera at `eye` looking at `target`, with `up` as the rough up direction.
    ///
    /// Built from cross products and one square root each, so the matrix is identical on every platform. A `target`
    /// equal to `eye`, or `up` parallel to the view direction, falls back to looking down -Z with +Y up.
    pub fn look_at(eye: Vec3, target: Vec3, up: Vec3, projection: Projection) -> Self {
        let back = (eye - target).try_normalize().unwrap_or(Vec3::Z);
        let right = up.cross(back).try_normalize().unwrap_or(Vec3::X);
        let up = back.cross(right);
        Self {
            transform: Mat4::from_cols(right.extend(0.0), up.extend(0.0), back.extend(0.0), eye.extend(1.0)),
            projection,
        }
    }

    /// The camera's position in the world.
    pub fn position(&self) -> Vec3 {
        self.transform.w_axis.truncate()
    }

    /// World-to-camera.
    pub fn view(&self) -> Mat4 {
        self.transform.inverse()
    }

    /// Camera-to-clip for a target of `aspect` (width / height). Depth runs `0..1` and +Y is up, as in wgpu; every
    /// renderer uses this one definition so the mock and the GPU renderer frame a shot identically.
    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        match self.projection {
            Projection::Perspective { fov_y_deg, near, far } => {
                directx::perspective(fov_y_deg.to_radians(), aspect, near, far)
            }
            Projection::Orthographic { height, near, far } => {
                let half_h = height * 0.5;
                let half_w = half_h * aspect;
                directx::orthographic(-half_w, half_w, -half_h, half_h, near, far)
            }
        }
    }

    /// Checks that the transform and projection are usable.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for a non-finite transform or a degenerate projection.
    pub fn validate(&self) -> Result<(), RenderError> {
        let bad = |reason: &str| Err(RenderError::invalid("camera", reason));
        if !self.transform.is_finite() {
            return bad("the transform must be finite");
        }
        match self.projection {
            Projection::Perspective { fov_y_deg, near, far } => {
                if !(fov_y_deg > 0.0 && fov_y_deg < 180.0 && near > 0.0 && far > near && far.is_finite()) {
                    return bad("perspective needs 0 < fov < 180 degrees and 0 < near < far");
                }
            }
            Projection::Orthographic { height, near, far } => {
                if !(height > 0.0 && height.is_finite() && far > near && far.is_finite() && near.is_finite()) {
                    return bad("orthographic needs height > 0 and near < far");
                }
            }
        }
        Ok(())
    }
}

/// A light. Intensities follow three.js (the prototype's rigs port unchanged): a surface facing a white directional
/// light of intensity 1 reflects `albedo / π` of it, so intensity 2 on albedo 1 gives about 0.64.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Light {
    /// Parallel light from far away (sun, key, fill, rim).
    Directional {
        /// The world-space direction *toward* the light (so `+Y` is a light shining from above).
        #[schemars(with = "[f32; 3]")]
        to_light: Vec3,
        /// Light colour.
        colour: Colour,
        /// Brightness multiplier, `>= 0`.
        intensity: f32,
        /// Whether this light casts shadows. The renderer fits the shadow frustum to the casters itself; where the
        /// shadow map's resolution and update rate come from is [`Quality`](crate::Quality).
        casts_shadow: bool,
    },
    /// Soft sky-and-ground ambient light: the base of every rig.
    Hemisphere {
        /// Colour of light arriving from straight up.
        sky: Colour,
        /// Colour of light arriving from straight down.
        ground: Colour,
        /// Brightness multiplier, `>= 0`.
        intensity: f32,
    },
    /// A point light (candle, lantern) that falls off smoothly to zero at `range`.
    Point {
        /// World position.
        #[schemars(with = "[f32; 3]")]
        position: Vec3,
        /// Light colour.
        colour: Colour,
        /// Brightness multiplier, `>= 0`.
        intensity: f32,
        /// Distance in metres where the light reaches zero, `> 0`.
        range: f32,
    },
    /// A spot light (stage light, torch): a point light confined to a cone (LK-08). Distance falloff is the point
    /// light's; inside the cone the light is full strength, fading to zero at the edge as three.js does.
    Spot {
        /// World position of the lamp.
        #[schemars(with = "[f32; 3]")]
        position: Vec3,
        /// The world-space direction the lamp points (away from the lamp, along the cone's axis). Need not be unit.
        #[schemars(with = "[f32; 3]")]
        direction: Vec3,
        /// Light colour.
        colour: Colour,
        /// Brightness multiplier, `>= 0`.
        intensity: f32,
        /// Distance in metres where the light reaches zero, `> 0`.
        range: f32,
        /// The cone's outer half-angle (axis to edge) in degrees, `(0, 90]` (three.js `angle`).
        cone_outer_deg: f32,
        /// How much of the cone is soft, `0..=1` (three.js `penumbra`): 0 is a hard edge at `cone_outer_deg`, 1 fades
        /// from the axis. The light is `smoothstep(cos(outer), cos(outer * (1 - penumbra)), cos(angle))` of full
        /// strength at `angle` degrees off the axis.
        penumbra: f32,
        /// Whether this light casts shadows. A renderer may not draw spot shadows yet (the shadow budget is
        /// [`Quality`](crate::Quality)'s); the flag is the author's wish.
        casts_shadow: bool,
    },
}

impl Light {
    fn validate(&self) -> Result<(), RenderError> {
        let (intensity, placed) = match *self {
            Self::Directional { to_light, intensity, .. } => {
                (intensity, to_light.is_finite() && to_light != Vec3::ZERO)
            }
            Self::Hemisphere { intensity, .. } => (intensity, true),
            Self::Point { position, intensity, range, .. } => (intensity, position.is_finite() && range > 0.0),
            Self::Spot { position, direction, intensity, range, cone_outer_deg, penumbra, .. } => (
                intensity,
                position.is_finite()
                    && direction.is_finite()
                    && direction != Vec3::ZERO
                    && range > 0.0
                    && cone_outer_deg > 0.0
                    && cone_outer_deg <= 90.0
                    && (0.0..=1.0).contains(&penumbra),
            ),
        };
        if placed && intensity >= 0.0 && intensity.is_finite() {
            return Ok(());
        }
        Err(RenderError::invalid(
            "light",
            format!(
                "{self:?} needs a finite non-zero direction or position, a range > 0, an intensity >= 0 and, for a \
                 spot, a cone in (0, 90] degrees with a penumbra in 0..=1"
            ),
        ))
    }
}

/// Linear distance fog (LK-08): surfaces fade to `colour` between `near` and `far`.
///
/// The distance is the depth along the camera's view direction in metres (three.js `Fog`), so it does not curve at
/// the edges of the picture. The blend is applied to lit surfaces and outlines before exposure and tone mapping; the
/// background is not fogged.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Fog {
    /// The colour distant surfaces fade to.
    pub colour: Colour,
    /// Depth in metres where fog starts, `>= 0`.
    pub near: f32,
    /// Depth in metres where surfaces are fully fogged, `> near`.
    pub far: f32,
}

impl Fog {
    /// How much of the fog colour covers a surface `depth` metres along the view direction: 0 at `near` and nearer,
    /// 1 at `far` and beyond, linear between. The one definition both renderers use.
    pub fn amount(&self, depth: f32) -> f32 {
        ((depth - self.near) / (self.far - self.near)).clamp(0.0, 1.0)
    }

    fn validate(&self) -> Result<(), RenderError> {
        if self.near >= 0.0 && self.far > self.near && self.far.is_finite() {
            return Ok(());
        }
        Err(RenderError::invalid("fog", format!("needs finite distances with 0 <= near < far, got {self:?}")))
    }
}

/// A run of [`Scene::joints`] that skins one instance.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct JointRange {
    /// Index of the first matrix in [`Scene::joints`].
    pub start: u32,
    /// How many matrices; the mesh's joint indices must be below this.
    pub len: u32,
}

/// A run of [`Scene::morph_weights`] that blends one instance's morph targets.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MorphRange {
    /// Index of the first weight in [`Scene::morph_weights`].
    pub start: u32,
    /// How many weights; exactly the mesh's morph target count, weight `i` driving target `i`.
    pub len: u32,
}

/// One mesh drawn with one material at one place.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Instance {
    /// What to draw.
    pub mesh: MeshId,
    /// How it looks.
    pub material: MaterialId,
    /// Model-to-world. For a skinned instance it is applied *after* skinning.
    pub transform: Mat4,
    /// The skinning matrices, for skinned meshes. Each is `joint_pose × inverse_bind`, mapping the mesh's bind-pose
    /// space to the model space `transform` starts from, so the renderer needs no skeleton (that is `omuzaki-rig`'s
    /// job). `None` draws the mesh rigid.
    pub skin: Option<JointRange>,
    /// The morph target weights, for meshes created with [`MorphSet`](crate::MorphSet)s. Morphing happens before
    /// skinning and before `transform`. `None` draws the base mesh (the same as all weights zero).
    pub morph: Option<MorphRange>,
    /// Whether this instance is drawn into shadow maps.
    pub casts_shadow: bool,
    /// Draw order among blended instances: lower first. Equal values sort back to front by distance.
    pub order: i16,
}

impl Instance {
    /// A rigid, shadow-casting instance.
    pub fn new(mesh: MeshId, material: MaterialId, transform: Mat4) -> Self {
        Self { mesh, material, transform, skin: None, morph: None, casts_shadow: true, order: 0 }
    }
}

/// What is behind everything.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Background {
    /// A flat colour (alpha allowed).
    Solid {
        /// The colour.
        colour: Colour,
    },
    /// Nothing: the frame's alpha shows the character alone (BR-01).
    Transparent,
}

impl Default for Background {
    fn default() -> Self {
        Self::Solid { colour: Colour::BLACK }
    }
}

/// How scene light is mapped to display range.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tonemap {
    /// ACES filmic, the curve the prototype uses.
    #[default]
    Aces,
    /// Clamp to `0..=1`: exact colours, for flat styles and tests.
    None,
}

/// Bloom: bright areas glow (LK-05).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Bloom {
    /// How much glow is added, `>= 0`.
    pub strength: f32,
    /// How far it spreads, `0..=1` of the image.
    pub radius: f32,
    /// Brightness above which pixels glow, in display units.
    pub threshold: f32,
}

/// Depth of field: things away from the focal plane blur.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DepthOfField {
    /// Distance from the camera, in metres, that is perfectly sharp.
    pub focus_distance: f32,
    /// Depth in metres of the sharp zone around the focus distance.
    pub focus_range: f32,
    /// The strongest blur, as a fraction of the image height.
    pub max_blur: f32,
}

/// Film grain, seeded from the frame index so a re-render is identical.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FilmGrain {
    /// Noise amplitude in display units, `0..=1`.
    pub strength: f32,
}

/// A simple colour grade.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ColourGrade {
    /// 1 leaves colour alone, 0 is greyscale, above 1 is more vivid.
    pub saturation: f32,
    /// 1 leaves contrast alone; above 1 steepens the curve around mid grey.
    pub contrast: f32,
}

/// The look of the whole frame: what the author *asks for* after the scene is drawn (LK-05).
///
/// Effects are requests: [`Quality::post_effects`](crate::Quality) can switch them off for cost, and a renderer that
/// lacks one skips it. [`FrameReport`](crate::FrameReport) says which passes ran.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Post {
    /// Scene-light multiplier before tone mapping.
    pub exposure: f32,
    /// Tone mapping.
    pub tonemap: Tonemap,
    /// Bloom, if wanted.
    pub bloom: Option<Bloom>,
    /// Depth of field, if wanted.
    pub depth_of_field: Option<DepthOfField>,
    /// Film grain, if wanted.
    pub grain: Option<FilmGrain>,
    /// Colour grade, if wanted.
    pub grade: Option<ColourGrade>,
}

impl Default for Post {
    fn default() -> Self {
        Self { exposure: 1.0, tonemap: Tonemap::Aces, bloom: None, depth_of_field: None, grain: None, grade: None }
    }
}

/// Which frame this is. Nothing in a renderer reads the OS clock; time-based effects use this, so a frame can be
/// re-rendered exactly (spec §8, principle 4).
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FrameInfo {
    /// Frame number: seeds film grain and sample jitter. On the live path, where `omuzaki-budget`'s scheduler reads
    /// the [`FrameReport`](crate::FrameReport)s, give every submission an index at least one higher than the last
    /// (timeline position belongs in `time`): it identifies the submission in `FrameReport::frame` and
    /// `GpuTimings::frame`. Offline renders may repeat an index on purpose to re-render a frame exactly.
    pub index: u64,
    /// Time on the project timeline.
    pub time: Seconds,
}

/// Everything one frame draws.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    /// Which frame this is.
    pub frame: FrameInfo,
    /// The viewpoint.
    pub camera: Camera,
    /// Lights, at most [`MAX_LIGHTS`].
    pub lights: Vec<Light>,
    /// Draw list, in submission order.
    pub instances: Vec<Instance>,
    /// Skinning matrices for all skinned instances, at most [`MAX_JOINTS`]. One flat buffer, so animation writes a
    /// frame's poses without allocating and the renderer uploads it in one copy.
    pub joints: Vec<Mat4>,
    /// Morph target weights for all morphed instances, at most [`MAX_MORPH_WEIGHTS`], each finite. Weights are not
    /// clamped (glTF allows any real value). One flat buffer, like `joints`.
    pub morph_weights: Vec<f32>,
    /// What is behind everything.
    pub background: Background,
    /// Distance fog, if wanted. Like `background`, [`Scene::clear`] leaves it as it is.
    pub fog: Option<Fog>,
    /// Image-based light, if wanted. The environment must be live when the frame renders (`UnknownHandle`
    /// otherwise). [`Scene::clear`] leaves it as it is.
    pub environment: Option<EnvironmentUse>,
    /// Scales every outline colour: lower it in dark scenes so line art doesn't glow (the prototype's `OUTLINE_LIGHT`).
    pub outline_light: f32,
    /// The frame's post-processing look.
    pub post: Post,
}

impl Scene {
    /// An empty scene seen by `camera`.
    pub fn new(camera: Camera) -> Self {
        Self {
            frame: FrameInfo::default(),
            camera,
            lights: Vec::new(),
            instances: Vec::new(),
            joints: Vec::new(),
            morph_weights: Vec::new(),
            background: Background::default(),
            fog: None,
            environment: None,
            outline_light: 1.0,
            post: Post::default(),
        }
    }

    /// Empties the per-frame lists (`lights`, `instances` and `joints`) but keeps their allocations, ready to be refilled
    /// for the next frame.
    ///
    /// Everything else stays as it was: `camera`, `frame`, `background`, `fog`, `environment`, `outline_light` and `post`.
    /// Lights do go, so
    /// a scene that wants the same lights every frame must push them again after each `clear`. To keep lights across
    /// frames, leave them out of the reset and clear only what changes:
    ///
    /// ```ignore
    /// scene.instances.clear();
    /// scene.joints.clear();
    /// // `scene.lights` still holds last frame's lights; refill `scene.instances` and `scene.joints` as usual.
    /// ```
    pub fn clear(&mut self) {
        self.lights.clear();
        self.instances.clear();
        self.joints.clear();
        self.morph_weights.clear();
    }

    /// Checks everything that doesn't need the renderer's resource tables, so every renderer rejects the same inputs
    /// the same way (handles are the renderer's to check).
    ///
    /// # Errors
    /// [`RenderError::Invalid`] or `LimitExceeded` naming the first broken rule.
    pub fn validate(&self) -> Result<(), RenderError> {
        self.camera.validate()?;
        if self.lights.len() > MAX_LIGHTS {
            return Err(RenderError::LimitExceeded { what: "lights in a scene", limit: MAX_LIGHTS as u64 });
        }
        if self.joints.len() > MAX_JOINTS {
            return Err(RenderError::LimitExceeded { what: "joint matrices in a scene", limit: MAX_JOINTS as u64 });
        }
        if self.morph_weights.len() > MAX_MORPH_WEIGHTS {
            return Err(RenderError::LimitExceeded {
                what: "morph weights in a scene",
                limit: MAX_MORPH_WEIGHTS as u64,
            });
        }
        if !(self.outline_light >= 0.0 && self.outline_light.is_finite()) {
            return Err(RenderError::invalid("scene", "outline_light must be >= 0"));
        }
        self.lights.iter().try_for_each(Light::validate)?;
        self.fog.iter().try_for_each(Fog::validate)?;
        self.environment.iter().try_for_each(EnvironmentUse::validate)?;
        for (i, inst) in self.instances.iter().enumerate() {
            if !inst.transform.is_finite() {
                return Err(RenderError::invalid("instance", format!("instance {i} has a non-finite transform")));
            }
            if let Some(skin) = inst.skin {
                let end = u64::from(skin.start) + u64::from(skin.len);
                if skin.len == 0 || end > self.joints.len() as u64 {
                    let reason = format!(
                        "instance {i} skins joints {}..{end} but the scene has {}",
                        skin.start,
                        self.joints.len()
                    );
                    return Err(RenderError::invalid("instance", reason));
                }
            }
            if let Some(morph) = inst.morph {
                let end = u64::from(morph.start) + u64::from(morph.len);
                if morph.len == 0 || end > self.morph_weights.len() as u64 {
                    let reason = format!(
                        "instance {i} morphs weights {}..{end} but the scene has {}",
                        morph.start,
                        self.morph_weights.len()
                    );
                    return Err(RenderError::invalid("instance", reason));
                }
            }
        }
        if self.morph_weights.iter().any(|w| !w.is_finite()) {
            return Err(RenderError::invalid("scene", "morph weights must be finite"));
        }
        if self.joints.iter().any(|m| !m.is_finite()) {
            return Err(RenderError::invalid("scene", "joint matrices must be finite"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white_key() -> Light {
        Light::Directional { to_light: Vec3::Y, colour: Colour::WHITE, intensity: 1.0, casts_shadow: false }
    }

    #[test]
    fn look_at_points_down_minus_z_toward_the_target() {
        let cam = Camera::look_at(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y, Camera::default().projection);
        assert_eq!(cam.position(), Vec3::new(0.0, 0.0, 5.0));
        let forward = cam.transform.transform_vector3(-Vec3::Z);
        assert!((forward - (-Vec3::Z)).length() < 1e-6, "looking from +Z toward the origin means facing -Z");
        let view_of_target = cam.view().transform_point3(Vec3::ZERO);
        assert!((view_of_target - Vec3::new(0.0, 0.0, -5.0)).length() < 1e-5);
    }

    #[test]
    fn look_at_survives_degenerate_input() {
        let p = Camera::default().projection;
        for cam in [Camera::look_at(Vec3::ONE, Vec3::ONE, Vec3::Y, p), Camera::look_at(Vec3::Y, Vec3::ZERO, Vec3::Y, p)]
        {
            assert!(cam.transform.is_finite());
        }
    }

    #[test]
    fn projection_depth_runs_zero_to_one() {
        let cam = Camera::default();
        let m = cam.projection_matrix(1.0);
        let near = m.project_point3(Vec3::new(0.0, 0.0, -0.05));
        let far = m.project_point3(Vec3::new(0.0, 0.0, -80.0));
        assert!(near.z.abs() < 1e-4 && (far.z - 1.0).abs() < 1e-4, "{near:?} {far:?}");
    }

    #[test]
    fn camera_validation_rejects_degenerate_lenses() {
        let mut cam = Camera::default();
        assert_eq!(cam.validate(), Ok(()));
        cam.projection = Projection::Perspective { fov_y_deg: 30.0, near: 1.0, far: 0.5 };
        assert!(cam.validate().is_err());
        cam.projection = Projection::Orthographic { height: 0.0, near: 0.0, far: 1.0 };
        assert!(cam.validate().is_err());
        cam = Camera { transform: Mat4::from_scale(Vec3::splat(f32::NAN)), ..Camera::default() };
        assert!(cam.validate().is_err());
    }

    #[test]
    fn clear_keeps_capacity_so_the_hot_loop_stays_allocation_free() {
        let mut scene = Scene::new(Camera::default());
        scene.lights.extend([white_key(); 3]);
        scene.joints.extend([Mat4::IDENTITY; 64]);
        let (l, j) = (scene.lights.capacity(), scene.joints.capacity());
        scene.clear();
        assert!(scene.lights.is_empty() && scene.joints.is_empty());
        assert_eq!((scene.lights.capacity(), scene.joints.capacity()), (l, j));
    }

    #[test]
    fn scene_validation_checks_limits_ranges_and_numbers() {
        let mesh = MeshId::from_parts(0, 1);
        let mat = MaterialId::from_parts(0, 1);
        let mut s = Scene::new(Camera::default());
        assert_eq!(s.validate(), Ok(()));

        s.lights = vec![white_key(); MAX_LIGHTS + 1];
        assert!(matches!(s.validate(), Err(RenderError::LimitExceeded { .. })));
        s.lights = vec![Light::Directional {
            to_light: Vec3::ZERO,
            colour: Colour::WHITE,
            intensity: 1.0,
            casts_shadow: false,
        }];
        assert!(s.validate().is_err(), "a light with no direction");
        s.lights.clear();

        s.fog = Some(Fog { colour: Colour::WHITE, near: 2.0, far: 2.0 });
        assert!(s.validate().is_err(), "fog needs far > near");
        s.fog = Some(Fog { colour: Colour::WHITE, near: -1.0, far: 2.0 });
        assert!(s.validate().is_err(), "fog can't start behind the camera");
        s.fog = Some(Fog { colour: Colour::WHITE, near: 0.0, far: 2.0 });
        assert_eq!(s.validate(), Ok(()));
        s.fog = None;
        let mut env = EnvironmentUse::new(crate::EnvironmentId::from_parts(0, 1));
        env.intensity = -1.0;
        s.environment = Some(env);
        assert!(s.validate().is_err(), "a negative environment intensity");
        s.environment = None;

        let mut inst = Instance::new(mesh, mat, Mat4::IDENTITY);
        inst.skin = Some(JointRange { start: 2, len: 3 });
        s.instances = vec![inst];
        s.joints = vec![Mat4::IDENTITY; 4];
        assert!(s.validate().unwrap_err().to_string().contains("joints 2..5"), "range past the palette");
        s.joints = vec![Mat4::IDENTITY; 5];
        assert_eq!(s.validate(), Ok(()));

        s.instances[0].transform = Mat4::from_scale(Vec3::splat(f32::INFINITY));
        assert!(s.validate().is_err());
    }

    /// A valid spot light; `tweak` breaks one thing.
    #[test]
    fn morph_ranges_and_weights_are_validated_and_cleared() {
        let mut s = Scene::new(Camera::default());
        let mut inst = Instance::new(MeshId::from_parts(0, 1), MaterialId::from_parts(0, 1), Mat4::IDENTITY);
        inst.morph = Some(MorphRange { start: 1, len: 2 });
        s.instances = vec![inst];
        s.morph_weights = vec![0.0, 0.5];
        assert!(s.validate().unwrap_err().to_string().contains("weights 1..3"), "range past the weights");
        s.morph_weights = vec![0.0, 0.5, -3.0];
        assert_eq!(s.validate(), Ok(()), "weights are not clamped");
        s.instances[0].morph = Some(MorphRange { start: 0, len: 0 });
        assert!(s.validate().is_err(), "an empty range");
        s.instances[0].morph = Some(MorphRange { start: 0, len: 3 });
        s.morph_weights[2] = f32::NAN;
        assert!(s.validate().unwrap_err().to_string().contains("finite"));
        s.morph_weights = vec![0.0; MAX_MORPH_WEIGHTS + 1];
        assert!(matches!(s.validate(), Err(RenderError::LimitExceeded { .. })));

        s.morph_weights = vec![0.0; 8];
        let capacity = s.morph_weights.capacity();
        s.clear();
        assert_eq!(s.morph_weights, Vec::<f32>::new());
        assert_eq!(s.morph_weights.capacity(), capacity, "clear keeps the allocation");
    }

    fn spot_with(tweak: impl FnOnce(&mut SpotFields)) -> Light {
        let mut f = SpotFields {
            position: Vec3::new(0.0, 2.0, 0.0),
            direction: -Vec3::Y,
            intensity: 3.0,
            range: 6.0,
            cone_outer_deg: 30.0,
            penumbra: 0.4,
        };
        tweak(&mut f);
        Light::Spot {
            position: f.position,
            direction: f.direction,
            colour: Colour::WHITE,
            intensity: f.intensity,
            range: f.range,
            cone_outer_deg: f.cone_outer_deg,
            penumbra: f.penumbra,
            casts_shadow: false,
        }
    }

    struct SpotFields {
        position: Vec3,
        direction: Vec3,
        intensity: f32,
        range: f32,
        cone_outer_deg: f32,
        penumbra: f32,
    }

    fn spot() -> Light {
        spot_with(|_| {})
    }

    #[test]
    fn spot_validation_checks_direction_cone_penumbra_and_range() {
        let mut s = Scene::new(Camera::default());
        s.lights = vec![spot()];
        assert_eq!(s.validate(), Ok(()));
        let broken = [
            spot_with(|f| f.direction = Vec3::ZERO),
            spot_with(|f| f.cone_outer_deg = 0.0),
            spot_with(|f| f.cone_outer_deg = 91.0),
            spot_with(|f| f.penumbra = 1.5),
            spot_with(|f| f.range = 0.0),
            spot_with(|f| f.position = Vec3::NAN),
            spot_with(|f| f.intensity = -1.0),
        ];
        for (i, light) in broken.into_iter().enumerate() {
            s.lights = vec![light];
            assert!(s.validate().is_err(), "case {i} should be invalid");
        }
    }

    #[test]
    fn fog_amount_is_linear_between_near_and_far() {
        let fog = Fog { colour: Colour::WHITE, near: 2.0, far: 6.0 };
        let amounts = [0.0, 2.0, 3.0, 4.0, 6.0, 9.0].map(|d| fog.amount(d));
        assert_eq!(amounts, [0.0, 0.0, 0.25, 0.5, 1.0, 1.0]);
    }

    #[test]
    fn fog_and_environment_survive_clear_and_clear_allocates_nothing() {
        let mut scene = Scene::new(Camera::default());
        scene.fog = Some(Fog { colour: Colour::WHITE, near: 1.0, far: 2.0 });
        scene.environment = Some(EnvironmentUse::new(crate::EnvironmentId::from_parts(0, 1)));
        scene.clear();
        assert!(scene.fog.is_some() && scene.environment.is_some(), "clear empties the draw lists only");
    }

    #[test]
    fn look_types_round_trip_and_have_schemas() {
        let lights = vec![
            white_key(),
            Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.9 },
            Light::Point { position: Vec3::new(1.0, 2.0, 3.0), colour: Colour::WHITE, intensity: 2.0, range: 5.0 },
            spot(),
        ];
        let json = serde_json::to_value(&lights).unwrap();
        assert_eq!(json[0]["to_light"], serde_json::json!([0.0, 1.0, 0.0]));
        assert_eq!(serde_json::from_value::<Vec<Light>>(json).unwrap(), lights);

        let post = Post { bloom: Some(Bloom { strength: 0.35, radius: 0.5, threshold: 0.9 }), ..Post::default() };
        assert_eq!(serde_json::from_value::<Post>(serde_json::to_value(post).unwrap()).unwrap(), post);
        let fog = Fog { colour: Colour::hex(0x002a_2238), near: 2.0, far: 9.0 };
        assert_eq!(serde_json::from_value::<Fog>(serde_json::to_value(fog).unwrap()).unwrap(), fog);
        for schema in [
            schemars::schema_for!(Light),
            schemars::schema_for!(Post),
            schemars::schema_for!(Background),
            schemars::schema_for!(Fog),
        ] {
            assert!(serde_json::to_value(schema).is_ok());
        }
    }
}
