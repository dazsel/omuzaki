//! What the macros submit to `inventory`, and what registries contribute: the data side of self-registered text
//! (ADR 0023 decision 4, design section 13). The catalog and the checks read these.

use schemars::Schema;

/// The kind of value a message argument takes. The variants match the [`Arg`](crate::Arg) variants, so `messages!`
/// can name them directly and a misspelt kind does not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArgKind {
    /// An id shown as it is.
    Id,
    /// A count; drives plurals.
    Int,
    /// A measured number.
    Num,
    /// A number with a unit.
    Qty,
    /// A nested [`Text`](crate::Text).
    Text,
    /// A list of arguments.
    List,
    /// English that stays English.
    Verbatim,
}

/// One declared message, as `messages!` records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageDecl {
    /// The message id (`char-part-not-found`).
    pub id: &'static str,
    /// The English source, in Fluent syntax.
    pub english: &'static str,
    /// The doc lines of the declaration, one per line and still carrying the space after `///`. Use
    /// [`note_text`](Self::note_text) to read them. They are the translator note, and the check requires one.
    pub note: &'static str,
    /// Argument names and kinds, in declaration order.
    pub args: &'static [(&'static str, ArgKind)],
    /// Marked `#[verbatim]`: a key legend or format hint that the pseudo-locale and translators leave as it is.
    pub verbatim: bool,
}

impl MessageDecl {
    /// The translator note: the doc lines trimmed and joined with single spaces.
    pub fn note_text(&self) -> String {
        self.note.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ")
    }
}

/// The messages of one `messages!` invocation, submitted to `inventory` with the package that wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageTable {
    /// The package whose code invoked the macro (`CARGO_PKG_NAME`).
    pub package: &'static str,
    /// Its messages in source order.
    pub messages: &'static [MessageDecl],
}

inventory::collect!(MessageTable);

/// One embedded Fluent file, as `register_locales!` submits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resource {
    /// The package whose `locales/` directory holds it.
    pub package: &'static str,
    /// The language directory it came from, as written in the macro (`ja`, `zh-Hans`).
    pub locale: &'static str,
    /// The file's Fluent source.
    pub source: &'static str,
}

inventory::collect!(Resource);

/// One piece of registered text a registry contributes: an op's summary, a part kind's title, an argument.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisteredText {
    /// The derived id of the item that owns the text (`op-face-set_profile`, `kind-twin_tails`).
    pub owner: String,
    /// The English value (the summary, the title).
    pub value: String,
    /// Attributes by name without the dot (`description`, `summary`) and their English values.
    pub attributes: Vec<(String, String)>,
    /// The schema of the owner's arguments, walked for titles and descriptions; `None` when it has none.
    pub schema: Option<Schema>,
    /// Where the text is shown and how much room it has, for the translators' context.
    pub context: String,
}

/// A registry's contribution to extraction: ops, part kinds and later registries each submit one of these. It lists
/// the text of the items registered by `package`, so a crate's English file is generated from its own items only.
pub struct TextSource {
    /// The text of every item `package` registered.
    pub text: fn(package: &str) -> Vec<RegisteredText>,
}

inventory::collect!(TextSource);
