//! Style ops: `character.set_style` applies a character style (`crate::style`) to a character by an amount,
//! `character.mix_styles` blends several by weight, each in one undoable patch, and `character.list_styles` lists them.

use omuzaki_char_api::{CharId, paths};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ops::character::access::{self, list};
use crate::ops::look::access as look_access;
use crate::style::{self, Options, Style, StyleError};

/// The op error for a style that could not be found or applied.
pub(crate) fn style_error(e: StyleError) -> OpError {
    let names = || style::styles().map(|all| list(all.iter().map(|s| s.id.as_str()))).unwrap_or_default();
    match e {
        StyleError::Unknown(id) => access::not_found("style", format!("there is no style `{id}`"))
            .hint(format!("the styles are: {}; character.list_styles lists them", names())),
        StyleError::Template { .. } => OpError::invalid("style", e.to_string()).hint(
            "move the character to one of those templates with body.set_template first, or leave keep_template out",
        ),
        StyleError::Missing(_) => {
            OpError::invalid("character", e.to_string()).hint("add the part with part.add, then apply the style")
        }
        StyleError::Weights(_) => OpError::invalid("styles", e.to_string())
            .hint("give each style a weight from 0 up, at least one above 0; they are scaled to add up to 1"),
        StyleError::File { .. } | StyleError::Invalid(_) => OpError::internal(e.to_string()),
    }
}

/// `true`, the default of the op's flags.
const fn yes() -> bool {
    true
}

/// The plan that restyles `character` by `mix`, with a summary of what changed and `extra` fields.
fn restyle_plan(
    ctx: &OpCtx<'_>,
    character: &CharId,
    mix: &[(Option<&'static Style>, f64)],
    options: Options,
    extra: Value,
) -> Result<Plan, OpError> {
    let before = access::load(ctx, character)?;
    let look_before = look_access::load(ctx, character)?;
    let restyled = style::restyle(&before, &look_before, mix, options).map_err(style_error)?;
    let after = restyled.def;
    let mut changes = Vec::new();
    let mut touched = Vec::new();
    if after.template != before.template {
        let path = paths::character(character).join("template");
        changes.push(access::change(ctx, path, Some(json!(after.template.as_str()))));
        touched.push("template".to_owned());
    }
    if after.face != before.face {
        let value =
            serde_json::to_value(&after.face).map_err(|e| OpError::internal(format!("a face did not serialise: {e}")))?;
        changes.push(access::change(ctx, paths::face(character), Some(value)));
        touched.push("face".to_owned());
    }
    for (id, part) in &after.parts {
        if before.parts.get(id).is_some_and(|b| b.params != part.params) {
            changes.push(access::change(ctx, paths::part_params(character, id), Some(part.params.clone())));
            touched.push(format!("parts/{id}"));
        }
    }
    let mut look_after = look_before.clone();
    if let Some(look) = restyled.look {
        let look_changes = look_access::changes(ctx, character, &look)?;
        if !look_changes.is_empty() {
            touched.push("look".to_owned());
        }
        changes.extend(look_changes);
        look_after = look;
    }
    let mut summary = json!({
        "character": character,
        "template": after.template.as_str(),
        "look": look_after.preset_id(),
        "shading": look_after.overrides.all.style,
        "changed": touched,
    });
    if let (Value::Object(summary), Value::Object(extra)) = (&mut summary, extra) {
        summary.extend(extra);
    }
    Ok(access::edit_plan(changes, summary))
}

/// Applies a character style: how the character is drawn (eye shape, lashes, iris and catchlights, mouth, nose,
/// brows, face shape, neck, proportions and how many heads tall) and shaded (its look preset and where it sits between
/// physical and cel), with its hairstyle, in one undoable step. `amount` blends from the character as it is (0) to the
/// full style (1). Styles never compound: the fields any style sets and this one leaves out go back to the kit's.
/// A template the style isn't tuned for moves the character to the style's own (a chibi becomes tall for `semi-real`),
/// unless `keep_template` asks to refuse instead. Colours, marks and the outfit are left as they are.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "style": "mature"},
        {"character": "kit", "style": "shoujo", "amount": 0.5},
        {"character": "kit", "style": "comic", "hair": false, "look": false}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetStyle {
    /// The character to restyle.
    pub character: CharId,
    /// The style's id (`character.list_styles` lists them).
    pub style: String,
    /// How far toward the style, `0` (as it is) to `1` (the full style, the default). Numbers blend; a discrete choice
    /// (a nose kind, the template, the hairstyle, the look preset) switches from `0.5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub amount: Option<f64>,
    /// Also cut the hair to the style's hairstyle (default `true`).
    #[serde(default = "yes")]
    pub hair: bool,
    /// Also shade with the style's look preset and its place on the style axis (default `true`).
    #[serde(default = "yes")]
    pub look: bool,
    /// Refuse a template the style isn't tuned for instead of moving the character to the style's (default `false`).
    #[serde(default)]
    pub keep_template: bool,
}

impl Op for SetStyle {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.set_style",
            summary: "Apply an art style (shoujo, chibi, comic, semi-real, ...): face, proportions, hair and shading together, by an amount.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let amount = self.amount.unwrap_or(1.0);
        if !(0.0..=1.0).contains(&amount) {
            return Err(OpError::invalid("amount", format!("{amount} is outside 0 (as it is) to 1 (the full style)"))
                .hint("give an amount from 0 to 1, or leave it out for the full style"));
        }
        let style = style::find(&self.style).map_err(style_error)?;
        let options = Options::new(self.hair, self.look, self.keep_template);
        let extra = json!({ "style": style.id, "amount": amount });
        restyle_plan(ctx, &self.character, &[(None, 1.0 - amount), (Some(style), amount)], options, extra)
    }
}

register_op!(SetStyle);

/// One style of a mix and its weight.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WeightedStyle {
    /// The style's id.
    pub style: String,
    /// Its weight, from 0 up; the weights are scaled to add up to 1.
    #[schemars(range(min = 0.0))]
    pub weight: f64,
}

