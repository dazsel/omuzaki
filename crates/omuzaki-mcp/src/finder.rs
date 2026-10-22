//! Reaching every op without listing every op: `ops_find`, `ops_describe`, `ops_run`, `tools_enable`, and `doc_get` for
//! the values a result names but doesn't print in full. A studio has a few hundred ops; a core tool list
//! ([`ToolMode::Core`](crate::catalog::ToolMode)) keeps their schemas out of the model's context until it asks for one.

use std::collections::BTreeSet;

use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{Grants, OpError, OpKind};
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::{Value, json};

use crate::catalog::{Catalog, Entry, Route};
use crate::render::{confirmation, value_of};
use crate::schema::{DRY_RUN, check_args};

/// The most ops `ops_find` lists.
const MAX_FOUND: usize = 25;
/// A value `doc_get` returns whole below this many bytes of JSON; a larger object or list comes back as an outline.
const OUTLINE_BYTES: usize = 8_000;
/// The longest scalar an outline shows inline, in characters.
const OUTLINE_SCALAR_CHARS: usize = 80;

/// `ops_find`: the ops whose name or summary holds every word of `query`, in `area` if given; without either, the
/// areas.
pub(crate) fn find(catalog: &Catalog, args: &JsonObject, grants: &Grants) -> Result<CallToolResult, OpError> {
    let query = text_arg(args, "query")?.unwrap_or_default().to_lowercase();
    let area = text_arg(args, "area")?;
    let areas = catalog.areas();
    if let Some(area) = area
        && !areas.contains_key(area)
    {
        let known: Vec<&str> = areas.keys().copied().collect();
        return Err(OpError::not_found(format!("there is no area named `{area}`"))
            .hint(format!("the areas are {}", known.join(", "))));
    }
    let words: Vec<&str> = query.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if words.is_empty() && area.is_none() {
        let lines: Vec<String> = areas.iter().map(|(area, n)| format!("  {area}: {n} ops")).collect();
        let text = format!(
            "{} ops in {} areas. Pass `query` (words for what you want) or `area` to list ops.\n{}",
            areas.values().sum::<usize>(),
            areas.len(),
            lines.join("\n")
        );
        return Ok(confirmation(text, json!({"ok": true, "areas": areas})));
    }
    let candidates: Vec<&Entry> = catalog.ops().filter(|e| area.is_none_or(|a| e.area() == Some(a))).collect();
    let scored = |all: bool| {
        let mut hits: Vec<(usize, &Entry)> =
            candidates.iter().filter_map(|e| score(e, &words, all).map(|s| (s, *e))).collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.op.cmp(&b.1.op)));
        hits
    };
    let mut hits = scored(true);
    let every_word = !hits.is_empty() || words.is_empty();
    if !every_word {
        hits = scored(false);
    }
    let total = hits.len();
    let shown: Vec<&Entry> = hits.into_iter().take(MAX_FOUND).map(|(_, e)| e).collect();
    let mut lines = vec![match (total, every_word) {
        (0, _) => format!("No op matches {query:?}. Try other words, or ops_find with no query for the areas."),
        (_, false) => format!("No op matches every word of {query:?}; the closest {}:", shown.len()),
        (n, true) if n > shown.len() => {
            format!("{n} ops match; the first {} (narrow the query for the rest):", shown.len())
        }
        (n, true) => format!("{n} op{} match{}:", if n == 1 { "" } else { "s" }, if n == 1 { "es" } else { "" }),
    }];
    lines.extend(shown.iter().map(|e| format!("  {}", found_line(e, grants))));
    if total > 0 {
        lines.push("Next: ops_describe {\"op\": ...} for an op's arguments, then ops_run.".to_owned());
    }
    let ops: Vec<Value> = shown.iter().map(|e| found_json(e, grants)).collect();
    Ok(confirmation(lines.join("\n"), json!({"ok": true, "total": total, "ops": ops})))
}

/// How well `entry` matches `words`: a hit in the name counts double. `None` when `all` asks for every word and one is
/// missing, or nothing matched.
fn score(entry: &Entry, words: &[&str], all: bool) -> Option<usize> {
    if words.is_empty() {
        return Some(0);
    }
    let name = entry.op.to_lowercase().replace(['.', '_'], " ");
    let summary = entry.summary().to_lowercase();
    let mut total = 0;
    for word in words {
        let hit = 2 * usize::from(name.contains(word)) + usize::from(summary.contains(word));
        if hit == 0 && all {
            return None;
        }
        total += hit;
    }
    (total > 0).then_some(total)
}

