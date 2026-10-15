//! Saved scripts and macros: what `script.save`, `script.delete`, `script.list` and `script.run` share about an item,
//! wherever it lives. In a project it is the document node `scripts/<id>`; in My scripts it is a [`StoredScript`].

use omuzaki_doc_api::{DocPath, DocRead, Patch, Value};
use omuzaki_ops_api::{Actor, OpCall, OpCtx, OpError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};

use crate::store::{ScriptKind, ScriptStore, StoredScript};
use crate::{Limits, Macro, RecordedCall, from_jsonl, to_jsonl};

/// The document node that holds a project's saved scripts, a map from id to item.
const SCRIPTS: &str = "scripts";

/// The longest name of a saved item, in characters.
pub const MAX_NAME_CHARS: usize = 100;

/// The largest macro file, in bytes. A macro is data, not source, so it may be longer than a script.
pub const MAX_MACRO_BYTES: usize = 1 << 20;

/// Where a saved item lives.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    /// In the project, as the document node `scripts/<id>`: undoable, journalled and visible to agents.
    Project,
    /// In My scripts, through the app's [`ScriptStore`]: the person's own, across projects.
    Mine,
}

/// What an item holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Body {
    /// Rhai source.
    Source(String),
    /// Recorded op calls.
    Steps(Vec<RecordedCall>),
}

impl Body {
    /// Checks the sizes and shapes a saved item must have.
    ///
    /// # Errors
    /// `Invalid`, naming `source` or `steps`.
    pub(crate) fn validated(source: Option<&str>, steps: Option<&[RecordedCall]>) -> Result<Self, OpError> {
        match (source, steps) {
            (Some(source), None) => {
                let limit = Limits::console().source_bytes;
                if source.len() > limit {
                    return Err(OpError::invalid(
                        "source",
                        format!("the script is {} bytes, over the {limit}-byte limit", source.len()),
                    )
                    .hint("split it into smaller scripts"));
                }
                Ok(Self::Source(source.to_owned()))
            }
            (None, Some(steps)) => {
                if steps.iter().any(|step| step.op.is_empty() || !step.args.is_object()) {
                    return Err(OpError::invalid("steps", "every step needs an `op` name and `args` as an object"));
                }
                let body = Self::Steps(steps.to_vec());
                let size = body.text("").len();
                if size > MAX_MACRO_BYTES {
                    return Err(OpError::invalid(
                        "steps",
                        format!("the macro is {size} bytes, over the {MAX_MACRO_BYTES}-byte limit"),
                    ));
                }
                Ok(body)
            }
            _ => Err(OpError::invalid("source", "give exactly one of `source` (a script) and `steps` (a macro)")
                .hint("a script is Rhai text; a macro is a list of {op, args} calls")),
        }
    }

    /// Script or macro.
    pub(crate) fn kind(&self) -> ScriptKind {
        match self {
            Self::Source(_) => ScriptKind::Script,
            Self::Steps(_) => ScriptKind::Macro,
        }
    }

    /// The text that is stored and hashed: the source, or the macro file named `name`.
    pub(crate) fn text(&self, name: &str) -> String {
        match self {
            Self::Source(source) => source.clone(),
            Self::Steps(steps) => to_jsonl(&Macro { name: name.to_owned(), steps: steps.clone() }),
        }
    }
}

/// An item as the ops see it, from either place.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    /// Its id.
    pub(crate) id: String,
    /// Its name.
    pub(crate) name: String,
    /// What it holds.
    pub(crate) body: Body,
    /// The hash of its stored text.
    pub(crate) hash: String,
    /// Who saved it last, when known (project items only).
    pub(crate) author: Option<Value>,
}

impl Item {
    /// The item as a project node.
    pub(crate) fn node(name: &str, body: &Body, author: &Actor, hash: &str) -> Value {
        let mut node = Map::new();
        node.insert("name".into(), json!(name));
        node.insert("kind".into(), json!(body.kind()));
        match body {
            Body::Source(source) => node.insert("source".into(), json!(source)),
            Body::Steps(steps) => node.insert("steps".into(), json!(steps)),
        };
        node.insert("author".into(), json!(author));
        node.insert("hash".into(), json!(hash));
        Value::Object(node)
    }

