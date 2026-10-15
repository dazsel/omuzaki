//! The static check: which ops a script names, and what is wrong with the way it names them, found in the syntax tree
//! without running anything or touching a [`Calls`](crate::Calls) handle (`docs/design/scripting.md` sections 3 and 6).

use std::collections::BTreeSet;
use std::str::FromStr;

use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Grants, Scope};
use rhai::{ASTNode, Expr, FnCallExpr, Position, Stmt};
use serde::{Deserialize, Serialize};

use crate::ScriptHost;
use crate::bindings::{Bindings, OpEntry};
use crate::host::line_col;

/// A problem or a finding at a place in the source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// What is wrong, in a sentence.
    pub message: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub column: u32,
}

/// One `ops::area::verb(..)` call found in the source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpUse {
    /// The registry name, `area.verb_noun`.
    pub name: String,
    /// 1-based line of the call.
    pub line: u32,
    /// 1-based column of the call.
    pub column: u32,
}

/// Which way an argument map literal disagrees with the op's schema.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldErrorKind {
    /// The map names a field the op doesn't have.
    Unknown,
    /// The map lacks a field the op requires.
    Missing,
}

/// An argument map written as a literal that disagrees with the op's schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    /// The op's registry name.
    pub op: String,
    /// The field at fault.
    pub field: String,
    /// Unknown or missing.
    pub kind: FieldErrorKind,
    /// What is wrong, naming the fields the op has.
    pub message: String,
    /// 1-based line of the unknown key, or of the call for a missing field.
    pub line: u32,
    /// 1-based column, as `line`.
    pub column: u32,
}

/// A call to an op a script may not make in v1 (a job, an edit with outside effects, a nested `script.run`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    /// The op's registry name.
    pub op: String,
    /// Why, as the run would say it.
    pub message: String,
    /// What to do instead.
    pub hint: Option<String>,
    /// 1-based line of the call.
    pub line: u32,
    /// 1-based column of the call.
    pub column: u32,
}

/// What [`ScriptHost::check`] found. The JSON keys are the contract with the console and `script.check`:
/// `ops`, `unknown_ops`, `field_errors`, `refused`, `scopes`, `ceiling` and `parse_errors`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    /// Every op call in the syntax tree, including calls inside `fn`s and closures, in source order.
    pub ops: Vec<OpUse>,
    /// The calls whose op the registry lacks. A run fails the same way (`Function not found: ops::face::set_profil`).
    pub unknown_ops: Vec<OpUse>,
    /// Unknown and missing fields of argument maps written as literals.
    pub field_errors: Vec<FieldError>,
    /// Calls to ops a v1 script may not call; a run refuses them at the call, with the same message and hint.
    pub refused: Vec<Refused>,
    /// The scopes the named ops need, sorted. [`effective_grants`] adds `read`.
    pub scopes: Vec<Scope>,
    /// The scopes the optional first-line comment `// omuzaki: grants = ["edit:character"]` allows. It can only narrow.
    pub ceiling: Option<Vec<Scope>>,
    /// Syntax errors (at most one: the parser stops at the first), a source over the size limit, and an unreadable
    /// ceiling comment.
    pub parse_errors: Vec<Diagnostic>,
}

impl CheckReport {
    /// Whether the script can run: nothing above is a problem.
    pub fn is_clean(&self) -> bool {
        self.parse_errors.is_empty()
            && self.unknown_ops.is_empty()
            && self.field_errors.is_empty()
            && self.refused.is_empty()
    }

    /// Every problem, earliest first (by line, then column): what an editor underlines.
    pub fn problems(&self) -> Vec<Diagnostic> {
        let mut all: Vec<Diagnostic> = self.parse_errors.clone();
        all.extend(self.unknown_ops.iter().map(|u| Diagnostic {
            message: format!("unknown op `{}`", u.name),
            line: u.line,
            column: u.column,
        }));
        all.extend(self.field_errors.iter().map(|f| Diagnostic {
            message: f.message.clone(),
            line: f.line,
            column: f.column,
        }));
        all.extend(self.refused.iter().map(|r| Diagnostic {
            message: r.message.clone(),
            line: r.line,
            column: r.column,
        }));
        all.sort_by_key(|d| (d.line, d.column));
        all
    }

