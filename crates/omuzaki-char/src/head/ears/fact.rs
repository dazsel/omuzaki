//! The ear cover as a fact: the disc of head each shell ear hides, published by the `head` part so that a hair part
//! reads it through `ctx.published` (a new fact, not a new field on the shared `Publishes` structs).

use super::{EarCover, Side};

omuzaki_char_api::fact! {
    /// The disc each shell ear hides, keyed by side (published by `head` beside the ear anchors). Hair that notches
    /// round an ear, or tucks locks behind it, reads it; a head with no shell ear publishes none. Its centre and radius
    /// are real-size metres at the character's stature, like the anchors beside it (`head` scales them by the body's
    /// stature before publishing); its normal is a direction.
    pub EarCoverFact = "ear_cover", omuzaki_char_api::Keyed<Side, EarCover>;
}

omuzaki_char_api::register_fact!(EarCoverFact);
// Compared with `==`: a side and a disc of plain floats, with no trait object to compare by its text.
omuzaki_char_api::register_fact_probe!(EarCoverFact);