    /// Reads a project node.
    ///
    /// # Errors
    /// A sentence saying what is wrong with the node.
    pub(crate) fn from_node(id: &str, node: &Value) -> Result<Self, String> {
        let fields = node.as_object().ok_or("the node is not a map")?;
        let name = fields.get("name").and_then(Value::as_str).ok_or("the node has no `name`")?.to_owned();
        let body = match fields.get("kind").and_then(Value::as_str) {
            Some("script") => {
                Body::Source(fields.get("source").and_then(Value::as_str).ok_or("a script needs `source`")?.to_owned())
            }
            Some("macro") => {
                let steps = serde_json::from_value(fields.get("steps").cloned().unwrap_or(Value::Null))
                    .map_err(|e| format!("`steps` is not a list of {{op, args}} calls: {e}"))?;
                Body::Steps(steps)
            }
            _ => return Err("`kind` must be `script` or `macro`".to_owned()),
        };
        let hash = content_hash(&body.text(&name));
        Ok(Self { id: id.to_owned(), name, body, hash, author: fields.get("author").cloned() })
    }

    /// Reads a My scripts item.
    ///
    /// # Errors
    /// A sentence saying what is wrong with a macro file that doesn't read.
    pub(crate) fn from_stored(stored: &StoredScript) -> Result<Self, String> {
        let body = match stored.kind {
            ScriptKind::Script => Body::Source(stored.body.clone()),
            ScriptKind::Macro => Body::Steps(from_jsonl(&stored.body).map_err(|e| e.to_string())?.steps),
        };
        Ok(Self { id: stored.id.clone(), name: stored.name.clone(), body, hash: stored.hash.clone(), author: None })
    }

    /// The item as My scripts keeps it.
    pub(crate) fn stored(id: &str, name: &str, body: &Body) -> StoredScript {
        let text = body.text(name);
        StoredScript {
            id: id.to_owned(),
            name: name.to_owned(),
            kind: body.kind(),
            hash: content_hash(&text),
            body: text,
        }
    }
}

/// The content hash of a saved item's text: the first 128 bits of its BLAKE3 hash, in hex. A consent card is
/// remembered per (project, hash), so two different texts must not share one.
pub fn content_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().chars().take(32).collect()
}

/// The path of the project's saved scripts.
fn scripts_path() -> DocPath {
    DocPath::root().join(SCRIPTS)
}

/// The path of one project item.
pub(crate) fn item_path(id: &str) -> DocPath {
    scripts_path().join(id)
}

/// The project's saved items by id, as written.
pub(crate) fn project_nodes(doc: &dyn DocRead) -> Vec<(String, Value)> {
    match doc.get(&scripts_path()) {
        Some(Value::Object(items)) => items.iter().map(|(id, node)| (id.clone(), node.clone())).collect(),
        _ => Vec::new(),
    }
}

/// The patch that writes (`node` is `Some`) or removes (`None`) the project item `id`. The first save creates the
/// `scripts` node with the item in it, as a patch can't create a node's parent.
///
/// # Errors
/// `Invalid` when `scripts` exists but is not a map; `NotFound` when there is nothing to remove.
pub(crate) fn project_patch(doc: &dyn DocRead, id: &str, node: Option<Value>) -> Result<Patch, OpError> {
    let at = item_path(id);
    match (doc.get(&scripts_path()), node) {
        (Some(Value::Object(_)), node) => {
            let before = doc.get(&at).cloned();
            if before.is_none() && node.is_none() {
                return Err(not_saved(id, Place::Project));
            }
            Ok(Patch::new().with(at, before, node))
        }
        (Some(_), _) => Err(OpError::invalid("id", "the document's `scripts` node is not a map")
            .hint("remove or repair `scripts` before saving a script into the project")),
        (None, Some(node)) => Ok(Patch::new().with(scripts_path(), None, Some(json!({ id: node })))),
        (None, None) => Err(not_saved(id, Place::Project)),
    }
}

/// The error for an id with nothing saved under it.
pub(crate) fn not_saved(id: &str, place: Place) -> OpError {
    let place = match place {
        Place::Project => "the project",
        Place::Mine => "My scripts",
    };
    OpError::not_found(format!("nothing is saved as `{id}` in {place}")).hint("list what is saved with `script.list`")
}

/// The My scripts store, when the app wired one.
///
/// # Errors
/// `Unavailable` naming the service when none is wired.
pub(crate) fn store<'a>(ctx: &OpCtx<'a>) -> Result<&'a dyn ScriptStore, OpError> {
    ctx.service::<dyn ScriptStore>()
}

/// A call of `op` with `args`, for an effect's `undo` and `redo`.
pub(crate) fn call(op: &str, args: Value) -> OpCall {
    OpCall { op: op.to_owned(), args }
}
