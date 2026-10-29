//! [`SceneDef::check`]: the one validator, shared by the ops, the store's reader, library planning and the stage
//! (design D13). Refusals stop an edit; findings don't.

use omuzaki_char_api::CharId;

use crate::anchor::Anchor;
use crate::camera::{CameraDef, Framing};
use crate::effects::WeatherArea;
use crate::error::{Finding, FindingKind, Findings, SceneError};
use crate::ids::MemberId;
use crate::lighting::{LightDef, LightShape};
use crate::scene::{MAX_CAMERAS, MAX_CAST, MAX_LIGHTS, MAX_PROPS, SceneDef};
use crate::settings::{Backdrop, Focus};

type Check = Result<(), SceneError>;

fn refuse(path: impl Into<String>, rule: impl Into<String>) -> Check {
    Err(SceneError::new(path, rule))
}

/// `value` is a finite number.
fn finite(path: &str, value: f32) -> Check {
    if value.is_finite() { Ok(()) } else { refuse(path, "must be a finite number") }
}

/// `value` is finite and at least 0.
fn non_negative(path: &str, value: f32) -> Check {
    finite(path, value)?;
    if value >= 0.0 { Ok(()) } else { refuse(path, format!("must be 0 or more, got {value}")) }
}

/// `value` is finite and within `lo..=hi`.
fn within(path: &str, value: f32, lo: f32, hi: f32) -> Check {
    finite(path, value)?;
    if (lo..=hi).contains(&value) {
        Ok(())
    } else {
        refuse(path, format!("must be between {lo} and {hi}, got {value}"))
    }
}

/// `value` is finite and above 0.
fn positive(path: &str, value: f32) -> Check {
    finite(path, value)?;
    if value > 0.0 { Ok(()) } else { refuse(path, format!("must be more than 0, got {value}")) }
}

fn all_finite(path: &str, values: &[f32]) -> Check {
    values.iter().try_for_each(|v| finite(path, *v))
}

fn at_most(path: &str, what: &str, len: usize, limit: usize) -> Check {
    if len <= limit { Ok(()) } else { refuse(path, format!("a scene has at most {limit} {what}, this has {len}")) }
}

impl SceneDef {
    /// Checks every rule of the scene contract (design D13) without knowing about the rest of the project: the
    /// findings that need it (a missing character) are left out; see [`check_with`](Self::check_with).
    ///
    /// # Errors
    /// The first [`SceneError`], naming the path and the rule. Paths are dotted from the scene node, in a stable order.
    pub fn check(&self) -> Result<Findings, SceneError> {
        self.check_with(&|_| true)
    }

    /// [`check`](Self::check), plus the findings that depend on the project: `has_character` says whether
    /// `characters/<id>` exists.
    ///
    /// # Errors
    /// As [`check`](Self::check).
    pub fn check_with(&self, has_character: &dyn Fn(&CharId) -> bool) -> Result<Findings, SceneError> {
        self.check_counts()?;
        self.check_settings()?;
        self.check_lighting()?;
        self.check_cameras()?;
        self.check_cast()?;
        self.check_props()?;
        self.check_weather()?;
        Ok(self.findings(has_character))
    }

    fn check_counts(&self) -> Check {
        // The ambient is one of the renderer's lights, so it takes a slot (design D3).
        at_most("lighting.lights", "lights besides the ambient", self.lighting.lights.len(), MAX_LIGHTS - 1)?;
        at_most("cameras", "cameras", self.cameras.len(), MAX_CAMERAS)?;
        at_most("cast", "cast members", self.cast.len(), MAX_CAST)?;
        at_most("props", "props", self.props.len(), MAX_PROPS)
    }

    fn check_settings(&self) -> Check {
        let s = &self.scene;
        within("scene.ground.opacity", s.ground.opacity, 0.0, 1.0)?;
        if let Some(r) = s.ground.radius {
            non_negative("scene.ground.radius", r)?;
        }
        if let Some(dof) = &s.post.depth_of_field {
            if let Focus::Metres(m) = dof.focus {
                non_negative("scene.post.depth_of_field.focus", m)?;
            }
            non_negative("scene.post.depth_of_field.range", dof.range)?;
            non_negative("scene.post.depth_of_field.max_blur", dof.max_blur)?;
        }
        if let Some(g) = s.post.grain {
            within("scene.post.grain", g, 0.0, 1.0)?;
        }
        if let Some(g) = &s.post.grade {
            non_negative("scene.post.grade.saturation", g.saturation)?;
            non_negative("scene.post.grade.contrast", g.contrast)?;
        }
        if let Some(subject) = &s.subject {
            self.require_member("scene.subject", subject)?;
        }
        if let Some(camera) = &s.camera
            && !self.cameras.contains_key(camera)
        {
            return refuse("scene.camera", format!("names camera `{camera}`, which the scene doesn't have"));
        }
        Ok(())
    }

