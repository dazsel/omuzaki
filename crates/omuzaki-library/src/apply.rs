//! Planning a recipe (design D3, D4, D6, D7): the one walk every apply, update and availability check makes, and the
//! apply itself, which plans each step on a scratch copy and returns one combined patch with the source rows and the
//! application record.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_char_api::{BodyTemplateId, CharId, PartId, paths};
use omuzaki_doc_api::diff::diff;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch, PatchError, Scratch, Value};
use omuzaki_library_api::{
    APPLIED_KEY, ApplicationRecord, Bindings, Dep, ItemId, ItemManifest, ItemRights, ItemScope, LibraryError, Params,
    PublisherCheck, RecipeError, RecordBindings, Resolved, ResolvedItem, StepRequest, Version, VersionReq, expand,
};
use omuzaki_ops_api::{OpCtx, OpError, OpKind, Scope, find};
use omuzaki_store_api::Commercial;
use serde_json::json;

use crate::records;

/// How deep bundles may nest.
pub(crate) const MAX_DEPTH: usize = 4;

/// Finds the item a bundle step names.
pub(crate) type Resolve<'a> = &'a dyn Fn(&ItemId, &VersionReq) -> Result<ResolvedItem, LibraryError>;

/// What a walk does at each step.
pub(crate) trait Visitor {
    /// One op step of the manifest being walked, at `index` in it, with its arguments expanded.
    ///
    /// # Errors
    /// Whatever stops the step.
    fn op(&mut self, index: usize, op: &str, args: Value) -> Result<(), OpError>;

    /// A nested item about to be walked.
    fn nested(&mut self, item: &ResolvedItem) {
        let _ = item;
    }
}

/// The one validation of a recipe tree: it expands the manifest (unbound placeholders, undeclared or out-of-range
/// parameters), hands each op step to `visitor` and walks each item step's resolved item with the step's parameters and
/// bindings. A failure inside a nested item has the outer step put in front of its field or message.
pub(crate) fn walk(
    resolve: Resolve<'_>,
    manifest: &ItemManifest,
    params: &Params,
    bindings: &Bindings,
    depth: usize,
    visitor: &mut dyn Visitor,
) -> Result<(), OpError> {
    if depth > MAX_DEPTH {
        return Err(OpError::invalid("step", format!("bundles nest deeper than {MAX_DEPTH}; is there a cycle?")));
    }
    for request in expand(manifest, bindings, params).map_err(recipe_error)? {
        match request {
            StepRequest::Op { index, op, args } => visitor.op(index, &op, args)?,
            StepRequest::Item { index, item, version, params, bind } => {
                let field = format!("step/{index}");
                let nested = resolve(&item, &version).map_err(|e| library_error(&field, &e))?;
                let mut inner = bindings.clone();
                if let Some(target) = bind.get("target") {
                    let part = PartId::new(target.as_str())
                        .map_err(|e| OpError::invalid(format!("{field}/bind/target"), e.to_string()))?;
                    inner.target = Some(part);
                }
                if let Some(name) = bind.get("as") {
                    inner.name = Some(name.clone());
                }
                visitor.nested(&nested);
                walk(resolve, &nested.manifest, &params, &inner, depth + 1, visitor)
                    .map_err(|e| step_error(index, &e))?;
            }
        }
    }
    Ok(())
}

