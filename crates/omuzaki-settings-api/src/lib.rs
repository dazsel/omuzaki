//! The settings contract (SE-10, `docs/design/settings.md`, ADR 0026).
//!
//! A *setting* is a person's taste or a computer's hardware choice that is neither project data nor a secret. This
//! crate defines what a setting is, how a crate declares one, and the [`service::Settings`] trait through which the
//! UI, the CLI, agents and consumers such as the renderer read and change them. The store that writes TOML files is
//! `omuzaki-settings`; `omuzaki-settings-mock` is the in-memory oracle for tests of other crates.
//!
//! | Module | Holds |
//! |---|---|
//! | [`meta`] | the four closed safety enums and [`meta::SettingMeta`] |
//! | [`setting`] | the [`setting::Setting`] trait: a typed key |
//! | [`registry`] | [`register_setting!`], [`registry::registrations`], [`registry::find`] |
//! | [`error`] | [`error::SettingError`] and [`error::SettingFinding`], both naming the key |
//! | [`secret`] | [`secret::SecretRef`]: a reference to a secret, never the value |
//! | [`event`] | [`event::Change`], [`event::SettingsEvent`], [`event::Subscription`] and its [`event::Publisher`] |
//! | [`service`] | [`service::Settings`], [`service::SettingsExt`], [`service::Snapshot`], layers and sources |
//! | [`profile`] | [`profile::ProfileId`], [`profile::ProfileInfo`] and name rules |
//! | [`session`] | [`session::SessionStore`] and the state it keeps (recent projects, windows, crash-loop guard) |
//! | [`check`] | [`check_crate!`]: the test every crate that registers keys runs |
//!
//! # Choices a reviewer may disagree with
//!
//! - **Marker type per key** (D2), so no crate edits a central list and the compiler checks each call site's value
//!   type. The registry is only for the generic readers (Settings page, `settings.list`, the store).
//! - **Four safety enums are closed on purpose.** [`meta::AgentWrite`], [`meta::SettingScope`], [`meta::InProfile`] and
//!   [`meta::Apply`] decide who may write and where a value lives. A gate that matches them must stop compiling when a
//!   variant is added, because a `_ =>` arm would grant or deny silently. Everything that can grow without a safety
//!   meaning (events, causes, sources, errors, records) is `#[non_exhaustive]` and built through constructors
//!   (ADR 0054 section 4).
//! - **[`meta::SettingMeta`] is built by [`meta::SettingMeta::new`], not a struct literal.** The seven fields are all
//!   arguments, so the author still decides each one (the design's "every field is required"), while a later optional
//!   fact is a `with_*` method and does not edit every key in the workspace.
//! - **Raw service methods are `resolve` and `set_raw`**, because [`service::SettingsExt`] owns `get::<K>` and
//!   `set::<K>` and two traits in scope may not share a method name.
//! - **Values are `serde_json::Value`** ([`omuzaki_doc_api::Value`]) at the service boundary so ops and the generated
//!   page need no type per key; TOML is the store's business. [`check`] proves each default survives TOML.
//! - **Time.** Change records carry the monotonic `Seconds` of a `Clock`; session state carries Unix seconds, because
//!   it is read after a restart.
//!
//! No `link()`: this crate registers nothing itself. Crates that call [`register_setting!`] are the registering ones
//! (ADR 0015).

#![deny(missing_docs)]

pub mod check;
pub mod error;
pub mod event;
pub mod meta;
pub mod profile;
pub mod registry;
pub mod secret;
pub mod service;
pub mod session;
pub mod setting;

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
