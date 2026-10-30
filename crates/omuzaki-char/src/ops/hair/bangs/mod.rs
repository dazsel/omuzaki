//! The bangs designer ops (CB-31): a hair part's `bangs` group edited through the contract, never by hand-writing
//! the generator's parameters.
//!
//! A bangs group is one `BangsGen` at `groups/<group>/generator` of a hair part. Every op reads the group through
//! [`access`], checks the whole `BangsGen` it would write ([`access::check`]) and writes the generator back as one
//! patch, so undo restores it exactly and a refusal changes nothing.
//!
//! | Module | Holds |
//! |---|---|
//! | [`set`] | `hair.bangs.set`: one field of a group (layers, clumps, tip heights, sweep, hook, wisps) |
//! | [`preset`] | `hair.bangs.preset`: restyle a group with one of the presets |
//! | [`list_presets`] | `hair.bangs.list_presets`: the presets, a query |
//! | [`presets`] | the six presets' `BangsGen` values: `blunt`, `side-swept`, `curtain`, `wispy`, `anime`, `none` |
//! | [`access`] | finding a bangs group, checking and writing it back, and the refusals the ops share |
//!
//! Brows showing through the fringe is `HairShading::brows_through`, set with `hair.set_shading`, not here.

pub mod list_presets;
pub mod preset;
pub mod presets;
pub mod set;

pub mod access;