/// Every `$parts.<key>` the tree of `manifest` creates, with the item that creates it. A key two items create is
/// refused, naming both; so is a cycle.
pub(crate) fn creates_keys(
    resolve: Resolve<'_>,
    manifest: &ItemManifest,
) -> Result<BTreeMap<String, (String, omuzaki_library_api::Creates)>, String> {
    fn collect(
        resolve: Resolve<'_>,
        m: &ItemManifest,
        path: &mut Vec<String>,
        owners: &mut BTreeMap<String, (String, omuzaki_library_api::Creates)>,
    ) -> Result<(), String> {
        path.push(m.id.clone());
        for (key, creates) in &m.creates {
            if let Some((first, _)) = owners.insert(key.clone(), (m.id.clone(), creates.clone())) {
                return Err(format!(
                    "`$parts.{key}` is created by both {first} and {}; a bundle can't hold both",
                    m.id
                ));
            }
        }
        for step in &m.steps {
            let omuzaki_library_api::Step::Item(nested) = step else { continue };
            let (Ok(id), Ok(req)) = (ItemId::new(&nested.item), VersionReq::parse(&nested.version)) else { continue };
            let Ok(found) = resolve(&id, &req) else { continue };
            if path.contains(&found.manifest.id) {
                return Err(format!("bundles cycle: {} -> {}", path.join(" -> "), found.manifest.id));
            }
            if path.len() > MAX_DEPTH {
                return Err(format!("bundles nest deeper than {MAX_DEPTH}: {}", path.join(" -> ")));
            }
            collect(resolve, &found.manifest, path, owners)?;
        }
        path.pop();
        Ok(())
    }
    let mut owners = BTreeMap::new();
    collect(resolve, manifest, &mut Vec::new(), &mut owners)?;
    Ok(owners)
}

/// What an apply requires of a step before planning it: a registered `Edit` op within the recipe ceiling
/// (`edit:character` or `edit:scene`, safety rule 1) whose arguments decode into its schema.
///
/// # Errors
/// `Invalid` on `step/<i>/op` for an unregistered op, `Denied` above the ceiling, and the decoder's error otherwise.
pub(crate) fn check_step(index: usize, op: &str, args: &Value) -> Result<(), OpError> {
    let field = format!("step/{index}/op");
    let registration = find(op)
        .ok_or_else(|| OpError::invalid(field.as_str(), format!("`{op}` is not a registered op in this build")))?;
    let meta = (registration.meta)();
    if meta.kind != OpKind::Edit {
        return Err(OpError::denied(format!(
            "step/{index}: `{op}` is a {:?}; recipes may only call edit ops",
            meta.kind
        )));
    }
    if !matches!(meta.scope, Scope::EditCharacter | Scope::EditScene) {
        return Err(OpError::denied(format!(
            "step/{index}: `{op}` needs the {} scope; recipes are limited to edit:character and edit:scene",
            meta.scope
        )));
    }
    (registration.decode)(args.clone()).map(drop).map_err(|e| step_error(index, &e))
}

/// A step's own error with the step in front of its field, or of its message when it names no field.
pub(crate) fn step_error(index: usize, e: &OpError) -> OpError {
    let mut e = e.clone();
    match e.field.take() {
        Some(field) => e.field = Some(format!("step/{index}/{field}")),
        None => e.message = format!("step/{index}: {}", e.message),
    }
    e
}

fn recipe_error(e: RecipeError) -> OpError {
    match e {
        RecipeError::Reserved(why) => OpError::unsupported(why),
        RecipeError::Invalid { field, message } => OpError::invalid(field, message),
        RecipeError::Unbound { field, placeholder, hint } => {
            OpError::invalid(field, format!("`{placeholder}` is not bound")).hint(hint)
        }
        RecipeError::NotTheTarget { field, expected } => OpError::invalid(
            field,
            format!("must be `{expected}`: an item may only change the target it is applied to"),
        ),
        RecipeError::Param { key, message } => OpError::invalid(format!("params/{key}"), message),
    }
}

/// A library error as the matching [`OpError`], on `field`, with what to do about it.
pub(crate) fn library_error(field: &str, e: &LibraryError) -> OpError {
    let message = if field.is_empty() { e.to_string() } else { format!("{field}: {e}") };
    let on = |error: OpError| if field.is_empty() { error } else { OpError { field: Some(field.to_owned()), ..error } };
    match e {
        LibraryError::NotFound(item) => on(OpError::not_found(message))
            .hint(format!("search with library.search, or install `{item}` (library.install)")),
        LibraryError::NoMatchingVersion { installed, .. } => on(OpError::invalid(field, message))
            .hint(format!("installed versions: {installed:?}; install one in the range (library.install)")),
        LibraryError::Ambiguous { .. } => on(OpError::invalid(field, message)).hint("name one publisher"),
        LibraryError::Unsupported { .. } => on(OpError::unsupported(message)),
        LibraryError::Storage(_) => on(OpError::internal(message)),
    }
}

