//! Readability measures (VF-06, spec §15 and §32): whether a character reads on stream, as numbers with gates.
//!
//! | Module | Rule (spec §32) | Measure |
//! |---|---|---|
//! | [`silhouette`] | readable silhouette | the filled outline at 64 px against other characters' (intersection over union), and a distinctiveness score |
//! | [`emote`] | the face reads at emote size | expressions at 112, 56 and 28 px over a stream's dark ground: which pairs read the same, and the eyes' and mouth's contrast against the skin |
//!
//! Like the likeness measures they take images, never a character, so a drawing and a render run through the same code,
//! and an agent can check its work in any style. The third rule, a palette's value contrast with colour-blind
//! simulation, is measured on a character's palette by `palette.check_contrast` (LK-03, `omuzaki-char`).

pub mod emote;
pub mod silhouette;
