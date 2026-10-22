//! `apply_for_test`: a recipe planned step by step on a scratch copy, with nothing written anywhere.

use omuzaki_char_api::{PartId, paths};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch, PatchError, Scratch};
use omuzaki_library_api::{
    Bindings, ItemId, ItemManifest, Library, LibraryError, Params, RecipeError, StepRequest, VersionReq, expand,
};
use omuzaki_ops_api::{Actor, OpCtx, OpError, OpKind, Scope, find};
use omuzaki_store_api::ContentHash;
use serde_json::Value;

/// How deep bundles may nest before the apply is refused as a cycle.
pub(crate) const MAX_DEPTH: usize = 4;

/// Expands `manifest` with its default parameters and plans each step on a [`Scratch`] of `doc` with
/// [`OpCtx::plan_step`], applying each step's patch to the scratch before planning the next. Returns the combined
/// patch against `doc`, which is never changed.
///
/// There are no records and no slots, and bundle steps are refused (see [`apply_with_library`]): this is what a
/// seed test needs to dry-run an item without linking `omuzaki-library`.
///
/// The recipe's safety rules (design D3) hold as in a real apply: each step must be a registered `Edit` op within the
/// `edit:character`/`edit:scene` ceiling, an argument named `character` or `scene` must be its placeholder (the
/// expansion refuses anything else), and every change must lie at or under the bound character's node, so an item
/// can't touch another character. `bindings.character` is therefore required.
///
/// # Errors
/// [`OpError`] on `step/<i>` for an expansion failure, a bundle step, a step outside the ceiling, a change outside
/// the target, or whatever the planned op returns.
pub fn apply_for_test(manifest: &ItemManifest, doc: &dyn DocRead, bindings: &Bindings) -> Result<Patch, OpError> {
    run(None, manifest, doc, bindings)
}

/// [`apply_for_test`] that resolves a bundle's item steps in `library`. A nested item shares the caller's
/// bindings, except that its `bind.target` and `bind.as` replace `$target` and `$name`.
///
/// # Errors
/// As [`apply_for_test`], and an `Invalid` error when a nested item is missing or bundles nest deeper than four.
pub fn apply_with_library(
    library: &dyn Library,
    manifest: &ItemManifest,
    doc: &dyn DocRead,
    bindings: &Bindings,
) -> Result<Patch, OpError> {
    run(Some(library), manifest, doc, bindings)
}

fn run(
    library: Option<&dyn Library>,
    manifest: &ItemManifest,
    doc: &dyn DocRead,
    bindings: &Bindings,
) -> Result<Patch, OpError> {
    let character = bindings
        .character
        .as_ref()
        .ok_or_else(|| OpError::invalid("character", "bind the character the recipe applies to"))?;
    let mut apply = Apply { target: paths::character(character), scratch: Scratch::of(doc), combined: Patch::new() };
    walk(library, manifest, &Params::new(), bindings, 0, &mut apply)?;
    Ok(apply.combined)
}

/// What a walk does at each op step. The walk itself, which is everything an apply checks before it touches an op,
/// is shared: [`Apply`] plans the step, and the library's availability check only asks whether the step could run.
pub(crate) trait OpVisitor {
    /// One op step of the manifest being walked, `index` being its position in that manifest, with its arguments
    /// expanded.
    ///
    /// # Errors
    /// Whatever stops the step.
    fn op(&mut self, index: usize, op: &str, args: Value) -> Result<(), OpError>;
}

/// The one validation of a recipe tree, run by every apply and by the library's availability check, so the two can't
/// disagree: it expands the manifest (reserved target, unbound placeholders, undeclared or out-of-range parameters,
/// nesting depth), hands each op step to `visitor`, and for each item step resolves the dependency and walks it with
/// the step's parameter overrides and bindings. The first failure ends the walk; one inside a nested item has the
/// outer step put in front of its field or message.
pub(crate) fn walk(
    library: Option<&dyn Library>,
    manifest: &ItemManifest,
    params: &Params,
    bindings: &Bindings,
    depth: usize,
    visitor: &mut dyn OpVisitor,
) -> Result<(), OpError> {
    if depth > MAX_DEPTH {
        return Err(OpError::invalid("step", format!("bundles nest deeper than {MAX_DEPTH}; is there a cycle?")));
    }
    for request in expand(manifest, bindings, params).map_err(recipe_error)? {
        match request {
            StepRequest::Op { index, op, args } => visitor.op(index, &op, args)?,
            StepRequest::Item { index, item, version, params, bind } => {
                let field = format!("step/{index}");
                let library = library.ok_or_else(|| {
                    OpError::invalid(
                        field.as_str(),
                        "this applies a bundle; use apply_with_library to resolve its items",
                    )
                })?;
                let nested = resolve(library, &item, &version, &field)?;
                let mut inner = bindings.clone();
                if let Some(target) = bind.get("target") {
                    let part = PartId::new(target.as_str())
                        .map_err(|e| OpError::invalid(format!("{field}/bind/target"), e.to_string()))?;
                    inner.target = Some(part);
                }
                if let Some(name) = bind.get("as") {
                    inner.name = Some(name.clone());
                }
                walk(Some(library), &nested, &params, &inner, depth + 1, visitor).map_err(|e| step_error(index, &e))?;
            }
        }
    }
    Ok(())
}

