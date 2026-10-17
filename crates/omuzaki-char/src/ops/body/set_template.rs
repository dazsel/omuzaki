//! `body.set_template`: changes the body template and reports which parts' fit moved (CB-40, CB-42).

use std::collections::{BTreeMap, BTreeSet};

use crate::template::registry;
use omuzaki_char_api::{
    BodyTemplateId, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef,
    PartId, RegistryBuilder, paths,
};
use omuzaki_core::glam::Vec3;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::params;
use crate::ops::character::access;

/// Where each part's vertices sit, by part: the bind-space positions of its meshes.
type Silhouette = BTreeMap<PartId, Vec<Vec3>>;

/// Each body is sampled at these fractions of its own span (sole to crown), from the feet up.
const SAMPLES: [f32; 3] = [0.25, 0.5, 0.75];
/// A sample takes the vertices within this distance of its height, in metres.
const BAND: f32 = 0.01;
/// A part whose outermost point at a sample moves by more than this has a new fit, in metres (5 mm).
const MOVED: f32 = 0.005;

/// Changes the character's body template (the skeleton and default sizes). The op reports which parts' fit moved: for
/// each part that is not the body, the outermost point of its meshes at three heights of the body is compared before
/// and after, and a move of more than 5 mm counts. A template that does not build the character is refused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "template": "tall"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetTemplate {
    /// The character whose template changes.
    pub character: CharId,
    /// The template's id, such as `soft-sd`. The registered templates are listed when it is unknown.
    pub template: String,
}

