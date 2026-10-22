//! A scene's lights (design D3, D8): `stage.add_light`, `stage.set_light`, `stage.remove_light`, and the rest of the
//! lighting with `stage.set_lighting`, `stage.set_light_rig` and `stage.light_rigs`.

use omuzaki_core::text::nearest;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::Colour;
use omuzaki_stage_api::{
    Ambient, Anchor, BloomDef, LightDef, LightId, LightRole, LightShape, MAX_LIGHTS, SceneId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::edit::{OrOff, SceneEdit, changed, free_id, unknown};
use crate::recipe::LightRig;

/// The shape of a light: a sun-like directional light, or a point light in the room.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LightKind {
    /// Parallel rays from a direction (`azimuth`, `elevation`); the one that casts shadows.
    Directional,
    /// From a point (`position`), fading out at `range`.
    Point,
}

/// Adds a light to a scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"kind": "directional", "role": "accent", "colour": "#ffd0e0", "azimuth": -120.0, "elevation": 20.0},
        {"scene": "night_stream", "kind": "point", "role": "practical", "position": [1.2, 1.4, -0.8], "range": 4.0, "intensity": 3.0}
    ],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct AddLight {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// Its id; the role's name (then `-2`, `-3`...) when left out, else `light`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<LightId>,
    /// `directional` or `point`.
    pub kind: LightKind,
    /// What it does in the rig: `key`, `fill`, `rim`, `accent` or `practical`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<LightRole>,
    /// Its colour (`#rrggbb`); white when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<Colour>,
    /// Its intensity, 0 or more; 1 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f32>,
    /// Whether it casts shadows (directional lights only; one per scene, so this takes it from the light that has it).
    #[serde(default)]
    pub shadow: bool,
    /// What it is placed against: `subject` (the default), `stage` or `member:<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// A directional light's direction around the subject, degrees, -180 to 180 (0 is from the front).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azimuth: Option<f32>,
    /// A directional light's height, degrees, -90 to 90.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevation: Option<f32>,
    /// A point light's position in metres from its anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// A point light's reach in metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<f32>,
}

/// The shape `kind` makes of the given fields, refusing the fields that belong to the other kind.
fn shape(
    kind: LightKind,
    azimuth: Option<f32>,
    elevation: Option<f32>,
    position: Option<[f32; 3]>,
    range: Option<f32>,
) -> Result<LightShape, OpError> {
    let not_for = |field: &str, kind: &str| {
        OpError::invalid(field, format!("a {kind} light has no {field}")).hint(match kind {
            "directional" => "a directional light takes azimuth and elevation",
            _ => "a point light takes position and range",
        })
    };
    match kind {
        LightKind::Directional => {
            if position.is_some() {
                return Err(not_for("position", "directional"));
            }
            if range.is_some() {
                return Err(not_for("range", "directional"));
            }
            Ok(LightShape::Directional { azimuth: azimuth.unwrap_or(0.0), elevation: elevation.unwrap_or(30.0) })
        }
        LightKind::Point => {
            if azimuth.is_some() {
                return Err(not_for("azimuth", "point"));
            }
            if elevation.is_some() {
                return Err(not_for("elevation", "point"));
            }
            Ok(LightShape::Point { position: position.unwrap_or([0.0, 1.5, 1.0]), range: range.unwrap_or(5.0) })
        }
    }
}

/// Clears the shadow on every other directional light, so `keep` is the one caster; the ids it was taken from.
fn take_shadow(lights: &mut std::collections::BTreeMap<LightId, LightDef>, keep: &LightId) -> Vec<String> {
    let mut moved = Vec::new();
    for (id, light) in lights.iter_mut() {
        if id != keep && light.shadow && light.is_directional() {
            light.shadow = false;
            moved.push(id.to_string());
        }
    }
    moved
}

fn role_name(role: LightRole) -> &'static str {
    match role {
        LightRole::Key => "key",
        LightRole::Fill => "fill",
        LightRole::Rim => "rim",
        LightRole::Accent => "accent",
        LightRole::Practical => "practical",
    }
}

