//! `character.create`: a new character from a body template, with the default parts or none.

use std::collections::BTreeMap;
use std::fmt::Display;

use omuzaki_char_api::{
    BodyTemplateId, CharId, CharacterDef, PartDef, PartId, PartKindId, find_kind, paths,
    requires_of,
};
use omuzaki_core::colour::Colour;
use omuzaki_doc_api::Value;
use omuzaki_face_api::{ExpressionSet, Palette, PaletteSlot, Validate};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access::{self, list};
use super::tree;
use crate::style;
use crate::style::merge;
use crate::template::registry;
use crate::template::{Template, TemplateDefaults};

/// Which parts a new character starts with.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StartingParts {
    /// The kit's default character: a body with five-finger hands, a head, hair, and an outfit of a shirt, a pleated
    /// skirt, tights and boots (ids `body`, `head`, `hair`, `shirt`, `skirt`, `legwear`, `boots`), on the template's own
    /// proportions: the character builds and renders straight away, dressed.
    #[default]
    Defaults,
    /// No parts at all: add each one with `part.add`.
    Empty,
}

/// Creates a character from a body template.
///
/// With `parts: "defaults"` (the default) the character starts as the kit's default character: a body with five-finger
/// hands, a head with open, round eyes, a brown bob with face-framing locks, and an outfit of a shirt, a pleated skirt,
/// tights and boots, on the template's own proportions and head, so it builds and renders at once, dressed.
/// With `parts: "empty"` it has no parts: add them one by one with `part.add`. Either way the palette starts with the
/// three standard skin colours (`skin`, `skin_shade`, `skin_line`) that the head and body refer to, and the face starts
/// as the template's, with the built-in expression library (moods, accents, visemes and blink) when the template brings
/// none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"id": "kit", "name": "Kit", "template": "standard", "seed": 7}
    ],
    "x-omuzaki-fixture" = "demo"
))]
pub struct CharacterCreate {
    /// The new character's id: lowercase letters, digits, `-` and `_`, starting with a letter or digit, at most 64
    /// characters. It names the character's folder in the project, so it never changes. Must not be in use.
    pub id: CharId,
    /// The display name: any non-empty text, shown in the UI. Rename it later with `character.rename`.
    #[schemars(length(min = 1))]
    pub name: String,
    /// The body template, which fixes the skeleton and the body's proportions: `soft-sd` (the chibi default), `mini`,
    /// `standard` or `tall`, among the templates this build has.
    pub template: BodyTemplateId,
    /// Seeds every procedural part, so rebuilding the character gives the same result. A whole number from 0 to
    /// 4294967295; 0 when left out.
    #[serde(default)]
    pub seed: u32,
    /// Which parts to start with: `defaults` (body, head, hair, shirt, skirt, legwear and boots; the default) or
    /// `empty` (none).
    #[serde(default)]
    pub parts: StartingParts,
    /// A character style to start from (`character.list_styles` lists them; `mature` on `standard` or `tall`): its
    /// face design, face shape, neck and proportions, and with the default parts the parts it brings (the mature
    /// style's braided locks and ahoge). Left out, the kit's own design.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
}

/// The skin colours new characters start with, under the palette slots the head and body parameters name (`@skin`,
/// `@skin_shade`, `@skin_line`). Without them the starting parts could not resolve their colours.
const STARTING_SKIN: [(&str, Colour); 3] = [
    ("skin", Colour::rgb(0xf9, 0xd3, 0xc5)),
    ("skin_shade", Colour::rgb(0xf2, 0xb4, 0xab)),
    ("skin_line", Colour::rgb(0xc9, 0x8a, 0x80)),
];

/// The kit's default character (`library/starter.toml`): the parts `parts: "defaults"` creates and what it changes of
/// their defaults and of the template's face.
const STARTER: &str = include_str!("../../../library/starter.toml");

/// The starter file: the face changes, then the parts in the order they are listed. Each builds after the parts it
/// reads (hair after the head, the outfit after the body), whatever the order here.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Starter {
    /// Merged over the template's face.
    #[serde(default)]
    face: Value,
    parts: Vec<StarterPart>,
}

/// One starting part: its id, its kind, and what it changes of its defaults.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StarterPart {
    id: String,
    kind: String,
    /// Merged over the part's defaults: the template's own numbers for `body` and `head`, the kind's otherwise.
    #[serde(default)]
    params: Value,
}

