//! `branch.merge` (`docs/design/branches.md` 7): another branch's point merged into this lane's document, unit by
//! unit, as one undoable step. The dry-run is the merge preview.
//!
//! The base comes from the version vectors (7.3): the document's `merges` record (only the points the store can
//! vouch for) joined with the branch's `inherits`, against the point's `contains`. Each path that differs is merged
//! three ways (`merge3`); the paths are grouped into units (`units.rs`), and each unit is merged automatically,
//! taken whole from one side, kept both ways (parts), resolved per parameter, or left out. A merge that leaves no
//! offered unit out is full and rewrites `merges` in the same transaction, so every way of reverting it reverts the
//! record too; one that leaves some out is a pick and records nothing. The result is validated (7.5) and library
//! application records follow what they made (7.6) before anything is applied.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_char_api::{CharacterDef, TreeError, find_kind, requires_of};
use omuzaki_doc_api::diff::diff;
use omuzaki_doc_api::merge::{MergeEntry, Outcome, merge3};
use omuzaki_doc_api::{DocPath, DocVersion, Value, lookup};
use omuzaki_history_api::branch::{BranchInfo, BranchName, LaneState, PointId};
use omuzaki_history_api::merge_base::{merge_base, merged_record};
use omuzaki_history_api::state::{BranchStore, HistoryView, StateRef};
use omuzaki_ops_api::{Actor, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{MERGE_OP, branch_error};
use crate::branches::merges::{MERGES, record_value, verified_merges};
use crate::history::units::{Unit, unit_of};

/// Merges another branch into this one: by default its head, or the point `at` (the reviewed one). Each unit (a part,
/// the face, the palette, a scene) merges on its own; a conflict needs a choice in `resolve`, and `exclude` leaves a
/// unit out (a pick, which doesn't record the merge, so the next merge offers the rest). Dry-run first: that is the
/// preview.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "from": "aria/oval" },
    { "from": "aria/oval", "resolve": { "project": "theirs" } },
    { "from": "aria/oval", "exclude": ["project"] }
]))]
pub struct BranchMerge {
    /// The branch to merge from.
    pub from: BranchName,
    /// The point to merge (a reviewed one); the branch's head by default.
    #[serde(default)]
    pub at: Option<PointId>,
    /// Only units under these paths (the rest are left out).
    #[serde(default)]
    pub scope: Vec<String>,
    /// Units to leave out, by key (`characters/kit/parts/hair`).
    #[serde(default)]
    pub exclude: Vec<String>,
    /// A choice per unit key: `"mine"`, `"theirs"`, `"both"` (parts: theirs is added as a disabled alternative), or
    /// `{"paths": {"<path>": "mine" | "theirs"}}` for each conflicting parameter.
    #[serde(default)]
    pub resolve: BTreeMap<String, Resolve>,
    /// The base to merge from instead of the computed one (an import, a test).
    #[serde(default)]
    pub base: Option<StateRef>,
    /// Refuse if this branch's document is no longer at this version (it changed since the preview).
    #[serde(default)]
    pub expect: Option<DocVersion>,
}

/// A unit's resolution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Resolve {
    /// The whole unit one way.
    Whole(Whole),
    /// Each conflicting parameter one way; the rest merge automatically.
    Paths {
        /// The side for each conflicting path.
        paths: BTreeMap<String, Pick>,
    },
}

/// A whole unit's resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Whole {
    /// This branch's unit, as it is.
    Mine,
    /// The other branch's unit.
    Theirs,
    /// Parts only: this branch's part stays and the other's is added beside it, disabled.
    Both,
}

/// A side for one parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Pick {
    /// This branch's value.
    Mine,
    /// The other branch's value.
    Theirs,
}

/// The fields of a library application record that change together (ADR 0019 D5, design 7.6).
const PIN: [&str; 8] = ["item", "version", "hash", "publisher", "deps", "rights", "created", "slot"];

/// One unit as the merge plans it.
struct UnitPlan {
    unit: Unit,
    entries: Vec<MergeEntry>,
    /// Both sides changed the application's pin differently: the unit is one conflict.
    pin_conflict: bool,
    status: &'static str,
    taken: &'static str,
    notes: Vec<String>,
}

