//! The `io.export_animation` op (`docs/design/vrma-export.md` §5): a library clip or an emote, on a character's own
//! skeleton, written as a motion file. It lives with the stage because the stage is where a character's body and the
//! pose library meet; the format itself is `omuzaki-io-api`'s animation exporter registry.

pub mod export_animation;
