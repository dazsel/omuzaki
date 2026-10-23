//! Face-shape ops (CB-10): `face.set_profile` blends a face-shape preset (or a profile given inline) into a character's
//! head outline and feature layout in one patch, and `face.list_profiles` lists the presets with their numbers.
//!
//! A profile touches two places: the head part's `params/outline` and the character's `face/layout`
//! (`omuzaki_char_api::ShapeProfile`). The document stores the blended numbers, never a reference to the preset, so undo
//! is exact and editing the library never changes a character; the head's `preset` records the preset the outline was
//! set from, and only when it was set in full (`blend = 1`), because a partial blend is no longer that preset.
//!
//! Each plan answers with the head's front silhouette and the layout's ratios before and after (CB-12), so an agent can
//! check "more oval" by its numbers rather than by eye.
//!
//! | Module | Holds |
//! |---|---|
//! | [`set`] | `face.set_profile` |
//! | [`list`] | `face.list_profiles`, a query |
//! | `report` | the silhouette and ratio numbers both answer with |

pub mod list;
pub mod set;

mod report;