fn patch_error(index: usize, e: &PatchError) -> OpError {
    let message = format!("step/{index}: {e}");
    match e {
        PatchError::Conflict { .. } => OpError::conflict(message),
        PatchError::Unreachable { .. } => OpError::invalid(format!("step/{index}"), message),
    }
}

/// The parameters a record keeps: the caller's values over the declared defaults.
pub(crate) fn filled_params(manifest: &ItemManifest, given: &Params) -> Params {
    manifest.params.iter().map(|(key, spec)| (key.clone(), given.get(key).unwrap_or(&spec.default).clone())).collect()
}

/// Where a resolved item came from, as a record says it.
pub(crate) fn resolved_from(scope: &ItemScope) -> Resolved {
    match scope {
        ItemScope::FirstParty => Resolved::FirstParty,
        ItemScope::Mine => Resolved::Mine,
        ItemScope::Team(id) => Resolved::Team(id.clone()),
    }
}

/// The scratch copy a recipe plans on, and every change made to it, in order: the combined patch.
pub(crate) struct Work {
    pub(crate) scratch: Scratch,
    pub(crate) combined: Patch,
}

impl Work {
    pub(crate) fn of(doc: &dyn DocRead) -> Self {
        Self { scratch: Scratch::of(doc), combined: Patch::new() }
    }

    /// Applies `patch` to the scratch and keeps it.
    pub(crate) fn edit(&mut self, patch: Patch) -> Result<(), OpError> {
        self.scratch.apply(&patch).map_err(|e| OpError::conflict(e.to_string()))?;
        self.combined.extend(patch);
        Ok(())
    }

    /// Sets `path` to `value` (or removes it), creating nothing else.
    pub(crate) fn set(&mut self, path: DocPath, value: Option<Value>) -> Result<(), OpError> {
        let before = self.scratch.get(&path).cloned();
        if before == value {
            return Ok(());
        }
        self.edit(Patch::new().with(path, before, value))
    }

    /// Makes sure the table at `path` exists.
    pub(crate) fn table(&mut self, path: DocPath) -> Result<(), OpError> {
        if self.scratch.get(&path).is_none() {
            self.set(path, Some(json!({})))?;
        }
        Ok(())
    }
}

/// Plans each op step on the scratch, as the runner would: the ceiling, the step's own scope (through
/// [`OpCtx::plan_step`]) and confinement to the target character's node.
struct Planner<'a, 'c> {
    ctx: &'a OpCtx<'c>,
    work: &'a mut Work,
    target: DocPath,
    deps: BTreeMap<String, Dep>,
    nested_rights: Vec<ItemRights>,
}

impl Visitor for Planner<'_, '_> {
    fn op(&mut self, index: usize, op: &str, args: Value) -> Result<(), OpError> {
        check_step(index, op, &args)?;
        let plan = self.ctx.plan_step(op, args, &self.work.scratch).map_err(|e| step_error(index, &e))?.1;
        if let Some(stray) = plan.patch.changes.iter().find(|c| !c.path.starts_with(&self.target)) {
            return Err(OpError::denied(format!(
                "step/{index}: changes `{}`, outside the target `{}`; a recipe can't touch anything else",
                stray.path, self.target
            )));
        }
        self.work.scratch.apply(&plan.patch).map_err(|e| patch_error(index, &e))?;
        self.work.combined.extend(plan.patch);
        Ok(())
    }

    fn nested(&mut self, item: &ResolvedItem) {
        if let Ok(version) = item.manifest.item_version() {
            self.deps.insert(item.manifest.id.clone(), Dep { version, hash: item.hash });
        }
        self.nested_rights.push(item.manifest.rights.clone());
    }
}

