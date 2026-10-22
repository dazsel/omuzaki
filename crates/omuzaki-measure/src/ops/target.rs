//! `measure.target.set` and `measure.target.remove` (design 6.2): a character's likeness targets, stored as assets and
//! linked by hash from `characters/<id>/likeness/<name>`. The link is the only document change, so undo removes it and
//! the store object stays until garbage collection finds it unreferenced.

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::{Commercial, Provenance, Source, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{FaceSubject, check_name, link_path, measure_subject};
use crate::face::likeness::Compare;
use crate::image::ImageRef;
use crate::target::{FORMAT, LikenessView, Target, TargetImage, measured};

/// Sets a character's likeness target: measures the reference face, stores the target asset and links it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetSet {
    /// The character's id.
    pub character: String,
    /// The target's name, such as `front`.
    pub name: String,
    /// The reference face. Its image must be an asset in the store (import it first), so the target never points at
    /// a file that can change.
    pub face: FaceSubject,
    /// The view (front only).
    #[serde(default)]
    pub view: LikenessView,
    /// The preset the target is meant for (default `cross_look`: a sheet compared with renders).
    #[serde(default)]
    pub compare: Compare,
}

/// The character's node, or `NotFound`.
fn character_node(ctx: &OpCtx<'_>, character: &str) -> Result<DocPath, OpError> {
    let path = DocPath::root().join("characters").join(character);
    if ctx.doc.get(&path).is_none() {
        let mut e = OpError::not_found(format!("no character '{character}'"));
        e.field = Some("character".to_owned());
        return Err(e);
    }
    Ok(path)
}

impl Op for TargetSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.target.set",
            summary: "Set a character's likeness target from a reference image in the store and its eye and mouth \
                      points: measures it (refusing as the measures do), stores the target and links it under a \
                      name, so measure.likeness can compare renders with {target: {character, name}}",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_name(&self.name, "name")?;
        let node = character_node(ctx, &self.character)?;
        let ImageRef::Asset { asset } = self.face.image else {
            return Err(OpError::invalid("face.image", "a likeness target's image must be an asset in the store")
                .hint("import the reference image first (io.import or the store), then pass {\"asset\": \"<hash>\"}"));
        };
        let resolved = measure_subject(ctx, &self.face, "face")?;
        let target = Target {
            format: FORMAT,
            view: self.view,
            compare: self.compare,
            refine: self.face.refine,
            exclude: self.face.exclude.clone(),
            skin: self.face.skin.clone(),
            image: TargetImage { asset },
            landmarks: self.face.landmarks,
            measured: measured(&resolved.outline, &resolved.spacing),
        };
        let text = target.to_toml().map_err(|e| OpError::internal(format!("writing the target: {e}")))?;
        let bytes = text.into_bytes();
        let hash = omuzaki_store_api::ContentHash::of(&bytes);
        let store = ctx.service::<dyn Store>()?;
        // The image's own provenance travels: its licence, commercial status and private flag.
        let source = store.provenance(&asset).ok();
        let provenance = Provenance {
            source: Source::Authored,
            author: None,
            licence: source.as_ref().map_or_else(|| "LicenseRef-Omuzaki-User".to_owned(), |p| p.licence.clone()),
            attribution: source.as_ref().and_then(|p| p.attribution.clone()),
            commercial: source.as_ref().map_or(Commercial::Unknown, |p| p.commercial),
            consent: None,
            private_test: resolved.private,
        };
        if !ctx.dry_run {
            store.put(&bytes, provenance).map_err(|e| OpError::internal(format!("storing the target: {e}")))?;
        }
        let link = json!({ "asset": hash.to_string(), "view": self.view });
        let likeness = node.join("likeness");
        let patch = match ctx.doc.get(&likeness) {
            None => Patch::new().with(likeness, None, Some(json!({ self.name.clone(): link }))),
            Some(Value::Object(_)) => {
                let path = link_path(&self.character, &self.name);
                let before = ctx.doc.get(&path).cloned();
                Patch::new().with(path, before, Some(link))
            }
            Some(other) => {
                return Err(OpError::conflict(format!(
                    "characters/{}/likeness holds {other}, not a table of targets",
                    self.character
                )));
            }
        };
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({
            "character": self.character,
            "name": self.name,
            "asset": hash.to_string(),
            "private": resolved.private,
            "dry_run": ctx.dry_run,
        }));
        Ok(plan)
    }
}

register_op!(TargetSet);

/// Removes a character's likeness target link (the stored target stays until garbage collection).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetRemove {
    /// The character's id.
    pub character: String,
    /// The target's name.
    pub name: String,
}

impl Op for TargetRemove {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.target.remove",
            summary: "Remove a character's likeness target by name (the link only; the stored target is collected \
                      once nothing refers to it)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_name(&self.name, "name")?;
        let node = character_node(ctx, &self.character)?;
        let path = link_path(&self.character, &self.name);
        let Some(before) = ctx.doc.get(&path).cloned() else {
            let mut e =
                OpError::not_found(crate::messages::english(&crate::messages::target_not_found(self.name.clone())));
            e.field = Some("name".to_owned());
            return Err(e);
        };
        let likeness = node.join("likeness");
        // The last target takes its table with it, so a character without targets has no `likeness` key.
        let only = ctx.doc.get(&likeness).and_then(Value::as_object).is_some_and(|t| t.len() == 1);
        let change = if only {
            Patch::new().with(likeness.clone(), ctx.doc.get(&likeness).cloned(), None)
        } else {
            Patch::new().with(path, Some(before), None)
        };
        let mut plan = Plan::edit(change);
        plan.output = Some(json!({ "character": self.character, "name": self.name, "removed": true }));
        Ok(plan)
    }
}

register_op!(TargetRemove);
