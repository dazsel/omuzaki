//! `Message`: what an error or finding constructor accepts while crates migrate to declared text (ADR 0023
//! decision 5).

use crate::Text;

/// Either raw English (a crate not yet migrated) or a declared [`Text`].
///
/// Constructors take `impl Into<Message>`, so `OpError::not_found("…")` keeps compiling while a crate moves to
/// `messages!`. When every crate is strict, the English conversions are removed.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// English written in the code; has no id and so cannot be translated.
    English(String),
    /// A declared message.
    Text(Text),
}

impl From<&str> for Message {
    fn from(english: &str) -> Self {
        Self::English(english.to_owned())
    }
}

impl From<String> for Message {
    fn from(english: String) -> Self {
        Self::English(english)
    }
}

impl From<Text> for Message {
    fn from(text: Text) -> Self {
        Self::Text(text)
    }
}
