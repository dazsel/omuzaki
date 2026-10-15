//! The answer to the server's authentication challenge.
//!
//! Nothing here is ever logged or put in an error: the password and the secrets derived from it stay in these
//! functions' locals and in the returned string.

use base64::{Engine, prelude::BASE64_STANDARD};
use sha2::{Digest, Sha256};

fn sha256_base64(first: &str, second: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(first.as_bytes());
    hasher.update(second.as_bytes());
    BASE64_STANDARD.encode(hasher.finalize())
}

/// The base64 secret: `base64(sha256(password + salt))`, the first step of the answer.
#[must_use]
pub fn secret(password: &str, salt: &str) -> String {
    sha256_base64(password, salt)
}

/// The `authentication` string for `Identify`: `base64(sha256(base64(sha256(password + salt)) + challenge))`, where
/// `salt` and `challenge` come from the server's `Hello`.
#[must_use]
pub fn auth_response(password: &str, salt: &str, challenge: &str) -> String {
    sha256_base64(&secret(password, salt), challenge)
}
