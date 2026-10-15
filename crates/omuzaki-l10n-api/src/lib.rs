//! Contract for localised text (SE-03, ADR 0023, design `docs/design/l10n-ops.md`).
//!
//! `omuzaki-l10n-api` is an API crate (§39 rule 1). It holds what every crate that says something to a person needs,
//! and the catalog that renders it:
//! - [`Text`] and [`Arg`]: a message id with typed arguments, which records store and every surface renders again in
//!   its own language; [`Message`] is what constructors accept while crates migrate
//! - [`ids`]: the derived ids of op summaries, part kinds and arguments, computed once for everybody
//! - [`messages!`] and [`register_locales!`]: a crate declares its sentences and embeds its Fluent files, and both
//!   register themselves (§39 rule 2)
//! - [`Catalog`]: one locale's text, with the fallback chain, the missing-text report and the pseudo-locale;
//!   [`negotiate()`] picks the locale from the OS preference list
//! - [`Locale`], [`SHIPPED`] and [`OVERRIDES`]: the languages, defined once
//! - [`extract`]: the generated English Fluent file of a package, from its registered text and messages (§3.3, §11)
//! - [`schema_text`]: the argument-path walk and the title rule of an op's or a part kind's schema (§3.1)
//! - [`pseudo()`]: the pseudo-locale transform the design system and the checks share
//!
//! Ids, codes and data are never translated; prose is a message id plus arguments.
#![deny(missing_docs)]

mod catalog;
pub mod extract;
pub mod ids;
mod locale;
mod macros;
mod message;
mod negotiate;
mod pseudo;
mod registry;
pub mod schema_text;
mod text;

pub use catalog::{Catalog, Missing, MissingReason};
pub use locale::{Locale, LocaleError, OVERRIDES, SHIPPED};
pub use message::Message;
pub use negotiate::{negotiate, os_locales};
pub use pseudo::{accent, pseudo};
pub use registry::{ArgKind, MessageDecl, MessageTable, RegisteredText, Resource, TextSource};
pub use text::{Arg, NotFinite, Text, Unit};

// Re-exports the macros expand to, so a caller needs neither `inventory` nor `omuzaki-core` in its own manifest.
#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
