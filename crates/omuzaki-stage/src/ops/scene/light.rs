//! A scene's lights (design D3, D8): `stage.add_light`, `stage.set_light`, `stage.remove_light`, and the rest of the
//! lighting with `stage.set_lighting`, `stage.set_light_rig` and `stage.light_rigs`.

use omuzaki_core::text::nearest;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::Colour;
use omuzaki_stage_api::{
    Ambient, Anchor, BloomDef, EnvironmentDef, FogDef, LightDef, LightId, LightRole, LightShape, MAX_LIGHTS, SceneId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::edit::{OrOff, SceneEdit, changed, free_id, unknown};
use crate::recipe::LightRig;

/// The shape of a light: a sun-like directional light, a point light in the room, or a spot light.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LightKind {
    /// Parallel rays from a direction (`azimuth`, `elevation`); the one that casts shadows.
    Directional,
    /// From a point (`position`), fading out at `range`.
    Point,
    /// From a lamp (`position`) toward `target` in a cone (`cone`, `penumbra`), fading out at `range`.
    Spot,
}

/// A spot light's place and beam, as the ops take them: every field optional, so an edit changes only what it names.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct ShapeFields {
    azimuth: Option<f32>,
    elevation: Option<f32>,
    position: Option<[f32; 3]>,
    range: Option<f32>,
    target: Option<[f32; 3]>,
    cone: Option<f32>,
    penumbra: Option<f32>,
}

impl ShapeFields {
    /// The fields given that `kind` doesn't take, by name, first one first.
    fn foreign(&self, kind: LightKind) -> Option<&'static str> {
        let given = [
            ("azimuth", self.azimuth.is_some(), kind == LightKind::Directional),
            ("elevation", self.elevation.is_some(), kind == LightKind::Directional),
            ("position", self.position.is_some(), kind != LightKind::Directional),
            ("range", self.range.is_some(), kind != LightKind::Directional),
            ("target", self.target.is_some(), kind == LightKind::Spot),
            ("cone", self.cone.is_some(), kind == LightKind::Spot),
            ("penumbra", self.penumbra.is_some(), kind == LightKind::Spot),
        ];
        given.into_iter().find(|(_, present, takes)| *present && !takes).map(|(name, ..)| name)
    }
}

fn kind_of(shape: &LightShape) -> LightKind {
    match shape {
        LightShape::Directional { .. } => LightKind::Directional,
        LightShape::Point { .. } => LightKind::Point,
        LightShape::Spot { .. } => LightKind::Spot,
    }
}

fn kind_name(kind: LightKind) -> &'static str {
    match kind {
        LightKind::Directional => "directional",
        LightKind::Point => "point",
        LightKind::Spot => "spot",
    }
}

/// What a light of `kind` takes, for a hint.
fn takes(kind: LightKind) -> &'static str {
    match kind {
        LightKind::Directional => "a directional light takes azimuth and elevation",
        LightKind::Point => "a point light takes position and range",
        LightKind::Spot => "a spot light takes position, target, cone, penumbra and range",
    }
}

/// Adds a light to a scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"kind": "directional", "role": "accent", "colour": "#ffd0e0", "azimuth": -120.0, "elevation": 20.0},
        {"scene": "night_stream", "kind": "point", "role": "practical", "position": [1.2, 1.4, -0.8], "range": 4.0, "intensity": 3.0},
        {"kind": "spot", "role": "accent", "position": [0.0, 2.6, 1.2], "target": [0.0, 1.0, 0.0], "cone": 25.0, "penumbra": 0.3, "intensity": 6.0}
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
    /// `directional`, `point` or `spot`.
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
    /// Whether it casts shadows: one directional light per scene, so on a directional light this takes it from the
    /// light that has it; on a spot it is kept for renderers that draw spot shadows. Not for point lights.
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
    /// A point or spot light's position from its anchor, in subject units (metres on a 1 m figure: x right, y up, z
    /// toward the front camera).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// A point or spot light's reach, in subject units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<f32>,
    /// What a spot light points at, in subject units; the subject's chest when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<[f32; 3]>,
    /// A spot light's cone, degrees from its axis to its edge, above 0 and up to 90; 30 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cone: Option<f32>,
    /// How soft a spot light's edge is, 0 (hard) to 1 (fading from the axis); 0.2 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub penumbra: Option<f32>,
}