impl UnitPlan {
    /// Whether the other branch brings anything here.
    fn offers(&self) -> bool {
        self.pin_conflict || self.entries.iter().any(|e| matches!(e.outcome, Outcome::Theirs | Outcome::Conflict))
    }

    fn conflicting(&self) -> bool {
        self.pin_conflict || self.entries.iter().any(|e| e.outcome == Outcome::Conflict)
    }
}

fn path(text: &str, field: &str) -> Result<DocPath, OpError> {
    DocPath::parse(text).map_err(|e| OpError::invalid(field.to_owned(), format!("{}: {}", e.path, e.reason)))
}

/// `value` with the `merges` record left out: bookkeeping, never merged as data.
fn without_merges(value: &Value) -> Value {
    let mut value = value.clone();
    if let Value::Object(map) = &mut value {
        map.remove(MERGES);
    }
    value
}

/// Sets (or removes) `at` in `root`, making missing parents as objects.
fn put(root: &mut Value, at: &DocPath, value: Option<Value>) {
    let Some((last, parents)) = at.segments().split_last() else {
        *root = value.unwrap_or(Value::Null);
        return;
    };
    let mut node = root;
    for segment in parents {
        if !node.is_object() {
            *node = Value::Object(serde_json::Map::new());
        }
        let Value::Object(map) = node else { return };
        node = map.entry(segment.clone()).or_insert_with(|| Value::Object(serde_json::Map::new()));
    }
    if !node.is_object() {
        *node = Value::Object(serde_json::Map::new());
    }
    if let Value::Object(map) = node {
        match value {
            Some(value) => {
                map.insert(last.clone(), value);
            }
            None => {
                map.remove(last);
            }
        }
    }
}

/// The pin of the application record at `key` in `tree`: the fields that change together.
fn pin<'a>(tree: &'a Value, key: &DocPath) -> Option<Vec<Option<&'a Value>>> {
    lookup(tree, key).map(|r| PIN.iter().map(|field| r.get(*field)).collect())
}

/// Whether both sides changed `key`'s application pin, to different pins.
fn pin_conflict(key: &DocPath, base: Option<&Value>, ours: &Value, theirs: &Value) -> bool {
    let (b, o, t) = (base.and_then(|b| pin(b, key)), pin(ours, key), pin(theirs, key));
    o != b && t != b && o != t
}

/// What the application record at `record` (in either tree) says it created.
fn created_by(record: &Value) -> Vec<DocPath> {
    let listed = record.get("created").and_then(Value::as_array).into_iter().flatten();
    listed.filter_map(Value::as_str).filter_map(|p| DocPath::parse(p).ok()).collect()
}

/// `characters/<c>/<what>/<id>` as a path.
fn char_path(character: &str, what: &str, id: &str) -> DocPath {
    DocPath::root().join("characters").join(character).join(what).join(id)
}

/// A slug from the last segment of a branch name, for the id of a part kept both ways.
fn suffix_of(name: &BranchName) -> String {
    let last = name.as_str().rsplit('/').next().unwrap_or("theirs");
    let slug: String = last
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    if slug.trim_matches('-').is_empty() { "theirs".to_owned() } else { slug }
}

impl BranchMerge {
    fn base_tree(
        &self,
        ctx: &OpCtx<'_>,
        ours: &Value,
        store: &dyn BranchStore,
        this: &BranchInfo,
        theirs: &omuzaki_history_api::branch::Point,
    ) -> Result<(Option<PointId>, Option<Value>), OpError> {
        if let Some(at) = &self.base {
            let tree = match at {
                StateRef::Now => ours.clone(),
                other => ctx
                    .service::<dyn HistoryView>()?
                    .tree(other)
                    .map_err(|e| OpError::invalid("base", e.to_string()))?
                    .as_ref()
                    .clone(),
            };
            return Ok((None, Some(tree)));
        }
        let ours_merges = verified_merges(ours, &this.id, store);
        let read = |id: &PointId| store.point(id).ok();
        let Some(base) = merge_base(&this.id, &ours_merges.join(&this.inherits), theirs, &read) else {
            return Ok((None, None));
        };
        Ok((Some(base), Some(store.tree(&base).map_err(branch_error)?.as_ref().clone())))
    }