fn commercial_rank(c: Commercial) -> u8 {
    match c {
        Commercial::Ok => 2,
        Commercial::Unknown => 1,
        Commercial::No => 0,
    }
}

/// The rights a record carries: the item's own, or a nested item's when that one is stricter (design D7).
fn strictest(own: &ItemRights, nested: &[ItemRights]) -> ItemRights {
    let stricter = |a: &ItemRights, b: &ItemRights| {
        (b.private_test && !a.private_test)
            || commercial_rank(b.commercial) < commercial_rank(a.commercial)
            || (a.owned && !b.owned)
    };
    nested.iter().fold(own.clone(), |held, next| if stricter(&held, next) { next.clone() } else { held })
}

/// What one apply asks for.
pub(crate) struct Request<'a> {
    pub(crate) item: &'a ResolvedItem,
    pub(crate) character: &'a CharId,
    pub(crate) target: Option<PartId>,
    pub(crate) name: Option<String>,
    pub(crate) params: Params,
    pub(crate) replace: Vec<PartId>,
}

/// What an apply did, for its output and its record.
pub(crate) struct Applied {
    pub(crate) app: String,
    pub(crate) record: ApplicationRecord,
    pub(crate) created: BTreeMap<String, PartId>,
    pub(crate) replaced: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

/// The character's body template, read from its node.
pub(crate) fn template_of(doc: &dyn DocRead, character: &CharId) -> Result<BodyTemplateId, OpError> {
    let node = doc
        .get(&paths::character(character))
        .ok_or_else(|| OpError::not_found(format!("no character {character}")).hint("list them with character.list"))?;
    let template = node.get("template").and_then(Value::as_str).ok_or_else(|| {
        OpError::unsupported(format!("{character} is an imported model; library items apply to kit characters"))
    })?;
    BodyTemplateId::new(template).map_err(|e| OpError::invalid("character", e.to_string()))
}

/// A part id for `wanted` that the character doesn't use yet: `wanted`, else `wanted-2`, `wanted-3`...
fn free_part(doc: &dyn DocRead, character: &CharId, wanted: &str, taken: &BTreeSet<PartId>) -> Result<PartId, OpError> {
    for n in 1..=taken.len() + 64 {
        let candidate = if n == 1 { wanted.to_owned() } else { format!("{wanted}-{n}") };
        let id = PartId::new(candidate.as_str()).map_err(|e| OpError::invalid("creates", e.to_string()))?;
        if doc.get(&paths::part(character, &id)).is_none() && !taken.contains(&id) {
            return Ok(id);
        }
    }
    Err(OpError::conflict(format!("no free part id near `{wanted}`")))
}

/// Removes what an application created (its parts and their source rows) and its record, on `work`.
pub(crate) fn remove_application(
    work: &mut Work,
    character: &CharId,
    app: &str,
    record: &ApplicationRecord,
) -> Result<(), OpError> {
    let node = paths::character(character);
    for created in &record.created {
        let path = node.join_path(created);
        work.set(path, None)?;
        if let Some(part) = created.strip_prefix("parts/") {
            work.set(node.join("sources").join(part), None)?;
        }
    }
    work.set(omuzaki_library_api::record_path(character, app), None)
}

/// A path joined with a record's relative path (`parts/hair/params`).
pub(crate) trait JoinPath {
    /// `self` followed by every segment of `relative`.
    fn join_path(&self, relative: &str) -> DocPath;
}

impl JoinPath for DocPath {
    fn join_path(&self, relative: &str) -> DocPath {
        relative.split('/').filter(|s| !s.is_empty()).fold(self.clone(), |p, s| p.join(s))
    }
}

/// The node of character `character` without its bookkeeping (records and source rows): what a recipe writes and what
/// an update merges.
pub(crate) fn content(node: Option<&Value>) -> Value {
    let mut node = node.cloned().unwrap_or_else(|| json!({}));
    if let Value::Object(map) = &mut node {
        map.remove(APPLIED_KEY);
        map.remove("sources");
    }
    node
}

/// Every value `after` holds that `before` doesn't, by path relative to the node: a new key whole, a changed leaf as
/// itself. This is a record's `wrote`.
pub(crate) fn wrote(before: &Value, after: &Value) -> BTreeMap<String, Value> {
    diff(before, after).changes.into_iter().filter_map(|c| c.after.map(|v| (c.path.segments().join("/"), v))).collect()
}

/// Writes the source row of each created part and the record, on `work`.
pub(crate) fn write_rows(
    work: &mut Work,
    character: &CharId,
    app: &str,
    record: &ApplicationRecord,
) -> Result<(), OpError> {
    let node = paths::character(character);
    let created_parts: Vec<&str> = record.created.iter().filter_map(|c| c.strip_prefix("parts/")).collect();
    if !created_parts.is_empty() {
        work.table(node.join("sources"))?;
        let row = records::source_row(&record.rights, &record.item, &record.version);
        for part in created_parts {
            work.set(node.join("sources").join(part), Some(row.clone()))?;
        }
    }
    work.table(node.join(APPLIED_KEY))?;
    let at = omuzaki_library_api::record_path(character, app);
    let value = record.to_node().map_err(|e| OpError::internal(e.to_string()))?;
    work.set(at, Some(value))
}

/// Plans `request` on `work` (design D4 to D7): the slot's previous application and any `replace` parts are removed
/// first, every step is planned on the scratch, and the source rows and the record are written after them.
pub(crate) fn apply(
    ctx: &OpCtx<'_>,
    resolve: Resolve<'_>,
    work: &mut Work,
    request: &Request<'_>,
) -> Result<Applied, OpError> {
    let Request { item, character, .. } = request;
    let manifest = &item.manifest;
    let id = manifest.item_id().map_err(|e| OpError::invalid("item", e.to_string()))?;
    let version: Version = manifest.item_version().map_err(|e| OpError::invalid("item", e.to_string()))?;
    let template = template_of(&work.scratch, character)?;
    let node = paths::character(character);
    let held = records::of_character(&work.scratch, character);
    let mut replaced = Vec::new();
    let mut warnings = Vec::new();

    // D6: the slot's previous item goes in the same transaction; the same item again is an update.
    let in_slot = manifest.slot.as_ref().and_then(|slot| {
        held.iter().find(|(_, r)| r.slot.as_deref() == Some(slot.as_str())).map(|found| (slot, found))
    });
    if let Some((slot, (app, old))) = in_slot {
        if old.item == id {
            return Err(OpError::invalid(
                "item",
                format!("{id} already holds the `{slot}` slot of {character} (application `{app}`)"),
            )
            .hint(format!(
                "change it with library.update {{\"character\": \"{character}\", \"application\": \"{app}\"}}"
            )));
        }
        remove_application(work, character, app, old)?;
        replaced.push(format!("{}@{} (application `{app}`)", old.item, old.version));
    }
    for part in &request.replace {
        let at = paths::part(character, part);
        if work.scratch.get(&at).is_none() {
            return Err(OpError::invalid("replace", format!("{character} has no part `{part}`")));
        }
        work.set(at, None)?;
        work.set(node.join("sources").join(part.as_str()), None)?;
        replaced.push(format!("part `{part}`"));
    }

    // The application's key: the slot, or the item's last segment, numbered when taken.
    let held = records::of_character(&work.scratch, character);
    // A member is named (`$name`): two applications may not add the same name (design D6).
    let name = request.name.clone().or_else(|| manifest.name.clone());
    if let Some(name) = &name {
        name_free(&held, character, name)?;
    }
    let base = manifest.slot.clone().unwrap_or_else(|| id.last_segment().to_owned());
    let app = (1..=held.len() + 1)
        .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
        .find(|key| !held.iter().any(|(k, _)| k == key))
        .unwrap_or(base);

    // Part ids for everything the tree creates; a hand-made part of the same id is kept and named.
    let owners = creates_keys(resolve, manifest).map_err(|why| OpError::invalid("item", why))?;
    let mut parts = BTreeMap::new();
    let mut taken = BTreeSet::new();
    for (key, (_, creates)) in &owners {
        let part = free_part(&work.scratch, character, &creates.id, &taken)?;
        if part.as_str() != creates.id {
            warnings.push(format!(
                "{character} already has a part `{}` that isn't from this item; the new one is `{part}` (pass \
                 `replace: [\"{}\"]` to replace it)",
                creates.id, creates.id
            ));
        }
        taken.insert(part.clone());
        parts.insert(key.clone(), part);
    }
    let bindings = Bindings {
        character: Some((*character).clone()),
        parts: parts.clone(),
        target: request.target.clone(),
        name,
        template: Some(template),
    };

    let before = content(work.scratch.get(&node));
    let mut planner = Planner { ctx, work, target: node.clone(), deps: BTreeMap::new(), nested_rights: Vec::new() };
    walk(resolve, manifest, &request.params, &bindings, 0, &mut planner)?;
    let (deps, nested_rights) = (planner.deps, planner.nested_rights);
    let after = content(work.scratch.get(&node));

    let created: Vec<String> = parts
        .values()
        .filter(|part| {
            before.pointer(&format!("/parts/{part}")).is_none() && after.pointer(&format!("/parts/{part}")).is_some()
        })
        .map(|part| format!("parts/{part}"))
        .collect();
    let mut record = ApplicationRecord::new(
        id,
        version,
        item.hash,
        resolved_from(&item.scope),
        strictest(&manifest.rights, &nested_rights),
    );
    record.publisher = verified(&item.publisher);
    record.slot.clone_from(&manifest.slot);
    record.bindings = RecordBindings {
        parts: parts.iter().map(|(k, p)| (k.clone(), p.to_string())).collect(),
        target: request.target.as_ref().map(ToString::to_string),
        name: bindings.name.clone(),
    };
    record.params = filled_params(manifest, &request.params);
    record.deps = deps;
    record.created = created;
    record.wrote = wrote(&before, &after);
    write_rows(work, character, &app, &record)?;
    let created = parts.into_iter().filter(|(_, p)| record.created.contains(&format!("parts/{p}"))).collect();
    Ok(Applied { app, record, created, replaced, warnings })
}

/// Refuses a member name another application on the character already added.
fn name_free(held: &[(String, ApplicationRecord)], character: &CharId, name: &str) -> Result<(), OpError> {
    match held.iter().find(|(_, r)| r.bindings.name.as_deref() == Some(name)) {
        Some((app, old)) => Err(OpError::invalid(
            "as",
            format!("{character} already has a `{name}` from {} (application `{app}`)", old.item),
        )
        .hint(format!("pass `as` to name this one differently, e.g. {{\"as\": \"{name}-2\"}}"))),
        None => Ok(()),
    }
}

/// Sets `relative` (a `/`-joined path) in `root` to `value`, creating the tables on the way.
fn set_in(root: &mut Value, relative: &str, value: Value) {
    let segments: Vec<&str> = relative.split('/').filter(|s| !s.is_empty()).collect();
    let Some((last, parents)) = segments.split_last() else {
        *root = value;
        return;
    };
    let mut at = root;
    for segment in parents {
        if !at.is_object() {
            *at = json!({});
        }
        let Value::Object(map) = at else { return };
        at = map.entry((*segment).to_owned()).or_insert_with(|| json!({}));
    }
    if !at.is_object() {
        *at = json!({});
    }
    if let Value::Object(map) = at {
        map.insert((*last).to_owned(), value);
    }
}

/// Removes `relative` from `root`, if it is there.
fn remove_in(root: &mut Value, relative: &str) {
    let segments: Vec<&str> = relative.split('/').filter(|s| !s.is_empty()).collect();
    let Some((last, parents)) = segments.split_last() else { return };
    let mut at = root;
    for segment in parents {
        let Some(next) = at.get_mut(*segment) else { return };
        at = next;
    }
    if let Value::Object(map) = at {
        map.remove(*last);
    }
}

/// What one update asks for.
pub(crate) struct UpdateRequest<'a> {
    pub(crate) character: &'a CharId,
    pub(crate) app: &'a str,
    pub(crate) old: &'a ApplicationRecord,
    pub(crate) item: &'a ResolvedItem,
    pub(crate) params: Params,
    /// Take the item's value where the person changed it too.
    pub(crate) prefer_item: bool,
}

