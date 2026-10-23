//! `omuzaki credential`: the credentials headless agents run from (design `agent-sessions.md` 7.6). It reads and
//! writes the project's private credential records directly, without opening the project, so it works while the
//! studio has the project open.

use std::path::Path;

use omuzaki_app::credentials;
use omuzaki_mcp::TokenRecord;
use omuzaki_ops_api::{Actor, Budget, Grants, OpError};
use serde_json::{Value, json};

use crate::cli::CredentialCommand;
use crate::session::TOKEN_VAR;

/// Runs `action` on the credentials of the project in `project`, for `person`. The secret of a credential just issued
/// is in the reply, the only time it is shown.
///
/// # Errors
/// What [`credentials`] reports: a label already used, a record that can't be read or stored.
pub fn run(project: Option<&Path>, action: &CredentialCommand, person: Actor, json: bool) -> Result<String, OpError> {
    let project = project.ok_or_else(|| {
        OpError::invalid("project", "a credential belongs to a project")
            .hint("pass the project folder: omuzaki --project <DIR> credential ...")
    })?;
    if !project.join("project.toml").is_file() {
        return Err(OpError::not_found(format!("no project in {}", project.display())));
    }
    match action {
        CredentialCommand::Issue { label, grant, spend_cap } => {
            let grants = grant.iter().fold(Grants::agent_default(), |grants, scope| grants.with(*scope));
            let grants = if spend_cap.is_some() { grants.with(omuzaki_ops_api::Scope::Spend) } else { grants };
            let budget = Budget { usd_micros: spend_cap.unwrap_or(0), ..Budget::default() };
            let issued = credentials::issue(project, label, grants, budget, person)?;
            if json {
                let mut body = described(issued.record());
                body["secret"] = json!(issued.secret());
                return crate::json::pretty(&body);
            }
            let record = issued.record();
            Ok(format!(
                "Issued the credential \"{}\" ({}).\n\n{}\n\nThis is the only time the secret is shown. Give it to the script or CI job in \
                 {TOKEN_VAR}, or in a file for `omuzaki mcp --token-file`; anyone who has it can do what it grants. Revoke \
                 it with `omuzaki credential revoke \"{}\"`.\n",
                record.label,
                summary(record),
                issued.secret(),
                record.label,
            ))
        }
        CredentialCommand::List => {
            let records = credentials::list(project)?;
            if json {
                return crate::json::pretty(&Value::Array(records.iter().map(described).collect()));
            }
            if records.is_empty() {
                return Ok("No credentials. Issue one with `omuzaki credential issue --label <NAME>`.\n".to_owned());
            }
            Ok(records.iter().map(|r| format!("{}\t{}\n", r.label, summary(r))).collect())
        }
        CredentialCommand::Revoke { label } => {
            if !credentials::revoke(project, label)? {
                let known: Vec<String> = credentials::list(project)?.into_iter().map(|r| r.label).collect();
                let hint = if known.is_empty() {
                    "there are none".to_owned()
                } else {
                    format!("there are: {}", known.join(", "))
                };
                return Err(OpError::not_found(format!("no credential is labelled \"{label}\"")).hint(hint));
            }
            if json {
                return crate::json::pretty(&json!({ "revoked": label }));
            }
            Ok(format!("Revoked the credential \"{label}\": its secret is refused from now on.\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_names_the_grants_and_the_cap_exactly() {
        let issued = |usd_micros| {
            omuzaki_mcp::IssuedCredential::new(
                "ci",
                Grants::agent_default(),
                Actor::System,
                Budget { usd_micros, ..Budget::default() },
            )
            .unwrap()
        };
        assert!(summary(issued(0).record()).ends_with("; spends nothing"));
        assert!(summary(issued(2_500_000).record()).ends_with("; spends up to $2.50"));
        assert!(summary(issued(999_999).record()).ends_with("; spends up to $0.999999"));
        assert!(summary(issued(3_000_000).record()).starts_with("grants read, edit"));
    }
}

/// A record as `--json` shows it: never the hash.
fn described(record: &TokenRecord) -> Value {
    let scopes: Vec<&str> = record.grants.scopes().map(|s| s.name()).collect();
    let usd = record.budget.map_or(0, |b| b.usd_micros);
    json!({ "label": record.label, "grants": scopes, "spend_cap_usd_micros": usd })
}

/// `grants read, edit, ...; spends up to $2.50` for the text output.
fn summary(record: &TokenRecord) -> String {
    let scopes: Vec<&str> = record.grants.scopes().map(|s| s.name()).collect();
    let usd = record.budget.map_or(0, |b| b.usd_micros);
    let spend = if usd == 0 {
        "spends nothing".to_owned()
    } else {
        let cents = format!("{:06}", usd % 1_000_000);
        let cents = cents.trim_end_matches('0');
        format!("spends up to ${}.{cents:0<2}", usd / 1_000_000)
    };
    format!("grants {}; {spend}", scopes.join(", "))
}
