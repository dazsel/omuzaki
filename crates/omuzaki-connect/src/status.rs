//! Whether a connector can run now: one function, so `connect.list`, the Connectors page and the preview agree.

use omuzaki_connect_api::{ConnectorConfig, Status, Target, Transport};

use crate::set::Entry;

/// The status of `entry` under `config`, in this order: not `enabled` is [`Status::Disabled`] (every connector starts
/// disabled); a target that did not resolve (`None`), or an MCP connector with neither a command line nor an endpoint,
/// is [`Status::Unconfigured`]; a key label whose key is not in the keychain is [`Status::NeedsKey`]; a
/// `requires_plan` list without the configured plan is [`Status::NeedsPlan`] (it runs, but its outputs are the
/// vendor's: D6); otherwise [`Status::Ready`].
pub fn status(entry: &Entry, config: &ConnectorConfig, target: Option<&Target>, key_present: bool) -> Status {
    let m = &entry.manifest;
    if !config.enabled {
        return Status::Disabled;
    }
    let Some(target) = target else {
        return Status::Unconfigured;
    };
    if m.transport == Transport::Mcp && config.command.is_none() && target.endpoint.is_none() {
        return Status::Unconfigured;
    }
    if target.secret.is_some() && !key_present {
        return Status::NeedsKey;
    }
    if !plan_allows(&m.licence.requires_plan, config.plan.as_deref()) {
        return Status::needs_plan(m.licence.requires_plan.clone());
    }
    Status::Ready
}

/// Whether `plan` is one `requires_plan` allows: always when the list is empty.
pub fn plan_allows(requires_plan: &[String], plan: Option<&str>) -> bool {
    requires_plan.is_empty() || plan.is_some_and(|p| requires_plan.iter().any(|r| r == p))
}