/// What an update did, for its output.
pub(crate) struct Updated {
    pub(crate) record: ApplicationRecord,
    /// Each changed path: before, after and why (`item`, `kept`, `conflict`).
    pub(crate) changes: Vec<Value>,
    /// The paths both changed, kept as the person had them unless `prefer_item`.
    pub(crate) conflicts: Vec<String>,
    /// Parameters the new version no longer declares.
    pub(crate) dropped: Vec<String>,
}

/// Updates an application (design D10): a three-way merge whose base is the character as the old apply left it (the
/// record's `wrote` laid over the character now), whose "theirs" is the new recipe planned on that base with the old
/// application's parts taken out, and whose "ours" is the character now. Where only the person changed something,
/// theirs is kept; where both did, the person's wins unless `prefer_item`, and it is reported. A part the person
/// removed stays removed.
pub(crate) fn update(
    ctx: &OpCtx<'_>,
    resolve: Resolve<'_>,
    work: &mut Work,
    request: &UpdateRequest<'_>,
) -> Result<Updated, OpError> {
    use omuzaki_doc_api::merge::{Side, merge3};

    let UpdateRequest { character, app, old, item, .. } = request;
    let manifest = &item.manifest;
    let id = manifest.item_id().map_err(|e| OpError::invalid("item", e.to_string()))?;
    let version = manifest.item_version().map_err(|e| OpError::invalid("item", e.to_string()))?;
    let template = template_of(&work.scratch, character)?;
    let node = paths::character(character);
    let current = work.scratch.get(&node).cloned().unwrap_or_else(|| json!({}));
    let ours = content(Some(&current));
    let mut base = ours.clone();
    for (path, value) in &old.wrote {
        set_in(&mut base, path, value.clone());
    }
    let mut start = base.clone();
    for path in &old.created {
        remove_in(&mut start, path);
    }

    let (params, dropped) = migrated_params(manifest, old, &request.params);

    // Theirs: the new recipe on the start state, with the old application's part ids.
    let mut theirs = Work::of(&work.scratch);
    let mut on_start = start.clone();
    if let (Value::Object(on_start), Value::Object(kept)) = (&mut on_start, &current) {
        for key in [APPLIED_KEY, "sources"] {
            if let Some(value) = kept.get(key) {
                on_start.insert(key.to_owned(), value.clone());
            }
        }
    }
    theirs.set(node.clone(), Some(on_start))?;
    let parts = kept_parts(resolve, manifest, old, &theirs.scratch, character)?;
    let target =
        old.bindings.target.as_deref().map(PartId::new).transpose().map_err(|e| OpError::internal(e.to_string()))?;
    let bindings = Bindings {
        character: Some((*character).clone()),
        parts: parts.clone(),
        target: target.clone(),
        name: old.bindings.name.clone(),
        template: Some(template),
    };
    let mut recipe =
        Planner { ctx, work: &mut theirs, target: node.clone(), deps: BTreeMap::new(), nested_rights: Vec::new() };
    walk(resolve, manifest, &params, &bindings, 0, &mut recipe)?;
    let (deps, nested_rights) = (recipe.deps, recipe.nested_rights);
    let theirs = content(theirs.scratch.get(&node));

    let merged = merge3(Some(&base), &ours, &theirs);
    let side = if request.prefer_item { Side::Theirs } else { Side::Ours };
    let settled = merged
        .patch(&ours, |_| Some(side))
        .map_err(|u| OpError::conflict(format!("the update can't settle {} paths", u.paths.len())))?;
    let (changes, conflicts) = reported(&merged, request.prefer_item);
    let mut prefixed = Patch::new();
    for change in settled.changes {
        let at = change.path.segments().iter().fold(node.clone(), |p, s| p.join(s.as_str()));
        prefixed = prefixed.with(at, change.before, change.after);
    }
    work.edit(prefixed)?;

    let mut record = ApplicationRecord::clone(old);
    record.item = id;
    record.version = version;
    record.hash = item.hash;
    record.from = resolved_from(&item.scope);
    record.publisher = verified(&item.publisher);
    record.slot.clone_from(&manifest.slot);
    record.bindings.parts = parts.iter().map(|(k, p)| (k.clone(), p.to_string())).collect();
    record.params = filled_params(manifest, &params);
    record.deps = deps;
    record.rights = strictest(&manifest.rights, &nested_rights);
    let now = content(work.scratch.get(&node));
    record.created = parts
        .values()
        .map(|part| format!("parts/{part}"))
        .filter(|path| start.pointer(&format!("/{path}")).is_none() && now.pointer(&format!("/{path}")).is_some())
        .collect();
    record.wrote = wrote(&start, &theirs);
    write_rows(work, character, app, &record)?;
    Ok(Updated { record, changes, conflicts, dropped })
}

