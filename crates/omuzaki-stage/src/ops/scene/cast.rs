//! A scene's cast (design D6, D8): `stage.place_character`, `stage.set_member`, `stage.remove_member` and
//! `stage.set_subject`.

use omuzaki_char_api::{CharId, VariantId, paths as char_paths};
use omuzaki_core::text::nearest;
use omuzaki_doc_api::{DocPath, DocRead, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::{Anchor, CastMember, MAX_CAST, MemberId, SceneDef, SceneId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::edit::{OrOff, SceneEdit, free_id, unknown};

/// How far to the right of the rightmost member a character placed without a position stands, metres.
pub const PLACE_GAP_M: f32 = 0.7;

/// Where a member placed without a position stands: [`PLACE_GAP_M`] right of the rightmost member, or the stage
/// centre for the first.
#[must_use]
pub fn next_place(def: &SceneDef) -> [f32; 2] {
    def.cast.values().map(|m| m.position[0]).reduce(f32::max).map_or([0.0, 0.0], |x| [x + PLACE_GAP_M, 0.0])
}

/// The character `id`, refused with the nearest one the project has.
fn require_character(doc: &dyn DocRead, field: &str, id: &CharId) -> Result<(), OpError> {
    if doc.get(&char_paths::character(id)).is_some() {
        return Ok(());
    }
    let known: Vec<String> = doc
        .get(&DocPath::root().join("characters"))
        .and_then(Value::as_object)
        .map(|c| c.keys().cloned().collect())
        .unwrap_or_default();
    let error = OpError::invalid(field, format!("there is no character '{id}'"));
    Err(match nearest(id.as_str(), known.iter().map(String::as_str)) {
        Some(near) => error.hint(format!("did you mean '{near}'? character.list lists the project's characters")),
        None => error.hint("character.list lists the project's characters"),
    })
}

/// The variant `variant` of `character`, refused with the nearest one it has.
fn require_variant(doc: &dyn DocRead, character: &CharId, variant: &VariantId) -> Result<(), OpError> {
    let variants = doc.get(&char_paths::variants(character)).and_then(Value::as_object);
    if variants.is_some_and(|v| v.contains_key(variant.as_str())) {
        return Ok(());
    }
    let known: Vec<&str> = variants.map(|v| v.keys().map(String::as_str).collect()).unwrap_or_default();
    let error = OpError::invalid("variant", format!("'{character}' has no variant '{variant}'"));
    Err(match nearest(variant.as_str(), known.iter().copied()) {
        Some(near) => error.hint(format!("did you mean '{near}'? its variants: {}", known.join(", "))),
        None if known.is_empty() => error.hint(format!("'{character}' has no variants")),
        None => error.hint(format!("its variants: {}", known.join(", "))),
    })
}

/// The member `member`, refused with the nearest one the scene has.
fn require_member(def: &SceneDef, field: &str, member: &MemberId) -> Result<(), OpError> {
    if def.cast.contains_key(member) {
        return Ok(());
    }
    Err(unknown(field, "member", member.as_str(), def.cast.keys().map(ToString::to_string)))
}

/// Places a character in a scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"scene": "night_stream", "character": "kit", "id": "host", "position": [-0.4, 0.2], "yaw": 15.0}
    ],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct PlaceCharacter {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The character.
    pub character: CharId,
    /// The member's id; the character's id (then `-2`, `-3`...) when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<MemberId>,
    /// Metres on the floor, x toward screen right and z toward the front camera; 0.7 m right of the rightmost member
    /// when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 2]>,
    /// Degrees, -180 to 180; 0 faces the front.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    /// A variant of the character to show here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<VariantId>,
}

impl Op for PlaceCharacter {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.place_character",
            summary: "Place a character in a scene as a cast member, at a position (or 0.7 m right of the rightmost \
                      member), facing a yaw, optionally as one of its variants.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let doc = ctx.doc;
        let mut edit = SceneEdit::open(doc, self.scene.as_ref())?;
        if edit.def.cast.len() >= MAX_CAST {
            return Err(OpError::invalid(
                "character",
                format!("the scene has {} cast members, the most a scene can have", edit.def.cast.len()),
            )
            .hint("remove one with stage.remove_member"));
        }
        require_character(doc, "character", &self.character)?;
        if let Some(variant) = &self.variant {
            require_variant(doc, &self.character, variant)?;
        }
        let cast = &edit.def.cast;
        let id = match &self.id {
            Some(id) if cast.contains_key(id) => {
                return Err(OpError::conflict(format!("the scene already has a member '{id}'"))
                    .hint("pick another id, or move that member with stage.set_member"));
            }
            Some(id) => id.clone(),
            None => {
                let taken = |s: &str| {
                    cast.keys().any(|k| k.as_str() == s) || MemberId::new(s).map_or(true, |m| m.is_reserved())
                };
                let base = if MemberId::new(self.character.as_str()).is_ok_and(|m| !m.is_reserved()) {
                    self.character.as_str()
                } else {
                    "member"
                };
                let free = free_id(base, &taken);
                MemberId::new(&free).map_err(|e| OpError::internal(format!("member id '{free}': {e}")))?
            }
        };
        let position = self.position.unwrap_or_else(|| next_place(&edit.def));
        let mut member = CastMember::new(self.character.clone());
        member.position = position;
        member.yaw = self.yaw.unwrap_or(0.0);
        member.variant.clone_from(&self.variant);
        edit.def.cast.insert(id.clone(), member);
        let output = json!({ "scene": edit.id.as_str(), "member": id.as_str(), "position": position });
        edit.finish(ctx, output)
    }
}