    fn check_names(&self, units: &BTreeMap<String, UnitPlan>) -> Result<(), OpError> {
        let unknown = |key: &String| !units.contains_key(key);
        if let Some(key) = self.resolve.keys().find(|k| unknown(k)) {
            return Err(OpError::invalid("resolve", format!("this merge has no unit {key}"))
                .hint("the dry-run lists the units and their keys"));
        }
        if let Some(key) = self.exclude.iter().find(|k| unknown(k)) {
            return Err(OpError::invalid("exclude", format!("this merge has no unit {key}"))
                .hint("the dry-run lists the units and their keys"));
        }
        Ok(())
    }
}

/// The paths the units chose, and what each unit took. `writes` maps a path to its merged value.
fn decide(
    merge: &BranchMerge,
    units: &mut BTreeMap<String, UnitPlan>,
    scope: &[DocPath],
    sides: (&Value, &Value),
    suffix: &str,
    writes: &mut BTreeMap<DocPath, Option<Value>>,
) -> Result<(), OpError> {
    let (ours, theirs) = sides;
    for (key, plan) in units.iter_mut() {
        let key_path = DocPath::parse(key).unwrap_or_default();
        let out_of_scope = !scope.is_empty() && !scope.iter().any(|s| key_path.starts_with(s));
        let excluded = merge.exclude.contains(key) || out_of_scope;
        let theirs_of = |entry: &MergeEntry| (entry.path.clone(), entry.theirs.clone());
        plan.status = if !plan.offers() {
            "unchanged"
        } else if plan.conflicting() {
            "conflict"
        } else {
            "clean"
        };
        if excluded {
            (plan.status, plan.taken) = (if plan.offers() { "excluded" } else { plan.status }, "left-out");
            continue;
        }
        match merge.resolve.get(key) {
            Some(Resolve::Whole(Whole::Mine)) => plan.taken = "mine",
            Some(Resolve::Whole(Whole::Theirs)) => {
                plan.taken = "theirs";
                writes.extend(plan.entries.iter().map(theirs_of));
            }
            Some(Resolve::Whole(Whole::Both)) => {
                if plan.unit.kind != "part" {
                    return Err(OpError::invalid(
                        "resolve",
                        format!("{key} isn't a part, so it can't be kept both ways"),
                    )
                    .hint("choose mine or theirs for it"));
                }
                plan.taken = "both";
                let id = keep_both(&key_path, (ours, theirs), suffix, writes);
                plan.notes.push(match id {
                    Some(id) => format!("theirs is kept beside yours as the disabled part {id}"),
                    None => "theirs has no such part, so there is nothing to keep beside yours".to_owned(),
                });
            }
            Some(Resolve::Paths { paths }) => {
                if plan.pin_conflict {
                    return Err(OpError::invalid("resolve", format!("{key}'s library item is pinned as a whole"))
                        .hint("choose mine or theirs for the whole unit"));
                }
                plan.taken = "per-parameter";
                for entry in &plan.entries {
                    match entry.outcome {
                        Outcome::Theirs => {
                            writes.insert(entry.path.clone(), entry.theirs.clone());
                        }
                        Outcome::Conflict => match paths.get(&entry.path.to_string()) {
                            Some(Pick::Theirs) => {
                                writes.insert(entry.path.clone(), entry.theirs.clone());
                            }
                            Some(Pick::Mine) => {}
                            None => plan.taken = "none",
                        },
                        _ => {}
                    }
                }
            }
            None if plan.conflicting() => plan.taken = "none",
            None => {
                plan.taken = "auto";
                let taken = plan.entries.iter().filter(|e| e.outcome == Outcome::Theirs).map(theirs_of);
                writes.extend(taken);
            }
        }
    }
    Ok(())
}