/// Blends several character styles by weight into one, in one undoable step: every number is the weighted mean of
/// what each style would set (eye size, lid shape, lash weight, mouth, face shape, neck, proportions, the place
/// between physical and cel), and every discrete choice (the template, the hairstyle, the look preset, a nose kind)
/// follows the heaviest style. `{"style": "shoujo", "weight": 2}, {"style": "semi-real", "weight": 1}` is a
/// semi-real face two thirds of the way to shoujo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "styles": [{"style": "shoujo", "weight": 2.0}, {"style": "mature", "weight": 1.0}]}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct MixStyles {
    /// The character to restyle.
    pub character: CharId,
    /// The styles and their weights (`character.list_styles` lists the styles).
    #[schemars(length(min = 1))]
    pub styles: Vec<WeightedStyle>,
    /// Also cut the hair to the heaviest style's hairstyle (default `true`).
    #[serde(default = "yes")]
    pub hair: bool,
    /// Also shade with the heaviest style's look preset and the mix's place on the style axis (default `true`).
    #[serde(default = "yes")]
    pub look: bool,
    /// Refuse a template the heaviest style isn't tuned for instead of moving the character to its (default `false`).
    #[serde(default)]
    pub keep_template: bool,
}

impl Op for MixStyles {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.mix_styles",
            summary: "Blend several art styles by weight into one: numbers averaged, discrete choices from the heaviest.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.styles.is_empty() {
            return Err(OpError::invalid("styles", "no styles to mix").hint("give one or more styles with weights"));
        }
        let mix = self
            .styles
            .iter()
            .map(|s| style::find(&s.style).map(|found| (Some(found), s.weight)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(style_error)?;
        let options = Options::new(self.hair, self.look, self.keep_template);
        let extra = json!({ "styles": self.styles });
        restyle_plan(ctx, &self.character, &mix, options, extra)
    }
}

register_op!(MixStyles);

/// Lists the character styles, grouped by family, with what each looks like, the templates it is tuned for, its
/// hairstyle, its look preset and its place between physical (0) and cel (1).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, {"family": "anime"}]))]
pub struct ListStyles {
    /// Only the styles of this family (`anime`, `western`, `real`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
}

impl Op for ListStyles {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.list_styles",
            summary: "List the art styles by family with a summary, templates, hairstyle, look and shading of each.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let all = style::styles().map_err(style_error)?;
        let styles: Vec<_> = all
            .iter()
            .filter(|s| self.family.as_ref().is_none_or(|f| *f == s.family))
            .map(|s| {
                json!({
                    "id": s.id,
                    "name": s.name,
                    "family": s.family,
                    "summary": s.summary,
                    "templates": s.templates,
                    "hair": s.hair,
                    "look": s.look,
                    "shading": s.shading,
                })
            })
            .collect();
        let mut families: Vec<&str> = all.iter().map(|s| s.family.as_str()).collect();
        families.sort_unstable();
        families.dedup();
        Ok(Plan::answer(json!({ "styles": styles, "families": families })))
    }
}

register_op!(ListStyles);