    /// The earliest problem, if any.
    pub fn first_problem(&self) -> Option<Diagnostic> {
        self.problems().into_iter().next()
    }
}

/// The grants a run gets (design section 3): the caller's, narrowed by the declared ceiling and by `read` plus the
/// scopes of the ops the script names. A script that only names `face.*` ops can't use `publish` even when its caller
/// holds it, and a ceiling naming a scope the caller lacks widens nothing. The ceiling always keeps `read`, which
/// every request needs; the caller's own `read` still decides whether it is held. The host adds no file roots.
pub fn effective_grants(caller: &Grants, report: &CheckReport) -> Grants {
    let named = Grants::of(std::iter::once(Scope::Read).chain(report.scopes.iter().copied()));
    let narrowed = caller.intersect(&named);
    match &report.ceiling {
        Some(ceiling) => narrowed.intersect(&Grants::of(std::iter::once(Scope::Read).chain(ceiling.iter().copied()))),
        None => narrowed,
    }
}

/// The check, plus the one problem a run treats differently: a ceiling it can't read.
pub(crate) struct Analysis {
    pub report: CheckReport,
    pub ceiling_error: Option<Diagnostic>,
}

impl ScriptHost {
    /// Parses and resolves `source` without running it: the ops it names with line and column (including calls inside
    /// `fn`s and closures), the names the registry lacks, the unknown and missing fields of argument maps written as
    /// literals, the scopes needed, the ceiling and the syntax errors. Reports every problem it finds, not just the
    /// first. It never runs anything and never reaches a [`Calls`](crate::Calls) handle.
    pub fn check(&self, source: &str) -> CheckReport {
        self.analyze(source).report
    }

    pub(crate) fn analyze(&self, source: &str) -> Analysis {
        let mut report = CheckReport::default();
        let mut ceiling_error = None;
        match ceiling_of(source) {
            Ok(ceiling) => report.ceiling = ceiling,
            Err(problem) => {
                report.parse_errors.push(problem.clone());
                ceiling_error = Some(problem);
            }
        }
        if source.len() > self.limits().source_bytes {
            report.parse_errors.push(Diagnostic {
                message: format!(
                    "the script is {} bytes, over the {}-byte source limit",
                    source.len(),
                    self.limits().source_bytes
                ),
                line: 1,
                column: 1,
            });
            return Analysis { report, ceiling_error };
        }
        match self.base_engine().compile(source) {
            Err(error) => {
                let (line, column) = line_col(error.1);
                report.parse_errors.push(Diagnostic { message: error.0.to_string(), line, column });
            }
            Ok(ast) => {
                let mut found = Found::default();
                ast.walk(&mut |nodes: &[ASTNode<'_>]| {
                    match nodes.last() {
                        Some(ASTNode::Expr(Expr::FnCall(call, at))) | Some(ASTNode::Stmt(Stmt::FnCall(call, at))) => {
                            found.call(&self.bindings, call, *at);
                        }
                        _ => {}
                    }
                    true
                });
                found.finish(&mut report);
            }
        }
        Analysis { report, ceiling_error }
    }
}

/// The calls the walk found, before they are sorted into the report.
#[derive(Default)]
struct Found {
    ops: Vec<OpUse>,
    unknown: Vec<OpUse>,
    fields: Vec<FieldError>,
    refused: Vec<Refused>,
    scopes: BTreeSet<Scope>,
}

impl Found {
    /// Files one call: an op call goes into the report, anything else is not the check's business.
    fn call(&mut self, bindings: &Bindings, call: &FnCallExpr, at: Position) {
        let path: Vec<&str> = call.namespace.path.iter().map(|segment| segment.name.as_str()).collect();
        if !Bindings::in_namespace(&path) {
            return;
        }
        let verb = call.name.as_str();
        let (line, column) = line_col(at);
        let Some(entry) = bindings.resolve(&path, verb) else {
            let name = Bindings::spelled(&path, verb).unwrap_or_else(|| format!("{}::{verb}", path.join("::")));
            self.unknown.push(OpUse { name, line, column });
            return;
        };
        self.ops.push(OpUse { name: entry.name.to_owned(), line, column });
        self.scopes.insert(entry.scope);
        if let Some(refusal) = &entry.refusal {
            self.refused.push(Refused {
                op: entry.name.to_owned(),
                message: refusal.message.clone(),
                hint: refusal.hint.clone(),
                line,
                column,
            });
            return;
        }
        if let Some(Expr::Map(literal, _)) = call.args.first() {
            let given: Vec<(&str, Position)> = literal.0.iter().map(|(key, _)| (key.name.as_str(), key.pos)).collect();
            self.fields.extend(field_errors(entry, &given, (line, column)));
        }
    }