    fn check_lighting(&self) -> Check {
        let l = &self.lighting;
        non_negative("lighting.exposure", l.exposure)?;
        non_negative("lighting.outline_light", l.outline_light)?;
        non_negative("lighting.back_light_rim", l.back_light_rim)?;
        if let Some(f) = l.skin_floor {
            non_negative("lighting.skin_floor", f)?;
        }
        non_negative("lighting.ambient.intensity", l.ambient.intensity)?;
        if let Some(b) = &l.bloom {
            non_negative("lighting.bloom.strength", b.strength)?;
            within("lighting.bloom.radius", b.radius, 0.0, 1.0)?;
            non_negative("lighting.bloom.threshold", b.threshold)?;
        }
        if let Some(fog) = &l.fog {
            non_negative("lighting.fog.near", fog.near)?;
            finite("lighting.fog.far", fog.far)?;
            if fog.far <= fog.near {
                return refuse("lighting.fog.far", format!("must be beyond `near` ({}), got {}", fog.near, fog.far));
            }
        }
        if let Some(environment) = &l.environment {
            non_negative("lighting.environment.intensity", environment.intensity)?;
            finite("lighting.environment.rotation", environment.rotation)?;
        }
        for (id, light) in &l.lights {
            self.check_light(&format!("lighting.lights.{id}"), light)?;
        }
        let casters: Vec<_> =
            l.lights.iter().filter(|(_, l)| l.is_directional() && l.shadow).map(|(id, _)| id).collect();
        match casters.get(1) {
            Some(second) => refuse(
                format!("lighting.lights.{second}.shadow"),
                format!("only one directional light may cast shadows, and `{}` already does", casters[0]),
            ),
            None => Ok(()),
        }
    }

    fn check_light(&self, path: &str, light: &LightDef) -> Check {
        non_negative(&format!("{path}.intensity"), light.intensity)?;
        self.check_anchor(&format!("{path}.anchor"), &light.anchor)?;
        match light.shape {
            LightShape::Directional { azimuth, elevation } => {
                within(&format!("{path}.azimuth"), azimuth, -180.0, 180.0)?;
                within(&format!("{path}.elevation"), elevation, -90.0, 90.0)
            }
            LightShape::Point { position, range } => {
                all_finite(&format!("{path}.position"), &position)?;
                positive(&format!("{path}.range"), range)
            }
            LightShape::Spot { position, target, cone, penumbra, range } => {
                all_finite(&format!("{path}.position"), &position)?;
                all_finite(&format!("{path}.target"), &target)?;
                // The beam's direction is target - position, so the two can't (nearly) coincide.
                if position.iter().zip(target).all(|(p, t)| (p - t).abs() < 1e-6) {
                    return refuse(format!("{path}.target"), "must not be the lamp's own position");
                }
                positive(&format!("{path}.cone"), cone)?;
                within(&format!("{path}.cone"), cone, 0.0, 90.0)?;
                within(&format!("{path}.penumbra"), penumbra, 0.0, 1.0)?;
                positive(&format!("{path}.range"), range)
            }
        }
    }

    fn check_weather(&self) -> Check {
        let Some(weather) = &self.weather else { return Ok(()) };
        weather.check().or_else(|(path, rule)| refuse(format!("weather.{path}"), rule))?;
        if let WeatherArea::Box(area) = &weather.area {
            self.check_anchor("weather.area.anchor", &area.anchor)?;
        }
        Ok(())
    }

    fn check_cameras(&self) -> Check {
        self.cameras.iter().try_for_each(|(id, camera)| self.check_camera(&format!("cameras.{id}"), camera))
    }