/// Adds theirs' part at `key` (`characters/<c>/parts/<p>`) beside ours' as a disabled alternative, with its source
/// row and weight brushes (design 7.4, CB-91). Children of theirs' part are not copied: they keep pointing at ours'.
/// Returns the new part's id, or `None` when theirs has no such part.
fn keep_both(
    key: &DocPath,
    (ours, theirs): (&Value, &Value),
    suffix: &str,
    writes: &mut BTreeMap<DocPath, Option<Value>>,
) -> Option<String> {
    let [_, character, _, part] = key.segments() else { return None };
    let mut copy = lookup(theirs, key)?.clone();
    if let Value::Object(fields) = &mut copy {
        fields.insert("enabled".to_owned(), Value::Bool(false));
    }
    let taken = |id: &str| {
        let at = char_path(character, "parts", id);
        lookup(ours, &at).is_some() || writes.contains_key(&at)
    };
    let base_id = format!("{part}-{suffix}");
    let mut id = base_id.clone();
    let mut n = 1;
    while taken(&id) {
        n += 1;
        id = format!("{base_id}-{n}");
    }
    writes.insert(char_path(character, "parts", &id), Some(copy));
    for what in ["sources", "weight_brushes"] {
        if let Some(row) = lookup(theirs, &char_path(character, what, part)) {
            writes.insert(char_path(character, what, &id), Some(row.clone()));
        }
    }
    Some(id)
}

/// Keeps every application record whose output the result still holds (design 7.6: rights never drop), and marks two
/// applications on one slot invalid. Returns the rights notes.
fn follow_applications(
    result: &mut Value,
    sides: [&Value; 2],
    writes: &mut BTreeMap<DocPath, Option<Value>>,
    invalid: &mut BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut notes = Vec::new();
    let characters = result.get("characters").and_then(Value::as_object).cloned().unwrap_or_default();
    for (character, _) in characters {
        let mut records: BTreeMap<String, Value> = BTreeMap::new();
        for side in sides {
            let at = DocPath::root().join("characters").join(character.as_str()).join("applied");
            for (id, record) in lookup(side, &at).and_then(Value::as_object).into_iter().flatten() {
                records.entry(id.clone()).or_insert_with(|| record.clone());
            }
        }
        for (id, record) in &records {
            let at = char_path(&character, "applied", id);
            if lookup(result, &at).is_some() {
                continue;
            }
            let remains = created_by(record).iter().any(|made| lookup(result, made).is_some());
            if remains {
                put(result, &at, Some(record.clone()));
                writes.insert(at, Some(record.clone()));
                notes.push(format!(
                    "kept the record of library item {id} on {character}: the merge keeps what it made, and its rights go with it"
                ));
            }
        }
        let applied = DocPath::root().join("characters").join(character.as_str()).join("applied");
        let mut slots: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (id, record) in lookup(result, &applied).and_then(Value::as_object).into_iter().flatten() {
            if let Some(slot) = record.get("slot").and_then(Value::as_str) {
                slots.entry(slot.to_owned()).or_default().push(id.clone());
            }
        }
        for (slot, ids) in slots.into_iter().filter(|(_, ids)| ids.len() > 1) {
            for id in &ids {
                invalid
                    .entry(char_path(&character, "applied", id).to_string())
                    .or_default()
                    .push(format!("{} claim the slot {slot}: keep one", ids.join(" and ")));
            }
        }
    }
    notes
}

/// Validates every character the merge changes (design 7.5): it still reads as a character, its parts form a tree,
/// and each changed part's parameters pass its kind's check. Failures are noted on the units involved.
fn validate(
    result: &Value,
    sides: [&Value; 2],
    writes: &BTreeMap<DocPath, Option<Value>>,
    invalid: &mut BTreeMap<String, Vec<String>>,
) {
    let mut touched: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for at in writes.keys() {
        if let [root, character, rest @ ..] = at.segments()
            && root == "characters"
        {
            let parts = touched.entry(character.clone()).or_default();
            if let [what, part, ..] = rest
                && (what == "parts" || what == "sources")
            {
                parts.insert(part.clone());
            }
        }
    }
    let part_unit = |c: &str, p: &str| char_path(c, "parts", p).to_string();
    for (character, parts) in touched {
        let at = DocPath::root().join("characters").join(character.as_str());
        let Some(node) = lookup(result, &at) else { continue };
        let read_before =
            sides.iter().any(|side| lookup(side, &at).is_some_and(|n| CharacterDef::from_node(n).is_ok()));
        // An imported character (an avatar, no template) has no parts to check.
        if node.get("avatar").is_some() && node.get("template").is_none() {
            continue;
        }
        let def = match CharacterDef::from_node(node) {
            Ok(def) => def,
            Err(error) => {
                if read_before {
                    invalid.entry(at.to_string()).or_default().push(format!("{character} no longer reads: {error}"));
                }
                continue;
            }
        };
        match def.build_order(&requires_of) {
            Ok(_) => {}
            Err(TreeError::MissingParent { part, parent }) => {
                let note = format!("part {part}'s parent {parent} isn't in the result");
                invalid.entry(part_unit(&character, part.as_str())).or_default().push(note.clone());
                invalid.entry(part_unit(&character, parent.as_str())).or_default().push(note);
            }
            Err(TreeError::Cycle(cycle)) => {
                let names: Vec<&str> = cycle.iter().map(|p| p.as_str()).collect();
                for part in &cycle {
                    invalid
                        .entry(part_unit(&character, part.as_str()))
                        .or_default()
                        .push(format!("these parts wait on each other: {}", names.join(", ")));
                }
            }
        }
        for (id, part) in def.parts.iter().filter(|(id, _)| parts.contains(id.as_str())) {
            let Some(kind) = find_kind(part.kind.as_str()) else { continue };
            if let Err(errors) = (kind.check)(&part.params) {
                invalid.entry(part_unit(&character, id.as_str())).or_default().push(format!("part {id}: {errors}"));
            }
        }
    }
}