/// The starter, parsed once per call (it is small, and `character.create` is not a hot path).
fn starter() -> Result<Starter, OpError> {
    toml::from_str(STARTER).map_err(|e| OpError::internal(format!("the built-in starter (library/starter.toml): {e}")))
}

/// A built-in id that turned out not to be a valid slug: a bug in the starter file, not in the caller's arguments.
fn built_in<T, E: Display>(what: &str, id: &str, made: Result<T, E>) -> Result<T, OpError> {
    made.map_err(|e| OpError::internal(format!("the built-in {what} id `{id}` isn't a valid id: {e}")))
}

impl StarterPart {
    /// The part, with parameters normalised through its kind.
    fn part(&self, defaults: &TemplateDefaults, template: &str) -> Result<(PartId, PartDef), OpError> {
        let kind = find_kind(&self.kind).ok_or_else(|| {
            OpError::unsupported(format!("this build has no `{}` part kind, which the default parts need", self.kind))
                .hint("create the character with parts=\"empty\" and add the parts this build offers with part.add")
        })?;
        let base = match self.kind.as_str() {
            "body" => serde_json::to_value(&defaults.body),
            "head" => serde_json::to_value(&defaults.head),
            _ => Ok((kind.defaults)()),
        };
        let mut raw = base.map_err(|e| {
            OpError::internal(format!("serialising the `{}` defaults of template `{template}` failed: {e}", self.kind))
        })?;
        merge(&mut raw, &self.params);
        let params = (kind.check)(&raw).map_err(|e| {
            OpError::internal(format!(
                "the starter gives `{}` parameters its own kind rejects on template `{template}`: {e}",
                self.kind
            ))
        })?;
        let part = PartDef {
            kind: built_in("part kind", &self.kind, PartKindId::new(&self.kind))?,
            parent: None,
            enabled: true,
            layer: tree::layer_of(kind),
            attach: None,
            params,
        };
        Ok((built_in("part", &self.id, PartId::new(&self.id))?, part))
    }
}

/// The palette new characters start with.
fn starting_palette() -> Palette {
    Palette(
        STARTING_SKIN
            .iter()
            .filter_map(|(slot, colour)| PaletteSlot::new(*slot).ok().map(|slot| (slot, *colour)))
            .collect(),
    )
}

/// The template called `id`.
fn find_template(id: &BodyTemplateId) -> Result<&'static dyn Template, OpError> {
    registry::find(id.as_str()).ok_or_else(|| {
        let known = registry::templates().iter().map(|t| t.id()).collect::<Vec<_>>();
        OpError::invalid("template", format!("there is no body template `{id}`"))
            .hint(format!("the templates in this build are: {}", list(known)))
    })
}

impl CharacterCreate {
    /// The character this op stores.
    fn definition(&self, template: &dyn Template) -> Result<CharacterDef, OpError> {
        let mut defaults = template.defaults();
        // A template that brings no expressions gets the built-in library, so a new face can emote and lip-sync.
        if defaults.face.expressions.0.is_empty() {
            defaults.face.expressions = ExpressionSet::library()
                .map_err(|e| OpError::internal(format!("the built-in expression library doesn't read: {e}")))?;
        }
        let starter = starter()?;
        if matches!(self.parts, StartingParts::Defaults) && !starter.face.is_null() {
            let mut face = serde_json::to_value(&defaults.face)
                .map_err(|e| OpError::internal(format!("serialising the template's face failed: {e}")))?;
            merge(&mut face, &starter.face);
            defaults.face = serde_json::from_value(face)
                .map_err(|e| OpError::internal(format!("the built-in starter's face doesn't read: {e}")))?;
        }
        defaults.face.validate().map_err(|e| {
            OpError::internal(format!("the default face of template `{}` is invalid: {e}", template.id()))
        })?;
        let style = self.style.as_deref().map(style::find).transpose().map_err(super::super::style::style_error)?;
        let mut starting = starter.parts;
        if let (StartingParts::Defaults, Some(style)) = (self.parts, style) {
            starting.retain(|part| !style.without.contains(&part.id));
            for extra in &style.parts {
                match starting.iter_mut().find(|part| part.id == extra.id) {
                    Some(part) => merge(&mut part.params, &extra.params),
                    None => starting.push(StarterPart {
                        id: extra.id.clone(),
                        kind: extra.kind.clone(),
                        params: extra.params.clone(),
                    }),
                }
            }
        }
        let parts = match self.parts {
            StartingParts::Defaults => {
                starting.iter().map(|part| part.part(&defaults, template.id())).collect::<Result<_, _>>()?
            }
            StartingParts::Empty => BTreeMap::new(),
        };
        let mut def = CharacterDef::new(self.name.trim().to_owned(), self.template.clone())
            .with_seed(self.seed)
            .with_palette(starting_palette())
            .with_skin_tone(None)
            .with_parts(parts)
            .with_face(defaults.face)
            .with_weight_brushes(BTreeMap::new());
        if let Some(style) = style {
            // An empty character has no head or body for the style to change: it gets the face design alone.
            if matches!(self.parts, StartingParts::Empty) {
                return Err(OpError::invalid("style", "a style needs the default parts (it changes the head and body)")
                    .hint("leave `parts` out, or create the character empty and apply the style once it has a head and body"));
            }
            style.apply(&mut def).map_err(super::super::style::style_error)?;
        }
        Ok(def)
    }
}

