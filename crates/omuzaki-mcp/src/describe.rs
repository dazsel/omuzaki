//! The words an agent reads: tool descriptions, annotations and the server's usage guide. Everything here is derived
//! from the registry's facts (kind, scope, undo) and from which tools actually exist, so it can't point an agent at a
//! tool that isn't there or go stale when ops change.

use omuzaki_ops_api::{Grants, OpInfo, OpKind, Scope, Undo};
use rmcp::model::ToolAnnotations;

/// How to verify work, derived from the tools the runner actually offers.
#[derive(Clone, Debug, Default)]
pub(crate) struct Guidance {
    /// "check it visually with `render_preview` and numerically with the `measure_*` tools", when those exist.
    verify: Option<String>,
}

impl Guidance {
    /// Looks at the registered op names to decide what can be recommended.
    pub(crate) fn new<'a>(op_names: impl IntoIterator<Item = &'a str> + Clone) -> Self {
        let has_preview = op_names.clone().into_iter().any(|n| n == "render.preview");
        let has_measure = op_names.into_iter().any(|n| n.starts_with("measure."));
        let verify = match (has_preview, has_measure) {
            (true, true) => Some("check it visually with `render_preview` and numerically with the `measure_*` tools"),
            (true, false) => Some("check it visually with `render_preview`"),
            (false, true) => Some("check it numerically with the `measure_*` tools"),
            (false, false) => None,
        };
        Self { verify: verify.map(str::to_owned) }
    }

    /// The "check afterwards" advice for edit tools, if there is anything to check with.
    pub(crate) fn verify(&self) -> Option<&str> {
        self.verify.as_deref()
    }
}

/// The full description of an op tool: what it does (the op's summary), what kind of call it is, whether this session
/// may run it, and how to use it safely.
pub(crate) fn tool_description(info: &OpInfo, grants: &Grants, guidance: &Guidance) -> String {
    let mut lines = vec![
        format!("{}: {}", info.name, info.summary),
        String::new(),
        kind_line(info).to_owned(),
        scope_line(info, grants),
    ];
    if info.kind != OpKind::Query {
        lines.push(
            "Preview first: pass dry_run=true to see exactly what would change, and the cost estimate, without applying \
             anything."
                .to_owned(),
        );
    }
    if info.kind == OpKind::Edit
        && let Some(verify) = guidance.verify()
    {
        lines.push(format!("Afterwards: {verify}."));
    }
    lines.join("\n")
}

fn kind_line(info: &OpInfo) -> &'static str {
    match (info.kind, info.undo) {
        (OpKind::Query, _) => "Read-only: never changes the project.",
        (OpKind::Job, Undo::Auto) => {
            "Long-running job (it may take a while). Its result in the document is undoable with history_undo."
        }
        (OpKind::Job, Undo::Explicit) => {
            "Long-running job (it may take a while) with effects beyond the document. Not undoable yet: history_undo \
             refuses to undo it, so be sure before applying."
        }
        (OpKind::Job, Undo::None) => "Long-running job (it may take a while). Not undoable.",
        (OpKind::Edit, Undo::Auto) => "Changes the project. Undoable with history_undo.",
        (OpKind::Edit, Undo::Explicit) => {
            "Changes the project and has effects beyond the document (files, devices, streams). Not undoable yet: \
             history_undo refuses to undo it, so be sure before applying."
        }
        (OpKind::Edit, Undo::None) => "Changes the project. Not undoable: be sure before applying.",
    }
}

/// Every call needs `read` (a plan or a conflict message reveals document values) plus the op's own scope; a preview
/// needs only `read`.
fn scope_line(info: &OpInfo, grants: &Grants) -> String {
    let scope = info.scope;
    let mut missing: Vec<Scope> = [Scope::Read, scope].into_iter().filter(|s| !grants.allows(*s)).collect();
    missing.dedup();
    if missing.is_empty() {
        return format!("Scope `{scope}`: granted in this session.");
    }
    let names: Vec<String> = missing.iter().map(|s| format!("`{s}`")).collect();
    let preview = if info.kind != OpKind::Query && grants.allows(Scope::Read) {
        " Previews (dry_run=true) still work."
    } else {
        ""
    };
    format!(
        "Scope `{scope}`: NOT granted in this session (missing {}), so calls are refused with `denied`. Ask the user \
         to grant it.{preview}",
        names.join(" and ")
    )
}