impl Op for BranchMerge {
    fn meta() -> OpMeta {
        OpMeta {
            name: MERGE_OP,
            summary: "Merge another branch into this one, part by part, as one undoable step.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    #[allow(clippy::too_many_lines, reason = "the merge's steps in the order design 7 gives them")]
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let store = ctx.service::<dyn BranchStore>()?;
        let this = store.this().map_err(branch_error)?;
        let source = store.branch(&self.from).map_err(branch_error)?;
        if source.id == this.id {
            return Err(OpError::invalid("from", "a branch can't be merged into itself"));
        }
        if let Some(expect) = self.expect
            && !ctx.dry_run
            && ctx.doc.version() != expect
        {
            return Err(OpError::conflict(format!("branch {} changed while you were reviewing", this.name))
                .hint("preview the merge again"));
        }
        let theirs_id = self.at.unwrap_or(source.head);
        let theirs_point = store.point(&theirs_id).map_err(branch_error)?;
        let theirs_full = store.tree(&theirs_id).map_err(branch_error)?;
        let ours_full = ctx.doc.get(&DocPath::root()).cloned().unwrap_or(Value::Null);
        let (base_id, base_full) = self.base_tree(ctx, &ours_full, store, &this, &theirs_point)?;
        let (ours, theirs) = (without_merges(&ours_full), without_merges(&theirs_full));
        let base = base_full.as_ref().map(without_merges);
        let merged = merge3(base.as_ref(), &ours, &theirs);

        // Group by unit; an application's pin is atomic.
        let trees: Vec<&Value> = [Some(&ours), Some(&theirs), base.as_ref()].into_iter().flatten().collect();
        let mut units: BTreeMap<String, UnitPlan> = BTreeMap::new();
        for entry in merged.entries {
            let unit = unit_of(&entry.path, &trees);
            let plan = units.entry(unit.key.clone()).or_insert_with(|| UnitPlan {
                unit,
                entries: Vec::new(),
                pin_conflict: false,
                status: "clean",
                taken: "auto",
                notes: Vec::new(),
            });
            plan.entries.push(entry);
        }
        for (key, plan) in &mut units {
            if plan.unit.kind == "application" {
                let at = DocPath::parse(key).unwrap_or_default();
                plan.pin_conflict = pin_conflict(&at, base.as_ref(), &ours, &theirs);
            }
        }
        self.check_names(&units)?;
        let scope = self.scope.iter().map(|s| path(s, "scope")).collect::<Result<Vec<_>, _>>()?;
        let mut writes: BTreeMap<DocPath, Option<Value>> = BTreeMap::new();
        decide(self, &mut units, &scope, (&ours, &theirs), &suffix_of(&self.from), &mut writes)?;

        // The result, its library records and its validation.
        let mut result = ours.clone();
        for (at, value) in &writes {
            put(&mut result, at, value.clone());
        }
        let mut invalid: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let rights = follow_applications(&mut result, [&ours, &theirs], &mut writes, &mut invalid);
        validate(&result, [&ours, &theirs], &writes, &mut invalid);
        for (key, notes) in invalid {
            let plan = units.entry(key.clone()).or_insert_with(|| UnitPlan {
                unit: unit_of(&DocPath::parse(&key).unwrap_or_default(), &[&result]),
                entries: Vec::new(),
                pin_conflict: false,
                status: "unchanged",
                taken: "auto",
                notes: Vec::new(),
            });
            plan.status = "invalid";
            plan.notes.extend(notes);
        }

        // Full or pick: a full merge records what it merged, in the same transaction.
        let full = units.values().all(|u| u.taken != "left-out" || !u.offers());
        let blocked: Vec<&UnitPlan> =
            units.values().filter(|u| u.status == "invalid" || (u.taken == "none" && u.conflicting())).collect();
        let mut result_full = result.clone();
        let record_before = ours_full.get(MERGES).cloned();
        if let Value::Object(map) = &mut result_full {
            map.remove(MERGES);
            if let Some(before) = &record_before {
                map.insert(MERGES.to_owned(), before.clone());
            }
            if full {
                let ours_merges = verified_merges(&ours_full, &this.id, store);
                let record = merged_record(&this.id, &ours_merges, &theirs_point);
                let names: BTreeMap<_, _> =
                    store.list().unwrap_or_default().into_iter().map(|b| (b.id, b.name.to_string())).collect();
                map.insert(MERGES.to_owned(), record_value(&record, |id| names.get(id).cloned()));
            }
        }
        let patch = diff(&ours_full, &result_full);

        // Who may apply it: a scene needs `edit:scene`, and an agent merging into the folder branch asks the person.
        let outside =
            patch.changes.iter().any(|c| c.path.segments().first().is_some_and(|s| s != "characters" && s != MERGES));
        let mut needs = Vec::new();
        if outside {
            needs.push(json!(Scope::EditScene));
        }
        let into_folder = matches!(this.lane, LaneState::Open { folder: true, .. });
        let asks_person = into_folder && matches!(ctx.actor, Actor::Agent { .. } | Actor::Plugin { .. });
        if asks_person {
            needs.push(json!("person"));
        }

        let titles: Vec<&str> =
            units.values().filter(|u| u.offers() && u.taken != "left-out").map(|u| u.unit.title.as_str()).collect();
        let label = format!("Merge {} into {} ({})", self.from, this.name, titles.join(", "));
        let clean = units.values().filter(|u| u.status == "clean").count();
        let summary = if blocked.is_empty() {
            format!("{} units merge cleanly{}", clean, if full { "" } else { "; this is a pick" })
        } else {
            format!("{clean} units merge cleanly; {} need a choice", blocked.len())
        };
        let unit_rows: Vec<Value> = units
            .values()
            .map(|u| {
                let changes: Vec<Value> = u
                    .entries
                    .iter()
                    .map(|e| {
                        json!({
                            "path": e.path.to_string(),
                            "base": e.base, "mine": e.ours, "theirs": e.theirs,
                            "result": lookup(&result, &e.path),
                        })
                    })
                    .collect();
                json!({
                    "key": u.unit.key, "title": u.unit.title, "kind": u.unit.kind,
                    "status": u.status, "taken": u.taken, "changes": changes,
                    "note": if u.notes.is_empty() { Value::Null } else { json!(u.notes.join("; ")) },
                })
            })
            .collect();
        let output = json!({
            "label": label,
            "from": self.from,
            "into": this.name,
            "base": base_id.map(|b| b.to_string()),
            "theirs": theirs_id.to_string(),
            "version": ctx.doc.version(),
            "full": full,
            "units": unit_rows,
            "rights": rights,
            "needs": needs,
            "summary": if base_full.is_none() { format!("{summary}; no common version: each difference is a choice") } else { summary },
        });

        if !blocked.is_empty() && !ctx.dry_run {
            let keys: Vec<&str> = blocked.iter().map(|u| u.unit.key.as_str()).collect();
            return Err(OpError::conflict(format!("these units need a choice: {}", keys.join(", "))).hint(
                "give each one mine or theirs in resolve (or leave it out with exclude); the dry-run shows them",
            ));
        }
        if outside {
            ctx.require_scope(Scope::EditScene)?;
        }
        if asks_person && !ctx.dry_run {
            ctx.require_person(format!("{label}: merging into the branch in the project folder"))?;
        }
        Ok(Plan { output: Some(output), ..Plan::edit(patch) })
    }
}
register_op!(BranchMerge);