impl Op for AddLight {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.add_light",
            summary: "Add a directional or point light to a scene; a shadow on it moves the scene's one shadow to it.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let lights = &mut edit.def.lighting.lights;
        if lights.len() >= MAX_LIGHTS - 1 {
            return Err(OpError::invalid(
                "kind",
                format!("the scene has {} lights besides the ambient, the most a scene can have", lights.len()),
            )
            .hint("remove one with stage.remove_light, or change one with stage.set_light"));
        }
        let id = match &self.id {
            Some(id) if lights.contains_key(id) => {
                return Err(OpError::conflict(format!("the scene already has a light '{id}'"))
                    .hint("pick another id, or change that light with stage.set_light"));
            }
            Some(id) => id.clone(),
            None => {
                let base = self.role.map_or("light", role_name);
                let free = free_id(base, &|s| lights.keys().any(|k| k.as_str() == s));
                LightId::new(&free).map_err(|e| OpError::internal(format!("light id '{free}': {e}")))?
            }
        };
        if self.shadow && self.kind == LightKind::Point {
            return Err(OpError::invalid("shadow", "only a directional light casts shadows"));
        }
        let light = LightDef {
            role: self.role,
            colour: self.colour.unwrap_or(Colour::WHITE),
            intensity: self.intensity.unwrap_or(1.0),
            shadow: self.shadow,
            anchor: self.anchor.clone().unwrap_or_default(),
            shape: shape(self.kind, self.azimuth, self.elevation, self.position, self.range)?,
        };
        lights.insert(id.clone(), light);
        let moved = if self.shadow { take_shadow(lights, &id) } else { Vec::new() };
        let output = json!({ "scene": edit.id.as_str(), "light": id.as_str(), "shadow_taken_from": moved });
        edit.finish(ctx, output)
    }
}

register_op!(AddLight);

/// Changes a light of a scene: only the fields given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"light": "key", "intensity": 2.0}, {"light": "rim", "colour": "#a0c0ff", "shadow": false}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetLight {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The light.
    pub light: LightId,
    /// Its role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<LightRole>,
    /// Its colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<Colour>,
    /// Its intensity, 0 or more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f32>,
    /// Whether it casts the scene's shadow (directional only); `true` takes it from the light that had it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<bool>,
    /// What it is placed against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// A directional light's direction, degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azimuth: Option<f32>,
    /// A directional light's height, degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevation: Option<f32>,
    /// A point light's position, metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// A point light's reach, metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<f32>,
}

impl Op for SetLight {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_light",
            summary: "Change a light of a scene: colour, intensity, direction or position, role, anchor, shadow. Only \
                      the fields given change.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let lights = &mut edit.def.lighting.lights;
        let known: Vec<String> = lights.keys().map(ToString::to_string).collect();
        let light = lights.get_mut(&self.light).ok_or_else(|| unknown("light", "light", self.light.as_str(), known))?;
        if let Some(role) = self.role {
            light.role = Some(role);
        }
        if let Some(colour) = self.colour {
            light.colour = colour;
        }
        if let Some(intensity) = self.intensity {
            light.intensity = intensity;
        }
        if let Some(anchor) = &self.anchor {
            light.anchor = anchor.clone();
        }
        match &mut light.shape {
            LightShape::Directional { azimuth, elevation } => {
                if self.position.is_some() || self.range.is_some() {
                    let field = if self.position.is_some() { "position" } else { "range" };
                    return Err(OpError::invalid(field, format!("'{}' is a directional light, with no {field}", self.light))
                        .hint("a directional light takes azimuth and elevation"));
                }
                *azimuth = self.azimuth.unwrap_or(*azimuth);
                *elevation = self.elevation.unwrap_or(*elevation);
            }
            LightShape::Point { position, range } => {
                if self.azimuth.is_some() || self.elevation.is_some() {
                    let field = if self.azimuth.is_some() { "azimuth" } else { "elevation" };
                    return Err(OpError::invalid(field, format!("'{}' is a point light, with no {field}", self.light))
                        .hint("a point light takes position and range"));
                }
                *position = self.position.unwrap_or(*position);
                *range = self.range.unwrap_or(*range);
            }
        }
        let mut moved = Vec::new();
        if let Some(shadow) = self.shadow {
            if shadow && !light.is_directional() {
                return Err(OpError::invalid("shadow", "only a directional light casts shadows"));
            }
            light.shadow = shadow;
            if shadow {
                moved = take_shadow(lights, &self.light);
            }
        }
        let output = json!({ "scene": edit.id.as_str(), "light": self.light.as_str(), "shadow_taken_from": moved });
        edit.finish(ctx, output)
    }
}

register_op!(SetLight);

/// Removes a light from a scene.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"light": "rim"}], "x-omuzaki-fixture" = "scenes"))]
pub struct RemoveLight {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The light.
    pub light: LightId,
}

impl Op for RemoveLight {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.remove_light",
            summary: "Remove a light from a scene.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let lights = &mut edit.def.lighting.lights;
        let known: Vec<String> = lights.keys().map(ToString::to_string).collect();
        lights.remove(&self.light).ok_or_else(|| unknown("light", "light", self.light.as_str(), known))?;
        let output = json!({ "scene": edit.id.as_str(), "removed": self.light.as_str() });
        edit.finish(ctx, output)
    }
}

register_op!(RemoveLight);

/// The ambient light: sky and ground colours and how strong it is. Only the fields given change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AmbientEdit {
    /// The colour from above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sky: Option<Colour>,
    /// The colour from below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground: Option<Colour>,
    /// Its intensity, 0 or more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f32>,
}

