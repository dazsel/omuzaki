//! The `settings.*` ops (`docs/design/settings.md` D4 and section 5, #767): every surface reads and changes settings
//! through these, the Settings window as much as an agent over MCP, so a change is an op in history and never a direct
//! write to the store.
//!
//! | Module | Ops |
//! |---|---|
//! | [`read`] | `settings.get`, `settings.list`, `settings.changes` (queries, scope `read`) |
//! | [`write`] | `settings.set`, `settings.reset`, `settings.reload` (edits, scope `edit:settings`, undone explicitly) |
//! | [`profile`] | `settings.profile.list` (query); `.save`, `.rename`, `.delete`, `.switch` (edits, a person's or confirmed) |
//! | [`session`] | `session.recent`, `session.get` (queries); `session.forget` and its undo `session.restore_recent` (a person's) |
//!
//! Every op reaches the app's `dyn Settings` service, so they work over this crate's [`Store`](crate::Store) and over
//! the in-memory mock alike. A write applies the key's [`AgentWrite`] rule before the service is called, in a dry run
//! too, so a dry run shows what would be asked: `Free` runs, `Confirm` waits for a person, `Never` is a person's alone.
//! The service is called in the run's mode, so a dry run validates the value and returns `before` and `after` but
//! writes nothing.

pub mod profile;
pub mod read;
pub mod session;
pub mod write;

#[cfg(test)]
mod tests;

use omuzaki_ops_api::{Actor, OpCtx, OpError, OpKind, OpMeta, Scope, Undo};
use omuzaki_settings_api::error::{SettingError, SettingErrorKind};
use omuzaki_settings_api::meta::{AgentWrite, SettingMeta};
use omuzaki_settings_api::service::{ListFilter, Resolved, Settings};
use serde_json::{Value, json};

/// The app's settings service.
pub(crate) fn service<'a>(ctx: &OpCtx<'a>) -> Result<&'a dyn Settings, OpError> {
    ctx.service::<dyn Settings>()
}

pub(crate) const fn query(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
}

pub(crate) const fn edit(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::EditSettings, undo: Undo::Explicit }
}

/// Applies `meta`'s agent rule (D4) to the run's actor: a person may write any key; an agent writes a `Free` key, waits
/// for a person on a `Confirm` key, and is refused a `Never` key.
pub(crate) fn may_write(ctx: &OpCtx<'_>, meta: &SettingMeta, verb: &str) -> Result<(), OpError> {
    if matches!(ctx.actor, Actor::Person { .. }) {
        return Ok(());
    }
    match meta.agent {
        AgentWrite::Free => Ok(()),
        AgentWrite::Confirm => ctx.require_person(format!("{verb} the `{}` setting", meta.key)),
        AgentWrite::Never => Err(OpError::person_only(format!(
            "only a person may {verb} the `{}` setting: it is about privacy, devices or what leaves this computer",
            meta.key
        ))),
    }
}

/// The op error for a service error, naming the argument at fault and, for an unknown key, the nearest of the keys
/// `settings` knows.
pub(crate) fn op_error(settings: &dyn Settings, e: &SettingError) -> OpError {
    match &e.kind {
        SettingErrorKind::UnknownKey => {
            let error = OpError::not_found(e.to_string());
            match nearest(settings, &e.key) {
                Some(near) => error.hint(format!("did you mean `{near}`? settings.list names every key")),
                None => error.hint("settings.list names every key"),
            }
        }
        SettingErrorKind::WrongType { .. } | SettingErrorKind::OutOfRange { .. } | SettingErrorKind::Invalid { .. } => {
            OpError::invalid("value", e.to_string())
                .hint(format!("settings.get {{\"key\": \"{}\"}} gives the value's schema and default", e.key))
        }
        SettingErrorKind::NotInProfile => OpError::invalid("layer", e.to_string()).hint("write it with layer \"base\""),
        _ => OpError::internal(e.to_string()),
    }
}

/// The key closest to `wanted`, when one is close enough to be what was meant.
fn nearest(settings: &dyn Settings, wanted: &str) -> Option<&'static str> {
    let wanted = wanted.to_lowercase();
    settings
        .list(&ListFilter::all())
        .iter()
        .map(|resolved| (distance(&wanted, resolved.meta.key), resolved.meta.key))
        .filter(|(d, key)| *d <= key.len().max(wanted.len()) / 3 + 1)
        .min()
        .map(|(_, key)| key)
}

/// Levenshtein distance over characters.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut last = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (last + usize::from(ca != *cb)).min(row[j] + 1).min(row[j + 1] + 1);
            last = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// A setting as the ops answer it: the value, the default, where it came from, and the key's static facts.
pub(crate) fn describe(resolved: &Resolved) -> Value {
    let meta = &resolved.meta;
    json!({
        "key": resolved.key,
        "value": resolved.value,
        "default": resolved.default,
        "modified": resolved.is_modified(),
        "source": resolved.source,
        "summary": meta.summary,
        "area": meta.area(),
        "scope": meta.scope,
        "agent": meta.agent,
        "profile": meta.profile,
        "apply": meta.apply,
        "version": meta.version,
    })
}