fn kind_word(entry: &Entry) -> &'static str {
    match entry.route {
        Route::Op { kind: OpKind::Edit, .. } => "edit",
        Route::Op { kind: OpKind::Query, .. } => "query",
        Route::Op { kind: OpKind::Job, .. } => "job",
        Route::Builtin(_) => "builtin",
    }
}

fn found_line(entry: &Entry, grants: &Grants) -> String {
    let granted = if grants.allows(entry.scope) { "" } else { ", NOT granted" };
    format!("{} ({}, `{}`{granted}): {}", entry.op, kind_word(entry), entry.scope, entry.summary())
}

fn found_json(entry: &Entry, grants: &Grants) -> Value {
    json!({
        "op": entry.op,
        "tool": entry.tool.name,
        "kind": kind_word(entry),
        "scope": entry.scope,
        "granted": grants.allows(entry.scope),
        "summary": entry.summary(),
    })
}

/// `ops_describe`: one op's description, schema and call.
pub(crate) fn describe(catalog: &Catalog, args: &JsonObject, listed: bool) -> Result<CallToolResult, OpError> {
    let entry = target(catalog, args)?;
    let schema = Value::Object(entry.tool.input_schema.as_ref().clone());
    let call = if listed {
        format!("Call it as the tool `{}`, or through ops_run.", entry.tool.name)
    } else {
        format!("Call it through ops_run {{\"op\": \"{}\", \"args\": {{...}}}}.", entry.op)
    };
    let text = format!(
        "{}\n\nUsage: {}\n{call}\nArguments (JSON Schema):\n{}",
        entry.tool.description.as_deref().unwrap_or_default(),
        entry.signature,
        serde_json::to_string_pretty(&schema).unwrap_or_else(|_| schema.to_string())
    );
    let data = json!({
        "ok": true,
        "op": entry.op,
        "tool": entry.tool.name,
        "kind": kind_word(entry),
        "scope": entry.scope,
        "listed": listed,
        "usage": entry.signature,
        "input_schema": schema,
    });
    Ok(confirmation(text, data))
}

/// `ops_run`: the op it names and the arguments to call that op's tool with.
pub(crate) fn gateway<'a>(
    catalog: &'a Catalog,
    args: &JsonObject,
    schema: &JsonObject,
) -> Result<(&'a Entry, JsonObject), OpError> {
    check_args(args, schema)?;
    let entry = target(catalog, args)?;
    let mut call = match args.get("args") {
        None | Some(Value::Null) => JsonObject::new(),
        Some(Value::Object(call)) => call.clone(),
        Some(_) => {
            return Err(OpError::invalid("args", "`args` must be an object of the op's arguments")
                .hint(format!("e.g. {}", entry.signature)));
        }
    };
    if let Some(dry_run) = args.get(DRY_RUN) {
        call.insert(DRY_RUN.to_owned(), dry_run.clone());
    }
    Ok((entry, call))
}

/// The op `args.op` names: any op tool, by op or tool name. Built-ins are refused, since they are called directly.
fn target<'a>(catalog: &'a Catalog, args: &JsonObject) -> Result<&'a Entry, OpError> {
    let name = text_arg(args, "op")?.ok_or_else(|| {
        OpError::invalid("op", "missing required argument `op`").hint("pass the op name, e.g. `face.set_profile`")
    })?;
    match catalog.get(name) {
        Some(entry) if entry.area().is_some() => Ok(entry),
        Some(entry) => Err(OpError::invalid("op", format!("`{name}` is a built-in tool, not an op"))
            .hint(format!("call `{}` directly", entry.tool.name))),
        None => {
            let similar = catalog.similar(name);
            let hint = if similar.is_empty() {
                "find it with ops_find {\"query\": \"...\"}".to_owned()
            } else {
                format!("did you mean `{}`? (or find it with ops_find)", similar.join("`, `"))
            };
            Err(OpError::not_found(format!("there is no op named `{name}`")).hint(hint))
        }
    }
}

