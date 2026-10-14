//! The OAuth protected resource metadata document a remote MCP client reads to find the authorization server.
//!
//! Follows RFC 9728 (OAuth 2.0 Protected Resource Metadata) and the MCP authorization specification, revision
//! 2025-11-25, "Protected Resource Metadata Discovery Requirements". The spec lets a server serve the document at the
//! well-known URI with the resource's path inserted after it (`/.well-known/oauth-protected-resource/mcp`) or at the
//! root (`/.well-known/oauth-protected-resource`). A client tries the inserted form first, so we list it first.

use hyper::Uri;
use omuzaki_ops_api::Scope;
use serde_json::{Value, json};

/// The well-known prefix RFC 9728 section 3.1 defines.
const WELL_KNOWN: &str = "/.well-known/oauth-protected-resource";

/// The metadata document for `resource`: the RFC 9728 fields this server uses, with bearer tokens sent only in the
/// `Authorization` header. A list with no entries is left out rather than sent as `[]`: RFC 9728 has no meaning for an
/// empty list, and a client may read it as "no scopes" when the server simply has not listed any.
pub(crate) fn metadata_json(resource: &str, authorization_servers: &[String], scopes: &[Scope]) -> Value {
    let mut document = json!({
        "resource": resource,
        "bearer_methods_supported": ["header"],
    });
    if !authorization_servers.is_empty() {
        document["authorization_servers"] = json!(authorization_servers);
    }
    if !scopes.is_empty() {
        let scopes_supported: Vec<&str> = scopes.iter().map(|scope| scope.name()).collect();
        document["scopes_supported"] = json!(scopes_supported);
    }
    document
}

/// The paths a client may request the metadata document from, in the order it should try them.
///
/// RFC 9728 section 3.1 says the resource's path is inserted after the well-known prefix with any terminating `/`
/// removed, so `/mcp/` and `/mcp` both give `/.well-known/oauth-protected-resource/mcp`. A resource at the root
/// (`/`) has only the root form, because inserting an empty path would give a double slash.
pub(crate) fn metadata_paths(resource: &Uri) -> Vec<String> {
    let path = resource.path().trim_end_matches('/');
    if path.is_empty() {
        return vec![WELL_KNOWN.to_owned()];
    }
    vec![format!("{WELL_KNOWN}{path}"), WELL_KNOWN.to_owned()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_document_is_exact() {
        let document = metadata_json(
            "https://mcp.example.com/mcp",
            &["https://auth.example.com".to_owned()],
            &[Scope::Read, Scope::EditCharacter],
        );
        assert_eq!(
            document,
            json!({
                "resource": "https://mcp.example.com/mcp",
                "authorization_servers": ["https://auth.example.com"],
                "scopes_supported": ["read", "edit:character"],
                "bearer_methods_supported": ["header"],
            })
        );
    }

    #[test]
    fn metadata_document_omits_empty_lists() {
        let document = metadata_json("https://mcp.example.com/", &[], &[]);
        assert_eq!(
            document,
            json!({
                "resource": "https://mcp.example.com/",
                "bearer_methods_supported": ["header"],
            })
        );
    }

    #[test]
    fn paths_for_a_resource_with_a_path_try_the_inserted_form_first() {
        let uri = Uri::from_static("https://mcp.example.com/mcp");
        assert_eq!(
            metadata_paths(&uri),
            vec![
                "/.well-known/oauth-protected-resource/mcp".to_owned(),
                "/.well-known/oauth-protected-resource".to_owned(),
            ]
        );
    }

    #[test]
    fn paths_for_the_root_have_only_the_root_form() {
        let uri = Uri::from_static("https://mcp.example.com/");
        assert_eq!(metadata_paths(&uri), vec!["/.well-known/oauth-protected-resource".to_owned()]);
    }

    #[test]
    fn paths_drop_a_terminating_slash_before_insertion() {
        let uri = Uri::from_static("https://mcp.example.com/mcp/");
        assert_eq!(
            metadata_paths(&uri),
            vec![
                "/.well-known/oauth-protected-resource/mcp".to_owned(),
                "/.well-known/oauth-protected-resource".to_owned(),
            ]
        );
    }

    #[test]
    fn paths_ignore_the_query_string() {
        let uri = Uri::from_static("https://mcp.example.com/mcp?x=1");
        assert_eq!(metadata_paths(&uri)[0], "/.well-known/oauth-protected-resource/mcp");
    }
}
