//! `apply_for_test`: a recipe planned step by step on a scratch copy, with nothing written anywhere.

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocRead, DocWrite, Patch, Scratch};
use omuzaki_library_api::{Bindings, ItemId, ItemManifest, Library, Params, StepRequest, VersionReq, expand};
use omuzaki_ops_api::{Actor, OpCtx, OpError};

/// How deep bundles may nest before the apply is refused as a cycle.
const MAX_DEPTH: usize = 4;

/// Expands `manifest` with its default parameters and plans each step on a [`Scratch`] of `doc` with
/// [`OpCtx::plan_step`], applying each step's patch to the scratch before planning the next. Returns the combined
/// patch against `doc`, which is never changed.
///
/// There are no records and no slots, and bundle steps are refused (see [`apply_with_library`]): this is what a
/// seed test needs to dry-run an item without linking `omuzaki-library`.
///
/// # Errors
/// [`OpError`] on `step/<i>` for an expansion failure, a bundle step, or whatever the planned op returns.
pub fn apply_for_test(manifest: &ItemManifest, doc: &dyn DocRead, bindings: &Bindings) -> Result<Patch, OpError> {
    run(None, manifest, doc, bindings, 0)
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
    run(Some(library), manifest, doc, bindings, 0)
}

fn run(
    library: Option<&dyn Library>,
    manifest: &ItemManifest,
    doc: &dyn DocRead,
    bindings: &Bindings,
    depth: usize,
) -> Result<Patch, OpError> {
    let mut scratch = Scratch::of(doc);
    let mut combined = Patch::new();
    steps(library, manifest, &Params::new(), bindings, &mut scratch, &mut combined, depth)?;
    Ok(combined)
}

fn steps(
    library: Option<&dyn Library>,
    manifest: &ItemManifest,
    params: &Params,
    bindings: &Bindings,
    scratch: &mut Scratch,
    combined: &mut Patch,
    depth: usize,
) -> Result<(), OpError> {
    if depth > MAX_DEPTH {
        return Err(OpError::invalid("step", format!("bundles nest deeper than {MAX_DEPTH}; is there a cycle?")));
    }
    let requests = expand(manifest, bindings, params).map_err(|e| OpError::invalid("step", e.to_string()))?;
    let actor = Actor::System;
    let clock = FakeClock::new(Seconds(0.0));
    for request in requests {
        match request {
            StepRequest::Op { index, op, args } => {
                // A dry-run context needs no grants: the step's scope is checked only on the real apply.
                let patch = {
                    let ctx = OpCtx::new(&*scratch, &actor, &clock).with_dry_run(true);
                    ctx.plan_step(&op, args, &*scratch).map_err(|e| step_error(index, &e))?.1.patch
                };
                scratch.apply(&patch).map_err(|e| OpError::conflict(format!("step/{index}: {e}")))?;
                combined.changes.extend(patch.changes);
            }
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
                    inner.target = omuzaki_char_api::PartId::new(target.as_str()).ok();
                }
                if let Some(name) = bind.get("as") {
                    inner.name = Some(name.clone());
                }
                steps(Some(library), &nested, &params, &inner, scratch, combined, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn resolve(library: &dyn Library, item: &ItemId, req: &VersionReq, field: &str) -> Result<ItemManifest, OpError> {
    library.resolve(item, req).map(|r| r.manifest).map_err(|e| OpError::invalid(field, e.to_string()))
}

fn step_error(index: usize, e: &OpError) -> OpError {
    OpError::invalid(format!("step/{index}"), e.to_string())
}