/// The shape `kind` makes of the given fields, refusing the fields that belong to another kind.
fn shape(kind: LightKind, f: ShapeFields) -> Result<LightShape, OpError> {
    if let Some(field) = f.foreign(kind) {
        return Err(OpError::invalid(field, format!("a {} light has no {field}", kind_name(kind))).hint(takes(kind)));
    }
    Ok(match kind {
        LightKind::Directional => {
            LightShape::Directional { azimuth: f.azimuth.unwrap_or(0.0), elevation: f.elevation.unwrap_or(30.0) }
        }
        LightKind::Point => {
            LightShape::Point { position: f.position.unwrap_or([0.0, 1.5, 1.0]), range: f.range.unwrap_or(5.0) }
        }
        // Above and in front of the subject, aimed at its chest, wide enough for the upper body.
        LightKind::Spot => LightShape::Spot {
            position: f.position.unwrap_or([0.0, 2.4, 1.2]),
            target: f.target.unwrap_or([0.0, 0.75, 0.0]),
            cone: f.cone.unwrap_or(30.0),
            penumbra: f.penumbra.unwrap_or(0.2),
            range: f.range.unwrap_or(6.0),
        },
    })
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
            summary: "Add a directional, point or spot light to a scene; a shadow on a directional light moves the \
                      scene's one shadow to it.",
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
            return Err(OpError::invalid("shadow", "a point light casts no shadow")
                .hint("a directional or spot light can cast one"));
        }
        let fields = ShapeFields {
            azimuth: self.azimuth,
            elevation: self.elevation,
            position: self.position,
            range: self.range,
            target: self.target,
            cone: self.cone,
            penumbra: self.penumbra,
        };
        let light = LightDef {
            role: self.role,
            colour: self.colour.unwrap_or(Colour::WHITE),
            intensity: self.intensity.unwrap_or(1.0),
            shadow: self.shadow,
            anchor: self.anchor.clone().unwrap_or_default(),
            shape: shape(self.kind, fields)?,
        };
        lights.insert(id.clone(), light);
        let moved =
            if self.shadow && self.kind == LightKind::Directional { take_shadow(lights, &id) } else { Vec::new() };
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
    /// Whether it casts shadows (not a point light); on a directional light `true` takes the scene's one shadow from
    /// the light that had it.
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
    /// A point or spot light's position, subject units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// A point or spot light's reach, subject units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<f32>,
    /// What a spot light points at, subject units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<[f32; 3]>,
    /// A spot light's cone, degrees from its axis to its edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cone: Option<f32>,
    /// How soft a spot light's edge is, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub penumbra: Option<f32>,
}

impl Op for SetLight {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_light",
            summary: "Change a light of a scene: colour, intensity, direction or position, a spot's target and cone, \
                      role, anchor, shadow. Only the fields given change.",
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
        let fields = ShapeFields {
            azimuth: self.azimuth,
            elevation: self.elevation,
            position: self.position,
            range: self.range,
            target: self.target,
            cone: self.cone,
            penumbra: self.penumbra,
        };
        let kind = kind_of(&light.shape);
        if let Some(field) = fields.foreign(kind) {
            return Err(OpError::invalid(
                field,
                format!("'{}' is a {} light, with no {field}", self.light, kind_name(kind)),
            )
            .hint(takes(kind)));
        }
        match &mut light.shape {
            LightShape::Directional { azimuth, elevation } => {
                *azimuth = fields.azimuth.unwrap_or(*azimuth);
                *elevation = fields.elevation.unwrap_or(*elevation);
            }
            LightShape::Point { position, range } => {
                *position = fields.position.unwrap_or(*position);
                *range = fields.range.unwrap_or(*range);
            }
            LightShape::Spot { position, target, cone, penumbra, range } => {
                *position = fields.position.unwrap_or(*position);
                *target = fields.target.unwrap_or(*target);
                *cone = fields.cone.unwrap_or(*cone);
                *penumbra = fields.penumbra.unwrap_or(*penumbra);
                *range = fields.range.unwrap_or(*range);
            }
        }
        let mut moved = Vec::new();
        if let Some(shadow) = self.shadow {
            if shadow && kind == LightKind::Point {
                return Err(OpError::invalid("shadow", "a point light casts no shadow")
                    .hint("a directional or spot light can cast one"));
            }
            light.shadow = shadow;
            if shadow && kind == LightKind::Directional {
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

/// Changes the lighting of a scene besides its lights: exposure, background, ambient, bloom, outline light, the skin's
/// shadow floor, fog and the environment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"exposure": 1.1},
        {"scene": "night_stream", "ambient": {"intensity": 0.4}, "skin_floor": "off", "bloom": {"strength": 0.6, "radius": 0.4, "threshold": 0.9}},
        {"fog": {"colour": "#2a2238", "near": 2.0, "far": 9.0}, "environment": {"kind": "sky", "zenith": "#6f86b8", "horizon": "#e8c9a0", "ground": "#3b3026", "intensity": 0.6}}
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
    /// How strongly lights behind the cast edge its silhouette in their colour (a stage's back light), 0 or more; 0
    /// lights with the toon ramp alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back_light_rim: Option<f32>,
    /// The darkest the skin's shadow gets, 0 or more, or `off` for each material's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin_floor: Option<OrOff<f32>>,
    /// The ambient light.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambient: Option<AmbientEdit>,
    /// The glow of bright parts, or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bloom: Option<OrOff<BloomDef>>,
    /// Distance fog (`colour`, `near` and `far` in subject units from the camera), or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fog: Option<OrOff<FogDef>>,
    /// Image-based light, or `off`: `kind` `sky` (with `zenith`, `horizon`, `ground` colours) or `hdri` (with `asset`,
    /// a stored Radiance `.hdr`), and `intensity`, `rotation` (degrees about the vertical) and `toon_ramp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<OrOff<EnvironmentDef>>,
}

impl Op for SetLighting {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_lighting",
            summary: "Change a scene's exposure, background, ambient light, bloom, outline light, back-light rim, skin \
                      shadow floor, fog or environment (a sky or an HDRI that lights the cast and can be the \
                      backdrop); only the fields given change.",
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
        if let Some(back_light_rim) = self.back_light_rim {
            l.back_light_rim = back_light_rim;
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
        if let Some(fog) = &self.fog {
            l.fog = fog.clone().into_option();
        }
        if let Some(environment) = &self.environment {
            l.environment = environment.clone().into_option();
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
        let internal =
            |name: &str, e: &dyn std::fmt::Display| OpError::internal(format!("the library rig '{name}': {e}"));
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