register_op!(PlaceCharacter);

/// Changes a cast member: only the fields given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"member": "main", "position": [0.3, -0.2]}, {"member": "main", "yaw": -20.0, "visible": false}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetMember {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The member.
    pub member: MemberId,
    /// Metres on the floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 2]>,
    /// Degrees; 0 faces the front.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    /// A variant of its character, or `off` for the character as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<OrOff<VariantId>>,
    /// Whether it is drawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Another character in its place (its variant is dropped unless one is given).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<CharId>,
}

impl Op for SetMember {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_member",
            summary: "Change a cast member: position, yaw, variant, visibility, or which character it is. Only the \
                      fields given change.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let doc = ctx.doc;
        let mut edit = SceneEdit::open(doc, self.scene.as_ref())?;
        require_member(&edit.def, "member", &self.member)?;
        if let Some(character) = &self.character {
            require_character(doc, "character", character)?;
        }
        let Some(member) = edit.def.cast.get_mut(&self.member) else {
            return Err(OpError::internal("the member went missing"));
        };
        if let Some(character) = &self.character
            && *character != member.character
        {
            member.character = character.clone();
            member.variant = None;
        }
        if let Some(variant) = &self.variant {
            member.variant = variant.clone().into_option();
        }
        if let Some(variant) = &member.variant {
            require_variant(doc, &member.character, variant)?;
        }
        member.position = self.position.unwrap_or(member.position);
        member.yaw = self.yaw.unwrap_or(member.yaw);
        member.visible = self.visible.unwrap_or(member.visible);
        let output = json!({ "scene": edit.id.as_str(), "member": self.member.as_str() });
        edit.finish(ctx, output)
    }
}

register_op!(SetMember);

/// Removes a member from a scene. Lights and cameras placed against it follow the subject instead, and if it was the
/// subject the next member becomes the subject, all in the same step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"member": "main"}], "x-omuzaki-fixture" = "scenes"))]
pub struct RemoveMember {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The member.
    pub member: MemberId,
}

impl Op for RemoveMember {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.remove_member",
            summary: "Remove a cast member; lights and cameras anchored to it follow the subject, and if it was the \
                      subject the next member becomes the subject, in the same undoable step.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        require_member(&edit.def, "member", &self.member)?;
        let def = &mut edit.def;
        def.cast.remove(&self.member);
        let gone = Anchor::Member(self.member.clone());
        let mut repointed = Vec::new();
        for (id, light) in &mut def.lighting.lights {
            if light.anchor == gone {
                light.anchor = Anchor::Subject;
                repointed.push(format!("lighting.lights.{id}.anchor"));
            }
        }
        for (id, camera) in &mut def.cameras {
            if camera.subject == gone {
                camera.subject = Anchor::Subject;
                repointed.push(format!("cameras.{id}.subject"));
            }
            if camera.anchor.as_ref() == Some(&gone) {
                camera.anchor = Some(Anchor::Subject);
                repointed.push(format!("cameras.{id}.anchor"));
            }
        }
        if def.scene.subject.as_ref() == Some(&self.member) {
            def.scene.subject = def.cast.keys().next().cloned();
        }
        let subject = def.scene.subject.clone();
        let output = json!({ "scene": edit.id.as_str(), "removed": self.member.as_str(), "subject": subject,
                             "repointed_to_subject": repointed });
        edit.finish(ctx, output)
    }
}

register_op!(RemoveMember);

/// Picks the member a scene's framings and subject-anchored lights follow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"member": "main"}], "x-omuzaki-fixture" = "scenes"))]
pub struct SetSubject {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The member.
    pub member: MemberId,
}

impl Op for SetSubject {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_subject",
            summary: "Pick the cast member a scene's camera framings and subject-anchored lights follow.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        require_member(&edit.def, "member", &self.member)?;
        edit.def.scene.subject = Some(self.member.clone());
        let output = json!({ "scene": edit.id.as_str(), "subject": self.member.as_str() });
        edit.finish(ctx, output)
    }
}

register_op!(SetSubject);
