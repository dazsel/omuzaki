//! Credentials a person issues in advance for headless agents (design `agent-sessions.md` 7.6): `omuzaki mcp` with no
//! studio has nobody to ask, so a script or CI job runs from a credential whose grants and budget the person set
//! beforehand.
//!
//! The records live in the project's private folder (`.omuzaki/credentials.json`, owner-only), each holding a keyed
//! hash of its secret, never the secret: whoever reads the folder can't connect. The secret is shown once, when it is
//! issued. Labels are unique, so a credential is revoked by its label.

#![allow(
    clippy::disallowed_methods,
    reason = "host code, not an op: the core's own files in the project's `.omuzaki` folder; the fence is for ops"
)]

use std::path::{Path, PathBuf};

use omuzaki_mcp::{IssuedCredential, TokenRecord};
use omuzaki_ops_api::{Actor, Budget, Grants, OpError};

use crate::studio::{PRIVATE_DIR, to_json, write_private};

/// The records of the credentials issued for this project.
const CREDENTIALS_FILE: &str = "credentials.json";
/// The longest label, in characters.
const MAX_LABEL: usize = 64;

fn credentials_file(project: &Path) -> PathBuf {
    project.join(PRIVATE_DIR).join(CREDENTIALS_FILE)
}

/// Issues a credential for `project` labelled `label`, whose headless sessions hold `grants` and start from `budget`,
/// on behalf of `issuer`, and stores its record. The secret is in the result only: show it to the person once.
///
/// # Errors
/// `invalid` for an empty or overlong label or one already used, and `internal` when the record can't be stored or
/// the system's random source fails.
pub fn issue(
    project: &Path,
    label: &str,
    grants: Grants,
    budget: Budget,
    issuer: Actor,
) -> Result<IssuedCredential, OpError> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL || label.chars().any(char::is_control) {
        return Err(OpError::invalid("label", format!("a label is 1 to {MAX_LABEL} characters of text")));
    }
    let mut records = list(project)?;
    if records.iter().any(|r| r.label == label) {
        return Err(OpError::invalid("label", format!("a credential is already labelled \"{label}\""))
            .hint("pick another label, or revoke that credential first"));
    }
    let credential =
        IssuedCredential::new(label, grants, issuer, budget).map_err(|e| OpError::internal(e.to_string()))?;
    records.push(credential.record().clone());
    store(project, &records)?;
    Ok(credential)
}

/// The records of every credential issued for `project`, oldest first.
///
/// # Errors
/// `internal` when the file is there but can't be read or parsed: nothing is assumed about credentials that can't be
/// read.
pub fn list(project: &Path) -> Result<Vec<TokenRecord>, OpError> {
    let file = credentials_file(project);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(OpError::internal(format!("the project's credentials can't be read: {error}"))),
    };
    serde_json::from_str(&text).map_err(|error| {
        OpError::internal(format!("the project's credentials can't be read: {error}"))
            .hint("issue the credentials again: delete .omuzaki/credentials.json in the project folder")
    })
}

/// The credential of `project` that `secret` presents, if one is issued and not revoked.
///
/// # Errors
/// As [`list`].
pub fn find(project: &Path, secret: &str) -> Result<Option<TokenRecord>, OpError> {
    Ok(list(project)?.into_iter().find(|record| record.is_presented_by(secret)))
}

/// Revokes the credential labelled `label`: its record is removed, so its secret is refused from now on. Returns
/// whether there was one.
///
/// # Errors
/// As [`list`], and `internal` when the records can't be stored.
pub fn revoke(project: &Path, label: &str) -> Result<bool, OpError> {
    let mut records = list(project)?;
    let before = records.len();
    records.retain(|r| r.label != label.trim());
    if records.len() == before {
        return Ok(false);
    }
    store(project, &records)?;
    Ok(true)
}

fn store(project: &Path, records: &[TokenRecord]) -> Result<(), OpError> {
    to_json(&records)
        .and_then(|text| write_private(&credentials_file(project), &text))
        .map_err(|error| OpError::internal(format!("the project's credentials can't be stored: {error}")))
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::Scope;

    use super::*;

    fn person() -> Actor {
        Actor::Person { name: "tester".into() }
    }

    #[test]
    fn an_issued_credential_is_found_by_its_secret_until_it_is_revoked() {
        let tmp = tempfile::tempdir().unwrap();
        let budget = Budget { usd_micros: 1_500_000, tokens: None, seconds: None };
        let grants = Grants::agent_default().with(Scope::Publish);
        let issued = issue(tmp.path(), " ci ", grants.clone(), budget, person()).unwrap();
        let stored = std::fs::read_to_string(credentials_file(tmp.path())).unwrap();
        assert!(!stored.contains(issued.secret()), "the secret is never stored");
        let found = find(tmp.path(), issued.secret()).unwrap().expect("its secret finds it");
        assert_eq!((found.label.as_str(), &found.grants, found.budget), ("ci", &grants, Some(budget)));
        assert_eq!(find(tmp.path(), "omuzaki_mcp_wrong").unwrap(), None);

        let taken = issue(tmp.path(), "ci", Grants::agent_default(), budget, person()).unwrap_err();
        assert!(taken.message.contains("already labelled"), "{taken:?}");
        assert!(issue(tmp.path(), "  ", Grants::agent_default(), budget, person()).is_err());

        assert!(revoke(tmp.path(), "ci").unwrap());
        assert!(!revoke(tmp.path(), "ci").unwrap(), "only once");
        assert_eq!(find(tmp.path(), issued.secret()).unwrap(), None, "a revoked credential is refused");
    }

    #[cfg(unix)]
    #[test]
    fn the_records_are_readable_by_their_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        issue(tmp.path(), "ci", Grants::agent_default(), Budget::default(), person()).unwrap();
        let mode = std::fs::metadata(credentials_file(tmp.path())).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }

    #[test]
    fn unreadable_records_are_an_error_not_an_empty_list() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(PRIVATE_DIR)).unwrap();
        std::fs::write(credentials_file(tmp.path()), "not json").unwrap();
        assert!(list(tmp.path()).is_err());
        assert!(find(tmp.path(), "omuzaki_mcp_any").is_err());
    }
}
