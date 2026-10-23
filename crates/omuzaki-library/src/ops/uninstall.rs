//! `library.uninstall`: remove items from the person's own library.

use omuzaki_library_api::{ItemId, ItemScope, Library, Version, VersionReq};
use omuzaki_ops_api::{Effect, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::HostLibrary;
use crate::common::{failed, host, item_id, version};
use crate::ops::install::call;

/// One item and version to remove.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pinned {
    /// The item id.
    pub item: String,
    /// The version.
    pub version: String,
}

/// Removes items from your library (design D9). Projects keep their pins, the copies they stored and what the items
/// made: a project never reads the library to build. Name one item (`item`, and `version` or the newest in your
/// library), or several exact versions (`items`, what undoing a folder install passes). A person's decision: an
/// agent's call asks the person first. Undo installs them again.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "item": "signer/hair/ribbon-bow", "version": "1.0.0" }]))]
pub struct LibraryUninstall {
    /// The item id.
    #[serde(default)]
    pub item: Option<String>,
    /// Its version; the newest in your library without it.
    #[serde(default)]
    pub version: Option<String>,
    /// Several exact versions instead.
    #[serde(default)]
    pub items: Vec<Pinned>,
}

/// The newest version of `id` in the person's library.
fn newest_mine(library: &HostLibrary, id: &ItemId) -> Result<Version, OpError> {
    let versions = library.versions(id).unwrap_or_default();
    versions
        .into_iter()
        .rev()
        .find(|v| library.resolve(id, &VersionReq::Exact(*v)).is_ok_and(|held| held.scope == ItemScope::Mine))
        .ok_or_else(|| {
            OpError::not_found(format!("{id} is not in your library"))
                .hint("list it with library.list {\"scope\": \"mine\"}")
        })
}

impl Op for LibraryUninstall {
    fn meta() -> OpMeta {
        OpMeta {
            name: "library.uninstall",
            summary: "Remove items from your own library.",
            kind: OpKind::Edit,
            scope: Scope::EditLibrary,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let library = host(ctx)?;
        let mut wanted = Vec::new();
        match (&self.item, self.items.is_empty()) {
            (Some(item), true) => {
                let id = item_id("item", item)?;
                let v = match &self.version {
                    Some(v) => version("version", v)?,
                    None => newest_mine(library, &id)?,
                };
                wanted.push((id, v));
            }
            (None, false) => {
                for (n, pin) in self.items.iter().enumerate() {
                    wanted.push((item_id(&format!("items/{n}/item"), &pin.item)?, version(&format!("items/{n}/version"), &pin.version)?));
                }
            }
            _ => return Err(OpError::invalid("item", "name one `item`, or several in `items`")),
        }
        for (id, v) in &wanted {
            let held = library.resolve(id, &VersionReq::Exact(*v)).map_err(|e| failed(&e))?;
            if held.scope != ItemScope::Mine {
                return Err(OpError::invalid("item", format!("{id}@{v} isn't in your library; it comes with Omuzaki")));
            }
        }
        let names: Vec<String> = wanted.iter().map(|(id, v)| format!("{id}@{v}")).collect();
        ctx.require_person(format!("remove {} from your library", names.join(", ")))?;
        let mut texts = Vec::new();
        for (id, v) in &wanted {
            let manifest = if ctx.dry_run {
                library.resolve(id, &VersionReq::Exact(*v)).map_err(|e| failed(&e))?.manifest
            } else {
                library.uninstall(id, v).map_err(|e| failed(&e))?
            };
            texts.push(toml::to_string(&manifest).map_err(|e| OpError::internal(e.to_string()))?);
        }
        let pins: Vec<_> = wanted.iter().map(|(id, v)| json!({ "item": id, "version": v.to_string() })).collect();
        let effect = Effect {
            summary: format!("removed {} from your library", names.join(", ")),
            undo: call("library.install", json!({ "manifests": texts })),
            redo: call("library.uninstall", json!({ "items": pins })),
        };
        let output = json!({ "removed": pins });
        Ok(Plan { output: Some(output), effect: Some(effect), ..Plan::default() })
    }
}

register_op!(LibraryUninstall);