/// `tools_enable`: turns `areas` on (or off) for this session. Returns the result and whether the list changed.
pub(crate) fn enable(
    catalog: &Catalog,
    enabled: &mut BTreeSet<String>,
    args: &JsonObject,
) -> Result<(CallToolResult, bool), OpError> {
    let areas = catalog.areas();
    let wanted: Vec<&str> = match args.get("areas") {
        Some(Value::Array(items)) if !items.is_empty() => items
            .iter()
            .map(|v| v.as_str().ok_or_else(|| OpError::invalid("areas", "each area is a string")))
            .collect::<Result<_, _>>()?,
        _ => {
            return Err(
                OpError::invalid("areas", "`areas` must be a list of area names").hint("e.g. [\"stage\", \"output\"]")
            );
        }
    };
    if let Some(unknown) = wanted.iter().find(|a| !areas.contains_key(*a)) {
        let known: Vec<&str> = areas.keys().copied().collect();
        return Err(OpError::invalid("areas", format!("there is no area named `{unknown}`"))
            .hint(format!("the areas are {}", known.join(", "))));
    }
    let disable = args.get("disable").and_then(Value::as_bool).unwrap_or(false);
    let before = enabled.clone();
    for area in &wanted {
        if disable {
            enabled.remove(*area);
        } else {
            enabled.insert((*area).to_owned());
        }
    }
    let changed = *enabled != before;
    let tools: Vec<&str> =
        catalog.ops().filter(|e| e.area().is_some_and(|a| wanted.contains(&a))).map(|e| e.tool.name.as_ref()).collect();
    let text = match (disable, changed) {
        (false, true) => format!(
            "Listed {} tools: {}. Your client should refresh its tool list now; until it does, call them through ops_run.",
            tools.len(),
            tools.join(", ")
        ),
        (false, false) => format!("Already listed: {}.", tools.join(", ")),
        (true, true) => format!("Took {} tools off the list; ops_run still calls them.", tools.len()),
        (true, false) => "Those areas weren't listed; nothing changed.".to_owned(),
    };
    let data = json!({"ok": true, "changed": changed, "enabled": enabled, "tools": tools});
    Ok((confirmation(text, data), changed))
}

/// The path `doc_get` reads.
pub(crate) fn doc_path(args: &JsonObject) -> Result<(DocPath, bool), OpError> {
    let text = text_arg(args, "path")?.ok_or_else(|| {
        OpError::invalid("path", "missing required argument `path`")
            .hint("e.g. `characters/mira`; `\"\"` reads the whole document")
    })?;
    let path = DocPath::parse(text).map_err(|e| {
        OpError::invalid("path", e.to_string()).hint("paths put `/` between keys, e.g. `characters/mira/face`")
    })?;
    Ok((path, args.get("full").and_then(Value::as_bool).unwrap_or(false)))
}

/// `doc_get`'s answer: the value whole, or an outline of a large object or list.
pub(crate) fn doc_value(path: &DocPath, value: Option<Value>, full: bool) -> Result<CallToolResult, OpError> {
    let shown = if path.to_string().is_empty() { "the document".to_owned() } else { format!("`{path}`") };
    let Some(value) = value else {
        let parent = path.to_string().rsplit_once('/').map_or_else(String::new, |(p, _)| p.to_owned());
        return Err(OpError::not_found(format!("nothing is stored at {shown}"))
            .hint(format!("read the parent (`{parent}`) to see what it holds")));
    };
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    let large = text.len() > OUTLINE_BYTES && (value.is_object() || value.is_array());
    if full || !large {
        let data = json!({"ok": true, "path": path.to_string(), "value": value_of(&value)});
        return Ok(confirmation(format!("{shown}:\n{text}"), data));
    }
    let children: Vec<(String, &Value)> = match &value {
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Value::Array(items) => items.iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(),
        _ => Vec::new(),
    };
    let base = path.to_string();
    let lines: Vec<String> = children.iter().map(|(key, v)| format!("  {key}: {}", outline(v))).collect();
    let outline_json: serde_json::Map<String, Value> =
        children.iter().map(|(key, v)| (key.clone(), json!(outline(v)))).collect();
    let deeper = children.first().map_or_else(String::new, |(k, _)| {
        if base.is_empty() { format!(" (e.g. `{k}`)") } else { format!(" (e.g. `{base}/{k}`)") }
    });
    let text = format!(
        "{shown} is large ({} KB), so here is its outline. Read a deeper path{deeper}, or pass full=true for all of it.\n{}",
        text.len() / 1000,
        lines.join("\n")
    );
    Ok(confirmation(text, json!({"ok": true, "path": base, "outline": outline_json})))
}