/// The publisher's fingerprint, when the signature checked.
fn verified(publisher: &PublisherCheck) -> Option<String> {
    match publisher {
        PublisherCheck::Verified { fingerprint, .. } => Some(fingerprint.clone()),
        _ => None,
    }
}

/// An update's parameters: the old values renamed by `migrate`, the ones the new version dropped named, the
/// caller's on top.
fn migrated_params(manifest: &ItemManifest, old: &ApplicationRecord, given: &Params) -> (Params, Vec<String>) {
    let renames = manifest.migrate.as_ref().map(|m| m.params.clone()).unwrap_or_default();
    let mut params = Params::new();
    let mut dropped = Vec::new();
    for (key, value) in &old.params {
        let key = renames.get(key).unwrap_or(key);
        if manifest.params.contains_key(key) {
            params.insert(key.clone(), value.clone());
        } else {
            dropped.push(key.clone());
        }
    }
    params.extend(given.clone());
    (params, dropped)
}

/// The part ids an update creates: the old application's where it had one, a free id for a key the new version adds.
fn kept_parts(
    resolve: Resolve<'_>,
    manifest: &ItemManifest,
    old: &ApplicationRecord,
    doc: &Scratch,
    character: &CharId,
) -> Result<BTreeMap<String, PartId>, OpError> {
    let owners = creates_keys(resolve, manifest).map_err(|why| OpError::invalid("item", why))?;
    let mut parts = BTreeMap::new();
    let mut taken = BTreeSet::new();
    for (key, (_, creates)) in &owners {
        let part = match old.bindings.parts.get(key).and_then(|p| PartId::new(p.as_str()).ok()) {
            Some(part) => part,
            None => free_part(doc, character, &creates.id, &taken)?,
        };
        taken.insert(part.clone());
        parts.insert(key.clone(), part);
    }
    Ok(parts)
}

/// What an update reports: every path the person sees change, and every conflict.
fn reported(merged: &omuzaki_doc_api::merge::Merge3, prefer_item: bool) -> (Vec<Value>, Vec<String>) {
    use omuzaki_doc_api::merge::Outcome;

    let mut changes = Vec::new();
    let mut conflicts = Vec::new();
    for entry in &merged.entries {
        let why = match entry.outcome {
            Outcome::Theirs => "item",
            Outcome::Ours | Outcome::Same => continue,
            Outcome::Conflict => {
                conflicts.push(entry.path.segments().join("/"));
                "conflict"
            }
        };
        let after = if entry.outcome == Outcome::Conflict && !prefer_item { &entry.ours } else { &entry.theirs };
        changes.push(json!({
            "path": entry.path.segments().join("/"),
            "before": entry.ours,
            "after": after,
            "why": why,
        }));
    }
    (changes, conflicts)
}