/// The kit's default character on `template` (what `character.create` with the default parts stores), for checks that
/// need a real, dressed character to build every kind in (`builder::conformance::check_linked`).
///
/// # Errors
/// The op's error for an unknown template or a built-in starter that doesn't read.
pub fn default_character(template: &BodyTemplateId) -> Result<CharacterDef, OpError> {
    let create = CharacterCreate {
        id: built_in("character id", "kit", CharId::new("kit"))?,
        name: "Kit".to_owned(),
        template: template.clone(),
        seed: 0,
        parts: StartingParts::Defaults,
        style: None,
    };
    create.definition(find_template(template)?)
}

impl Op for CharacterCreate {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.create",
            summary: "Create a character from a body template, dressed in the default parts or with none.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let path = paths::character(&self.id);
        if ctx.doc.get(&path).is_some() {
            return Err(OpError::invalid("id", format!("a character `{}` already exists", self.id))
                .hint("choose another id, or change the existing character with the part.* ops"));
        }
        if self.name.trim().is_empty() {
            return Err(OpError::invalid("name", "the name is empty")
                .hint("give the character a display name, for example \"Mira\""));
        }
        let template = find_template(&self.template)?;
        let def = self.definition(template)?;
        let problems = tree::problems(&def);
        if !problems.is_empty() {
            return Err(OpError::internal(format!(
                "the starting parts of template `{}` can't be built: {}",
                self.template,
                tree::describe(&problems)
            )));
        }
        let order = def.build_order(&requires_of).map_err(|e| OpError::internal(e.to_string()))?;
        let mut stored = Value::Null;
        def.to_node(&mut stored)
            .map_err(|e| OpError::internal(format!("serialising the new character failed: {e}")))?;
        let summary = json!({
            "character": self.id,
            "name": def.name,
            "template": self.template,
            "seed": self.seed,
            "parts": order,
        });
        Ok(access::edit_plan(access::create(ctx, path, stored), summary))
    }
}

register_op!(CharacterCreate);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starting_palette_has_the_three_skin_slots_the_defaults_name() {
        let palette = starting_palette();
        assert_eq!(palette.0.len(), 3, "a slot name that isn't a valid slug would be dropped silently");
        let head = omuzaki_char_api::HeadParams::default();
        for colour in [&head.skin.base, &head.skin.shade, &head.skin.line] {
            assert!(palette.resolve(colour).is_ok(), "the head's default {colour} must resolve");
        }
    }

    #[test]
    fn the_starter_reads_and_its_ids_are_valid_slugs() {
        let starter = starter().unwrap_or_else(|e| panic!("{e}"));
        assert!(!starter.parts.is_empty());
        for part in &starter.parts {
            assert!(PartId::new(&part.id).is_ok() && PartKindId::new(&part.kind).is_ok(), "{}", part.id);
        }
    }

    #[test]
    fn merging_sets_the_fields_it_names_and_keeps_the_rest() {
        let mut base = json!({"a": 1, "b": {"c": 2, "d": [1, 2]}, "e": "x"});
        merge(&mut base, &json!({"b": {"c": 3, "d": [9]}, "f": true}));
        assert_eq!(base, json!({"a": 1, "b": {"c": 3, "d": [9]}, "e": "x", "f": true}));
    }
}
