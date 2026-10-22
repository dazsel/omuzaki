//! Weather in the world (particles.md D10): what the frame step takes from a resolved scene. Resolve turns the
//! scene's [`WeatherDef`] into metres once per edit: an explicit area into a [`WeatherVolume`], and `"auto"` into the
//! floor bounds the frame step raises to the weather's height, so a live override falls over the same ground.

use std::collections::BTreeMap;

use omuzaki_core::glam::{Affine3A, Mat4, Quat, Vec2, Vec3};
use omuzaki_render_api::{Camera, ParticleParams};
use omuzaki_stage_api::{Anchor, MemberId, SceneDef, SpriteDef, WeatherArea};

use super::service::key_of;
use crate::scene::resolve::Figure;

/// A horizontal rectangle in the world where weather is born.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct WeatherVolume {
    /// The rectangle's centre and turn: the emitter of its batches.
    pub emitter: Mat4,
    /// Half extents along the emitter's X and Z, metres.
    pub half: Vec2,
}

impl WeatherVolume {
    /// The rectangle's area in square metres.
    pub fn area_m2(&self) -> f32 {
        4.0 * self.half.x * self.half.y
    }
}

/// The floor bounds `"auto"` weather falls over: the cast's feet and the active camera, grown by one subject unit.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AutoArea {
    /// The low X and Z corner, metres.
    pub min: Vec2,
    /// The high X and Z corner, metres.
    pub max: Vec2,
}

impl AutoArea {
    /// The volume at `height_m` above the floor.
    pub fn at(&self, height_m: f32) -> WeatherVolume {
        let centre = (self.min + self.max) * 0.5;
        WeatherVolume {
            emitter: Mat4::from_translation(Vec3::new(centre.x, height_m, centre.y)),
            half: (self.max - self.min) * 0.5,
        }
    }
}

impl Default for AutoArea {
    /// The 1 m figure at the origin grown by 1 m.
    fn default() -> Self {
        Self { min: Vec2::splat(-1.25), max: Vec2::splat(1.25) }
    }
}

/// The scene's weather in metres.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedWeather {
    /// What the particles look like.
    pub sprite: SpriteDef,
    /// How they move and look.
    pub params: ParticleParams,
    /// Births per square metre per second at intensity 1.
    pub density: f32,
    /// `0..=1`.
    pub intensity: f32,
    /// Subject units above the floor where they appear, for `"auto"`.
    pub height: f32,
    /// An explicit area; `None` is `"auto"`.
    pub volume: Option<WeatherVolume>,
}

/// What the effects take from a resolved scene.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneEffects {
    /// The subject's height in metres: the unit of member offsets and weather heights.
    pub unit: f32,
    /// The key of the subject member's id, when the cast has one.
    pub subject: Option<u64>,
    /// The target's width over its height, for the camera's top strip.
    pub aspect: f32,
    /// Where `"auto"` weather falls.
    pub auto: AutoArea,
    /// The scene's weather.
    pub weather: Option<ResolvedWeather>,
}

impl Default for SceneEffects {
    fn default() -> Self {
        Self { unit: 1.0, subject: None, aspect: 16.0 / 9.0, auto: AutoArea::default(), weather: None }
    }
}

impl SceneEffects {
    /// Resolves the effects of `def` with the cast's `figures`, its `subject`, and the active `camera`. Cold.
    pub fn resolve(
        def: &SceneDef,
        figures: &BTreeMap<MemberId, Figure>,
        subject: Option<(&MemberId, &Figure)>,
        camera: Option<&Camera>,
        aspect: f32,
    ) -> Self {
        let unit = subject.map_or(1.0, |(_, f)| f.height_m);
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        let mut take = |p: Vec3| {
            min = min.min(Vec2::new(p.x, p.z));
            max = max.max(Vec2::new(p.x, p.z));
        };
        for figure in figures.values() {
            let b = figure.landmarks.body;
            for corner in [
                Vec3::new(b.min.x, 0.0, b.min.z),
                Vec3::new(b.max.x, 0.0, b.min.z),
                Vec3::new(b.min.x, 0.0, b.max.z),
                Vec3::new(b.max.x, 0.0, b.max.z),
            ] {
                take(figure.root.transform_point3(corner));
            }
        }
        if figures.is_empty() {
            take(Vec3::new(-0.25, 0.0, -0.25));
            take(Vec3::new(0.25, 0.0, 0.25));
        }
        if let Some(camera) = camera {
            take(camera.transform.w_axis.truncate());
        }
        let grow = Vec2::splat(unit);
        let auto = if min.is_finite() && max.is_finite() {
            AutoArea { min: min - grow, max: max + grow }
        } else {
            AutoArea::default()
        };
        let weather = def.weather.as_ref().map(|w| {
            let volume = match &w.area {
                WeatherArea::Auto(_) => None,
                WeatherArea::Box(area) => {
                    let (root, scale) = match &area.anchor {
                        Anchor::Stage => (Affine3A::IDENTITY, 1.0),
                        Anchor::Subject => subject.map_or((Affine3A::IDENTITY, 1.0), |(_, f)| (f.root, f.height_m)),
                        Anchor::Member(id) => figures.get(id).map_or((Affine3A::IDENTITY, 1.0), |f| (f.root, f.height_m)),
                    };
                    let lo = Vec3::from(area.min) * scale;
                    let hi = Vec3::from(area.max) * scale;
                    let top = Vec3::new(f32::midpoint(lo.x, hi.x), hi.y, f32::midpoint(lo.z, hi.z));
                    let (_, rotation, _) = Mat4::from(root).to_scale_rotation_translation();
                    Some(WeatherVolume {
                        emitter: Mat4::from_rotation_translation(rotation, root.transform_point3(top)),
                        half: Vec2::new((hi.x - lo.x) * 0.5, (hi.z - lo.z) * 0.5),
                    })
                }
            };
            ResolvedWeather {
                sprite: w.sprite,
                params: w.particles.clone(),
                density: w.density,
                intensity: w.intensity,
                height: w.height,
                volume,
            }
        });
        Self { unit, subject: subject.map(|(id, _)| key_of(id.as_str())), aspect, auto, weather }
    }
}

/// The turn about +Y of `m`'s forward (-Z) axis, as a rotation: keeps a camera's heading and drops its tilt.
pub fn yaw_of(m: &Mat4) -> Quat {
    let forward = -m.z_axis.truncate();
    let heading = Vec2::new(forward.x, forward.z);
    if heading.length_squared() < 1e-12 {
        return Quat::IDENTITY;
    }
    // Looking down -Z is a yaw of 0.
    Quat::from_rotation_y((-heading.x).atan2(-heading.y))
}