impl Op for SetTemplate {
    fn meta() -> OpMeta {
        OpMeta {
            name: "body.set_template",
            summary: "Change the character's body template and return which parts' fit changed (moved more than 5 mm)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let templates = registry::templates();
        let names: Vec<&str> = templates.iter().map(|t| t.id()).collect();
        let found = registry::find(&self.template)
            .ok_or_else(|| params::unknown_name("template", "body template", &self.template, &names))?;

        let def = access::load(ctx, &self.character)?;
        let body = params::load(&self.character, &def)?;
        let previous = def.template.clone();
        if previous.as_str() == found.id() {
            return Ok(access::edit_plan(
                Vec::new(),
                json!({ "character": self.character, "template": self.template, "previous": previous, "fit_changed": [] }),
            ));
        }

        let mut after = def.clone();
        after.template = BodyTemplateId::new(found.id()).map_err(|e| OpError::internal(e.to_string()))?;
        let before_fit = silhouette(&self.character, &def)?;
        let after_fit = silhouette(&self.character, &after)?;
        let fit_changed: Vec<String> =
            moved_parts(&before_fit, &after_fit, &body.part).iter().map(ToString::to_string).collect();

        let template_path = paths::character(&self.character).join("template");
        let changes = vec![access::change(ctx, template_path, Some(json!(found.id())))];
        Ok(access::edit_plan(
            changes,
            json!({
                "character": self.character,
                "template": self.template,
                "previous": previous,
                "fit_changed": fit_changed,
            }),
        ))
    }
}

register_op!(SetTemplate);

/// Builds `def` and gathers every part's vertices.
///
/// # Errors
/// `invalid` naming the template when the character does not build with it, with the builder's reason.
fn silhouette(character: &CharId, def: &CharacterDef) -> Result<Silhouette, OpError> {
    let built: BuiltCharacter = RegistryBuilder
        .build(character, def, &BuildOptions::default(), &BuildServices::default())
        .map_err(|e| build_refusal(def, &e))?;
    Ok(gather(&built))
}

/// The refusal for a character that does not build with its template.
fn build_refusal(def: &CharacterDef, e: &BuildError) -> OpError {
    OpError::invalid("template", format!("the character does not build with template `{}`: {e}", def.template))
        .hint("choose a template the character's parts can build on, or fix the part named in the message")
}

/// Every part's bind-space vertices, from the meshes it built.
fn gather(built: &BuiltCharacter) -> Silhouette {
    let mut parts = Silhouette::new();
    for mesh in &built.meshes {
        parts.entry(mesh.part.clone()).or_default().extend(mesh.mesh.geometry.positions.iter().copied());
    }
    parts
}

/// The parts other than `body` whose outermost point moved by more than [`MOVED`] at one of the body's sample heights,
/// in id order. Each build is sampled over its own body's span (sole to crown), at the same normalised fractions, so a
/// taller or shorter template is compared like with like: a fit that scales with the body does not count as moved. A
/// character without body vertices in either build has no heights to compare, so nothing has moved.
pub(crate) fn moved_parts(before: &Silhouette, after: &Silhouette, body: &PartId) -> Vec<PartId> {
    let span = |fit: &Silhouette| height_span(fit.get(body).map_or(&[][..], Vec::as_slice));
    let (Some(old_span), Some(new_span)) = (span(before), span(after)) else {
        return Vec::new();
    };
    let parts: BTreeSet<&PartId> = before.keys().chain(after.keys()).filter(|part| *part != body).collect();
    parts
        .into_iter()
        .filter(|part| {
            SAMPLES.iter().any(|&f| {
                moved(
                    outermost(before.get(*part).map(Vec::as_slice), at_fraction(old_span, f)),
                    outermost(after.get(*part).map(Vec::as_slice), at_fraction(new_span, f)),
                )
            })
        })
        .cloned()
        .collect()
}

/// The height `fraction` of the way from the bottom of `(low, high)` to its top.
fn at_fraction((low, high): (f32, f32), fraction: f32) -> f32 {
    low + (high - low) * fraction
}

/// The lowest and highest heights of `points`, or `None` for no points.
fn height_span(points: &[Vec3]) -> Option<(f32, f32)> {
    points.iter().map(|p| p.y).fold(None, |span, y| match span {
        None => Some((y, y)),
        Some((lo, hi)) => Some((lo.min(y), hi.max(y))),
    })
}

/// The distance from the body's axis to the outermost of `points` within [`BAND`] of height `h`, or `None` when no
/// point is that high.
fn outermost(points: Option<&[Vec3]>, h: f32) -> Option<f32> {
    points?.iter().filter(|p| (p.y - h).abs() <= BAND).map(|p| p.x.hypot(p.z)).reduce(f32::max)
}

/// Whether an outermost distance changed by more than [`MOVED`]. A point that appears or vanishes counts as a move.
fn moved(before: Option<f32>, after: Option<f32>) -> bool {
    match (before, after) {
        (None, None) => false,
        (Some(a), Some(b)) => (a - b).abs() > MOVED,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(name: &str) -> PartId {
        PartId::new(name).unwrap()
    }

    /// A ring of points at radius `r` around the body's axis, at each height in `heights`.
    fn ring(r: f32, heights: &[f32]) -> Vec<Vec3> {
        heights.iter().flat_map(|&y| [Vec3::new(r, y, 0.0), Vec3::new(-r, y, 0.0)]).collect()
    }

    #[test]
    fn a_garment_that_moves_more_than_5_mm_is_reported_and_the_body_never_is() {
        let body = part("body");
        let shirt = part("shirt");
        let heights = [0.0, 0.5, 1.0];
        let before: Silhouette = [(body.clone(), ring(0.10, &heights)), (shirt.clone(), ring(0.12, &heights))].into();
        let mut after = before.clone();
        after.insert(body.clone(), ring(0.30, &heights));
        assert_eq!(moved_parts(&before, &after, &body), Vec::<PartId>::new(), "the body is excluded");

        after.insert(shirt.clone(), ring(0.124, &heights));
        assert_eq!(moved_parts(&before, &after, &body), Vec::<PartId>::new(), "4 mm is within tolerance");

        after.insert(shirt.clone(), ring(0.13, &heights));
        assert_eq!(moved_parts(&before, &after, &body), vec![shirt], "10 mm moves the fit");
    }

    #[test]
    fn a_part_that_vanishes_or_appears_at_a_sample_height_counts_as_moved() {
        let body = part("body");
        let vest = part("vest");
        let before: Silhouette = [(body.clone(), ring(0.10, &[0.0, 1.0]))].into();
        let after: Silhouette = [(body.clone(), ring(0.10, &[0.0, 1.0])), (vest.clone(), ring(0.12, &[0.5]))].into();
        assert_eq!(moved_parts(&before, &after, &body), vec![vest]);
    }

    #[test]
    fn a_taller_body_compares_each_fit_at_its_own_normalised_heights() {
        let body = part("body");
        let shirt = part("shirt");
        // Before: body 0..1, shirt at 0, 0.5 and 1. After: body 0..1.2 (taller), and the shirt scales with it, so at each
        // normalised height it has the same outermost point and nothing has moved.
        let before: Silhouette =
            [(body.clone(), ring(0.10, &[0.0, 0.5, 1.0])), (shirt.clone(), ring(0.12, &[0.0, 0.5, 1.0]))].into();
        let mut after: Silhouette =
            [(body.clone(), ring(0.10, &[0.0, 0.6, 1.2])), (shirt.clone(), ring(0.12, &[0.0, 0.6, 1.2]))].into();
        assert_eq!(moved_parts(&before, &after, &body), Vec::<PartId>::new(), "a fit that scales with the body holds");

        after.insert(shirt.clone(), ring(0.14, &[0.0, 0.6, 1.2]));
        assert_eq!(moved_parts(&before, &after, &body), vec![shirt], "a 20 mm move at matching heights is reported");
    }

    #[test]
    fn a_character_without_body_vertices_has_nothing_to_fit_to() {
        let body = part("body");
        let shirt = part("shirt");
        let before: Silhouette = [(shirt.clone(), ring(0.12, &[0.5]))].into();
        let after: Silhouette = [(shirt, ring(0.5, &[0.5]))].into();
        assert_eq!(moved_parts(&before, &after, &body), Vec::<PartId>::new());
    }
}