    fn finish(self, report: &mut CheckReport) {
        report.ops = sorted(self.ops);
        report.unknown_ops = sorted(self.unknown);
        report.field_errors = self.fields;
        report.field_errors.sort_by_key(|f| (f.line, f.column, f.field.clone()));
        report.refused = self.refused;
        report.refused.sort_by_key(|r| (r.line, r.column));
        report.scopes = self.scopes.into_iter().collect();
    }
}

fn sorted(mut uses: Vec<OpUse>) -> Vec<OpUse> {
    uses.sort_by(|a, b| (a.line, a.column, &a.name).cmp(&(b.line, b.column, &b.name)));
    uses
}

/// What an argument map literal gets wrong against the op's schema. Nothing is reported for a schema that doesn't list
/// its properties (the fake registry's `true`, or an op that takes any map).
fn field_errors(entry: &OpEntry, given: &[(&str, Position)], call: (u32, u32)) -> Vec<FieldError> {
    let Some(properties) = entry.schema.get("properties").and_then(Value::as_object) else { return Vec::new() };
    let open = entry.schema.get("additionalProperties").is_some_and(|extra| !matches!(extra, Value::Bool(false)));
    let known: Vec<&str> = properties.keys().map(String::as_str).collect();
    let mut errors = Vec::new();
    if !open {
        for (key, at) in given {
            if !properties.contains_key(*key) {
                let (line, column) = line_col(*at);
                errors.push(FieldError {
                    op: entry.name.to_owned(),
                    field: (*key).to_owned(),
                    kind: FieldErrorKind::Unknown,
                    message: format!("`{}` has no field `{key}`; its fields are {}", entry.name, list(&known)),
                    line,
                    column,
                });
            }
        }
    }
    let required =
        entry.schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str);
    for field in required {
        if !given.iter().any(|(key, _)| *key == field) {
            errors.push(FieldError {
                op: entry.name.to_owned(),
                field: field.to_owned(),
                kind: FieldErrorKind::Missing,
                message: format!("`{}` needs the field `{field}`", entry.name),
                line: call.0,
                column: call.1,
            });
        }
    }
    errors
}

fn list(names: &[&str]) -> String {
    if names.is_empty() {
        return "none".to_owned();
    }
    names.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")
}

/// The ceiling in the first line, if there is one: `// omuzaki: grants = ["edit:character"]`. Any other `omuzaki:`
/// directive, a list that is not JSON strings and a scope that doesn't exist are errors, so a typo never leaves a
/// script with more than it meant to have.
fn ceiling_of(source: &str) -> Result<Option<Vec<Scope>>, Diagnostic> {
    let first = source.lines().next().unwrap_or_default();
    let Some(directive) = first.trim_start().strip_prefix("//").map(str::trim).and_then(|c| c.strip_prefix("omuzaki:"))
    else {
        return Ok(None);
    };
    let problem = |message: String| Diagnostic { message, line: 1, column: 1 };
    let Some(list) = directive.trim().strip_prefix("grants").map(str::trim_start).and_then(|d| d.strip_prefix('='))
    else {
        return Err(problem("the first-line directive must read `// omuzaki: grants = [\"scope\", ...]`".to_owned()));
    };
    let names: Vec<String> = serde_json::from_str(list.trim()).map_err(|_| {
        problem("the ceiling must be a list of scope names in quotes, like [\"edit:character\"]".to_owned())
    })?;
    let mut scopes = BTreeSet::new();
    for name in &names {
        scopes.insert(
            Scope::from_str(name).map_err(|_| problem(format!("the ceiling names an unknown scope `{name}`")))?,
        );
    }
    Ok(Some(scopes.into_iter().collect()))
}