/// One dry run in progress: the scratch copy every step plans against, and the patch collected so far.
struct Apply {
    /// `characters/<id>` of the bound character: every change must lie at or under it.
    target: DocPath,
    scratch: Scratch,
    combined: Patch,
}

impl OpVisitor for Apply {
    fn op(&mut self, index: usize, op: &str, args: Value) -> Result<(), OpError> {
        // A dry-run context needs no grants, so the recipe ceiling is enforced here, before planning.
        check_step(index, op, &args)?;
        let (actor, clock) = (Actor::System, FakeClock::new(Seconds(0.0)));
        let patch = {
            let ctx = OpCtx::new(&self.scratch, &actor, &clock).with_dry_run(true);
            ctx.plan_step(op, args, &self.scratch).map_err(|e| step_error(index, &e))?.1.patch
        };
        confine(index, &patch, &self.target)?;
        self.scratch.apply(&patch).map_err(|e| patch_error(index, &e))?;
        self.combined.changes.extend(patch.changes);
        Ok(())
    }
}

/// What an apply requires of a step before it plans it, and so what availability requires: a registered `Edit` op
/// within the recipe ceiling (safety rules 1 and 3), whose arguments decode into the op's schema. Nothing here reads
/// the document, so it holds for every character; what a plan finds in the document is the apply's alone.
///
/// # Errors
/// `Invalid` on `step/<i>/op` for an unregistered op, `Denied` above the ceiling, and the decoder's own error, with
/// the step in front of its field, for missing, mistyped or unknown arguments.
pub(crate) fn check_step(index: usize, op: &str, args: &Value) -> Result<(), OpError> {
    check_ceiling(index, op)?;
    match find(op) {
        Some(registration) => (registration.decode)(args.clone()).map(drop).map_err(|e| step_error(index, &e)),
        None => Ok(()),
    }
}

/// Safety rule 1: a step names a registered op of kind `Edit` with scope `edit:character` or `edit:scene`.
fn check_ceiling(index: usize, op: &str) -> Result<(), OpError> {
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
    Ok(())
}

/// Safety rule 2, patches: every change lies at or under the target node.
fn confine(index: usize, patch: &Patch, target: &DocPath) -> Result<(), OpError> {
    match patch.changes.iter().find(|c| !c.path.starts_with(target)) {
        Some(stray) => Err(OpError::denied(format!(
            "step/{index}: changes `{}`, outside the target `{target}`; a recipe can't touch anything else",
            stray.path
        ))),
        None => Ok(()),
    }
}

fn resolve(library: &dyn Library, item: &ItemId, req: &VersionReq, field: &str) -> Result<ItemManifest, OpError> {
    library.resolve(item, req).map(|r| r.manifest).map_err(|e| library_error(field, &e))
}

/// A planned op's own error with the step put in front of its field, or of its message when it names no field. The
/// code, hint and detail are the op's, so a caller can still branch on them.
pub(crate) fn step_error(index: usize, e: &OpError) -> OpError {
    let mut e = e.clone();
    match e.field.take() {
        Some(field) => e.field = Some(format!("step/{index}/{field}")),
        None => e.message = format!("step/{index}: {}", e.message),
    }
    e
}

/// A recipe expansion failure as an [`OpError`] on the field it names, keeping what each kind says.
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

/// A failed resolve of the item at `field`, as the matching [`OpError`] code: a missing item is `NotFound`, a range
/// or publisher the caller can fix is `Invalid` with the way to fix it as the hint, and a broken store is `Internal`.
fn library_error(field: &str, e: &LibraryError) -> OpError {
    let message = format!("{field}: {e}");
    let on_field = |error: OpError| OpError { field: Some(field.to_owned()), ..error };
    match e {
        LibraryError::NotFound(item) => on_field(OpError::not_found(message))
            .hint(format!("install `{item}` (library.install), or remove the step")),
        LibraryError::NoMatchingVersion { installed, .. } => on_field(OpError::invalid(field, message))
            .hint(format!("installed versions: {installed:?}; install one in the range (library.install)")),
        LibraryError::Ambiguous { .. } => on_field(OpError::invalid(field, message)).hint("name one publisher"),
        LibraryError::Unsupported { .. } => on_field(OpError::unsupported(message)),
        LibraryError::Storage(_) => on_field(OpError::internal(message)),
    }
}

/// A patch that no longer fits the scratch: `Conflict` when the document disagrees with what the step expected, and
/// `Invalid` on the step when the change itself can't be made.
fn patch_error(index: usize, e: &PatchError) -> OpError {
    let message = format!("step/{index}: {e}");
    match e {
        PatchError::Conflict { .. } => OpError::conflict(message),
        PatchError::Unreachable { .. } => OpError::invalid(format!("step/{index}"), message),
    }
}

/// A fresh part id for a create key. A key may be as long as a part id itself, so the id is a short prefix and a hash of
/// the key: bounded, deterministic, and one per key.
///
/// # Errors
/// A sentence if the id still isn't valid, which would be a bug here.
pub(crate) fn fresh_part_id(key: &str) -> Result<PartId, String> {
    let hash = ContentHash::of(key.as_bytes()).to_string();
    PartId::new(format!("seed-{}", &hash[..16])).map_err(|e| format!("create key {key:?}: {e}"))
}
