//! Look ops (spec LK-06): the character's look, edited as `look.*` ops.
//!
//! A look is one node, `characters/<id>/look` (`look.toml`): a preset id and the person's overrides
//! (`omuzaki_char_api::LookDef`). Every edit is resolved against the registered presets before anything is written, so
//! an unknown preset or a parameter out of range is refused with the nearest valid name, and each edit replaces the
//! node whole: one patch, so undo restores the previous look exactly.
//!
//! | Module | Holds |
//! |---|---|
//! | [`set_preset`] | `look.set_preset`: draw the character in a preset, clearing or keeping the overrides |
//! | [`set_param`] | `look.set_param`: set or clear one override |
//! | [`list_presets`] | `look.list_presets`: the registered presets, a query |
//! | `access` | reading the look node, writing it back, and the refusals the ops share |

pub mod list_presets;
pub mod set_param;
pub mod set_preset;

pub(crate) mod access;