/// Changes the lighting of a scene besides its lights: exposure, background, ambient, bloom, outline light and the
/// skin's shadow floor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"exposure": 1.1},
        {"scene": "night_stream", "ambient": {"intensity": 0.4}, "skin_floor": "off", "bloom": {"strength": 0.6, "radius": 0.4, "threshold": 0.9}}
    ],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetLighting {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The light multiplier before tone mapping, 0 or more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f32>,
    /// The rig's background colour (what a `lighting` backdrop shows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Colour>,
    /// How much the scene's light colours the outlines, 0 or more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline_light: Option<f32>,
    /// The darkest the skin's shadow gets, 0 or more, or `off` for each material's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin_floor: Option<OrOff<f32>>,
    /// The ambient light.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambient: Option<AmbientEdit>,
    /// The glow of bright parts, or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bloom: Option<OrOff<BloomDef>>,
}

impl Op for SetLighting {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_lighting",
            summary: "Change a scene's exposure, background, ambient light, bloom, outline light or skin shadow floor; \
                      only the fields given change.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let l = &mut edit.def.lighting;
        if let Some(exposure) = self.exposure {
            l.exposure = exposure;
        }
        if let Some(background) = self.background {
            l.background = background;
        }
        if let Some(outline_light) = self.outline_light {
            l.outline_light = outline_light;
        }
        if let Some(floor) = &self.skin_floor {
            l.skin_floor = floor.clone().into_option();
        }
        if let Some(ambient) = &self.ambient {
            let Ambient { sky, ground, intensity } = l.ambient;
            l.ambient = Ambient {
                sky: ambient.sky.unwrap_or(sky),
                ground: ambient.ground.unwrap_or(ground),
                intensity: ambient.intensity.unwrap_or(intensity),
            };
        }
        if let Some(bloom) = &self.bloom {
            l.bloom = bloom.clone().into_option();
        }
        let id = edit.id.clone();
        let plan = edit.finish(ctx, serde_json::Value::Null)?;
        let output = json!({ "scene": id.as_str(), "changed": changed(&plan.patch) });
        Ok(Plan { output: Some(output), ..plan })
    }
}

register_op!(SetLighting);

/// Lights a scene with a rig from the library: its lights, ambient, exposure, background, bloom and skin floor, as
/// values the scene owns from then on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"rig": "perform-facecam"}, {"scene": "night_stream", "rig": "viewer-moonlit"}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetLightRig {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The rig, by name (`stage.light_rigs` lists them).
    pub rig: String,
}

impl Op for SetLightRig {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_light_rig",
            summary: "Light a scene with a library rig (stage.light_rigs lists them): replaces its lights, ambient, \
                      exposure, background and bloom in one undoable step.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let names: Vec<&str> = LightRig::names().collect();
        if !names.contains(&self.rig.as_str()) {
            let mut error = OpError::invalid("rig", format!("there is no light rig '{}'", self.rig));
            error = match nearest(&self.rig, names.iter().copied()) {
                Some(near) => error.hint(format!("did you mean '{near}'? the rigs: {}", names.join(", "))),
                None => error.hint(format!("the rigs: {}", names.join(", "))),
            };
            return Err(error);
        }
        let internal = |name: &str, e: &dyn std::fmt::Display| OpError::internal(format!("the library rig '{name}': {e}"));
        let rig = LightRig::load(&self.rig).map_err(|e| internal(&self.rig, &e))?;
        let lighting = rig.to_lighting().map_err(|e| internal(&self.rig, &e))?;
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        // What it replaces: the rig the lighting came from, and whether it was changed since (D3: the rig is a note).
        let previous = edit.def.lighting.rig.clone();
        let edited = previous.as_deref().map(|name| {
            LightRig::load(name).and_then(|r| r.to_lighting()).map_or(true, |stock| stock != edit.def.lighting)
        });
        edit.def.lighting = lighting;
        let output = json!({ "scene": edit.id.as_str(), "rig": self.rig, "title": rig.title,
                             "replaced": { "rig": previous, "edited": edited } });
        edit.finish(ctx, output)
    }
}

register_op!(SetLightRig);

/// Lists the light rigs of the library.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct LightRigs {}

impl Op for LightRigs {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.light_rigs",
            summary: "List the light rigs a scene can be lit with (stage.set_light_rig): name, title, mood, tags and lights.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut rigs = Vec::new();
        for name in LightRig::names() {
            let rig = LightRig::load(name).map_err(|e| OpError::internal(format!("the library rig '{name}': {e}")))?;
            let lighting = rig.to_lighting().map_err(|e| OpError::internal(format!("the rig '{name}': {e}")))?;
            let lights: Vec<String> = lighting.lights.keys().map(ToString::to_string).collect();
            rigs.push(json!({ "name": name, "title": rig.title, "summary": rig.summary, "tags": rig.tags,
                              "lights": lights, "exposure": lighting.exposure, "background": lighting.background,
                              "bloom": lighting.bloom.is_some() }));
        }
        Ok(Plan::answer(json!({ "rigs": rigs })))
    }
}

register_op!(LightRigs);