    fn check_camera(&self, path: &str, c: &CameraDef) -> Check {
        within(&format!("{path}.yaw"), c.yaw, -180.0, 180.0)?;
        within(&format!("{path}.pitch"), c.pitch, -90.0, 90.0)?;
        finite(&format!("{path}.fov"), c.fov)?;
        if c.fov <= 0.0 || c.fov >= 180.0 {
            return refuse(
                format!("{path}.fov"),
                format!("must be between 0 and 180 degrees (exclusive), got {}", c.fov),
            );
        }
        finite(&format!("{path}.zoom"), c.zoom)?;
        if c.zoom <= 0.0 {
            return refuse(format!("{path}.zoom"), format!("must be more than 0, got {}", c.zoom));
        }
        all_finite(&format!("{path}.shift"), &c.shift)?;
        if c.subject == Anchor::Stage {
            return refuse(format!("{path}.subject"), "a preset frames `subject` or `member:<id>`, not `stage`");
        }
        self.check_anchor(&format!("{path}.subject"), &c.subject)?;
        if let Some(anchor) = &c.anchor {
            self.check_anchor(&format!("{path}.anchor"), anchor)?;
        }
        for (name, v) in [("position", c.position), ("target", c.target)] {
            if let Some(v) = v {
                all_finite(&format!("{path}.{name}"), &v)?;
            } else if c.framing == Framing::Free {
                return refuse(format!("{path}.{name}"), "a free camera needs `position` and `target`");
            }
        }
        Ok(())
    }

    fn check_cast(&self) -> Check {
        for (id, member) in &self.cast {
            let path = format!("cast.{id}");
            if id.is_reserved() {
                return refuse(&path, format!("`{id}` names an anchor, so it can't be a member id"));
            }
            all_finite(&format!("{path}.position"), &member.position)?;
            within(&format!("{path}.yaw"), member.yaw, -180.0, 180.0)?;
            if let Some((group, driver, _)) = member.drive.invalid() {
                return refuse(
                    format!("{path}.drive.{}.{}", group.name(), driver.name()),
                    "a driver's weight is a number from 0 to 1",
                );
            }
        }
        Ok(())
    }

    fn check_props(&self) -> Check {
        for (id, prop) in &self.props {
            let path = format!("props.{id}");
            all_finite(&format!("{path}.position"), &prop.position)?;
            all_finite(&format!("{path}.rotation"), &prop.rotation)?;
            positive(&format!("{path}.scale"), prop.scale)?;
            self.check_anchor(&format!("{path}.anchor"), &prop.anchor)?;
            if prop.attach.is_some() && prop.anchor == Anchor::Stage {
                return refuse(
                    format!("{path}.attach"),
                    "a prop follows a bone of a member: anchor it to `subject` or `member:<id>`",
                );
            }
        }
        Ok(())
    }

    fn check_anchor(&self, path: &str, anchor: &Anchor) -> Check {
        anchor.member().map_or(Ok(()), |m| self.require_member(path, m))
    }

    fn require_member(&self, path: &str, member: &MemberId) -> Check {
        if self.cast.contains_key(member) {
            Ok(())
        } else {
            refuse(path, format!("names member `{member}`, who isn't in the cast"))
        }
    }

    fn findings(&self, has_character: &dyn Fn(&CharId) -> bool) -> Findings {
        let mut found = Findings::new();
        if self.lighting.lights.is_empty() {
            found.push(Finding {
                kind: FindingKind::NoLights,
                path: "lighting.lights".into(),
                message: "the lighting has no light other than the ambient, so nothing will cast a shape".into(),
            });
        }
        if matches!(self.scene.backdrop, Backdrop::Environment {}) && self.lighting.environment.is_none() {
            found.push(Finding {
                kind: FindingKind::NoEnvironment,
                path: "scene.backdrop".into(),
                message: "the backdrop is the environment but the lighting has none, so its background colour shows"
                    .into(),
            });
        }
        for (id, prop) in &self.props {
            if crate::props::find_prop_kind(prop.kind.as_str()).is_none() {
                found.push(Finding {
                    kind: FindingKind::UnknownPropKind,
                    path: format!("props.{id}.kind"),
                    message: format!("no prop kind `{}` in this build, so the stage skips this prop", prop.kind),
                });
            }
        }
        for (id, member) in &self.cast {
            if !has_character(&member.character) {
                found.push(Finding {
                    kind: FindingKind::MissingCharacter,
                    path: format!("cast.{id}.character"),
                    message: format!("character `{}` doesn't exist, so the stage skips this member", member.character),
                });
            }
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        found
    }
}