/// One child in an outline: a short scalar as itself, anything else as its type and size.
fn outline(value: &Value) -> String {
    let bytes = || value.to_string().len();
    match value {
        Value::Object(map) => format!("object, {} keys, {} bytes", map.len(), bytes()),
        Value::Array(items) => format!("list of {}, {} bytes", items.len(), bytes()),
        _ => {
            let text = value.to_string();
            if text.chars().count() <= OUTLINE_SCALAR_CHARS {
                text
            } else {
                format!("text, {} chars", text.chars().count())
            }
        }
    }
}

fn text_arg<'a>(args: &'a JsonObject, key: &str) -> Result<Option<&'a str>, OpError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.as_str())),
        Some(_) => Err(OpError::invalid(key, format!("`{key}` must be a string"))),
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::{OpInfo, OpMeta, Scope, Undo};

    use super::*;
    use crate::catalog::ToolMode;
    use crate::describe::Guidance;

    fn op(name: &'static str, summary: &'static str, scope: Scope) -> OpInfo {
        let meta = OpMeta { name, summary, kind: OpKind::Edit, scope, undo: Undo::Auto };
        let schema = serde_json::from_value(json!({"type": "object", "properties": {"on": {"type": "boolean"}}}));
        OpInfo::new(meta, schema.unwrap())
    }

    fn catalog(mode: ToolMode) -> Catalog {
        let ops = [
            op("output.virtual_camera", "Send the program to a virtual camera other apps can pick.", Scope::Broadcast),
            op("output.window", "Open a capture window for OBS.", Scope::Broadcast),
            op("stage.set_backdrop", "Set the stage's backdrop: a colour, an image or transparent.", Scope::EditScene),
            op("face.set_profile", "Shape the face's profile.", Scope::EditCharacter),
        ];
        Catalog::build(&ops, &Grants::agent_default(), &Guidance::default(), (false, false), mode).unwrap()
    }

    fn args(value: Value) -> JsonObject {
        match value {
            Value::Object(object) => object,
            other => panic!("not an object: {other}"),
        }
    }

    fn text(result: &CallToolResult) -> String {
        result.content[0].as_text().unwrap().text.clone()
    }

    #[test]
    fn find_matches_every_word_in_name_or_summary_and_says_what_is_not_granted() {
        let catalog = catalog(ToolMode::Core);
        let found = find(&catalog, &args(json!({"query": "virtual camera"})), &Grants::agent_default()).unwrap();
        let text = text(&found);
        assert!(text.starts_with("1 op matches:"), "{text}");
        assert!(text.contains("output.virtual_camera (edit, `broadcast`, NOT granted): Send the program"), "{text}");
        let found = find(&catalog, &args(json!({"query": "transparent backdrop"})), &Grants::agent_default()).unwrap();
        assert_eq!(found.structured_content.unwrap()["ops"][0]["op"], "stage.set_backdrop");
    }

    #[test]
    fn find_falls_back_to_the_closest_and_lists_areas_without_a_query() {
        let catalog = catalog(ToolMode::Core);
        let found = find(&catalog, &args(json!({"query": "obs camera xyz"})), &Grants::agent_default()).unwrap();
        assert!(text(&found).starts_with("No op matches every word"), "{}", text(&found));
        let areas = find(&catalog, &JsonObject::new(), &Grants::agent_default()).unwrap();
        assert!(text(&areas).contains("  output: 2 ops"));
        let err = find(&catalog, &args(json!({"area": "nope"})), &Grants::agent_default()).unwrap_err();
        assert!(err.hint.unwrap().contains("face, output, stage"));
    }

    #[test]
    fn describe_shows_the_schema_and_how_to_call_it() {
        let catalog = catalog(ToolMode::Core);
        let described = describe(&catalog, &args(json!({"op": "stage_set_backdrop"})), false).unwrap();
        let text = text(&described);
        assert!(text.starts_with("stage.set_backdrop: Set the stage's backdrop"), "{text}");
        assert!(text.contains("ops_run {\"op\": \"stage.set_backdrop\""), "{text}");
        assert!(text.contains("\"dry_run\""), "{text}");
        let err = describe(&catalog, &args(json!({"op": "history.undo"})), false).unwrap_err();
        assert_eq!(err.hint.as_deref(), Some("call `history_undo` directly"));
        let err = describe(&catalog, &args(json!({"op": "stage.set_backdrp"})), false).unwrap_err();
        assert!(err.hint.unwrap().contains("stage_set_backdrop"));
    }

    #[test]
    fn the_gateway_passes_args_and_dry_run_to_the_op() {
        let catalog = catalog(ToolMode::Core);
        let schema = catalog.get("ops_run").unwrap().tool.input_schema.clone();
        let (entry, call) =
            gateway(&catalog, &args(json!({"op": "output.window", "args": {"on": true}, "dry_run": true})), &schema)
                .unwrap();
        assert_eq!(entry.op, "output.window");
        assert_eq!(Value::Object(call), json!({"on": true, "dry_run": true}));
        assert!(gateway(&catalog, &args(json!({"op": "output.window", "argz": {}})), &schema).is_err());
    }

    #[test]
    fn enabling_an_area_lists_its_ops_and_says_whether_anything_changed() {
        let catalog = catalog(ToolMode::Core);
        let mut enabled = BTreeSet::new();
        let listed =
            |enabled: &BTreeSet<String>| catalog.listed(enabled).filter(|t| t.name.starts_with("output")).count();
        assert_eq!(listed(&enabled), 0);
        let (_, changed) = enable(&catalog, &mut enabled, &args(json!({"areas": ["output"]}))).unwrap();
        assert!(changed);
        assert_eq!(listed(&enabled), 2);
        let (_, changed) = enable(&catalog, &mut enabled, &args(json!({"areas": ["output"]}))).unwrap();
        assert!(!changed);
        let (_, changed) =
            enable(&catalog, &mut enabled, &args(json!({"areas": ["output"], "disable": true}))).unwrap();
        assert!(changed && enabled.is_empty());
        assert!(enable(&catalog, &mut enabled, &args(json!({"areas": ["outputs"]}))).is_err());
    }

    #[test]
    fn a_core_list_has_the_finder_and_the_all_list_has_every_op() {
        let core = catalog(ToolMode::Core);
        let names: Vec<String> = core.listed(&BTreeSet::new()).map(|t| t.name.to_string()).collect();
        assert!(names.contains(&"ops_run".to_owned()) && names.contains(&"tools_enable".to_owned()));
        assert!(!names.iter().any(|n| n.starts_with("face_")), "{names:?}");
        assert!(core.get("face_set_profile").is_some(), "an unlisted op stays callable");
        let all = catalog(ToolMode::All);
        let names: Vec<String> = all.listed(&BTreeSet::new()).map(|t| t.name.to_string()).collect();
        assert!(names.contains(&"face_set_profile".to_owned()) && !names.contains(&"ops_run".to_owned()));
        assert!(names.contains(&"ops_find".to_owned()) && names.contains(&"doc_get".to_owned()));
    }

    #[test]
    fn doc_get_outlines_a_large_value_and_names_a_missing_one() {
        let path = DocPath::parse("characters").unwrap();
        let big = json!({"mira": {"mesh": "x".repeat(9_000)}, "kit": {"name": "Kit"}});
        let text = text(&doc_value(&path, Some(big.clone()), false).unwrap());
        assert!(text.contains("is large") && text.contains("  mira: object, 1 keys"), "{text}");
        assert!(text.contains("`characters/mira`") || text.contains("`characters/kit`"), "{text}");
        let whole = doc_value(&path, Some(big), true).unwrap();
        assert!(text_of(&whole).contains(&"x".repeat(9_000)));
        let small = doc_value(&path, Some(json!({"kit": 1})), false).unwrap();
        assert!(text_of(&small).contains("\"kit\": 1"));
        let err = doc_value(&DocPath::parse("characters/zed").unwrap(), None, false).unwrap_err();
        assert!(err.hint.unwrap().contains("`characters`"));
    }

    fn text_of(result: &CallToolResult) -> String {
        text(result)
    }
}