/// MCP annotations. They are hints for clients' approval UIs, derived only from registry facts.
pub(crate) fn annotations(info: &OpInfo) -> ToolAnnotations {
    let reaches_outside = matches!(info.scope, Scope::Net | Scope::Publish | Scope::Spend | Scope::Broadcast);
    let read_only = info.kind == OpKind::Query;
    ToolAnnotations::new()
        .read_only(read_only)
        // Edits are undoable, so they are not "destructive" in MCP's sense; only a non-query that can't be undone is.
        .destructive(!read_only && info.undo == Undo::None)
        .idempotent(read_only)
        .open_world(reaches_outside)
}

/// The server-level guide a client shows its model once per session (`instructions` in the handshake).
pub(crate) fn instructions(grants: &Grants, guidance: &Guidance) -> String {
    let granted: Vec<&str> = Scope::ALL.into_iter().filter(|s| grants.allows(*s)).map(Scope::name).collect();
    let mut lines = vec![
        "Omuzaki is a character studio. Every tool is an operation on the project; the tool list is generated from the \
         op registry."
            .to_owned(),
        "Working method:".to_owned(),
        "- Look before you edit: tools marked read-only never change anything.".to_owned(),
        "- Preview edits with dry_run=true to see the exact changes; then call again without it to apply.".to_owned(),
        "- Group edits that belong together with tx_begin ... tx_commit so they undo as one step; history_undo and \
         history_redo step through history."
            .to_owned(),
    ];
    if let Some(verify) = guidance.verify() {
        lines.push(format!("- After editing, {verify}. Don't assume an edit looks right."));
    }
    lines.push(
        "- A failed call is a tool error with `code`, `field` and `hint`: fix the named argument and retry. `denied` \
         means this session lacks a scope; tell the user rather than retrying."
            .to_owned(),
    );
    lines.push(format!("Scopes granted this session: {}.", granted.join(", ")));
    if !grants.allows(Scope::Read) {
        lines.push(
            "WARNING: `read` is not granted, and every call needs it: all tools will be refused until the user grants \
             it."
            .to_owned(),
        );
    }
    if grants.allows(Scope::Read) {
        lines.push(
            "Read the resource omuzaki://log to see what changed in the project, including edits by the user and other \
             agents (a client that can't read resources calls history_log instead, with after_n to skip what it has \
             seen); if your client supports resource subscriptions, subscribe to it to be notified."
                .to_owned(),
        );
        lines.push(
            "Read omuzaki://project for a summary of the project and omuzaki://character/<id> for everything stored for \
             one character, instead of guessing what exists."
                .to_owned(),
        );
        lines.push(
            "You don't have to poll for that: when the user or another agent has changed the project since your last \
             call, the result starts with a \"Heads-up\" naming who, which tool and which paths (also in \
             structuredContent.since_your_last_call). Re-read what you rely on before editing it. No heads-up means \
             nothing changed; your own edits are never listed."
                .to_owned(),
        );
    }
    lines.push(
        "Tool names are area_verb (history_undo); each description starts with the op name as area.verb, which also \
         works as a tool name."
            .to_owned(),
    );
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::OpMeta;
    use rmcp::model::ToolAnnotations;

    use super::*;

    fn info(kind: OpKind, scope: Scope, undo: Undo) -> OpInfo {
        let meta = OpMeta { name: "face.set_profile", summary: "Set the face profile.", kind, scope, undo };
        OpInfo::new(meta, schemars::Schema::default())
    }

    fn guidance() -> Guidance {
        Guidance::new(["face.set_profile", "render.preview", "measure.silhouette"])
    }

    #[test]
    fn edit_description_says_kind_scope_preview_and_what_to_check() {
        let text = tool_description(
            &info(OpKind::Edit, Scope::EditCharacter, Undo::Auto),
            &Grants::agent_default(),
            &guidance(),
        );
        assert!(text.starts_with("face.set_profile: Set the face profile.\n"));
        for part in [
            "Undoable with history_undo",
            "`edit:character`: granted",
            "dry_run=true",
            "`render_preview`",
            "`measure_*`",
        ] {
            assert!(text.contains(part), "missing {part:?} in:\n{text}");
        }
    }

    #[test]
    fn ungranted_scopes_are_flagged_and_queries_skip_preview_advice() {
        let publish =
            tool_description(&info(OpKind::Job, Scope::Publish, Undo::Explicit), &Grants::agent_default(), &guidance());
        assert!(publish.contains("NOT granted") && publish.contains("Long-running"));
        assert!(publish.contains("refuses to undo") && !publish.contains("reverses them"), "{publish}");
        let query =
            tool_description(&info(OpKind::Query, Scope::Read, Undo::None), &Grants::agent_default(), &guidance());
        assert!(query.contains("Read-only") && !query.contains("dry_run") && !query.contains("Afterwards"));
    }

    #[test]
    fn every_call_needs_read_and_a_missing_read_is_called_out() {
        let no_read = Grants::of([Scope::EditCharacter]);
        let text = tool_description(&info(OpKind::Edit, Scope::EditCharacter, Undo::Auto), &no_read, &guidance());
        assert!(
            text.contains("NOT granted") && text.contains("missing `read`") && !text.contains("still work"),
            "{text}"
        );
        let preview =
            tool_description(&info(OpKind::Edit, Scope::Publish, Undo::Auto), &Grants::agent_default(), &guidance());
        assert!(
            preview.contains("missing `publish`") && preview.contains("Previews (dry_run=true) still work."),
            "{preview}"
        );
    }

    #[test]
    fn never_recommends_tools_that_do_not_exist() {
        let none = Guidance::new(["face.set_profile"]);
        let text =
            tool_description(&info(OpKind::Edit, Scope::EditCharacter, Undo::Auto), &Grants::agent_default(), &none);
        assert!(!text.contains("render.") && !text.contains("measure."));
        let guide = instructions(&Grants::agent_default(), &none);
        assert!(!guide.contains("render.preview") && !guide.contains("measure."));
    }

    #[test]
    fn annotations_follow_kind_and_undo() {
        let q = annotations(&info(OpKind::Query, Scope::Read, Undo::None));
        assert_eq!((q.read_only_hint, q.destructive_hint), (Some(true), Some(false)));
        let edit = annotations(&info(OpKind::Edit, Scope::EditScene, Undo::Auto));
        assert_eq!(
            (edit.read_only_hint, edit.destructive_hint, edit.open_world_hint),
            (Some(false), Some(false), Some(false))
        );
        let final_edit: ToolAnnotations = annotations(&info(OpKind::Edit, Scope::Publish, Undo::None));
        assert_eq!((final_edit.destructive_hint, final_edit.open_world_hint), (Some(true), Some(true)));
    }

    #[test]
    fn instructions_warn_when_nothing_can_work() {
        assert!(instructions(&Grants::of([Scope::Render]), &guidance()).contains("WARNING: `read` is not granted"));
        assert!(!instructions(&Grants::agent_default(), &guidance()).contains("WARNING"));
        assert!(!instructions(&Grants::of([Scope::Render]), &guidance()).contains("omuzaki://log"));
    }

    #[test]
    fn instructions_say_to_read_the_log_and_subscribe_only_if_supported() {
        let text = instructions(&Grants::agent_default(), &guidance());
        assert!(
            text.contains("Read the resource omuzaki://log")
                && text.contains("history_log")
                && text.contains("if your client supports resource subscriptions")
        );
        assert!(text.contains("omuzaki://project") && text.contains("omuzaki://character/<id>"));
        assert!(!instructions(&Grants::of([Scope::Render]), &guidance()).contains("omuzaki://project"));
        assert!(text.contains("area_verb (history_undo)") && text.contains("starts with the op name as area.verb"));
    }

    #[test]
    fn instructions_explain_the_heads_up_only_to_sessions_that_can_see_it() {
        let text = instructions(&Grants::agent_default(), &guidance());
        assert!(text.contains("\"Heads-up\"") && text.contains("since_your_last_call") && text.contains("Re-read"));
        assert!(!instructions(&Grants::of([Scope::Render]), &guidance()).contains("Heads-up"));
    }

    #[test]
    fn instructions_list_the_granted_scopes() {
        let text = instructions(&Grants::agent_default(), &guidance());
        assert!(text.contains("read, edit:character, edit:scene, edit:library, edit:settings, render."));
        assert!(!text.contains("publish"));
    }
}
