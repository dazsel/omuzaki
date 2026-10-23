//! `face.set_profile`: blend a face shape into a character's head outline and feature layout in one patch.

use omuzaki_char_api::{CharId, CharacterDef, HeadParams, PartId, ShapeProfile, paths};
use omuzaki_doc_api::Change;
use omuzaki_face_api::{ParamErrors, Validate};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::report;
use crate::head::presets::{ShapeProfileError, ShapeProfiles};
use crate::ops::character::access;
use crate::ops::layout::chin::{head_params, head_part};
use crate::ops::layout::edit::{changed_fields, layout_changes, refuse_new_problems};

/// The part kind the head generator registers.
const HEAD_KIND: &str = "head";

/// The face shape to blend toward: a preset's id, or a profile written out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ProfileSource {
    /// A preset's id: `round-u`, `egg-oval`, `mochi`, `heart` or `long` (`face.list_profiles`).
    Preset(String),
    /// A head outline and a feature layout given in full, in head-local metres.
    Inline(Box<ShapeProfile>),
}

/// Blends a face shape (CB-10) into a character: the head's outline and the face's feature layout move together, in one
/// patch, so one undo restores both. `blend` 1 (the default) takes the shape in full and records the preset on the
/// head; 0 changes nothing. Answers with the head's silhouette and the layout's ratios before and after.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "profile": "egg-oval"},
        {"character": "kit", "profile": "mochi", "blend": 0.5}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct FaceSetProfile {
    /// The character whose face shape changes.
    pub character: CharId,
    /// The shape: a preset's id, or `{outline, layout}` written out.
    pub profile: ProfileSource,
    /// How far to move from the current shape toward `profile`, from 0 (unchanged) to 1 (exactly the profile).
    #[serde(default = "full")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub blend: f64,
    /// The head part to reshape; by default the character's one enabled head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<PartId>,
}

fn full() -> f64 {
    1.0
}

impl Op for FaceSetProfile {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.set_profile",
            summary: "Blend a face-shape preset (round-u, egg-oval, mochi, heart, long) or an inline profile into a \
                      character's head outline and feature layout in one patch; answers with the silhouette and \
                      layout ratios before and after",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(0.0..=1.0).contains(&self.blend) {
            return Err(OpError::invalid("blend", format!("{} is not between 0 and 1", self.blend))
                .hint("give a blend from 0 (unchanged) to 1 (exactly the profile)"));
        }
        let (preset, profile) = self.target()?;
        let def = access::load(ctx, &self.character)?;
        let (part, head) = self.head(&def)?;
        let current = ShapeProfile::of(&head, &def.face);
        let after = if self.blend == 0.0 { current } else { current.lerp(&profile, self.blend) };

        refuse_new_problems(&current.layout, &after.layout)?;
        let mut changes = Vec::new();
        if after.outline != current.outline {
            changes.extend(self.outline_change(ctx, &part, &after)?);
        }
        if self.blend > 0.0 {
            changes.extend(self.preset_change(ctx, &part, preset.filter(|_| self.blend >= 1.0)));
        }
        let fields = changed_fields(&current.layout, &after.layout)?;
        changes.extend(layout_changes(ctx, &self.character, &fields)?);

        let written: Vec<String> = changes.iter().map(|c| c.path.to_string()).collect();
        Ok(access::edit_plan(
            changes,
            json!({
                "character": self.character,
                "part": part,
                "profile": preset.unwrap_or("inline"),
                "blend": self.blend,
                "before": report::numbers(&head, &current),
                "after": report::numbers(&head, &after),
                "changed": written,
            }),
        ))
    }
}

impl FaceSetProfile {
    /// The preset's id (`None` for an inline profile) and the profile to blend toward.
    fn target(&self) -> Result<(Option<&str>, ShapeProfile), OpError> {
        match &self.profile {
            ProfileSource::Preset(id) => match ShapeProfiles::get(id) {
                Ok(preset) => Ok((Some(id.as_str()), preset.profile)),
                Err(ShapeProfileError::Unknown(_)) => {
                    Err(OpError::invalid("profile", format!("there is no face-shape preset called `{id}`")).hint(
                        format!(
                            "use one of {}, or write the profile out as {{outline, layout}}; face.list_profiles \
                             lists them",
                            omuzaki_char_api::PRESET_IDS.join(", ")
                        ),
                    ))
                }
                Err(e) => Err(OpError::internal(e.to_string())),
            },
            ProfileSource::Inline(profile) => {
                invalid("profile/outline", profile.outline.validate())?;
                invalid("profile/layout", profile.layout.validate())?;
                Ok((None, **profile))
            }
        }
    }

    /// The head part to reshape and its parameters.
    fn head(&self, def: &CharacterDef) -> Result<(PartId, HeadParams), OpError> {
        let Some(id) = &self.part else {
            return head_part(def, &self.character).map(|(id, params)| (id.clone(), params));
        };
        match def.parts.get(id) {
            Some(part) if part.kind.as_str() == HEAD_KIND => head_params(id, part).map(|params| (id.clone(), params)),
            Some(part) => Err(OpError::invalid("part", format!("part `{id}` is a {}, not a head", part.kind))
                .hint("name the character's head part, or leave `part` out to use it")),
            None => Err(access::not_found("part", format!("character `{}` has no part `{id}`", self.character))
                .hint("leave `part` out to use the character's head")),
        }
    }

    /// The change that writes `after`'s outline into the head's parameters, creating the parameters' table if the head
    /// has none.
    fn outline_change(&self, ctx: &OpCtx<'_>, part: &PartId, after: &ShapeProfile) -> Result<Vec<Change>, OpError> {
        let path = paths::part_params(&self.character, part).join("outline");
        let value = serde_json::to_value(after.outline)
            .map_err(|e| OpError::internal(format!("a head outline did not serialise: {e}")))?;
        Ok(if ctx.doc.get(&path).is_some() { vec![access::change(ctx, path, Some(value))] } else { access::create(ctx, path, value) })
    }

    /// The change that records `preset` on the head, or clears a recorded one that the new outline no longer is.
    fn preset_change(&self, ctx: &OpCtx<'_>, part: &PartId, preset: Option<&str>) -> Vec<Change> {
        let path = paths::part_params(&self.character, part).join("preset");
        let stored = ctx.doc.get(&path);
        let wanted = preset.map(|id| Value::String(id.to_owned()));
        match (stored, wanted) {
            (Some(now), Some(want)) if *now == want => Vec::new(),
            (None, None) => Vec::new(),
            (Some(_), want) => vec![access::change(ctx, path, want)],
            (None, Some(want)) => access::create(ctx, path, want),
        }
    }
}

/// `Invalid` naming the first of `errors` under `prefix`, listing them all.
fn invalid(prefix: &str, result: Result<(), ParamErrors>) -> Result<(), OpError> {
    let Err(ParamErrors(errors)) = result else { return Ok(()) };
    let list: Vec<String> = errors.iter().map(|e| format!("{prefix}/{}: {}", e.path, e.message)).collect();
    let first = errors.first().map_or_else(|| prefix.to_owned(), |e| format!("{prefix}/{}", e.path));
    Err(OpError::invalid(first, list.join("; ")).hint("give each field a value inside the range its message gives"))
}

register_op!(FaceSetProfile);
