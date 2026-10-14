//! The `WWW-Authenticate` challenge a remote MCP request gets when its token is missing or not enough.
//!
//! Follows the MCP authorization specification, revision 2025-11-25, "Protected Resource Metadata Discovery
//! Requirements" and "Runtime Insufficient Scope Errors", and RFC 6750 section 3 for the header syntax. Every
//! parameter is optional in RFC 6750 except the scheme, so the order below is ours: `realm`, `error`, `scope`,
//! `error_description`, `resource_metadata`.
//!
//! Where the spec and the task issue differ, the spec wins: a 401 carries `scope` when there is one (the spec says
//! servers SHOULD send it), not only an insufficient-scope 403.

use hyper::StatusCode;
use hyper::header::HeaderValue;

/// The realm named in every challenge.
const REALM: &str = "omuzaki-mcp";
/// The fallback if a header could not be built. It can't happen: [`quote`] only emits printable ASCII, which a
/// header value always accepts. It exists so [`Challenge::header`] needs no `unwrap`.
const FALLBACK_HEADER: HeaderValue = HeaderValue::from_static("Bearer");

/// Why the request was refused.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// No `Authorization` header came with the request.
    Missing,
    /// The token is unknown, expired or revoked.
    InvalidToken,
    /// The token is valid but doesn't carry the scope this call needs.
    InsufficientScope,
}

/// One refusal, ready to turn into a response.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Challenge<'a> {
    /// What went wrong.
    pub failure: Failure,
    /// A human-readable reason for `error_description`, if any.
    pub description: Option<&'a str>,
    /// Space-separated scope names the client should ask for. Empty means no `scope` parameter.
    pub scope: &'a str,
    /// The URL of the protected resource metadata document, so the client can find the authorization server.
    pub resource_metadata: &'a str,
}

impl Challenge<'_> {
    /// The status code: 403 for an insufficient scope, 401 otherwise (spec "Runtime Insufficient Scope Errors").
    pub(crate) fn status(&self) -> StatusCode {
        match self.failure {
            Failure::InsufficientScope => StatusCode::FORBIDDEN,
            Failure::Missing | Failure::InvalidToken => StatusCode::UNAUTHORIZED,
        }
    }

    /// The `WWW-Authenticate` value, e.g. `Bearer realm="omuzaki-mcp", error="invalid_token", resource_metadata="…"`.
    pub(crate) fn header(&self) -> HeaderValue {
        let mut params = vec![format!("realm=\"{REALM}\"")];
        if let Some(error) = self.error_code() {
            params.push(format!("error=\"{error}\""));
        }
        if !self.scope.is_empty() {
            params.push(format!("scope=\"{}\"", quote(self.scope)));
        }
        if let Some(description) = self.description {
            params.push(format!("error_description=\"{}\"", quote(description)));
        }
        params.push(format!("resource_metadata=\"{}\"", quote(self.resource_metadata)));
        HeaderValue::from_str(&format!("Bearer {}", params.join(", "))).unwrap_or(FALLBACK_HEADER)
    }

    /// The RFC 6750 `error` code, or `None` when the request carried no token at all.
    fn error_code(&self) -> Option<&'static str> {
        match self.failure {
            Failure::Missing => None,
            Failure::InvalidToken => Some("invalid_token"),
            Failure::InsufficientScope => Some("insufficient_scope"),
        }
    }
}

/// Makes `text` safe inside a quoted header parameter: `"` and `\` are backslash-escaped (RFC 9110 quoted-string),
/// and anything outside printable ASCII becomes `?`, so the header always parses.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            ' '..='~' => out.push(ch),
            _ => out.push('?'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: &str = "https://mcp.example.com/.well-known/oauth-protected-resource/mcp";

    fn challenge<'a>(failure: Failure, description: Option<&'a str>, scope: &'a str) -> Challenge<'a> {
        Challenge { failure, description, scope, resource_metadata: META }
    }

    fn text(c: &Challenge<'_>) -> String {
        c.header().to_str().map(str::to_owned).unwrap_or_default()
    }

    #[test]
    fn missing_token_has_no_error_and_names_the_metadata() {
        let c = challenge(Failure::Missing, None, "");
        assert_eq!(c.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(text(&c), format!("Bearer realm=\"omuzaki-mcp\", resource_metadata=\"{META}\""));
    }

    #[test]
    fn invalid_token_is_401_with_its_error() {
        let c = challenge(Failure::InvalidToken, None, "");
        assert_eq!(c.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            text(&c),
            format!("Bearer realm=\"omuzaki-mcp\", error=\"invalid_token\", resource_metadata=\"{META}\"")
        );
    }

    #[test]
    fn invalid_token_with_description() {
        let c = challenge(Failure::InvalidToken, Some("token expired"), "");
        assert_eq!(
            text(&c),
            format!(
                "Bearer realm=\"omuzaki-mcp\", error=\"invalid_token\", error_description=\"token expired\", \
                 resource_metadata=\"{META}\""
            )
        );
    }

    #[test]
    fn insufficient_scope_is_403_with_scope() {
        let c = challenge(Failure::InsufficientScope, None, "edit:character render");
        assert_eq!(c.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            text(&c),
            format!(
                "Bearer realm=\"omuzaki-mcp\", error=\"insufficient_scope\", scope=\"edit:character render\", \
                 resource_metadata=\"{META}\""
            )
        );
    }

    #[test]
    fn insufficient_scope_with_description() {
        let c = challenge(Failure::InsufficientScope, Some("need render"), "render");
        assert_eq!(
            text(&c),
            format!(
                "Bearer realm=\"omuzaki-mcp\", error=\"insufficient_scope\", scope=\"render\", \
                 error_description=\"need render\", resource_metadata=\"{META}\""
            )
        );
    }

    #[test]
    fn missing_token_still_names_the_scope_the_spec_asks_for() {
        let c = challenge(Failure::Missing, None, "read");
        assert_eq!(text(&c), format!("Bearer realm=\"omuzaki-mcp\", scope=\"read\", resource_metadata=\"{META}\""));
    }

    #[test]
    fn quoted_values_escape_quotes_and_backslashes() {
        let c = challenge(Failure::InvalidToken, Some(r#"say "no" \ twice"#), "");
        assert_eq!(
            text(&c),
            format!(
                "Bearer realm=\"omuzaki-mcp\", error=\"invalid_token\", \
                 error_description=\"say \\\"no\\\" \\\\ twice\", resource_metadata=\"{META}\""
            )
        );
    }

    #[test]
    fn non_ascii_and_control_characters_become_question_marks() {
        let c = challenge(Failure::InvalidToken, Some("naïve\ttoken"), "");
        assert!(text(&c).contains("error_description=\"na?ve?token\""));
    }

    #[test]
    fn every_header_is_a_valid_header_value() {
        let descriptions = [None, Some(""), Some("a \"b\" \\c\u{1F600}\r\n")];
        let failures = [Failure::Missing, Failure::InvalidToken, Failure::InsufficientScope];
        for failure in failures {
            for description in descriptions {
                let c = challenge(failure, description, "read edit:scene");
                let parsed = HeaderValue::from_str(c.header().to_str().unwrap_or_default());
                assert!(parsed.is_ok(), "{failure:?} {description:?}");
            }
        }
    }

    #[test]
    fn metadata_url_with_quotes_is_escaped() {
        let c = Challenge {
            failure: Failure::Missing,
            description: None,
            scope: "",
            resource_metadata: r#"https://x.example/"odd"\path"#,
        };
        assert_eq!(text(&c), r#"Bearer realm="omuzaki-mcp", resource_metadata="https://x.example/\"odd\"\\path""#);
    }
}
